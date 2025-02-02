use crate::transfer::{internal::Idle, TransferError};

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
pub(crate) type Transfer = Idle<TransferData>;

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
        io_kit_sys::ret::kIOReturnAborted => Err(TransferError::Cancelled),
        iokit_c::kIOUSBPipeStalled => Err(TransferError::Stall),
        _ => Err(TransferError::Unknown),
    }
}
