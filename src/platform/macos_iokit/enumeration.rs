use std::{collections::VecDeque, io::ErrorKind};

use core_foundation::{
    base::{CFType, TCFType},
    data::CFData,
    number::CFNumber,
    string::CFString,
    ConcreteCFType,
};
use io_kit_sys::{
    kIOMasterPortDefault, kIORegistryIterateParents, kIORegistryIterateRecursively,
    keys::kIOServicePlane, ret::kIOReturnSuccess, usb::lib::kIOUSBDeviceClassName,
    IORegistryEntryGetChildIterator, IORegistryEntryGetParentEntry,
    IORegistryEntryGetRegistryEntryID, IORegistryEntrySearchCFProperty,
    IOServiceGetMatchingServices, IOServiceMatching,
};
use log::debug;

use crate::{DeviceInfo, Error, InterfaceInfo, Speed};

use super::iokit::{IoService, IoServiceIterator};

fn usb_service_iter() -> Result<IoServiceIterator, Error> {
    unsafe {
        let dictionary = IOServiceMatching(kIOUSBDeviceClassName);
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
    Ok(usb_service_iter()?.filter_map(probe_device))
}

pub(crate) fn service_by_registry_id(registry_id: u64) -> Result<IoService, Error> {
    usb_service_iter()?
        .find(|dev| get_id(dev) == Some(registry_id))
        .ok_or(Error::new(ErrorKind::NotFound, "not found by registry id"))
}

fn probe_device(device: IoService) -> Option<DeviceInfo> {
    let registry_id = get_id(&device)?;
    log::debug!("Probing device {registry_id}");

    // Can run `ioreg -p IOUSB -l` to see all properties
    let location_id = get_integer_property(&device, "locationID")?;
    let port_chain: Vec<u32> = get_port_chain(&device).collect();
    Some(DeviceInfo {
        registry_id,
        location_id,
        bus_number: (location_id >> 24) as u8,
        port_number: *port_chain.last().unwrap_or(&0),
        port_chain,
        device_address: get_integer_property(&device, "USB Address")?,
        vendor_id: get_integer_property(&device, "idVendor")?,
        product_id: get_integer_property(&device, "idProduct")?,
        device_version: get_integer_property(&device, "bcdDevice")?,
        class: get_integer_property(&device, "bDeviceClass")?,
        subclass: get_integer_property(&device, "bDeviceSubClass")?,
        protocol: get_integer_property(&device, "bDeviceProtocol")?,
        speed: get_integer_property(&device, "Device Speed").and_then(map_speed),
        manufacturer_string: get_string_property(&device, "USB Vendor Name"),
        product_string: get_string_property(&device, "USB Product Name"),
        serial_number: get_string_property(&device, "USB Serial Number"),
        interfaces: get_children(&device).map_or(Vec::new(), |iter| {
            iter.flat_map(|child| {
                Some(InterfaceInfo {
                    interface_number: get_integer_property(&child, "bInterfaceNumber")?,
                    class: get_integer_property(&child, "bInterfaceClass")?,
                    subclass: get_integer_property(&child, "bInterfaceSubClass")?,
                    protocol: get_integer_property(&child, "bInterfaceProtocol")?,
                    interface_string: get_string_property(&child, "kUSBString")
                        .or_else(|| get_string_property(&child, "USB Interface Name")),
                })
            })
            .collect()
        }),
    })
}

fn get_id(device: &IoService) -> Option<u64> {
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

fn get_data_property(device: &IoService, property: &'static str) -> Option<Vec<u8>> {
    get_property::<CFData>(device, property).map(|d| d.to_vec())
}

fn get_integer_property<T: TryFrom<i64>>(device: &IoService, property: &'static str) -> Option<T> {
    get_property::<CFNumber>(device, property)
        .and_then(|n| n.to_i64())
        .and_then(|n| n.try_into().ok())
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

fn get_parent(device: &IoService) -> Result<IoService, Error> {
    unsafe {
        let mut handle = 0;
        let r = IORegistryEntryGetParentEntry(device.get(), kIOServicePlane as *mut _, &mut handle);
        if r != kIOReturnSuccess {
            debug!("IORegistryEntryGetParentEntry failed: {r}");
            return Err(Error::from_raw_os_error(r));
        }

        Ok(IoService::new(handle))
    }
}

fn get_port_number(device: &IoService) -> Option<u32> {
    get_integer_property::<u32>(device, "PortNum").or_else(|| {
        if let Ok(parent) = get_parent(device) {
            return get_data_property(&parent, "port")
                .map(|d| u32::from_ne_bytes(d[0..4].try_into().unwrap()));
        }
        None
    })
}

fn get_port_chain(device: &IoService) -> impl Iterator<Item = u32> {
    let mut port_chain = VecDeque::new();

    if let Some(port_number) = get_port_number(device) {
        port_chain.push_back(port_number);
    }

    if let Ok(mut hub) = get_parent(device) {
        loop {
            let port_number = match get_port_number(&hub) {
                Some(p) => p,
                None => break,
            };
            if port_number == 0 {
                break;
            }
            port_chain.push_front(port_number);

            let session_id = match get_integer_property::<u64>(&hub, "sessionID") {
                Some(session_id) => session_id,
                None => break,
            };

            hub = match get_parent(&hub) {
                Ok(hub) => hub,
                Err(_) => break,
            };

            // Ignore the same sessionID
            if session_id
                == match get_integer_property::<u64>(&hub, "sessionID") {
                    Some(session_id) => session_id,
                    None => break,
                }
            {
                port_chain.pop_front();
                continue;
            }
        }
    }

    port_chain.into_iter()
}

fn map_speed(speed: u32) -> Option<Speed> {
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
