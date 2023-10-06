use std::fmt::Debug;
use std::mem::ManuallyDrop;

use super::TransferRequest;

pub struct RequestBuffer {
    pub(crate) buf: *mut u8,
    pub(crate) capacity: usize,
    pub(crate) requested: usize,
}

impl RequestBuffer {
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

    pub fn reuse(v: Vec<u8>, len: usize) -> RequestBuffer {
        let mut v = ManuallyDrop::new(v);
        v.reserve_exact(len.saturating_sub(len));
        RequestBuffer {
            buf: v.as_mut_ptr(),
            capacity: v.capacity(),
            requested: len,
        }
    }
}

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

    pub fn actual_length(&self) -> usize {
        self.transferred
    }

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

impl Drop for ResponseBuffer {
    fn drop(&mut self) {
        unsafe { drop(Vec::from_raw_parts(self.buf, 0, self.capacity)) }
    }
}

impl TransferRequest for Vec<u8> {
    type Response = ResponseBuffer;
}
