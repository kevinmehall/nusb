use std::mem::{self, ManuallyDrop};

use io_kit_sys::ret::{kIOReturnSuccess, IOReturn};

use crate::transfer::{Allocator, Buffer, Completion, Direction};

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
        TransferData {
            buf: empty.as_mut_ptr(),
            capacity: 0,
            requested_len: 0,
            actual_len: 0,
            status: kIOReturnSuccess,
        }
    }

    pub fn put_buffer(&mut self, buffer: Buffer, direction: Direction) {
        // Assumes that there is no previous buffer; this would leak it
        debug_assert!(self.capacity == 0);
        let buffer = ManuallyDrop::new(buffer);
        self.buf = buffer.ptr;
        self.capacity = buffer.capacity;
        self.actual_len = 0;
        self.requested_len = match direction {
            Direction::Out => buffer.len,
            Direction::In => buffer.requested_len,
        };
    }

    /// # Safety
    /// The transfer must have been completed to initialize the buffer. The direction must be correct.
    pub unsafe fn take_completion(&mut self, direction: Direction) -> Completion {
        let status = super::status_to_transfer_result(self.status);

        let mut empty = ManuallyDrop::new(Vec::new());
        let ptr = mem::replace(&mut self.buf, empty.as_mut_ptr());
        let capacity = mem::replace(&mut self.capacity, 0);
        let len = match direction {
            Direction::Out => self.requested_len,
            Direction::In => self.actual_len,
        };
        let requested_len = mem::replace(&mut self.requested_len, 0);
        let actual_len = mem::replace(&mut self.actual_len, 0) as usize;

        let buffer = Buffer {
            ptr,
            len,
            requested_len,
            capacity,
            allocator: Allocator::Default,
        };

        Completion {
            status,
            actual_len,
            buffer,
        }
    }
}

unsafe impl Send for TransferData {}
unsafe impl Sync for TransferData {}
