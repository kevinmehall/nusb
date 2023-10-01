use std::{
    ffi::c_void,
    mem::{self, ManuallyDrop},
    ptr::null_mut,
};

use rustix::io::Errno;

use crate::{
    control::{ControlIn, ControlOut, SETUP_PACKET_SIZE},
    transfer_internal, Completion, EndpointType, TransferStatus,
};

use super::usbfs::{
    Urb, USBDEVFS_URB_TYPE_BULK, USBDEVFS_URB_TYPE_CONTROL, USBDEVFS_URB_TYPE_INTERRUPT,
    USBDEVFS_URB_TYPE_ISO,
};

/// Linux-specific transfer state.
///
/// This logically contains a `Vec` with urb.buffer and capacity.
/// It also owns the `urb` allocation itself, which is stored out-of-line
/// to avoid violating noalias when submitting the transfer while holding
/// `&mut TransferData`.
#[repr(C)]
pub(crate) struct TransferData {
    urb: *mut Urb,
    capacity: usize,
}

unsafe impl Send for TransferData {}

impl TransferData {
    fn urb_mut(&mut self) -> &mut Urb {
        // SAFETY: if we have `&mut`, the transfer is not pending
        unsafe { &mut *self.urb }
    }

    fn fill(&mut self, v: Vec<u8>, len: usize, user_data: *mut c_void) {
        let mut v = ManuallyDrop::new(v);
        let urb = self.urb_mut();
        urb.buffer = v.as_mut_ptr();
        urb.buffer_length = len.try_into().expect("buffer size should fit in i32");
        urb.usercontext = user_data;
        urb.actual_length = 0;
        self.capacity = v.capacity();
    }

    /// SAFETY: requires that the transfer has completed and `length` bytes are initialized
    unsafe fn take_buf(&mut self, length: usize) -> Vec<u8> {
        let urb = self.urb_mut();
        assert!(!urb.buffer.is_null());
        let ptr = mem::replace(&mut urb.buffer, null_mut());
        let capacity = mem::replace(&mut self.capacity, 0);
        assert!(length <= capacity);
        Vec::from_raw_parts(ptr, length, capacity)
    }
}

impl Drop for TransferData {
    fn drop(&mut self) {
        unsafe {
            if !self.urb_mut().buffer.is_null() {
                drop(Vec::from_raw_parts(self.urb_mut().buffer, 0, self.capacity));
            }
            drop(Box::from_raw(self.urb));
        }
    }
}

impl transfer_internal::Platform for super::Interface {
    type TransferData = TransferData;

    fn make_transfer_data(&self, endpoint: u8, ep_type: crate::EndpointType) -> TransferData {
        let ep_type = match ep_type {
            EndpointType::Control => USBDEVFS_URB_TYPE_CONTROL,
            EndpointType::Interrupt => USBDEVFS_URB_TYPE_INTERRUPT,
            EndpointType::Bulk => USBDEVFS_URB_TYPE_BULK,
            EndpointType::Isochronous => USBDEVFS_URB_TYPE_ISO,
        };

        TransferData {
            urb: Box::into_raw(Box::new(Urb {
                ep_type,
                endpoint,
                status: 0,
                flags: 0,
                buffer: null_mut(),
                buffer_length: 0,
                actual_length: 0,
                start_frame: 0,
                number_of_packets_or_stream_id: 0,
                error_count: 0,
                signr: 0,
                usercontext: null_mut(),
            })),
            capacity: 0,
        }
    }

    fn cancel(&self, data: &TransferData) {
        unsafe {
            self.cancel_urb(data.urb);
        }
    }
}

impl transfer_internal::PlatformSubmit<Vec<u8>> for super::Interface {
    unsafe fn submit(&self, data: Vec<u8>, transfer: &mut TransferData, user_data: *mut c_void) {
        let ep = transfer.urb_mut().endpoint;
        let len = if ep & 0x80 == 0 {
            data.len()
        } else {
            data.capacity()
        };
        transfer.fill(data, len, user_data);

        // SAFETY: we just properly filled the buffer and it is not already pending
        unsafe { self.submit_urb(transfer.urb) }
    }

    unsafe fn take_completed(transfer: &mut TransferData) -> Completion<Vec<u8>> {
        let status = urb_status(transfer.urb_mut());
        let len = transfer.urb_mut().actual_length as usize;

        // SAFETY: transfer is completed (precondition) and `actual_length` bytes were initialized.
        let data = unsafe { transfer.take_buf(len) };
        Completion { data, status }
    }
}

impl transfer_internal::PlatformSubmit<ControlIn> for super::Interface {
    unsafe fn submit(&self, data: ControlIn, transfer: &mut TransferData, user_data: *mut c_void) {
        let buf_len = SETUP_PACKET_SIZE + data.length as usize;
        let mut buf = Vec::with_capacity(buf_len);
        buf.extend_from_slice(&data.setup_packet());
        transfer.fill(buf, buf_len, user_data);

        // SAFETY: we just properly filled the buffer and it is not already pending
        unsafe { self.submit_urb(transfer.urb) }
    }

    unsafe fn take_completed(transfer: &mut TransferData) -> Completion<Vec<u8>> {
        let status = urb_status(transfer.urb_mut());
        let len = transfer.urb_mut().actual_length as usize;

        // SAFETY: transfer is completed (precondition) and `actual_length`
        // bytes were initialized with setup buf in front
        let mut data = unsafe { transfer.take_buf(SETUP_PACKET_SIZE + len) };
        data.splice(0..SETUP_PACKET_SIZE, []);
        Completion { data, status }
    }
}

impl transfer_internal::PlatformSubmit<ControlOut<'_>> for super::Interface {
    unsafe fn submit(&self, data: ControlOut, transfer: &mut TransferData, user_data: *mut c_void) {
        let buf_len = SETUP_PACKET_SIZE + data.data.len();
        let mut buf = Vec::with_capacity(buf_len);
        buf.extend_from_slice(
            &data
                .setup_packet()
                .expect("data length should fit in setup packet's u16"),
        );
        buf.extend_from_slice(data.data);
        transfer.fill(buf, buf_len, user_data);

        // SAFETY: we just properly filled the buffer and it is not already pending
        unsafe { self.submit_urb(transfer.urb) }
    }

    unsafe fn take_completed(transfer: &mut TransferData) -> Completion<usize> {
        let status = urb_status(transfer.urb_mut());
        let len = transfer.urb_mut().actual_length as usize;
        drop(transfer.take_buf(0));
        Completion { data: len, status }
    }
}

fn urb_status(urb: &Urb) -> TransferStatus {
    if urb.status == 0 {
        return TransferStatus::Complete;
    }

    // It's sometimes positive, sometimes negative, but rustix panics if negative.
    match Errno::from_raw_os_error(urb.status.abs()) {
        Errno::NODEV | Errno::SHUTDOWN => TransferStatus::Disconnected,
        Errno::PIPE => TransferStatus::Stall,
        Errno::NOENT | Errno::CONNRESET => TransferStatus::Cancelled,
        Errno::PROTO | Errno::ILSEQ | Errno::OVERFLOW | Errno::COMM | Errno::TIME => {
            TransferStatus::Fault
        }
        _ => TransferStatus::UnknownError,
    }
}
