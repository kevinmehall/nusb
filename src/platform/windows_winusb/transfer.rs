use std::{
    mem::{self, ManuallyDrop},
    sync::Arc,
};

use windows_sys::Win32::{
    Foundation::{
        GetLastError, ERROR_DEVICE_NOT_CONNECTED, ERROR_FILE_NOT_FOUND, ERROR_GEN_FAILURE,
        ERROR_NO_SUCH_DEVICE, ERROR_OPERATION_ABORTED, ERROR_REQUEST_ABORTED, ERROR_SEM_TIMEOUT,
        ERROR_SUCCESS, ERROR_TIMEOUT,
    },
    System::IO::{GetOverlappedResult, OVERLAPPED},
};

use super::{threadpool::Timer, Interface};
use crate::transfer::{Buffer, Completion, Direction, TransferError};

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum TransferKind {
    Regular,
    #[cfg(not(target_vendor = "win7"))]
    Isochronous,
}

#[repr(C)]
pub(crate) struct TransferHeader {
    // Must remain first so the address passed as OVERLAPPED can be cast back
    // to TransferHeader in the shared threadpool-I/O callback.
    pub(crate) overlapped: OVERLAPPED,
    pub(crate) kind: TransferKind,
}

#[repr(C)]
pub struct TransferData {
    // First member of a repr(C) struct. TransferHeader itself begins with
    // OVERLAPPED, so all three pointers have the same address.
    pub(crate) header: TransferHeader,

    pub(crate) buf: *mut u8,
    pub(crate) capacity: u32,
    pub(crate) request_len: u32,

    pub(crate) intf: Arc<Interface>,
    pub(crate) endpoint: u8,
    pub(crate) error_from_submit: Result<(), TransferError>,

    pub(crate) timeout: Option<Timer>,
}

unsafe impl Send for TransferData {}
unsafe impl Sync for TransferData {}

impl TransferData {
    pub(crate) fn new(intf: Arc<Interface>, endpoint: u8) -> TransferData {
        let mut empty = ManuallyDrop::new(Vec::with_capacity(0));

        TransferData {
            header: TransferHeader {
                overlapped: unsafe { mem::zeroed() },
                kind: TransferKind::Regular,
            },
            buf: empty.as_mut_ptr(),
            capacity: 0,
            request_len: 0,
            intf,
            endpoint,
            timeout: None,
            error_from_submit: Ok(()),
        }
    }

    pub fn set_buffer(&mut self, buf: Buffer) {
        debug_assert!(self.capacity == 0);
        let buf = ManuallyDrop::new(buf);
        self.capacity = buf.capacity;
        self.buf = buf.ptr;
        self.header.overlapped.InternalHigh = 0;
        self.request_len = match Direction::from_address(self.endpoint) {
            Direction::Out => buf.len,
            Direction::In => buf.requested_len,
        };
    }

    pub fn take_completion(&mut self, intf: &Interface) -> Completion {
        let mut actual_len: u32 = 0;

        let status = self.error_from_submit.and_then(|()| {
            unsafe {
                GetOverlappedResult(intf.handle, &self.header.overlapped, &mut actual_len, 0)
            };

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
        self.header.overlapped.InternalHigh = 0;

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
