use std::{
    collections::{HashMap, VecDeque},
    ffi::{OsStr, OsString},
    io::ErrorKind,
};

use log::debug;
use windows_sys::Win32::Devices::{
    Properties::{
        DEVPKEY_Device_Address, DEVPKEY_Device_BusReportedDeviceDesc, DEVPKEY_Device_CompatibleIds,
        DEVPKEY_Device_EnumeratorName, DEVPKEY_Device_HardwareIds, DEVPKEY_Device_InstanceId,
        DEVPKEY_Device_LocationInfo, DEVPKEY_Device_LocationPaths, DEVPKEY_Device_Parent,
        DEVPKEY_Device_Service,
    },
    Usb::{GUID_DEVINTERFACE_USB_DEVICE, GUID_DEVINTERFACE_USB_HOST_CONTROLLER},
};

use crate::{DeviceInfo, Error, InterfaceInfo};

use super::{
    cfgmgr32::{self, get_device_interface_property, DevInst},
    hub::HubPort,
    util::WCString,
};

pub fn list_devices() -> Result<impl Iterator<Item = DeviceInfo>, Error> {
    let bus_devs = cfgmgr32::list_interfaces(GUID_DEVINTERFACE_USB_HOST_CONTROLLER, None)
        .iter()
        .flat_map(|i| get_device_interface_property::<WCString>(i, DEVPKEY_Device_InstanceId))
        .flat_map(|d| DevInst::from_instance_id(&d))
        .flat_map(|d| d.children())
        .map(|d| d.instance_id().to_string())
        .enumerate()
        .map(|v| (v.1, (v.0 + 1) as u8))
        .collect::<HashMap<String, u8>>();
    let devs: Vec<DeviceInfo> = cfgmgr32::list_interfaces(GUID_DEVINTERFACE_USB_DEVICE, None)
        .iter()
        .flat_map(|i| get_device_interface_property::<WCString>(i, DEVPKEY_Device_InstanceId))
        .flat_map(|d| DevInst::from_instance_id(&d))
        .flat_map(|i| probe_device(i, &bus_devs))
        .collect();
    Ok(devs.into_iter())
}

pub fn probe_device(devinst: DevInst, bus_devs: &HashMap<String, u8>) -> Option<DeviceInfo> {
    let instance_id = devinst.get_property::<OsString>(DEVPKEY_Device_InstanceId)?;
    debug!("Probing device {instance_id:?}");

    let parent_instance_id = devinst.get_property::<OsString>(DEVPKEY_Device_Parent)?;

    let hub_port = HubPort::by_child_devinst(devinst).ok()?;
    let info = hub_port.get_info().ok()?;

    let product_string = devinst
        .get_property::<OsString>(DEVPKEY_Device_BusReportedDeviceDesc)
        .and_then(|s| s.into_string().ok());

    let serial_number = if info.device_desc.iSerialNumber != 0 {
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

    let (bus_number, port_chain) = get_port_chain(devinst, bus_devs);
    let port_chain = port_chain.collect::<Vec<u32>>();

    Some(DeviceInfo {
        instance_id,
        parent_instance_id,
        devinst,
        driver: Some(driver).filter(|s| !s.is_empty()),
        bus_number,
        port_number: *port_chain.last().unwrap_or(&0),
        port_chain,
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

/// Find the path to open for an interface of a device
///
/// If the whole device is bound to WinUSB, it can be opened directly. For a
/// composite device, USB interfaces are represented by child device nodes.
pub(crate) fn find_device_interface_path(dev: DevInst, intf: u8) -> Result<WCString, Error> {
    let driver = dev
        .get_property::<OsString>(DEVPKEY_Device_Service)
        .and_then(|s| s.into_string().ok())
        .unwrap_or_default();

    if driver.eq_ignore_ascii_case("usbccgp") {
        let child = dev
            .children()
            .find(|i| get_interface_number(*i) == Some(intf))
            .ok_or_else(|| Error::new(ErrorKind::NotFound, "Interface not found"))?;

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
    } else if driver.eq_ignore_ascii_case("winusb") {
        let paths = dev.interfaces(GUID_DEVINTERFACE_USB_DEVICE);

        let Some(path) = paths.iter().next() else {
            return Err(Error::new(
                ErrorKind::Other,
                "Failed to find device path for WinUSB device",
            ));
        };

        Ok(path.to_owned())
    } else {
        return Err(Error::new(
            ErrorKind::Unsupported,
            format!("Device driver is {driver:?}, not WinUSB"),
        ));
    }
}

fn get_port_chain(dev: DevInst, bus_devs: &HashMap<String, u8>) -> (u8, impl Iterator<Item = u32>) {
    let mut bus_number = 0;
    let mut port_chain = VecDeque::new();

    if let Some(port_number) = get_port_number(dev) {
        port_chain.push_back(port_number);
    }

    if let Some(mut parent) = dev.parent() {
        loop {
            if parent
                .get_property::<OsString>(DEVPKEY_Device_EnumeratorName)
                .unwrap_or_default()
                .eq_ignore_ascii_case("USB")
            {
                if let Some(port_number) = get_port_number(parent) {
                    if port_number != 0 {
                        port_chain.push_front(port_number);
                        if let Some(d) = parent.parent() {
                            parent = d;
                            continue;
                        }
                    } else {
                        if let Some(bus_dev) =
                            parent.get_property::<WCString>(DEVPKEY_Device_InstanceId)
                        {
                            bus_number = *bus_devs.get(&bus_dev.to_string()).unwrap_or(&0);
                        }
                    }
                }
            }
            break;
        }
    }

    (bus_number, port_chain.into_iter())
}

fn get_port_number(devinst: DevInst) -> Option<u32> {
    // Find Port_#xxxx
    // Port_#0002.Hub_#000D
    if let Some(location_info) = devinst.get_property::<OsString>(DEVPKEY_Device_LocationInfo) {
        let s = location_info.to_string_lossy();
        if &s[0..6] == "Port_#" {
            if let Ok(n) = s[6..10].parse::<u32>() {
                return Some(n);
            }
        }
    }
    // Find last #USB(x)
    // PCIROOT(B2)#PCI(0300)#PCI(0000)#USBROOT(0)#USB(1)#USB(2)#USBMI(3)
    if let Some(location_paths) =
        devinst.get_property::<Vec<OsString>>(DEVPKEY_Device_LocationPaths)
    {
        for location_path in location_paths {
            let s = location_path.to_string_lossy();
            for b in s.split('#').rev() {
                if b.contains("USB(") {
                    if let Ok(n) = b[5..b.len()].parse::<u32>() {
                        return Some(n);
                    }
                    break;
                }
            }
        }
    }
    devinst.get_property::<u32>(DEVPKEY_Device_Address)
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
