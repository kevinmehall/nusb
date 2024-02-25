use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
};

use log::{debug, error};
use windows_sys::Win32::Devices::{
    Properties::{
        DEVPKEY_Device_Address, DEVPKEY_Device_BusNumber, DEVPKEY_Device_BusReportedDeviceDesc,
        DEVPKEY_Device_CompatibleIds, DEVPKEY_Device_HardwareIds, DEVPKEY_Device_InstanceId,
        DEVPKEY_Device_Parent, DEVPKEY_Device_Service,
    },
    Usb::{GUID_DEVINTERFACE_USB_DEVICE, USB_DEVICE_SPEED},
};

use crate::{DeviceInfo, Error, InterfaceInfo, Speed};

use super::{
    cfgmgr32::{self, get_device_interface_property, DevInst},
    hub::HubPort,
    util::WCString,
};

pub fn list_devices() -> Result<impl Iterator<Item = DeviceInfo>, Error> {
    let devs: Vec<DeviceInfo> = cfgmgr32::list_interfaces(GUID_DEVINTERFACE_USB_DEVICE, None)
        .iter()
        .flat_map(|i| get_device_interface_property::<WCString>(i, DEVPKEY_Device_InstanceId))
        .flat_map(|d| DevInst::from_instance_id(&d))
        .flat_map(probe_device)
        .collect();
    Ok(devs.into_iter())
}

pub fn probe_device(devinst: DevInst) -> Option<DeviceInfo> {
    let instance_id = devinst.get_property::<OsString>(DEVPKEY_Device_InstanceId)?;
    debug!("Probing device {instance_id:?}");

    let parent_instance_id = devinst.get_property::<OsString>(DEVPKEY_Device_Parent)?;
    let bus_number = devinst.get_property::<u32>(DEVPKEY_Device_BusNumber)?;
    let port_number = devinst.get_property::<u32>(DEVPKEY_Device_Address)?;

    let hub_port = HubPort::by_child_devinst(devinst).ok()?;
    let info = hub_port.get_node_connection_info().ok()?;

    let product_string = devinst
        .get_property::<OsString>(DEVPKEY_Device_BusReportedDeviceDesc)
        .and_then(|s| s.into_string().ok());

    let serial_number = if info.DeviceDescriptor.iSerialNumber != 0 {
        (&instance_id)
            .to_str()
            .and_then(|s| s.rsplit_once("\\").map(|(_, s)| s.to_string()))
    } else {
        None
    };

    let driver = devinst
        .get_property::<OsString>(DEVPKEY_Device_Service)
        .and_then(|s| s.into_string().ok())
        .unwrap_or_default();

    let mut interfaces = if driver.eq_ignore_ascii_case("usbccgp") {
        devinst
            .children()
            .flat_map(|intf| {
                let interface_number = get_interface_number(intf)?;
                let (class, subclass, protocol) = intf
                    .get_property::<Vec<OsString>>(DEVPKEY_Device_CompatibleIds)?
                    .iter()
                    .find_map(|s| parse_compatible_id(s))?;
                let interface_string = intf
                    .get_property::<OsString>(DEVPKEY_Device_BusReportedDeviceDesc)
                    .and_then(|s| s.into_string().ok());

                Some(InterfaceInfo {
                    interface_number,
                    class,
                    subclass,
                    protocol,
                    interface_string,
                })
            })
            .collect()
    } else {
        Vec::new()
    };

    interfaces.sort_unstable_by_key(|i| i.interface_number);

    Some(DeviceInfo {
        instance_id,
        parent_instance_id,
        devinst,
        port_number,
        driver: Some(driver).filter(|s| !s.is_empty()),
        bus_number: bus_number as u8,
        device_address: info.DeviceAddress as u8,
        vendor_id: info.DeviceDescriptor.idVendor,
        product_id: info.DeviceDescriptor.idProduct,
        device_version: info.DeviceDescriptor.bcdDevice,
        class: info.DeviceDescriptor.bDeviceClass,
        subclass: info.DeviceDescriptor.bDeviceSubClass,
        protocol: info.DeviceDescriptor.bDeviceProtocol,
        speed: map_speed(info.Speed),
        manufacturer_string: None,
        product_string,
        serial_number,
        interfaces,
    })
}

