mod transfer;
use std::io;
use std::num::NonZeroU32;

use rustix::io::Errno;
pub(crate) use transfer::TransferData;
mod usbfs;

#[cfg(not(target_os = "android"))]
mod enumeration;

#[cfg(not(target_os = "android"))]
pub use enumeration::{list_buses, list_devices, SysfsPath};

#[cfg(not(target_os = "android"))]
mod hotplug;

#[cfg(not(target_os = "android"))]
pub(crate) use hotplug::LinuxHotplugWatch as HotplugWatch;

mod events;

mod device;
pub(crate) use device::LinuxDevice as Device;
pub(crate) use device::LinuxEndpoint as Endpoint;
pub(crate) use device::LinuxInterface as Interface;

use crate::transfer::TransferError;
use crate::ErrorKind;

#[derive(Copy, Clone, Debug, Hash, PartialEq, Eq)]
pub struct DeviceId {
    pub(crate) bus: u8,
    pub(crate) addr: u8,
}

fn errno_to_transfer_error(e: Errno) -> TransferError {
    match e {
        Errno::NODEV | Errno::SHUTDOWN => TransferError::Disconnected,
        Errno::PIPE => TransferError::Stall,
        Errno::NOENT | Errno::CONNRESET | Errno::TIMEDOUT => TransferError::Cancelled,
        Errno::PROTO | Errno::ILSEQ | Errno::OVERFLOW | Errno::COMM | Errno::TIME => {
            TransferError::Fault
        }
        Errno::INVAL => TransferError::InvalidArgument,
        _ => TransferError::Unknown(e.raw_os_error() as u32),
    }
}

pub fn format_os_error_code(f: &mut std::fmt::Formatter<'_>, code: u32) -> std::fmt::Result {
    write!(f, "errno {}", code)
}

impl crate::error::Error {
    pub(crate) fn new_os(kind: ErrorKind, message: &'static str, code: Errno) -> Self {
        Self {
            kind,
            code: NonZeroU32::new(code.raw_os_error() as u32),
            message,
        }
    }

    pub(crate) fn new_io(kind: ErrorKind, message: &'static str, err: io::Error) -> Self {
        Self {
            kind,
            code: err.raw_os_error().and_then(|i| NonZeroU32::new(i as u32)),
            message,
        }
    }
}
