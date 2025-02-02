use std::{
    mem::{self, ManuallyDrop, MaybeUninit},
    slice,
};

use log::debug;
use windows_sys::Win32::{
    Foundation::{
        ERROR_DEVICE_NOT_CONNECTED, ERROR_FILE_NOT_FOUND, ERROR_GEN_FAILURE, ERROR_NO_SUCH_DEVICE,
        ERROR_OPERATION_ABORTED, ERROR_REQUEST_ABORTED, ERROR_SEM_TIMEOUT, ERROR_SUCCESS,
        ERROR_TIMEOUT, WIN32_ERROR,
    },
    System::IO::OVERLAPPED,
};

use crate::transfer::{internal::notify_completion, Direction, TransferError};

#[repr(C)]
pub struct TransferData {
    // first member of repr(C) struct; can cast pointer between types
    // overlapped.Internal contains the stauts
    // overlapped.InternalHigh contains the number of bytes transferred
    pub(crate) overlapped: OVERLAPPED,
    pub(crate) buf: *mut u8,
    pub(crate) capacity: usize,
    pub(crate) request_len: u32,
    pub(crate) endpoint: u8,
}

unsafe impl Send for TransferData {}
unsafe impl Sync for TransferData {}

impl TransferData {
    pub(crate) fn new(endpoint: u8, capacity: usize) -> TransferData {
        let request_len = match Direction::from_address(endpoint) {
            Direction::Out => 0,
            Direction::In => capacity.try_into().expect("transfer size must fit in u32"),
        };

        let mut v = ManuallyDrop::new(Vec::with_capacity(capacity));

        TransferData {
            overlapped: unsafe { mem::zeroed() },
            buf: v.as_mut_ptr(),
            capacity: v.capacity(),
            request_len,
            endpoint,
        }
    }

    #[inline]
    pub fn endpoint(&self) -> u8 {
        self.endpoint
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
        self.request_len = len.try_into().expect("transfer size must fit in u32");
    }

    #[inline]
    pub fn actual_len(&self) -> usize {
        self.overlapped.InternalHigh
    }

    pub fn status(&self) -> Result<(), TransferError> {
        match self.overlapped.Internal as WIN32_ERROR {
            ERROR_SUCCESS => Ok(()),
            ERROR_GEN_FAILURE => Err(TransferError::Stall),
            ERROR_REQUEST_ABORTED | ERROR_TIMEOUT | ERROR_SEM_TIMEOUT | ERROR_OPERATION_ABORTED => {
                Err(TransferError::Cancelled)
            }
            ERROR_FILE_NOT_FOUND | ERROR_DEVICE_NOT_CONNECTED | ERROR_NO_SUCH_DEVICE => {
                Err(TransferError::Disconnected)
            }
            _ => Err(TransferError::Unknown),
        }
    }

    /// Safety: Must be an IN transfer and must have completed to initialize the buffer
    pub unsafe fn take_vec(&mut self) -> Vec<u8> {
        let mut n = ManuallyDrop::new(Vec::new());
        let v = unsafe { Vec::from_raw_parts(self.buf, self.actual_len(), self.capacity) };
        self.capacity = n.capacity();
        self.buf = n.as_mut_ptr();
        self.overlapped.InternalHigh = 0;
        v
    }
}

impl Drop for TransferData {
    fn drop(&mut self) {
        unsafe { drop(Vec::from_raw_parts(self.buf, 0, self.capacity)) }
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
