use wasm_bindgen_futures::JsFuture;
use web_sys::UsbDevice;

use crate::{maybe_future::Ready, BusInfo, DeviceInfo, Error, InterfaceInfo, MaybeFuture};

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

pub fn list_buses() -> impl MaybeFuture<Output = Result<impl Iterator<Item = BusInfo>, Error>> {
    Ready(Ok(Vec::<BusInfo>::new().into_iter()))
}

pub(crate) fn device_to_info(device: UsbDevice) -> DeviceInfo {
    DeviceInfo {
        vendor_id: device.vendor_id(),
        product_id: device.product_id(),
        device_version: ((device.device_version_major() as u16) << 8)
            | device.device_version_minor() as u16,
        usb_version: ((device.usb_version_major() as u16) << 8) | device.usb_version_minor() as u16,
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
