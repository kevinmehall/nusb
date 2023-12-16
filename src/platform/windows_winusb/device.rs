use std::{
    collections::HashMap,
    ffi::OsString,
    io::{self, ErrorKind},
    os::windows::prelude::OwnedHandle,
    sync::Arc,
};

use log::{debug, error};
use windows_sys::Win32::{
    Devices::{
        Properties::DEVPKEY_Device_Address,
        Usb::{
            WinUsb_Free, WinUsb_Initialize, WinUsb_SetCurrentAlternateSetting,
            GUID_DEVINTERFACE_USB_DEVICE, WINUSB_INTERFACE_HANDLE,
        },
    },
    Foundation::{FALSE, TRUE},
};

use crate::{
    descriptors::{validate_config_descriptor, DESCRIPTOR_TYPE_CONFIGURATION},
    transfer::{EndpointType, TransferHandle},
    DeviceInfo, Error,
};

use super::{
    hub::HubHandle,
    setupapi::DeviceInfoSet,
    util::{create_file, raw_handle},
};

pub(crate) struct WindowsDevice {
    config_descriptors: Vec<Vec<u8>>,
    interface_paths: HashMap<u8, OsString>,
}

impl WindowsDevice {
    pub(crate) fn from_device_info(d: &DeviceInfo) -> Result<Arc<WindowsDevice>, Error> {
        debug!("Creating device for {:?}", d.instance_id);

        // Look up the device again in case the DeviceInfo is stale.
        // In particular, don't trust its `port_number` because another device might now be connected to
        // that port, and we'd get its descriptors instead.
        let dset = DeviceInfoSet::get(Some(GUID_DEVINTERFACE_USB_DEVICE), Some(&d.instance_id))?;
        let Some(dev) = dset.iter_devices().next() else {
            return Err(Error::new(ErrorKind::NotConnected, "Device not connected"));
        };

        let hub_handle = HubHandle::by_instance_id(&d.parent_instance_id)
            .ok_or_else(|| Error::new(ErrorKind::Other, "failed to open parent hub"))?;
        let Some(hub_port_number) = dev.get_u32_property(DEVPKEY_Device_Address) else {
            return Err(Error::new(
                ErrorKind::NotConnected,
                "Could not find hub port number",
            ));
        };

        let connection_info = hub_handle.get_node_connection_info(hub_port_number)?;

        let num_configurations = connection_info.DeviceDescriptor.bNumConfigurations;

        let config_descriptors = (0..num_configurations)
            .flat_map(|i| {
                let res =
                    hub_handle.get_descriptor(hub_port_number, DESCRIPTOR_TYPE_CONFIGURATION, i, 0);
                match res {
                    Ok(v) => validate_config_descriptor(&v[..]).map(|_| v),
                    Err(e) => {
                        error!("Failed to read config descriptor {}: {}", i, e);
                        None
                    }
                }
            })
            .collect();

        Ok(Arc::new(WindowsDevice {
            interface_paths: d.interfaces.clone(),
            config_descriptors,
        }))
    }

    pub(crate) fn active_configuration_value(&self) -> u8 {
        1
    }

    pub(crate) fn configuration_descriptors(&self) -> impl Iterator<Item = &[u8]> {
        self.config_descriptors.iter().map(|d| &d[..])
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
