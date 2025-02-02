use std::{
    mem::{ManuallyDrop, MaybeUninit},
    ptr::{addr_of_mut, null_mut},
    slice,
    time::Instant,
};

use rustix::io::Errno;

use crate::transfer::{
    internal::Pending, ControlIn, ControlOut, Direction, TransferError, SETUP_PACKET_SIZE,
};
use crate::{descriptors::TransferType, util::write_copy_of_slice};

use super::{
    errno_to_transfer_error,
    usbfs::{
        Urb, USBDEVFS_URB_TYPE_BULK, USBDEVFS_URB_TYPE_CONTROL, USBDEVFS_URB_TYPE_INTERRUPT,
        USBDEVFS_URB_TYPE_ISO,
    },
};

/// Linux-specific transfer state.
///
/// This logically contains a `Vec` with urb.buffer and capacity.
/// It also owns the `urb` allocation itself, which is stored out-of-line
/// to enable isochronous transfers to allocate the variable-length
/// `iso_packet_desc` array.
pub struct TransferData {
    urb: *mut Urb,
    capacity: usize,
    pub(crate) deadline: Option<Instant>,
}

unsafe impl Send for TransferData {}
unsafe impl Sync for TransferData {}

impl TransferData {
    pub(super) fn new(endpoint: u8, ep_type: TransferType, capacity: usize) -> TransferData {
        let ep_type = match ep_type {
            TransferType::Control => USBDEVFS_URB_TYPE_CONTROL,
            TransferType::Interrupt => USBDEVFS_URB_TYPE_INTERRUPT,
            TransferType::Bulk => USBDEVFS_URB_TYPE_BULK,
            TransferType::Isochronous => USBDEVFS_URB_TYPE_ISO,
        };

        let request_len: i32 = match Direction::from_address(endpoint) {
            Direction::Out => 0,
            Direction::In => capacity.try_into().unwrap(),
        };

        let mut v = ManuallyDrop::new(Vec::with_capacity(capacity));

        TransferData {
            urb: Box::into_raw(Box::new(Urb {
                ep_type,
                endpoint,
                status: 0,
                flags: 0,
                buffer: v.as_mut_ptr(),
                buffer_length: request_len,
                actual_length: 0,
                start_frame: 0,
                number_of_packets_or_stream_id: 0,
                error_count: 0,
                signr: 0,
                usercontext: null_mut(),
            })),
            capacity: v.capacity(),
            deadline: None,
        }
    }

    pub(super) fn new_control_out(data: ControlOut) -> TransferData {
        let len = SETUP_PACKET_SIZE + data.data.len();
        let mut t = TransferData::new(0x00, TransferType::Control, len);

        write_copy_of_slice(
            &mut t.buffer_mut()[..SETUP_PACKET_SIZE],
            &data.setup_packet(),
        );
        write_copy_of_slice(
            &mut t.buffer_mut()[SETUP_PACKET_SIZE..SETUP_PACKET_SIZE + data.data.len()],
            &data.data,
        );
        unsafe {
            t.set_request_len(len);
        }

        t
    }

    pub(super) fn new_control_in(data: ControlIn) -> TransferData {
        let len = SETUP_PACKET_SIZE + data.length as usize;
        let mut t = TransferData::new(0x80, TransferType::Control, len);
        write_copy_of_slice(
            &mut t.buffer_mut()[..SETUP_PACKET_SIZE],
            &data.setup_packet(),
        );
        unsafe {
            t.set_request_len(len);
        }
        t
    }

    #[inline]
    pub fn endpoint(&self) -> u8 {
        unsafe { (*self.urb).endpoint }
    }

    #[inline]
    pub(super) fn urb(&self) -> &Urb {
        unsafe { &*self.urb }
    }

    #[inline]
    pub(super) fn urb_mut(&mut self) -> &mut Urb {
        unsafe { &mut *self.urb }
    }

    #[inline]
    pub(super) fn urb_ptr(&self) -> *mut Urb {
        self.urb
    }

    #[inline]
    pub fn buffer(&self) -> &[MaybeUninit<u8>] {
        unsafe { slice::from_raw_parts(self.urb().buffer.cast(), self.capacity) }
    }

    #[inline]
    pub fn buffer_mut(&mut self) -> &mut [MaybeUninit<u8>] {
        unsafe { slice::from_raw_parts_mut(self.urb().buffer.cast(), self.capacity) }
    }

    #[inline]
    pub fn request_len(&self) -> usize {
        self.urb().buffer_length as usize
    }

    #[inline]
    pub unsafe fn set_request_len(&mut self, len: usize) {
        assert!(len <= self.capacity);
        self.urb_mut().buffer_length = len.try_into().unwrap();
    }

    #[inline]
    pub fn actual_len(&self) -> usize {
        self.urb().actual_length as usize
    }

    #[inline]
    pub fn status(&self) -> Result<(), TransferError> {
        if self.urb().status == 0 {
            return Ok(());
        }

        // It's sometimes positive, sometimes negative, but rustix panics if negative.
        Err(errno_to_transfer_error(Errno::from_raw_os_error(
            self.urb().status.abs(),
        )))
    }

    #[inline]
    pub fn control_in_data(&self) -> &[u8] {
        debug_assert!(self.urb().endpoint == 0x80);
        let urb = self.urb();
        unsafe {
            slice::from_raw_parts(
                urb.buffer.add(SETUP_PACKET_SIZE),
                urb.actual_length as usize,
            )
        }
    }
}

impl Pending<TransferData> {
    pub fn urb_ptr(&self) -> *mut Urb {
        // Get urb pointer without dereferencing as `TransferData`, because
        // it may be mutably aliased.
        unsafe { *addr_of_mut!((*self.as_ptr()).urb) }
    }
}

impl Drop for TransferData {
    fn drop(&mut self) {
        unsafe {
            drop(Vec::from_raw_parts((*self.urb).buffer, 0, self.capacity));
            drop(Box::from_raw(self.urb));
        }
    }
}
