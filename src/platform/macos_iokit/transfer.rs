use std::mem::ManuallyDrop;

use io_kit_sys::ret::{kIOReturnSuccess, IOReturn};

use crate::transfer::TransferError;
pub struct TransferData {
    pub(super) buf: *mut u8,
    pub(super) capacity: u32,
    pub(super) requested_len: u32,
    pub(super) actual_len: u32,
    pub(super) status: IOReturn,
}

impl Drop for TransferData {
    fn drop(&mut self) {
        unsafe { drop(Vec::from_raw_parts(self.buf, 0, self.capacity as usize)) }
    }
}

impl TransferData {
    pub(super) fn new() -> TransferData {
        let mut empty = ManuallyDrop::new(Vec::with_capacity(0));
        unsafe { Self::from_raw(empty.as_mut_ptr(), 0, 0) }
    }

    pub(super) unsafe fn from_raw(buf: *mut u8, requested_len: u32, capacity: u32) -> TransferData {
        TransferData {
            buf,
            capacity,
            requested_len,
            actual_len: 0,
            status: kIOReturnSuccess,
        }
    }

    #[inline]
    pub fn status(&self) -> Result<(), TransferError> {
        super::status_to_transfer_result(self.status)
    }
}

unsafe impl Send for TransferData {}
unsafe impl Sync for TransferData {}
