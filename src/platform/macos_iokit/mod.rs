use std::num::NonZeroU32;

use crate::transfer::TransferError;
use crate::ErrorKind;

mod transfer;
use io_kit_sys::ret::IOReturn;
pub(crate) use transfer::TransferData;

mod enumeration;
mod events;
pub use enumeration::{list_buses, list_devices};

mod device;
pub(crate) use device::MacDevice as Device;
pub(crate) use device::MacEndpoint as Endpoint;
pub(crate) use device::MacInterface as Interface;

mod hotplug;
pub(crate) use hotplug::MacHotplugWatch as HotplugWatch;

mod iokit;
mod iokit_c;
mod iokit_usb;

/// Device ID is the registry entry ID
pub type DeviceId = u64;

fn status_to_transfer_result(status: IOReturn) -> Result<(), TransferError> {
    #[allow(non_upper_case_globals)]
    #[deny(unreachable_patterns)]
    match status {
        io_kit_sys::ret::kIOReturnSuccess | io_kit_sys::ret::kIOReturnUnderrun => Ok(()),
        io_kit_sys::ret::kIOReturnNoDevice => Err(TransferError::Disconnected),
        io_kit_sys::ret::kIOReturnAborted | iokit_c::kIOUSBTransactionTimeout => {
            Err(TransferError::Cancelled)
        }
        iokit_c::kIOUSBPipeStalled => Err(TransferError::Stall),
        io_kit_sys::ret::kIOReturnBadArgument => Err(TransferError::InvalidArgument), // used for `submit_err`
        _ => Err(TransferError::Unknown(status as u32)),
    }
}

pub fn format_os_error_code(f: &mut std::fmt::Formatter<'_>, code: u32) -> std::fmt::Result {
    write!(f, "error 0x{:08x}", code)
}

impl crate::error::Error {
    pub(crate) fn new_os(kind: ErrorKind, message: &'static str, code: IOReturn) -> Self {
        Self {
            kind,
            code: NonZeroU32::new(code as u32),
            message,
        }
    }
}
