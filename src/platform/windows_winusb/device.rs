use std::{
    collections::HashMap,
    ffi::OsString,
    io::{self, ErrorKind},
    os::windows::prelude::OwnedHandle,
    sync::Arc,
};

use log::{debug, error};
use windows_sys::Win32::{
    Devices::Usb::{WinUsb_Free, WinUsb_Initialize, WINUSB_INTERFACE_HANDLE, WinUsb_SetCurrentAlternateSetting},
    Foundation::{FALSE, TRUE},
};

use crate::{
    transfer::{EndpointType, TransferHandle},
    DeviceInfo, Error,
};

use super::util::{create_file, raw_handle};

pub(crate) struct WindowsDevice {
    interface_paths: HashMap<u8, OsString>,
}

impl WindowsDevice {
    pub(crate) fn from_device_info(d: &DeviceInfo) -> Result<Arc<WindowsDevice>, Error> {
        debug!("Creating device for {:?}", d.instance_id);

        Ok(Arc::new(WindowsDevice {
            interface_paths: d.interfaces.clone(),
        }))
    }

    pub(crate) fn set_configuration(&self, _configuration: u8) -> Result<(), Error> {
        Err(io::Error::new(
            ErrorKind::Unsupported,
            "set_configuration not supported by WinUSB",
        ))
    }

    pub(crate) fn reset(&self) -> Result<(), Error> {
        Err(io::Error::new(
            ErrorKind::Unsupported,
            "reset not supported by WinUSB",
        ))
    }

    pub(crate) fn claim_interface(
        self: &Arc<Self>,
        interface: u8,
    ) -> Result<Arc<WindowsInterface>, Error> {
        let path = self.interface_paths.get(&interface).ok_or_else(|| {
            Error::new(ErrorKind::NotFound, "interface not found or not compatible")
        })?;

        let handle = create_file(path)?;

        super::events::register(&handle)?;

        let winusb_handle = unsafe {
            let mut h = 0;
            if WinUsb_Initialize(raw_handle(&handle), &mut h) == FALSE {
                error!("WinUsb_Initialize failed: {:?}", io::Error::last_os_error());
                return Err(io::Error::last_os_error());
            }
            h
        };

        Ok(Arc::new(WindowsInterface {
            handle,
            winusb_handle,
        }))
    }
}

pub(crate) struct WindowsInterface {
    pub(crate) handle: OwnedHandle,
    pub(crate) winusb_handle: WINUSB_INTERFACE_HANDLE,
}

impl WindowsInterface {
    pub(crate) fn make_transfer(
        self: &Arc<Self>,
        endpoint: u8,
        ep_type: EndpointType,
    ) -> TransferHandle<super::TransferData> {
        TransferHandle::new(super::TransferData::new(self.clone(), endpoint, ep_type))
    }

    pub fn set_alt_setting(&self, alt_setting: u8) -> Result<(), Error> {
        unsafe {
            let r = WinUsb_SetCurrentAlternateSetting(raw_handle(&self.handle), alt_setting.into());
            if r == TRUE {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        }
    }
}

impl Drop for WindowsInterface {
    fn drop(&mut self) {
        unsafe {
            WinUsb_Free(self.winusb_handle);
        }
    }
}