/// Find USB interfaces of the device and their Windows interface paths.
///
/// If the whole device is bound to WinUSB, it can be opened directly. For a
/// composite device, USB interfaces are represented by child device nodes.
pub(crate) fn find_device_interfaces(dev: DevInst) -> HashMap<u8, WCString> {
    let driver = dev
        .get_property::<OsString>(DEVPKEY_Device_Service)
        .and_then(|s| s.into_string().ok())
        .unwrap_or_default();

    debug!("Driver is {:?}", driver);

    let mut interfaces = HashMap::new();
    if driver.eq_ignore_ascii_case("usbccgp") {
        interfaces.extend(dev.children().flat_map(probe_interface));
    } else if driver.eq_ignore_ascii_case("winusb") {
        let paths = dev.interfaces(GUID_DEVINTERFACE_USB_DEVICE);

        if let Some(path) = paths.iter().next() {
            interfaces.insert(0, path.to_owned());
        } else {
            error!("Failed to find path for winusb device");
        }
    }

    interfaces
}

fn get_interface_number(intf_dev: DevInst) -> Option<u8> {
    let hw_ids = intf_dev.get_property::<Vec<OsString>>(DEVPKEY_Device_HardwareIds);
    hw_ids
        .as_deref()
        .unwrap_or_default()
        .iter()
        .find_map(|id| parse_hardware_id(id))
        .or_else(|| {
            error!("Failed to parse interface number in hardware IDs: {hw_ids:?}");
            None
        })
}

/// Parse interface number from a Hardware ID value
fn parse_hardware_id(s: &OsStr) -> Option<u8> {
    let s = s.to_str()?;
    let s = s.rsplit_once("&MI_")?.1;
    u8::from_str_radix(s.get(0..2)?, 16).ok()
}

#[test]
fn test_parse_hardware_id() {
    assert_eq!(parse_hardware_id(OsStr::new("")), None);
    assert_eq!(
        parse_hardware_id(OsStr::new("USB\\VID_1234&PID_5678&MI_0A")),
        Some(10)
    );
    assert_eq!(
        parse_hardware_id(OsStr::new("USB\\VID_9999&PID_AAAA&REV_0101&MI_01")),
        Some(1)
    );
}

/// Parse class, subclass, protocol from a Compatible ID value
fn parse_compatible_id(s: &OsStr) -> Option<(u8, u8, u8)> {
    let s = s.to_str()?;
    let s = s.strip_prefix("USB\\Class_")?;
    let class = u8::from_str_radix(s.get(0..2)?, 16).ok()?;
    let s = s.get(2..)?.strip_prefix("&SubClass_")?;
    let subclass = u8::from_str_radix(s.get(0..2)?, 16).ok()?;
    let s = s.get(2..)?.strip_prefix("&Prot_")?;
    let protocol = u8::from_str_radix(s.get(0..2)?, 16).ok()?;
    Some((class, subclass, protocol))
}

#[test]
fn test_parse_compatible_id() {
    assert_eq!(parse_compatible_id(OsStr::new("")), None);
    assert_eq!(parse_compatible_id(OsStr::new("USB\\Class_03")), None);
    assert_eq!(
        parse_compatible_id(OsStr::new("USB\\Class_03&SubClass_11&Prot_22")),
        Some((3, 17, 34))
    );
}

/// Probe a device node for a child device (USB interface) of a composite device
/// to see if it is usable with WinUSB and find the Windows interface used to
/// open it.
fn probe_interface(cdev: DevInst) -> Option<(u8, WCString)> {
    let id = cdev.instance_id();
    debug!("Probing interface `{id}` of composite device");

    let driver = cdev.get_property::<OsString>(DEVPKEY_Device_Service);
    if !driver
        .as_ref()
        .is_some_and(|d| d.eq_ignore_ascii_case("winusb"))
    {
        debug!("Driver is {driver:?}, not usable.");
        return None;
    }

    let intf_num = get_interface_number(cdev)?;

    let reg_key = cdev.registry_key().unwrap();
    let guid = match reg_key.query_value_guid("DeviceInterfaceGUIDs") {
        Ok(s) => s,
        Err(e) => match reg_key.query_value_guid("DeviceInterfaceGUID") {
            Ok(s) => s,
            Err(f) => {
                if e.kind() == f.kind() {
                    error!("Failed to get DeviceInterfaceGUID or DeviceInterfaceGUIDs from registry: {e}");
                } else {
                    error!("Failed to get DeviceInterfaceGUID or DeviceInterfaceGUIDs from registry: {e}, {f}");
                }
                return None;
            }
        },
    };

    let paths = cdev.interfaces(guid);
    let Some(path) = paths.iter().next() else {
        error!("Failed to find interface path");
        return None;
    };

    debug!("Found usable interface {intf_num} at {path}");

    Some((intf_num, path.to_owned()))
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
