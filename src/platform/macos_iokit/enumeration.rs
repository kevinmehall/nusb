use std::io::ErrorKind;

use core_foundation::{
    base::{CFType, TCFType},
    number::CFNumber,
    string::CFString,
    ConcreteCFType,
};
use io_kit_sys::{
    kIOMasterPortDefault, kIORegistryIterateParents, kIORegistryIterateRecursively,
    keys::kIOServicePlane, ret::kIOReturnSuccess, usb::lib::kIOUSBDeviceClassName,
    IORegistryEntryGetChildIterator, IORegistryEntryGetRegistryEntryID,
    IORegistryEntrySearchCFProperty, IOServiceGetMatchingServices, IOServiceMatching,
};
use log::debug;

use crate::{DeviceInfo, Error, InterfaceInfo, Speed};

use super::iokit::{IoService, IoServiceIterator};
/// IOKit class name for PCI USB XHCI high-speed controllers (USB 3.0+)
#[allow(non_upper_case_globals)]
const kAppleUSBXHCI: *const ::std::os::raw::c_char =
    b"AppleUSBXHCI\x00" as *const [u8; 13usize] as *const ::std::os::raw::c_char;
/// IOKit class name for PCI USB EHCI high-speed controllers (USB 2.0)
#[allow(non_upper_case_globals)]
const kAppleUSBEHCI: *const ::std::os::raw::c_char =
    b"AppleUSBEHCI\x00" as *const [u8; 13usize] as *const ::std::os::raw::c_char;
/// IOKit class name for PCI USB OHCI full-speed controllers (USB 1.1)
#[allow(non_upper_case_globals)]
const kAppleUSBOHCI: *const ::std::os::raw::c_char =
    b"AppleUSBOHCI\x00" as *const [u8; 13usize] as *const ::std::os::raw::c_char;
/// IOKit class name for virtual internal controller (T2 chip)
#[allow(non_upper_case_globals)]
const kAppleUSBVHCI: *const ::std::os::raw::c_char =
    b"AppleUSBVHCI\x00" as *const [u8; 13usize] as *const ::std::os::raw::c_char;

pub(crate) enum AppleUSBController {
    /// xHCI controller (USB 3.0+)
    XHCI,
    /// EHCI controller (USB 2.0)
    EHCI,
    /// OHCI controller (USB 1.1)
    OHCI,
    /// VHCI controller (T2 chip)
    VHCI,
}

fn usb_service_iter(service: *const ::std::os::raw::c_char) -> Result<IoServiceIterator, Error> {
    unsafe {
        let dictionary = IOServiceMatching(service);
        if dictionary.is_null() {
            return Err(Error::new(ErrorKind::Other, "IOServiceMatching failed"));
        }

        let mut iterator = 0;
        let r = IOServiceGetMatchingServices(kIOMasterPortDefault, dictionary, &mut iterator);
        if r != kIOReturnSuccess {
            return Err(Error::from_raw_os_error(r));
        }

        Ok(IoServiceIterator::new(iterator))
    }
}

pub fn list_devices() -> Result<impl Iterator<Item = DeviceInfo>, Error> {
    Ok(usb_service_iter(kIOUSBDeviceClassName)?.filter_map(probe_device))
}

pub fn list_root_hubs() -> Result<impl Iterator<Item = DeviceInfo>, Error> {
    // Chain all the HCI types into one iterator
    // A bit of a hack, could maybe probe IOPCIDevice and filter on children with IOClass.starts_with("AppleUSB")
    Ok(usb_service_iter(kAppleUSBXHCI)?
        .filter_map(|h| probe_root_hub(h, AppleUSBController::XHCI))
        .chain(
            usb_service_iter(kAppleUSBEHCI)?
                .filter_map(|h| probe_root_hub(h, AppleUSBController::EHCI))
                .chain(
                    usb_service_iter(kAppleUSBOHCI)?
                        .filter_map(|h| probe_root_hub(h, AppleUSBController::OHCI))
                        .chain(
                            usb_service_iter(kAppleUSBVHCI)?
                                .filter_map(|h| probe_root_hub(h, AppleUSBController::VHCI)),
                        ),
                ),
        ))
}

