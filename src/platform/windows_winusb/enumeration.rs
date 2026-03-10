use std::ffi::{OsStr, OsString};

use log::debug;
use windows_sys::Win32::Devices::{
    Properties::{
        DEVPKEY_Device_Address, DEVPKEY_Device_BusReportedDeviceDesc, DEVPKEY_Device_DeviceDesc,
        DEVPKEY_Device_HardwareIds, DEVPKEY_Device_InstanceId, DEVPKEY_Device_LocationPaths,
        DEVPKEY_Device_Parent, DEVPKEY_Device_Service,
    },
    Usb::{GUID_DEVINTERFACE_USB_DEVICE, GUID_DEVINTERFACE_USB_HUB},
};

use crate::{
    descriptors::{
        decode_string_descriptor, language_id::US_ENGLISH, ConfigurationDescriptor,
        DESCRIPTOR_TYPE_CONFIGURATION, DESCRIPTOR_TYPE_STRING,
    },
    maybe_future::{blocking::Blocking, MaybeFuture},
    platform::windows_winusb::util::DEFAULT_TRANSFER_TIMEOUT,
    BusInfo, DeviceInfo, Error, ErrorKind, InterfaceInfo, UsbControllerType,
};

use super::{
    cfgmgr32::{self, get_device_interface_property, DevInst},
    hub::HubPort,
    util::WCString,
};

pub fn list_devices() -> impl MaybeFuture<Output = Result<impl Iterator<Item = DeviceInfo>, Error>>
{
    Blocking::new(|| {
        let devs: Vec<DeviceInfo> = cfgmgr32::list_interfaces(GUID_DEVINTERFACE_USB_DEVICE, None)
            // get USB_HUB devices as well, like other platforms. ROOT_HUBs will be dropped by probe_device
            .iter()
            .chain(cfgmgr32::list_interfaces(GUID_DEVINTERFACE_USB_HUB, None).iter())
            .flat_map(|i| get_device_interface_property::<WCString>(i, DEVPKEY_Device_InstanceId))
            .flat_map(|d| DevInst::from_instance_id(&d))
            .flat_map(probe_device)
            .collect();
        Ok(devs.into_iter())
    })
}

pub fn list_buses() -> impl MaybeFuture<Output = Result<impl Iterator<Item = BusInfo>, Error>> {
    Blocking::new(|| {
        let devs: Vec<BusInfo> = cfgmgr32::list_interfaces(GUID_DEVINTERFACE_USB_HUB, None)
            .iter()
            .flat_map(|i| get_device_interface_property::<WCString>(i, DEVPKEY_Device_InstanceId))
            .flat_map(|d| DevInst::from_instance_id(&d))
            .flat_map(probe_bus)
            .collect();
        Ok(devs.into_iter())
    })
}

