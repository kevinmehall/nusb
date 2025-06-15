//! Utilities for using IOKit APIs.
//!
//! Based on Kate Temkin's [usrs](https://github.com/ktemkin/usrs)
//! licensed under MIT OR Apache-2.0.

use core_foundation_sys::uuid::CFUUIDBytes;
use io_kit_sys::{ret::IOReturn, IOIteratorNext, IOObjectRelease};

use super::iokit_c::{self, CFUUIDGetUUIDBytes, IOCFPlugInInterface};

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

/// Helper for calling IOKit function pointers.
macro_rules! call_iokit_function {
    ($ptr:expr, $function:ident($($args:expr),*)) => {{
        use std::ffi::c_void;
        let func = (**$ptr).$function.expect("function pointer from IOKit was null");
        func($ptr as *mut c_void, $($args),*)
    }};
}
pub(crate) use call_iokit_function;

/// Wrapper around a **IOCFPluginInterface that automatically releases it.
#[derive(Debug)]
pub(crate) struct PluginInterface {
    interface: *mut *mut IOCFPlugInInterface,
}

impl PluginInterface {
    pub(crate) fn new(interface: *mut *mut IOCFPlugInInterface) -> Self {
        Self { interface }
    }

    /// Fetches the inner pointer for passing to IOKit functions.
    pub(crate) fn get(&self) -> *mut *mut IOCFPlugInInterface {
        self.interface
    }
}

impl Drop for PluginInterface {
    fn drop(&mut self) {
        unsafe {
            call_iokit_function!(self.interface, Release());
        }
    }
}

/// Alias to select the "version 650" version of UsbDevice, and and "version 700" of UsbInterface.
/// These are supported on macOS versions back to 10.10, which is older than Rust's minimum supported version of 10.12.
pub(crate) type UsbDevice = iokit_c::IOUSBDeviceStruct650;
pub(crate) type UsbInterface = iokit_c::IOUSBInterfaceStruct700;

pub(crate) fn usb_device_type_id() -> CFUUIDBytes {
    unsafe { CFUUIDGetUUIDBytes(iokit_c::kIOUSBDeviceInterfaceID650()) }
}

pub(crate) fn usb_interface_type_id() -> CFUUIDBytes {
    unsafe { CFUUIDGetUUIDBytes(iokit_c::kIOUSBInterfaceInterfaceID700()) }
}

pub(crate) fn check_iokit_return(r: IOReturn) -> Result<(), IOReturn> {
    match r {
        io_kit_sys::ret::kIOReturnSuccess => Ok(()),
        e => Err(e),
    }
}