pub(crate) fn service_by_registry_id(registry_id: u64) -> Result<IoService, Error> {
    usb_service_iter(kIOUSBDeviceClassName)?
        .find(|dev| get_registry_id(dev) == Some(registry_id))
        .ok_or(Error::new(ErrorKind::NotFound, "not found by registry id"))
}

pub(crate) fn probe_device(device: IoService) -> Option<DeviceInfo> {
    let registry_id = get_registry_id(&device)?;
    log::debug!("Probing device {registry_id:08x}");

    let location_id = get_integer_property(&device, "locationID")? as u32;

    // Can run `ioreg -p IOUSB -l` to see all properties
    Some(DeviceInfo {
        registry_id,
        location_id,
        bus_id: format!("{:02x}", (location_id >> 24) as u8),
        device_address: get_integer_property(&device, "USB Address")? as u8,
        port_chain: parse_location_id(location_id),
        vendor_id: get_integer_property(&device, "idVendor")? as u16,
        product_id: get_integer_property(&device, "idProduct")? as u16,
        device_version: get_integer_property(&device, "bcdDevice")? as u16,
        class: get_integer_property(&device, "bDeviceClass")? as u8,
        subclass: get_integer_property(&device, "bDeviceSubClass")? as u8,
        protocol: get_integer_property(&device, "bDeviceProtocol")? as u8,
        max_packet_size_0: get_integer_property(&device, "bMaxPacketSize0")? as u8,
        speed: get_integer_property(&device, "Device Speed").and_then(map_speed),
        manufacturer_string: get_string_property(&device, "USB Vendor Name"),
        product_string: get_string_property(&device, "USB Product Name"),
        serial_number: get_string_property(&device, "USB Serial Number"),
        interfaces: get_children(&device).map_or(Vec::new(), |iter| {
            iter.flat_map(|child| {
                Some(InterfaceInfo {
                    interface_number: get_integer_property(&child, "bInterfaceNumber")? as u8,
                    class: get_integer_property(&child, "bInterfaceClass")? as u8,
                    subclass: get_integer_property(&child, "bInterfaceSubClass")? as u8,
                    protocol: get_integer_property(&child, "bInterfaceProtocol")? as u8,
                    interface_string: get_string_property(&child, "kUSBString")
                        .or_else(|| get_string_property(&child, "USB Interface Name")),
                })
            })
            .collect()
        }),
    })
}

pub(crate) fn probe_root_hub(
    device: IoService,
    host_controller: AppleUSBController,
) -> Option<DeviceInfo> {
    let registry_id = get_registry_id(&device)?;
    log::debug!("Probing root_hub {registry_id:08x}");

    let location_id = get_integer_property(&device, "locationID")? as u32;
    // "IOPCIPrimaryMatch" = "0x15e98086 0x15ec8086 0x15f08086 0x0b278086"
    // can be varying array length and appears to be different parts of Host Controller - all with same VID - so we'll just take first
    let (vid, pid) = if let Some(pci) = get_string_property(&device, "IOPCIPrimaryMatch") {
        match (pci.get(2..6), pci.get(6..10)) {
            (Some(svid), Some(spid)) => {
                let pid = u16::from_str_radix(svid, 16).unwrap_or(0x0000);
                let vid = u16::from_str_radix(spid, 16).unwrap_or(0x0000);
                (vid, pid)
            }
            _ => (0x0000, 0x0000),
        }
    } else {
        (0x0000, 0x0000)
    };

    let (device_version, protocol) = match host_controller {
        AppleUSBController::XHCI => (0x300, 0x03),
        AppleUSBController::EHCI => (0x200, 0x01),
        AppleUSBController::OHCI => (0x110, 0x00),
        AppleUSBController::VHCI => (0x000, 0x00),
    };

    // Can run `ioreg -rc AppleUSBXHCI -d 1` to see all properties
    Some(DeviceInfo {
        registry_id,
        location_id,
        bus_id: format!("{:02x}", (location_id >> 24) as u8),
        device_address: 0,
        port_chain: parse_location_id(location_id),
        vendor_id: vid,
        product_id: pid,
        device_version,
        class: 0x09,
        subclass: 0x00,
        protocol,
        max_packet_size_0: 64,
        speed: None,
        manufacturer_string: get_string_property(&device, "IOProviderClass"),
        product_string: get_string_property(&device, "IOClass"),
        serial_number: get_string_property(&device, "name"), // name is unique system bus name but not always present
        interfaces: Vec::new(),
    })
}

