use std::{
    cell::UnsafeCell,
    mem::{self, ManuallyDrop},
    sync::Arc,
};

use windows_sys::Win32::{
    Foundation::{
        ERROR_DEVICE_NOT_CONNECTED, ERROR_FILE_NOT_FOUND, ERROR_GEN_FAILURE, ERROR_NO_SUCH_DEVICE,
        ERROR_OPERATION_ABORTED, ERROR_REQUEST_ABORTED, ERROR_SEM_TIMEOUT, ERROR_SUCCESS,
        ERROR_TIMEOUT,
    },
    System::IO::OVERLAPPED,
};

use super::{threadpool::Timer, Interface};
use crate::transfer::{Buffer, Completion, Direction, TransferError};

pub(crate) fn transfer_error_from_win32(e: u32) -> Result<(), TransferError> {
    match e {
        ERROR_SUCCESS => Ok(()),
        ERROR_GEN_FAILURE => Err(TransferError::Stall),
        ERROR_REQUEST_ABORTED | ERROR_TIMEOUT | ERROR_SEM_TIMEOUT | ERROR_OPERATION_ABORTED => {
            Err(TransferError::Cancelled)
        }
        ERROR_FILE_NOT_FOUND | ERROR_DEVICE_NOT_CONNECTED | ERROR_NO_SUCH_DEVICE => {
            Err(TransferError::Disconnected)
        }
        e => Err(TransferError::Unknown(e)),
    }
}

#[repr(C)]
pub struct TransferData {
    // first member of repr(C) struct; can cast pointer between types
    // `UnsafeCell` because the OS can touch it while we have `&TransferData`
    pub(crate) overlapped: UnsafeCell<OVERLAPPED>,

    pub(crate) buf: *mut u8,
    pub(crate) endpoint: u8,
    pub(crate) capacity: u32,
    pub(crate) requested_len: u32,
    pub(crate) actual_len: u32,
    pub(crate) error: Result<(), TransferError>,

    pub(crate) intf: Arc<Interface>,
    pub(crate) timeout: Option<Timer>,
}

unsafe impl Send for TransferData {}
unsafe impl Sync for TransferData {}

impl TransferData {
    pub(crate) fn new(intf: Arc<Interface>, endpoint: u8) -> TransferData {
        let mut empty = ManuallyDrop::new(Vec::with_capacity(0));

        TransferData {
            overlapped: unsafe { mem::zeroed() },
            buf: empty.as_mut_ptr(),
            capacity: 0,
            requested_len: 0,
            actual_len: 0,
            intf,
            endpoint,
            timeout: None,
            error: Ok(()),
        }
    }

    pub fn set_buffer(&mut self, buf: Buffer) {
        debug_assert!(self.capacity == 0);
        let buf = ManuallyDrop::new(buf);
        self.capacity = buf.capacity;
        self.buf = buf.ptr;
        self.actual_len = 0;
        self.requested_len = match Direction::from_address(self.endpoint) {
            Direction::Out => buf.len,
            Direction::In => buf.requested_len,
        };
    }

    pub fn take_completion(&mut self) -> Completion {
        let status = mem::replace(&mut self.error, Ok(()));

        let mut empty = ManuallyDrop::new(Vec::new());
        let ptr = mem::replace(&mut self.buf, empty.as_mut_ptr());
        let capacity = mem::replace(&mut self.capacity, 0);
        let requested_len = mem::replace(&mut self.requested_len, 0);
        let actual_len = mem::replace(&mut self.actual_len, 0);
        let len = match Direction::from_address(self.endpoint) {
            Direction::Out => requested_len,
            Direction::In => actual_len,
        };

        Completion {
            status,
            actual_len: actual_len as usize,
            buffer: Buffer {
                ptr,
                len,
                requested_len,
                capacity,
                allocator: crate::transfer::Allocator::Default,
            },
        }
    }
}

impl Drop for TransferData {
    fn drop(&mut self) {
        unsafe {
            drop(Vec::from_raw_parts(self.buf, 0, self.capacity as usize));
        }
    }
}
