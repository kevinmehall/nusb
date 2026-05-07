use std::sync::Arc;

use wasm_bindgen_futures::{js_sys::Array, wasm_bindgen::JsCast, JsFuture};
use web_sys::UsbDevice;

use crate::{
    descriptors::ConfigurationDescriptor,
    error::ErrorKind,
    maybe_future::Ready,
    platform::webusb::device::{extract_decriptors, extract_string},
    BusInfo, DeviceInfo, Error, InterfaceInfo, MaybeFuture,
};

use super::{js_value_to_error, UniqueUsbDevice, WebFuture};

pub fn list_devices() -> impl MaybeFuture<Output = Result<impl Iterator<Item = DeviceInfo>, Error>>
{
    async fn inner() -> Result<Vec<DeviceInfo>, Error> {
        let usb = super::usb()?;
        let devices = JsFuture::from(usb.get_devices())
            .await
            .map_err(js_value_to_error)?;

        let devices: Array = JsCast::unchecked_from_js(devices.into());

        let mut result = vec![];
        for device in devices {
            let device: UsbDevice = JsCast::unchecked_from_js(device);
            JsFuture::from(device.open())
                .await
                .map_err(js_value_to_error)?;

            let device = Arc::new(UniqueUsbDevice::new(device));

            let device_info = device_to_info(device.clone()).await?;
            result.push(device_info);
            JsFuture::from(device.close())
                .await
                .map_err(js_value_to_error)?;
        }

        Ok(result)
    }

    WebFuture(async move { Ok(inner().await?.into_iter()) })
}

pub fn list_buses() -> impl MaybeFuture<Output = Result<impl Iterator<Item = BusInfo>, Error>> {
    Ready(Ok(Vec::<BusInfo>::new().into_iter()))
}

pub(crate) async fn device_to_info(device: Arc<UniqueUsbDevice>) -> Result<DeviceInfo, Error> {
    Ok(DeviceInfo {
        bus_id: "webusb".to_string(),
        device_address: 0,
        vendor_id: device.vendor_id(),
        product_id: device.product_id(),
        device_version: ((device.device_version_major() as u16) << 8)
            | device.device_version_minor() as u16,
        usb_version: ((device.usb_version_major() as u16) << 8) | device.usb_version_minor() as u16,
        class: device.device_class(),
        subclass: device.device_subclass(),
        protocol: device.device_protocol(),
        speed: None,
        manufacturer_string: device.manufacturer_name(),
        product_string: device.product_name(),
        serial_number: device.serial_number(),
        interfaces: {
            let descriptors = extract_decriptors(&device).await?;
            let mut interfaces = vec![];
            for descriptor in descriptors.into_iter() {
                let Some(configuration) = ConfigurationDescriptor::new(&descriptor) else {
                    return Err(Error::new(
                        ErrorKind::Other,
                        "invalid configuration descriptor",
                    ));
                };
                for interface_group in configuration.interfaces() {
                    let alternate = interface_group.first_alt_setting();
                    let interface_string = if let Some(id) = alternate.string_index() {
                        extract_string(&device, id.get() as u16).await.ok()
                    } else {
                        None
                    };

                    interfaces.push(InterfaceInfo {
                        interface_number: interface_group.interface_number(),
                        class: alternate.class(),
                        subclass: alternate.subclass(),
                        protocol: alternate.protocol(),
                        interface_string,
                    });
                }
            }
            interfaces
        },
        port_chain: vec![],
        device: device.clone(),
    })
}
