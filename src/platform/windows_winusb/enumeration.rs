use std::io::ErrorKind;

use log::debug;
use windows_sys::Win32::Devices::{
    Properties::{
        DEVPKEY_Device_Address, DEVPKEY_Device_BusNumber, DEVPKEY_Device_DeviceDesc,
        DEVPKEY_Device_FriendlyName, DEVPKEY_Device_InstanceId, DEVPKEY_Device_Manufacturer,
        DEVPKEY_Device_Parent, DEVPKEY_Device_Service, DEVPKEY_NAME,
    },
    Usb::{GUID_DEVINTERFACE_USB_DEVICE, USB_DEVICE_SPEED},
};

use crate::{DeviceInfo, Error, Speed};

use super::{
    hub::HubHandle,
    setupapi::{self, DeviceInfoSet},
};

pub fn list_devices() -> Result<impl Iterator<Item = DeviceInfo>, Error> {
    let dset =
        DeviceInfoSet::get_by_setup_class(GUID_DEVINTERFACE_USB_DEVICE, None).map_err(|_| {
            Error::new(
                ErrorKind::UnexpectedEof,
                String::from("failed to list devices"),
            )
        })?;

    let devs: Vec<_> = dset.iter_devices().flat_map(probe_device).collect();
    Ok(devs.into_iter())
}

pub fn probe_device(dev: setupapi::DeviceInfo) -> Option<DeviceInfo> {
    let instance_id = dev.get_string_property(DEVPKEY_Device_InstanceId)?;
    debug!("Probing device {instance_id:?}");
    let parent_instance_id = dev.get_string_property(DEVPKEY_Device_Parent)?;
    let bus_number = dev.get_u32_property(DEVPKEY_Device_BusNumber)?;
    let port_number = dev.get_u32_property(DEVPKEY_Device_Address)?;

    let hub = HubHandle::by_instance_id(&parent_instance_id)?;
    let info = hub.get_node_connection_info(port_number).ok()?;

    // Windows sets some SetupAPI properties from string descriptors read at enumeration,
    // but if the device doesn't set the descriptor, we don't want the value Windows made up.
    let product_string = if info.DeviceDescriptor.iProduct != 0 {
        dev.get_string_property(DEVPKEY_Device_FriendlyName)
            .and_then(|s| s.into_string().ok())
    } else {
        None
    };

    let manufacturer_string = if info.DeviceDescriptor.iProduct != 0 {
        dev.get_string_property(DEVPKEY_Device_Manufacturer)
            .and_then(|s| s.into_string().ok())
    } else {
        None
    };

    let serial_number = if info.DeviceDescriptor.iSerialNumber != 0 {
        (&instance_id)
            .to_str()
            .and_then(|s| s.rsplit_once("\\").map(|(_, s)| s.to_string()))
    } else {
        None
    };

    let driver = dev
        .get_string_property(DEVPKEY_Device_Service)
        .and_then(|s| s.into_string().ok());

    Some(DeviceInfo {
        instance_id,
        parent_instance_id,
        port_number,
        driver,
        bus_number: bus_number as u8,
        device_address: info.DeviceAddress as u8,
        vendor_id: info.DeviceDescriptor.idVendor,
        product_id: info.DeviceDescriptor.idProduct,
        device_version: info.DeviceDescriptor.bcdDevice,
        class: info.DeviceDescriptor.bDeviceClass,
        subclass: info.DeviceDescriptor.bDeviceSubClass,
        protocol: info.DeviceDescriptor.bDeviceProtocol,
        speed: map_speed(info.Speed),
        manufacturer_string,
        product_string,
        serial_number,
    })
}

fn map_speed(speed: u8) -> Option<Speed> {
    #![allow(non_upper_case_globals)]
    use windows_sys::Win32::Devices::Usb::{
        UsbFullSpeed, UsbHighSpeed, UsbLowSpeed, UsbSuperSpeed,
    };

    match speed as USB_DEVICE_SPEED {
        UsbLowSpeed => Some(Speed::Low),
        UsbFullSpeed => Some(Speed::Full),
        UsbHighSpeed => Some(Speed::High),
        UsbSuperSpeed => Some(Speed::Super),
        _ => None,
    }
}
