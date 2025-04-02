use std::{
    mem::{ManuallyDrop, MaybeUninit},
    slice,
};

use io_kit_sys::ret::{kIOReturnSuccess, IOReturn};

use crate::transfer::{Direction, TransferError};

use super::status_to_transfer_result;

pub struct TransferData {
    pub(super) endpoint_addr: u8,
    pub(super) buf: *mut u8,
    pub(super) capacity: usize,
    pub(super) request_len: usize,
    pub(super) actual_len: usize,
    pub(super) status: IOReturn,
}

impl Drop for TransferData {
    fn drop(&mut self) {
        unsafe { drop(Vec::from_raw_parts(self.buf, 0, self.capacity)) }
    }
}

impl TransferData {
    pub(super) fn new(endpoint_addr: u8, capacity: usize) -> TransferData {
        let request_len = match Direction::from_address(endpoint_addr) {
            Direction::Out => 0,
            Direction::In => capacity,
        };

        let mut v = ManuallyDrop::new(Vec::with_capacity(capacity));

        TransferData {
            endpoint_addr,
            buf: v.as_mut_ptr(),
            capacity: v.capacity(),
            actual_len: 0,
            request_len,
            status: kIOReturnSuccess,
        }
    }

    #[inline]
    pub fn endpoint(&self) -> u8 {
        self.endpoint_addr
    }

    #[inline]
    pub fn buffer(&self) -> &[MaybeUninit<u8>] {
        unsafe { slice::from_raw_parts(self.buf.cast(), self.capacity) }
    }

    #[inline]
    pub fn buffer_mut(&mut self) -> &mut [MaybeUninit<u8>] {
        unsafe { slice::from_raw_parts_mut(self.buf.cast(), self.capacity) }
    }

    #[inline]
    pub fn request_len(&self) -> usize {
        self.request_len as usize
    }

    #[inline]
    pub unsafe fn set_request_len(&mut self, len: usize) {
        assert!(len <= self.capacity);
        self.request_len = len;
    }

    #[inline]
    pub fn actual_len(&self) -> usize {
        self.actual_len as usize
    }

    #[inline]
    pub fn status(&self) -> Result<(), TransferError> {
        status_to_transfer_result(self.status)
    }

    /// Safety: Must be an IN transfer and must have completed to initialize the buffer
    pub unsafe fn take_vec(&mut self) -> Vec<u8> {
        let mut n = ManuallyDrop::new(Vec::new());
        let v = unsafe { Vec::from_raw_parts(self.buf, self.actual_len as usize, self.capacity) };
        self.capacity = n.capacity();
        self.buf = n.as_mut_ptr();
        self.actual_len = 0;
        v
    }
}

unsafe impl Send for TransferData {}
unsafe impl Sync for TransferData {}
