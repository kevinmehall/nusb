use io_kit_sys::{IOIteratorNext, IOObjectRelease};

pub(crate) struct IoObject(u32);

impl IoObject {
    // Safety: `handle` must be an IOObject handle. Ownership is transferred.
    pub unsafe fn new(handle: u32) -> IoObject {
        IoObject(handle)
    }
    pub fn get(&self) -> u32 {
        self.0
    }
}

impl Drop for IoObject {
    fn drop(&mut self) {
        unsafe {
            IOObjectRelease(self.0);
        }
    }
}

pub(crate) struct IoService(IoObject);

impl IoService {
    // Safety: `handle` must be an IOService handle. Ownership is transferred.
    pub unsafe fn new(handle: u32) -> IoService {
        IoService(IoObject(handle))
    }
    pub fn get(&self) -> u32 {
        self.0 .0
    }
}

pub(crate) struct IoServiceIterator(IoObject);

impl IoServiceIterator {
    // Safety: `handle` must be an IoIterator of IoService. Ownership is transferred.
    pub unsafe fn new(handle: u32) -> IoServiceIterator {
        IoServiceIterator(IoObject::new(handle))
    }
}

impl Iterator for IoServiceIterator {
    type Item = IoService;

    fn next(&mut self) -> Option<Self::Item> {
        unsafe {
            let handle = IOIteratorNext(self.0.get());
            if handle != 0 {
                Some(IoService::new(handle))
            } else {
                None
            }
        }
    }
}
