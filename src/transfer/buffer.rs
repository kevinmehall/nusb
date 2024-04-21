use std::fmt::Debug;
use std::mem::ManuallyDrop;

use super::TransferRequest;

/// A buffer for requesting an IN transfer.
///
/// A `RequestBuffer` is passed when submitting an `IN` transfer to define the
/// requested length and provide a buffer to receive data into. The buffer is
/// returned in the [`Completion`][`crate::transfer::Completion`] as a `Vec<u8>`
/// with the data read from the endpoint. The `Vec`'s allocation can turned back
/// into a `RequestBuffer` to re-use it for another transfer.
///
/// You can think of a `RequestBuffer` as a `Vec` with uninitialized contents.
pub struct RequestBuffer {
    pub(crate) buf: *mut u8,
    pub(crate) capacity: usize,
    pub(crate) requested: usize,
}

impl RequestBuffer {
    /// Create a `RequestBuffer` of the specified size.
    pub fn new(len: usize) -> RequestBuffer {
        let mut v = ManuallyDrop::new(Vec::with_capacity(len));
        RequestBuffer {
            buf: v.as_mut_ptr(),
            capacity: v.capacity(),
            requested: len,
        }
    }

    pub(crate) fn into_vec(self) -> (Vec<u8>, usize) {
        let s = ManuallyDrop::new(self);
        let v = unsafe { Vec::from_raw_parts(s.buf, 0, s.capacity) };
        (v, s.requested)
    }

    /// Create a `RequestBuffer` by re-using the allocation of a `Vec`.
    pub fn reuse(v: Vec<u8>, len: usize) -> RequestBuffer {
        let mut v = ManuallyDrop::new(v);
        v.clear();
        v.reserve_exact(len);
        RequestBuffer {
            buf: v.as_mut_ptr(),
            capacity: v.capacity(),
            requested: len,
        }
    }
}

unsafe impl Send for RequestBuffer {}
unsafe impl Sync for RequestBuffer {}

impl Drop for RequestBuffer {
    fn drop(&mut self) {
        unsafe { drop(Vec::from_raw_parts(self.buf, 0, self.capacity)) }
    }
}

impl Debug for RequestBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestBuffer")
            .field("requested", &self.requested)
            .finish_non_exhaustive()
    }
}

impl TransferRequest for RequestBuffer {
    type Response = Vec<u8>;
}

/// Returned buffer and actual length for a completed OUT transfer.
///
/// When an `OUT` transfer completes, a `ResponseBuffer` is returned in the
/// `Completion`. The [`actual_length`][`ResponseBuffer::actual_length`] tells
/// you how many bytes were successfully sent, which may be useful in the case
/// of a partially-completed transfer.
///
/// The `ResponseBuffer` can be turned into an empty `Vec` to re-use the allocation
/// for another transfer, or dropped to free the memory.
pub struct ResponseBuffer {
    pub(crate) buf: *mut u8,
    pub(crate) capacity: usize,
    pub(crate) transferred: usize,
}

impl ResponseBuffer {
    pub(crate) fn from_vec(v: Vec<u8>, transferred: usize) -> ResponseBuffer {
        let mut v = ManuallyDrop::new(v);
        ResponseBuffer {
            buf: v.as_mut_ptr(),
            capacity: v.capacity(),
            transferred,
        }
    }

    /// Get the number of bytes successfully transferred.
    pub fn actual_length(&self) -> usize {
        self.transferred
    }

    /// Extract the buffer as an empty `Vec` to re-use in another transfer.
    pub fn reuse(self) -> Vec<u8> {
        let s = ManuallyDrop::new(self);
        unsafe { Vec::from_raw_parts(s.buf, 0, s.capacity) }
    }
}

impl Debug for ResponseBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResponseBuffer")
            .field("transferred", &self.transferred)
            .finish_non_exhaustive()
    }
}

unsafe impl Send for ResponseBuffer {}
unsafe impl Sync for ResponseBuffer {}

impl Drop for ResponseBuffer {
    fn drop(&mut self) {
        unsafe { drop(Vec::from_raw_parts(self.buf, 0, self.capacity)) }
    }
}

impl TransferRequest for Vec<u8> {
    type Response = ResponseBuffer;
}
