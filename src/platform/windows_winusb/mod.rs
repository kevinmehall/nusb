mod enumeration;
use std::num::NonZeroU32;

pub use enumeration::{list_buses, list_devices};

mod events;

mod device;
pub(crate) use device::WindowsDevice as Device;
pub(crate) use device::WindowsEndpoint as Endpoint;
pub(crate) use device::WindowsInterface as Interface;

mod transfer;

mod cfgmgr32;
mod hub;
mod registry;
pub(crate) use cfgmgr32::DevInst;
use windows_sys::Win32::Foundation::WIN32_ERROR;
pub(crate) use DevInst as DeviceId;
mod hotplug;
mod util;
pub(crate) use hotplug::WindowsHotplugWatch as HotplugWatch;

use crate::ErrorKind;

pub fn format_os_error_code(f: &mut std::fmt::Formatter<'_>, code: u32) -> std::fmt::Result {
    write!(f, "error {}", code)
}

impl crate::error::Error {
    pub(crate) fn new_os(kind: ErrorKind, message: &'static str, code: WIN32_ERROR) -> Self {
        Self {
            kind,
            code: NonZeroU32::new(code as u32),
            message,
        }
    }
}
