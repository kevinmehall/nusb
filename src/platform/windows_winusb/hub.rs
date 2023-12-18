use std::{
    alloc::{self, Layout},
    ffi::{c_void, OsStr},
    io, mem,
    os::windows::prelude::OwnedHandle,
    ptr::{addr_of, null_mut},
    slice,
};

use log::error;
use windows_sys::Win32::{
    Devices::Usb::{
        GUID_DEVINTERFACE_USB_HUB, IOCTL_USB_GET_DESCRIPTOR_FROM_NODE_CONNECTION,
        IOCTL_USB_GET_NODE_CONNECTION_INFORMATION_EX, USB_DESCRIPTOR_REQUEST,
        USB_DESCRIPTOR_REQUEST_0, USB_NODE_CONNECTION_INFORMATION_EX,
    },
    Foundation::TRUE,
    System::IO::DeviceIoControl,
};

use crate::Error;

use super::{
    setupapi::DeviceInfoSet,
    util::{create_file, raw_handle},
};

/// Safe wrapper around hub ioctls used to get descriptors for child devices.
pub struct HubHandle(OwnedHandle);

impl HubHandle {
    pub fn by_instance_id(instance_id: &OsStr) -> Option<HubHandle> {
        let devs = DeviceInfoSet::get(Some(GUID_DEVINTERFACE_USB_HUB), Some(instance_id)).ok()?;
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
    ) -> Result<USB_NODE_CONNECTION_INFORMATION_EX, Error> {
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
                let err = io::Error::last_os_error();
                error!("Hub DeviceIoControl failed: {err:?}");
                Err(err)
            }
        }
    }

    pub fn get_descriptor(
        &self,
        port_number: u32,
        descriptor_type: u8,
        descriptor_index: u8,
        language_id: u16,
    ) -> Result<Vec<u8>, Error> {
        let length = 4096;

        unsafe {
            let layout = Layout::from_size_align(
                mem::size_of::<USB_DESCRIPTOR_REQUEST>() + length,
                mem::align_of::<USB_DESCRIPTOR_REQUEST>(),
            )
            .unwrap();

            let req = alloc::alloc(layout).cast::<USB_DESCRIPTOR_REQUEST>();

            req.write(USB_DESCRIPTOR_REQUEST {
                ConnectionIndex: port_number,
                SetupPacket: USB_DESCRIPTOR_REQUEST_0 {
                    bmRequest: 0x80,
                    bRequest: 0x06,
                    wValue: ((descriptor_type as u16) << 8) | descriptor_index as u16,
                    wIndex: language_id,
                    wLength: length as u16,
                },
                Data: [0],
            });

            let mut bytes_returned: u32 = 0;
            let r = DeviceIoControl(
                raw_handle(&self.0),
                IOCTL_USB_GET_DESCRIPTOR_FROM_NODE_CONNECTION,
                req as *const c_void,
                layout.size() as u32,
                req as *mut c_void,
                layout.size() as u32,
                &mut bytes_returned,
                null_mut(),
            );

            let res = if r == TRUE {
                let start = addr_of!((*req).Data[0]);
                let end = (req as *mut u8).offset(bytes_returned as isize);
                let len = end.offset_from(start) as usize;
                let vec = slice::from_raw_parts(start, len).to_owned();
                Ok(vec)
            } else {
                let err = io::Error::last_os_error();
                error!("Hub get descriptor failed: {err:?}");
                Err(err)
            };

            alloc::dealloc(req as *mut _, layout);

            res
        }
    }
}
