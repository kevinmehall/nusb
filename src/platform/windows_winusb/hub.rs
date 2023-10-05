use std::{
    ffi::{c_void, OsStr},
    mem,
    os::windows::prelude::OwnedHandle,
    ptr::null_mut,
};

use log::{debug, error};
use windows_sys::Win32::{
    Devices::Usb::{
        GUID_DEVINTERFACE_USB_HUB, IOCTL_USB_GET_NODE_CONNECTION_INFORMATION_EX,
        USB_NODE_CONNECTION_INFORMATION_EX,
    },
    Foundation::{GetLastError, TRUE, WIN32_ERROR},
    System::IO::DeviceIoControl,
};

use super::{
    setupapi::DeviceInfoSet,
    util::{create_file, raw_handle},
};

/// Safe wrapper around hub ioctls used to get descriptors for child devices.
pub struct HubHandle(OwnedHandle);

impl HubHandle {
    pub fn by_instance_id(instance_id: &OsStr) -> Option<HubHandle> {
        let devs =
            DeviceInfoSet::get_by_setup_class(GUID_DEVINTERFACE_USB_HUB, Some(instance_id)).ok()?;
        let Some(hub_interface) = devs.iter_interfaces(GUID_DEVINTERFACE_USB_HUB).next() else {
            error!("Failed to find hub interface");
            return None;
        };

        let hub_path = hub_interface.get_path()?;

        match create_file(&hub_path) {
            Ok(f) => Some(HubHandle(f)),
            Err(e) => {
                error!("Failed to open hub {hub_path:?}: {e}");
                None
            }
        }
    }

    pub fn get_node_connection_info(
        &self,
        port_number: u32,
    ) -> Result<USB_NODE_CONNECTION_INFORMATION_EX, WIN32_ERROR> {
        unsafe {
            let mut info: USB_NODE_CONNECTION_INFORMATION_EX = mem::zeroed();
            info.ConnectionIndex = port_number;
            let mut bytes_returned: u32 = 0;
            let r = DeviceIoControl(
                raw_handle(&self.0),
                IOCTL_USB_GET_NODE_CONNECTION_INFORMATION_EX,
                &info as *const _ as *const c_void,
                mem::size_of_val(&info) as u32,
                &mut info as *mut _ as *mut c_void,
                mem::size_of_val(&info) as u32,
                &mut bytes_returned,
                null_mut(),
            );

            if r == TRUE {
                Ok(info)
            } else {
                let err = GetLastError();
                error!("Hub DeviceIoControl failed: {err:?}");
                Err(err)
            }
        }
    }
}