pub(crate) fn get_registry_id(device: &IoService) -> Option<u64> {
    unsafe {
        let mut out = 0;
        let r = IORegistryEntryGetRegistryEntryID(device.get(), &mut out);

        if r == kIOReturnSuccess {
            Some(out)
        } else {
            // not sure this can actually fail.
            debug!("IORegistryEntryGetRegistryEntryID failed with {r}");
            None
        }
    }
}

fn get_property<T: ConcreteCFType>(device: &IoService, property: &'static str) -> Option<T> {
    unsafe {
        let cf_property = CFString::from_static_string(property);

        let raw = IORegistryEntrySearchCFProperty(
            device.get(),
            kIOServicePlane as *mut i8,
            cf_property.as_CFTypeRef() as *const _,
            std::ptr::null(),
            kIORegistryIterateRecursively | kIORegistryIterateParents,
        );

        if raw.is_null() {
            debug!("Device does not have property `{property}`");
            return None;
        }

        let res = CFType::wrap_under_create_rule(raw).downcast_into();

        if res.is_none() {
            debug!("Failed to convert device property `{property}`");
        }

        res
    }
}

fn get_string_property(device: &IoService, property: &'static str) -> Option<String> {
    get_property::<CFString>(device, property).map(|s| s.to_string())
}

fn get_integer_property(device: &IoService, property: &'static str) -> Option<i64> {
    let n = get_property::<CFNumber>(device, property)?;
    n.to_i64().or_else(|| {
        debug!("failed to convert {property} value {n:?} to i64");
        None
    })
}

fn get_children(device: &IoService) -> Result<IoServiceIterator, Error> {
    unsafe {
        let mut iterator = 0;
        let r =
            IORegistryEntryGetChildIterator(device.get(), kIOServicePlane as *mut _, &mut iterator);
        if r != kIOReturnSuccess {
            debug!("IORegistryEntryGetChildIterator failed: {r}");
            return Err(Error::from_raw_os_error(r));
        }

        Ok(IoServiceIterator::new(iterator))
    }
}

fn map_speed(speed: i64) -> Option<Speed> {
    // https://developer.apple.com/documentation/iokit/1425357-usbdevicespeed
    match speed {
        0 => Some(Speed::Low),
        1 => Some(Speed::Full),
        2 => Some(Speed::High),
        3 => Some(Speed::Super),
        4 | 5 => Some(Speed::SuperPlus),
        _ => None,
    }
}

fn parse_location_id(id: u32) -> Vec<u8> {
    let mut chain = vec![];
    let mut shift = id << 8;

    while shift != 0 {
        let port = shift >> 28;
        chain.push(port as u8);
        shift = shift << 4;
    }

    chain
}

#[test]
fn test_parse_location_id() {
    assert_eq!(parse_location_id(0x01234567), vec![2, 3, 4, 5, 6, 7]);
    assert_eq!(parse_location_id(0xff875000), vec![8, 7, 5]);
    assert_eq!(parse_location_id(0x08400000), vec![4]);
    assert_eq!(parse_location_id(0x02040100), vec![0, 4, 0, 1]);
    assert_eq!(parse_location_id(0), vec![]);
}
