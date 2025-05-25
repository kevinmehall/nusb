use std::mem::{self, ManuallyDrop};

use log::debug;
use windows_sys::Win32::{
    Foundation::{
        GetLastError, ERROR_DEVICE_NOT_CONNECTED, ERROR_FILE_NOT_FOUND, ERROR_GEN_FAILURE,
        ERROR_NO_SUCH_DEVICE, ERROR_OPERATION_ABORTED, ERROR_REQUEST_ABORTED, ERROR_SEM_TIMEOUT,
        ERROR_SUCCESS, ERROR_TIMEOUT,
    },
    System::IO::{GetOverlappedResult, OVERLAPPED},
};

use crate::transfer::{internal::notify_completion, Buffer, Completion, Direction, TransferError};

use super::Interface;

#[repr(C)]
pub struct TransferData {
    // first member of repr(C) struct; can cast pointer between types
    // overlapped.Internal contains the status
    // overlapped.InternalHigh contains the number of bytes transferred
    pub(crate) overlapped: OVERLAPPED,
    pub(crate) buf: *mut u8,
    pub(crate) capacity: u32,
    pub(crate) request_len: u32,
    pub(crate) endpoint: u8,
    pub(crate) error_from_submit: Result<(), TransferError>,
}

unsafe impl Send for TransferData {}
unsafe impl Sync for TransferData {}

impl TransferData {
    pub(crate) fn new(endpoint: u8) -> TransferData {
        let mut empty = ManuallyDrop::new(Vec::with_capacity(0));

        TransferData {
            overlapped: unsafe { mem::zeroed() },
            buf: empty.as_mut_ptr(),
            capacity: 0,
            request_len: 0,
            endpoint,
            error_from_submit: Ok(()),
        }
    }

    #[inline]
    pub fn actual_len(&self) -> usize {
        self.overlapped.InternalHigh
    }

    pub fn set_buffer(&mut self, buf: Buffer) {
        debug_assert!(self.capacity == 0);
        let buf = ManuallyDrop::new(buf);
        self.capacity = buf.capacity;
        self.buf = buf.ptr;
        self.overlapped.InternalHigh = 0;
        self.request_len = match Direction::from_address(self.endpoint) {
            Direction::Out => buf.len,
            Direction::In => buf.requested_len,
        };
    }

    pub fn take_completion(&mut self, intf: &Interface) -> Completion {
        let mut actual_len: u32 = 0;

        let status = self.error_from_submit.and_then(|()| {
            unsafe { GetOverlappedResult(intf.handle, &self.overlapped, &mut actual_len, 0) };

            match unsafe { GetLastError() } {
                ERROR_SUCCESS => Ok(()),
                ERROR_GEN_FAILURE => Err(TransferError::Stall),
                ERROR_REQUEST_ABORTED
                | ERROR_TIMEOUT
                | ERROR_SEM_TIMEOUT
                | ERROR_OPERATION_ABORTED => Err(TransferError::Cancelled),
                ERROR_FILE_NOT_FOUND | ERROR_DEVICE_NOT_CONNECTED | ERROR_NO_SUCH_DEVICE => {
                    Err(TransferError::Disconnected)
                }
                e => Err(TransferError::Unknown(e)),
            }
        });

        let mut empty = ManuallyDrop::new(Vec::new());
        let ptr = mem::replace(&mut self.buf, empty.as_mut_ptr());
        let capacity = mem::replace(&mut self.capacity, 0);
        let len = match Direction::from_address(self.endpoint) {
            Direction::Out => self.request_len,
            Direction::In => actual_len,
        };
        let requested_len = mem::replace(&mut self.request_len, 0);
        self.overlapped.InternalHigh = 0;

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

pub(super) fn handle_event(completion: *mut OVERLAPPED) {
    let t = completion as *mut TransferData;
    {
        let transfer = unsafe { &mut *t };

        debug!(
            "Transfer {t:?} on endpoint {:02x} complete: status {}, {} bytes",
            transfer.endpoint,
            transfer.overlapped.Internal,
            transfer.actual_len(),
        );
    }
    unsafe { notify_completion::<TransferData>(t) }
}
