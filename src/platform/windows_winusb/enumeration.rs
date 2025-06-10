use std::{
    ffi::{OsStr, OsString},
    io::ErrorKind,
};

use log::debug;
use windows_sys::Win32::Devices::{
    Properties::{
        DEVPKEY_Device_Address, DEVPKEY_Device_BusNumber, DEVPKEY_Device_BusReportedDeviceDesc,
        DEVPKEY_Device_CompatibleIds, DEVPKEY_Device_HardwareIds, DEVPKEY_Device_InstanceId,
        DEVPKEY_Device_Parent, DEVPKEY_Device_Service,
    },
    Usb::GUID_DEVINTERFACE_USB_DEVICE,
};

use crate::{
    descriptors::{
        decode_string_descriptor, language_id::US_ENGLISH, validate_config_descriptor,
        Configuration, DESCRIPTOR_TYPE_CONFIGURATION, DESCRIPTOR_TYPE_STRING,
    },
    DeviceInfo, Error, InterfaceInfo,
};

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
    let info = hub_port.get_info().ok()?;

    let product_string = devinst
        .get_property::<OsString>(DEVPKEY_Device_BusReportedDeviceDesc)
        .and_then(|s| s.into_string().ok());

    let serial_number = if info.device_desc.iSerialNumber != 0 {
        // Experimentally confirmed, the string descriptor is cached and this does
        // not perform IO. However, the language ID list is not cached, so we
        // have to assume 0x0409 (which will be right 99% of the time).
        hub_port
            .get_descriptor(
                DESCRIPTOR_TYPE_STRING,
                info.device_desc.iSerialNumber,
                US_ENGLISH,
            )
            .ok()
            .and_then(|data| decode_string_descriptor(&data).ok())
    } else {
        None
    };

    let driver = get_driver_name(devinst);

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
        list_interfaces_from_desc(&hub_port, info.active_config).unwrap_or(Vec::new())
    };

    interfaces.sort_unstable_by_key(|i| i.interface_number);

    Some(DeviceInfo {
        instance_id,
        parent_instance_id,
        devinst,
        port_number,
        driver: Some(driver).filter(|s| !s.is_empty()),
        bus_number: bus_number as u8,
        device_address: info.address,
        vendor_id: info.device_desc.idVendor,
        product_id: info.device_desc.idProduct,
        device_version: info.device_desc.bcdDevice,
        class: info.device_desc.bDeviceClass,
        subclass: info.device_desc.bDeviceSubClass,
        protocol: info.device_desc.bDeviceProtocol,
        speed: info.speed,
        manufacturer_string: None,
        product_string,
        serial_number,
        interfaces,
    })
}

fn list_interfaces_from_desc(hub_port: &HubPort, active_config: u8) -> Option<Vec<InterfaceInfo>> {
    let buf = hub_port
        .get_descriptor(
            DESCRIPTOR_TYPE_CONFIGURATION,
            active_config.saturating_sub(1),
            0,
        )
        .ok()?;
    let len = validate_config_descriptor(&buf)?;
    let desc = Configuration::new(&buf[..len]);

    if desc.configuration_value() != active_config {
        return None;
    }

    Some(
        desc.interfaces()
            .map(|i| {
                let i_desc = i.first_alt_setting();

                InterfaceInfo {
                    interface_number: i.interface_number(),
                    class: i_desc.class(),
                    subclass: i_desc.subclass(),
                    protocol: i_desc.protocol(),
                    interface_string: None,
                }
            })
            .collect(),
    )
}

pub(crate) fn get_driver_name(dev: DevInst) -> String {
    dev.get_property::<OsString>(DEVPKEY_Device_Service)
        .and_then(|s| s.into_string().ok())
        .unwrap_or_default()
}

/// Get the device path to open for a whole device bound to WinUSB.
pub(crate) fn get_winusb_device_path(dev: DevInst) -> Result<WCString, Error> {
    let paths = dev.interfaces(GUID_DEVINTERFACE_USB_DEVICE);

    let Some(path) = paths.iter().next() else {
        return Err(Error::new(
            ErrorKind::Other,
            "Failed to find device path for WinUSB device",
        ));
    };

    Ok(path.to_owned())
}

/// Find the child PDO of a USBCCGP device for an interface.
///
/// To handle the case when multiple interfaces are represented by one PDO (e.g.
/// with interface association descriptors), this returns the highest-numbered
/// interface less than or equal to the target interface.
///
/// Returns the discovered interface number and DevInst.
pub(crate) fn find_usbccgp_child(dev: DevInst, interface: u8) -> Option<(u8, DevInst)> {
    dev.children()
        .filter_map(|child| Some((get_interface_number(child)?, child)))
        .filter(|(interface_number, _)| *interface_number <= interface)
        .max_by_key(|(interface_number, _)| *interface_number)
}

/// Get the device path to open for a child PDO of a USBCCGP device.
pub(crate) fn get_usbccgp_winusb_device_path(child: DevInst) -> Result<WCString, Error> {
    let Some(driver) = child.get_property::<OsString>(DEVPKEY_Device_Service) else {
        return Err(Error::new(
            ErrorKind::Unsupported,
            "Could not determine driver for interface",
        ));
    };

    if !driver.eq_ignore_ascii_case("winusb") {
        return Err(Error::new(
            ErrorKind::Unsupported,
            format!("Interface driver is {driver:?}, not WinUSB"),
        ));
    }

    let reg_key = child.registry_key().unwrap();
    let guid = match reg_key.query_value_guid("DeviceInterfaceGUIDs") {
        Ok(s) => s,
        Err(e) => match reg_key.query_value_guid("DeviceInterfaceGUID") {
            Ok(s) => s,
            Err(f) => {
                if e.kind() == f.kind() {
                    debug!("Failed to get DeviceInterfaceGUID or DeviceInterfaceGUIDs from registry: {e}");
                } else {
                    debug!("Failed to get DeviceInterfaceGUID or DeviceInterfaceGUIDs from registry: {e}, {f}");
                }
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    "Could not find DeviceInterfaceGUIDs in registry. WinUSB driver may not be correctly installed for this interface."
                ));
            }
        },
    };

    let paths = child.interfaces(guid);
    let Some(path) = paths.iter().next() else {
        return Err(Error::new(
            ErrorKind::Other,
            "Failed to find device path for WinUSB interface",
        ));
    };

    Ok(path.to_owned())
}

fn get_interface_number(intf_dev: DevInst) -> Option<u8> {
    let hw_ids = intf_dev.get_property::<Vec<OsString>>(DEVPKEY_Device_HardwareIds);
    hw_ids
        .as_deref()
        .unwrap_or_default()
        .iter()
        .find_map(|id| parse_hardware_id(id))
        .or_else(|| {
            debug!("Failed to parse interface number in hardware IDs: {hw_ids:?}");
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