pub fn probe_device(devinst: DevInst) -> Option<DeviceInfo> {
    let instance_id = devinst.get_property::<OsString>(DEVPKEY_Device_InstanceId)?;
    if instance_id.to_string_lossy().starts_with("USB\\ROOT_HUB") {
        return None;
    }

    debug!("Probing device {instance_id:?}");

    let parent_instance_id = devinst.get_property::<OsString>(DEVPKEY_Device_Parent)?;
    let port_number = devinst.get_property::<u32>(DEVPKEY_Device_Address)?;

    let hub_port = HubPort::by_child_devinst(devinst).ok()?;
    let info = hub_port.get_info().ok()?;

    let product_string = devinst
        .get_property::<OsString>(DEVPKEY_Device_BusReportedDeviceDesc)
        .and_then(|s| s.into_string().ok());
    // DEVPKEY_Device_Manufacturer exists but is often wrong and appears not to be read from the string descriptor but the .inf file

    let serial_number = if info.device_desc.iSerialNumber != 0 {
        // Experimentally confirmed, the string descriptor is cached and this does
        // not perform IO. However, the language ID list is not cached, so we
        // have to assume 0x0409 (which will be right 99% of the time).
        hub_port
            .get_descriptor(
                DESCRIPTOR_TYPE_STRING,
                info.device_desc.iSerialNumber,
                US_ENGLISH,
                DEFAULT_TRANSFER_TIMEOUT,
            )
            .ok()
            .and_then(|data| decode_string_descriptor(&data).ok())
    } else {
        None
    };

    let driver = get_driver_name(devinst);

    let mut interfaces =
        list_interfaces_from_desc(&hub_port, info.active_config).unwrap_or_default();

    if driver.eq_ignore_ascii_case("usbccgp") {
        // Populate interface descriptor strings when available from child device nodes.
        devinst
            .children()
            .flat_map(|intf| {
                let interface_number = get_interface_number(intf)?;
                let interface_string =
                    intf.get_property::<OsString>(DEVPKEY_Device_BusReportedDeviceDesc)?;
                Some((interface_number, interface_string))
            })
            .for_each(|(intf_num, interface_string)| {
                if let Some(interface_info) = interfaces
                    .iter_mut()
                    .find(|i| i.interface_number == intf_num)
                {
                    interface_info.interface_string = interface_string.into_string().ok();
                }
            });
    }

    let location_paths = devinst
        .get_property::<Vec<OsString>>(DEVPKEY_Device_LocationPaths)
        .unwrap_or_default();

    let (bus_id, port_chain) = location_paths
        .iter()
        .find_map(|p| parse_location_path(p))
        .unwrap_or_default();

    Some(DeviceInfo {
        instance_id,
        location_paths,
        parent_instance_id,
        devinst,
        port_number,
        port_chain,
        driver: Some(driver).filter(|s| !s.is_empty()),
        bus_id,
        device_address: info.address,
        vendor_id: info.device_desc.idVendor,
        product_id: info.device_desc.idProduct,
        device_version: info.device_desc.bcdDevice,
        usb_version: info.device_desc.bcdUSB,
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

pub fn probe_bus(devinst: DevInst) -> Option<BusInfo> {
    let instance_id = devinst.get_property::<OsString>(DEVPKEY_Device_InstanceId)?;
    // Skip non-root hubs; buses which have instance IDs starting with "USB\\ROOT_HUB"
    if !instance_id.to_string_lossy().starts_with("USB\\ROOT_HUB") {
        return None;
    }

    debug!("Probing bus {instance_id:?}");

    let parent_instance_id = devinst.get_property::<WCString>(DEVPKEY_Device_Parent)?;
    let parent_devinst = DevInst::from_instance_id(&parent_instance_id)?;
    // parent service contains controller type in service field
    let controller_type = parent_devinst
        .get_property::<OsString>(DEVPKEY_Device_Service)
        .and_then(|s| UsbControllerType::from_str(&s.to_string_lossy()));

    let root_hub_description = devinst
        .get_property::<OsString>(DEVPKEY_Device_DeviceDesc)?
        .to_string_lossy()
        .to_string();

    let driver = get_driver_name(devinst);

    let location_paths = devinst
        .get_property::<Vec<OsString>>(DEVPKEY_Device_LocationPaths)
        .unwrap_or_default();

    let (bus_id, _) = location_paths
        .iter()
        .find_map(|p| parse_location_path(p))
        .unwrap_or_default();

    Some(BusInfo {
        instance_id,
        parent_instance_id: parent_instance_id.into(),
        location_paths,
        devinst,
        driver: Some(driver).filter(|s| !s.is_empty()),
        bus_id,
        controller_type,
        root_hub_description,
    })
}

fn list_interfaces_from_desc(hub_port: &HubPort, active_config: u8) -> Option<Vec<InterfaceInfo>> {
    let buf = hub_port
        .get_descriptor(
            DESCRIPTOR_TYPE_CONFIGURATION,
            active_config.saturating_sub(1),
            0,
            DEFAULT_TRANSFER_TIMEOUT,
        )
        .ok()?;
    let desc = ConfigurationDescriptor::new(&buf[..])?;

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
            "failed to find device path for WinUSB device",
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
            "could not determine driver for interface",
        ));
    };

    if !driver.eq_ignore_ascii_case("winusb") {
        debug!("Incompatible driver {driver:?} for interface, not WinUSB");
        return Err(Error::new(
            ErrorKind::Unsupported,
            "incompatible driver is installed for this interface",
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
            ErrorKind::Unsupported,
            "failed to find device path for WinUSB interface",
        )
        .log_debug());
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

fn parse_location_path(s: &OsStr) -> Option<(String, Vec<u8>)> {
    let s = s.to_str()?;

    let usbroot = "#USBROOT(";
    let start_i = s.find(usbroot)?;
    let close_i = s[start_i + usbroot.len()..].find(')')?;
    let (bus, mut s) = s.split_at(start_i + usbroot.len() + close_i + 1);

    let mut path = vec![];

    while let Some((_, next)) = s.split_once("#USB(") {
        let (port_num, next) = next.split_once(")")?;
        path.push(port_num.parse().ok()?);
        s = next;
    }

    Some((bus.to_owned(), path))
}

#[test]
fn test_parse_location_path() {
    assert_eq!(
        parse_location_path(OsStr::new(
            "PCIROOT(0)#PCI(0201)#PCI(0000)#USBROOT(0)#USB(23)#USB(2)#USB(1)#USB(3)#USB(4)"
        )),
        Some((
            "PCIROOT(0)#PCI(0201)#PCI(0000)#USBROOT(0)".into(),
            vec![23, 2, 1, 3, 4]
        ))
    );
    assert_eq!(
        parse_location_path(OsStr::new(
            "PCIROOT(0)#PCI(0201)#PCI(0000)#USBROOT(1)#USB(16)"
        )),
        Some(("PCIROOT(0)#PCI(0201)#PCI(0000)#USBROOT(1)".into(), vec![16]))
    );
    assert_eq!(
        parse_location_path(OsStr::new(
            "ACPI(_SB_)#ACPI(PCI0)#ACPI(S11_)#ACPI(S00_)#ACPI(RHUB)#ACPI(HS04)"
        )),
        None
    );
}
