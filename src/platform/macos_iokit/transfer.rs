use std::{
    ffi::c_void,
    mem::{self, ManuallyDrop},
    ptr::null_mut,
    sync::Arc,
};

use io_kit_sys::ret::{kIOReturnNoDevice, kIOReturnSuccess, IOReturn};
use log::{error, info};

use crate::{
    platform::macos_iokit::iokit_c::IOUSBDevRequest,
    transfer::{
        notify_completion, Completion, ControlIn, ControlOut, PlatformSubmit, PlatformTransfer,
        RequestBuffer, ResponseBuffer, TransferError,
    },
};

use super::{iokit::call_iokit_function, iokit_usb::EndpointInfo};

extern "C" fn transfer_callback(refcon: *mut c_void, result: IOReturn, len: *mut c_void) {
    info!(
        "Completion callback for transfer {refcon:?}, status={result:x}, len={len}",
        len = len as usize
    );

    unsafe {
        let callback_data = {
            let inner = &mut *(refcon as *mut TransferDataInner);
            inner.actual_len = len as usize;
            inner.status = result;
            inner.callback_data
        };
        notify_completion::<super::TransferData>(callback_data)
    }
}

pub struct TransferData {
    pipe_ref: u8,
    buf: *mut u8,
    capacity: usize,
    inner: *mut TransferDataInner,
    device: Arc<super::Device>,
    interface: Option<Arc<super::Interface>>,
}

impl Drop for TransferData {
    fn drop(&mut self) {
        if !self.buf.is_null() {
            unsafe { drop(Vec::from_raw_parts(self.buf, 0, self.capacity)) }
        }
        unsafe { drop(Box::from_raw(self.inner)) }
    }
}

/// Bring the data accessed on the transfer callback out-of-line
/// so that we can have a reference to it while the callback may
/// write to other fields concurrently. This could be included
/// in TransferData with the proposed [UnsafePinned](https://github.com/rust-lang/rfcs/pull/3467)
pub struct TransferDataInner {
    actual_len: usize,
    callback_data: *mut c_void,
    status: IOReturn,
}

impl TransferData {
    pub(super) fn new(
        device: Arc<super::Device>,
        interface: Arc<super::Interface>,
        endpoint: &EndpointInfo,
    ) -> TransferData {
        TransferData {
            pipe_ref: endpoint.pipe_ref,
            buf: null_mut(),
            capacity: 0,
            inner: Box::into_raw(Box::new(TransferDataInner {
                actual_len: 0,
                callback_data: null_mut(),
                status: kIOReturnSuccess,
            })),
            device,
            interface: Some(interface),
        }
    }

    pub(super) fn new_control(device: Arc<super::Device>) -> TransferData {
        TransferData {
            pipe_ref: 0,
            buf: null_mut(),
            capacity: 0,
            inner: Box::into_raw(Box::new(TransferDataInner {
                actual_len: 0,
                callback_data: null_mut(),
                status: kIOReturnSuccess,
            })),
            device,
            interface: None,
        }
    }

    /// SAFETY: Requires that the transfer is not active
    unsafe fn fill(&mut self, buf: Vec<u8>, callback_data: *mut c_void) {
        let mut buf = ManuallyDrop::new(buf);
        self.buf = buf.as_mut_ptr();
        self.capacity = buf.capacity();

        let inner = &mut *self.inner;
        inner.actual_len = 0;
        inner.status = kIOReturnSuccess;
        inner.callback_data = callback_data;
    }

    /// SAFETY: requires that the transfer has completed and `length` bytes are initialized
    unsafe fn take_buf(&mut self, length: usize) -> Vec<u8> {
        assert!(!self.buf.is_null());
        let ptr = mem::replace(&mut self.buf, null_mut());
        let capacity = mem::replace(&mut self.capacity, 0);
        assert!(length <= capacity);
        Vec::from_raw_parts(ptr, length, capacity)
    }

    /// SAFETY: requires that the transfer is not active, but is fully prepared (as it is when submitting the transfer fails)
    unsafe fn check_submit_result(&mut self, res: IOReturn) {
        if res != kIOReturnSuccess {
            error!("Failed to submit transfer: {res:x}");
            let callback_data = {
                let inner = &mut *self.inner;
                inner.status = res;
                inner.callback_data
            };

            // Complete the transfer in the place of the callback
            notify_completion::<super::TransferData>(callback_data)
        }
    }

    /// SAFETY: requires that the transfer is in a completed state
    unsafe fn take_status(&mut self) -> (Result<(), TransferError>, usize) {
        let inner = unsafe { &*self.inner };

        #[allow(non_upper_case_globals)]
        #[deny(unreachable_patterns)]
        let status = match inner.status {
            kIOReturnSuccess => Ok(()),
            kIOReturnNoDevice => Err(TransferError::Disconnected),
            _ => Err(TransferError::Unknown),
        };

        (status, inner.actual_len)
    }
}

unsafe impl Send for TransferData {}

impl PlatformTransfer for TransferData {
    fn cancel(&self) {
        // TODO
    }
}

