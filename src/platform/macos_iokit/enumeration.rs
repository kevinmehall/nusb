use std::{io::ErrorKind, iter};

use core_foundation::{
    base::{CFType, TCFType},
    number::CFNumber,
    string::CFString,
    ConcreteCFType,
};
use io_kit_sys::{
    kIOMasterPortDefault, kIORegistryIterateParents, kIORegistryIterateRecursively,
    keys::kIOServicePlane,
    ret::kIOReturnSuccess,
    types::{io_iterator_t, io_object_t},
    usb::lib::kIOUSBDeviceClassName,
    IOIteratorNext, IOObjectRelease, IORegistryEntrySearchCFProperty, IOServiceGetMatchingServices,
    IOServiceMatching,
};
use log::{error, info};

use crate::{DeviceInfo, Error, Speed};

pub fn list_devices() -> Result<impl Iterator<Item = DeviceInfo>, Error> {
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

        let devices: Vec<DeviceInfo> = iter::from_fn(|| {
            let device = IOIteratorNext(iterator);
            if device != 0 {
                let dev = probe_device(device);
                IOObjectRelease(device);
                Some(dev)
            } else {
                None
            }
        })
        .filter_map(|d| d)
        .collect();
        IOObjectRelease(iterator);

        Ok(devices.into_iter())
    }
}

fn probe_device(device: io_object_t) -> Option<DeviceInfo> {
    // Can run `ioreg -p IOUSB -l` to see all properties
    let location_id: u32 = get_integer_property(device, "locationID")?;
    log::info!("Probing device {location_id}");

    Some(DeviceInfo {
        location_id,
        bus_number: 0, // TODO: does this exist on macOS?
        device_address: get_integer_property(device, "USB Address")?,
        vendor_id: get_integer_property(device, "idVendor")?,
        product_id: get_integer_property(device, "idProduct")?,
        device_version: get_integer_property(device, "bcdDevice")?,
        class: get_integer_property(device, "bDeviceClass")?,
        subclass: get_integer_property(device, "bDeviceSubClass")?,
        protocol: get_integer_property(device, "bDeviceProtocol")?,
        speed: get_integer_property(device, "Device Speed").and_then(map_speed),
        manufacturer_string: get_string_property(device, "USB Vendor Name"),
        product_string: get_string_property(device, "USB Product Name"),
        serial_number: get_string_property(device, "USB Serial Number"),
    })
}

fn get_property<T: ConcreteCFType>(device: io_iterator_t, property: &'static str) -> Option<T> {
    unsafe {
        let cf_property = CFString::from_static_string(property);

        let raw = IORegistryEntrySearchCFProperty(
            device,
            kIOServicePlane as *mut i8,
            cf_property.as_CFTypeRef() as *const _,
            std::ptr::null(),
            kIORegistryIterateRecursively | kIORegistryIterateParents,
        );

        if raw.is_null() {
            info!("Device does not have property `{property}`");
            return None;
        }

        let res = CFType::wrap_under_create_rule(raw).downcast_into();

        if res.is_none() {
            error!("Failed to convert device property `{property}`");
        }

        res
    }
}

fn get_string_property(device: io_iterator_t, property: &'static str) -> Option<String> {
    get_property::<CFString>(device, property).map(|s| s.to_string())
}

fn get_integer_property<T: TryFrom<i64>>(
    device: io_iterator_t,
    property: &'static str,
) -> Option<T> {
    get_property::<CFNumber>(device, property)
        .and_then(|n| n.to_i64())
        .and_then(|n| n.try_into().ok())
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
