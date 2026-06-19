use wasm_bindgen_futures::JsFuture;
use web_sys::{UsbDevice, UsbDeviceFilter, UsbDeviceRequestOptions};

use crate::{
    enumeration::{DeviceSelector, FilterRule},
    DeviceInfo, Error, InterfaceInfo, MaybeFuture,
};

use super::{js_value_to_error, WebFuture};

pub fn list_devices() -> impl MaybeFuture<Output = Result<impl Iterator<Item = DeviceInfo>, Error>>
{
    WebFuture(async move {
        let usb = super::usb()?;
        let devices = JsFuture::from(usb.get_devices())
            .await
            .map_err(js_value_to_error)?;
        Ok(devices.into_iter().map(device_to_info))
    })
}

pub(crate) fn device_to_info(device: UsbDevice) -> DeviceInfo {
    DeviceInfo {
        vendor_id: device.vendor_id(),
        product_id: device.product_id(),
        device_version: ((device.device_version_major() as u16) << 8)
            | ((device.device_version_minor() as u16) << 4)
            | (device.device_version_subminor() as u16),
        usb_version: ((device.usb_version_major() as u16) << 8)
            | ((device.usb_version_minor() as u16) << 4)
            | (device.usb_version_subminor() as u16),
        class: device.device_class(),
        subclass: device.device_subclass(),
        protocol: device.device_protocol(),
        manufacturer_string: device.manufacturer_name(),
        product_string: device.product_name(),
        serial_number: device.serial_number(),
        interfaces: if let Some(config) = device.configuration() {
            config
                .interfaces()
                .into_iter()
                .filter_map(|iface| {
                    let alt = iface.alternates().iter().next()?;
                    Some(InterfaceInfo {
                        interface_number: iface.interface_number(),
                        class: alt.interface_class(),
                        subclass: alt.interface_subclass(),
                        protocol: alt.interface_protocol(),
                        interface_string: alt.interface_name(),
                    })
                })
                .collect()
        } else {
            vec![]
        },
        device: device.clone(),
    }
}

pub fn request_devices(
    selector: &DeviceSelector,
) -> impl MaybeFuture<Output = Result<impl Iterator<Item = DeviceInfo>, Error>> {
    let filters = selector_to_filters(selector);
    WebFuture(async move {
        let usb = super::usb()?;
        let device = if filters.is_empty() {
            // WebUSB treats an empty filter list as matching all devices, but
            // contradicting `.and_xxx()` calls will result in an empty list,
            // so we don't want that behavior.
            None
        } else {
            JsFuture::from(usb.request_device(&UsbDeviceRequestOptions::new(&filters)))
                .await
                .inspect_err(|e| log::debug!("requestDevice failed with {:?}", e))
                .ok()
                .map(device_to_info)
        };

        Ok(device.into_iter())
    })
}

fn selector_to_filters(selector: &DeviceSelector) -> Vec<UsbDeviceFilter> {
    selector
        .rules
        .iter()
        .map(|rule| {
            let filter = UsbDeviceFilter::new();
            let FilterRule {
                vendor_id,
                product_id,
                class,
                subclass,
                protocol,
                ref serial_number,
            } = *rule;

            if let Some(vendor_id) = vendor_id {
                filter.set_vendor_id(vendor_id);
            }
            if let Some(product_id) = product_id {
                filter.set_product_id(product_id);
            }
            if let Some(class) = class {
                filter.set_class_code(class);
            }
            if let Some(subclass) = subclass {
                filter.set_subclass_code(subclass);
            }
            if let Some(protocol) = protocol {
                filter.set_protocol_code(protocol);
            }
            if let Some(ref serial_number) = serial_number {
                filter.set_serial_number(serial_number);
            }
            filter
        })
        .collect::<Vec<_>>()
}
