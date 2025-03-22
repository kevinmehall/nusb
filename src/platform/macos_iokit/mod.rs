mod transfer;
use io_kit_sys::ret::IOReturn;
use once_cell::sync::OnceCell;
pub(crate) use transfer::TransferData;

mod enumeration;
mod events;
pub use enumeration::{list_buses, list_devices};

mod device;
pub(crate) use device::MacDevice as Device;
pub(crate) use device::MacInterface as Interface;

mod hotplug;
pub(crate) use hotplug::MacHotplugWatch as HotplugWatch;

use crate::transfer::TransferError;

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

type OsVersion = (u8, u8, u8);

pub(crate) fn os_version() -> OsVersion {
    static VERSION: OnceCell<OsVersion> = OnceCell::new();
    *VERSION.get_or_init(|| {
        read_osproductversion()
            .or_else(|| read_osrelease())
            .unwrap_or((10, 0, 0))
    })
}

fn read_osproductversion() -> Option<OsVersion> {
    unsafe {
        let mut buffer: [libc::c_char; 64] = [0; 64];
        let mut len: libc::size_t = buffer.len() - 1;

        let ret = libc::sysctlbyname(
            "kern.osproductversion\0".as_ptr() as *const libc::c_char,
            buffer.as_mut_ptr() as *mut _,
            &mut len,
            std::ptr::null_mut(),
            0,
        );

        if ret != 0 {
            return None;
        }

        let os_product_version_string = std::ffi::CStr::from_ptr(buffer.as_ptr()).to_str().ok()?;
        let mut parts = os_product_version_string.split(".");
        let major = parts.next().and_then(|s| s.parse().ok())?;
        let minor = parts.next().and_then(|s| s.parse().ok())?;
        let patch = parts.next().and_then(|s| s.parse().ok())?;
        let version = (major, minor, patch);

        return Some(version);
    }
}

fn read_osrelease() -> Option<OsVersion> {
    unsafe {
        let mut buffer: [libc::c_char; 64] = [0; 64];
        let mut len: libc::size_t = buffer.len() - 1;

        let ret = libc::sysctlbyname(
            "kern.osrelease\0".as_ptr() as *const libc::c_char,
            buffer.as_mut_ptr() as *mut _,
            &mut len,
            std::ptr::null_mut(),
            0,
        );

        if ret != 0 {
            return None;
        }

        let os_release_string = std::ffi::CStr::from_ptr(buffer.as_ptr()).to_str().ok()?;
        let mut parts = os_release_string.split(".");
        let darwin_major: u8 = parts.next().and_then(|s| s.parse().ok())?;
        let darwin_minor: u8 = parts.next().and_then(|s| s.parse().ok())?;

        let major;
        let minor;
        let patch;
        if darwin_major == 1 && darwin_minor < 4 {
            major = 10;
            minor = 0;
            patch = 0;
        } else if darwin_major < 6 {
            major = 10;
            minor = 1;
            patch = 0;
        } else if darwin_major < 20 {
            major = 10;
            minor = darwin_major - 4;
            patch = darwin_minor;
        } else {
            major = darwin_major - 9;
            minor = darwin_minor;
            patch = 0;
        }
        let version = (major, minor, patch);

        return Some(version);
    }
}