impl PlatformSubmit<Vec<u8>> for TransferData {
    unsafe fn submit(&mut self, data: Vec<u8>, callback_data: *mut std::ffi::c_void) {
        //assert!(ep & 0x80 == 0);
        let len = data.len();
        self.fill(data, callback_data);

        // SAFETY: we just properly filled the buffer and it is not already pending
        let res = call_iokit_function!(
            self.interface.as_ref().unwrap().interface.raw,
            WritePipeAsync(
                self.pipe_ref,
                self.buf as *mut c_void,
                u32::try_from(len).expect("request too large"),
                transfer_callback,
                self.inner as *mut c_void
            )
        );
        info!("Submitted OUT transfer {inner:?}", inner = self.inner);
        self.check_submit_result(res);
    }

    unsafe fn take_completed(&mut self) -> crate::transfer::Completion<ResponseBuffer> {
        let (status, actual_len) = self.take_status();

        // SAFETY: self is completed (precondition) and `actual_length` bytes were initialized.
        let data = ResponseBuffer::from_vec(unsafe { self.take_buf(0) }, actual_len);
        Completion { data, status }
    }
}

impl PlatformSubmit<RequestBuffer> for TransferData {
    unsafe fn submit(&mut self, data: RequestBuffer, callback_data: *mut std::ffi::c_void) {
        //assert!(ep & 0x80 == 0x80);
        //assert!(ty == USBDEVFS_URB_TYPE_BULK || ty == USBDEVFS_URB_TYPE_INTERRUPT);

        let (data, len) = data.into_vec();
        self.fill(data, callback_data);

        // SAFETY: we just properly filled the buffer and it is not already pending
        let res = call_iokit_function!(
            self.interface.as_ref().unwrap().interface.raw,
            ReadPipeAsync(
                self.pipe_ref,
                self.buf as *mut c_void,
                u32::try_from(len).expect("request too large"),
                transfer_callback,
                self.inner as *mut c_void
            )
        );
        info!("Submitted IN transfer {inner:?}", inner = self.inner);

        self.check_submit_result(res);
    }

    unsafe fn take_completed(&mut self) -> crate::transfer::Completion<Vec<u8>> {
        let (status, actual_len) = self.take_status();

        // SAFETY: self is completed (precondition) and `actual_length` bytes were initialized.
        let data = unsafe { self.take_buf(actual_len) };
        Completion { data, status }
    }
}

impl PlatformSubmit<ControlIn> for TransferData {
    unsafe fn submit(&mut self, data: ControlIn, callback_data: *mut std::ffi::c_void) {
        assert!(self.pipe_ref == 0);

        let buf = Vec::with_capacity(data.length as usize);
        self.fill(buf, callback_data);

        let mut req = IOUSBDevRequest {
            bmRequestType: data.request_type(),
            bRequest: data.request,
            wValue: data.value,
            wIndex: data.index,
            wLength: data.length,
            pData: self.buf as *mut c_void,
            wLenDone: 0,
        };

        // SAFETY: we just properly filled the buffer and it is not already pending
        let res = call_iokit_function!(
            self.device.device.raw,
            DeviceRequestAsync(&mut req, transfer_callback, self.inner as *mut c_void)
        );
        info!(
            "Submitted Control IN transfer {inner:?}",
            inner = self.inner
        );
        self.check_submit_result(res);
    }

    unsafe fn take_completed(&mut self) -> crate::transfer::Completion<Vec<u8>> {
        let (status, actual_len) = self.take_status();

        // SAFETY: self is completed (precondition) and `actual_length` bytes were initialized.
        let data = unsafe { self.take_buf(actual_len) };
        Completion { data, status }
    }
}

impl PlatformSubmit<ControlOut<'_>> for TransferData {
    unsafe fn submit(&mut self, data: ControlOut<'_>, callback_data: *mut std::ffi::c_void) {
        assert!(self.pipe_ref == 0);

        let buf = data.data.to_vec();
        let len = buf.len();
        self.fill(buf, callback_data);

        let mut req = IOUSBDevRequest {
            bmRequestType: data.request_type(),
            bRequest: data.request,
            wValue: data.value,
            wIndex: data.index,
            wLength: u16::try_from(len).expect("request too long"),
            pData: self.buf as *mut c_void,
            wLenDone: 0,
        };

        // SAFETY: we just properly filled the buffer and it is not already pending
        let res = call_iokit_function!(
            self.device.device.raw,
            DeviceRequestAsync(&mut req, transfer_callback, self.inner as *mut c_void)
        );
        info!(
            "Submitted Control OUT transfer {inner:?}",
            inner = self.inner
        );
        self.check_submit_result(res);
    }

    unsafe fn take_completed(&mut self) -> crate::transfer::Completion<ResponseBuffer> {
        let (status, actual_len) = self.take_status();

        // SAFETY: self is completed (precondition) and `actual_length` bytes were initialized.
        let data = ResponseBuffer::from_vec(unsafe { self.take_buf(0) }, actual_len);
        Completion { data, status }
    }
}
