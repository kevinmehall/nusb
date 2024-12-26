use wasm_bindgen_futures::{js_sys::Array, wasm_bindgen::JsCast, JsFuture};
use web_sys::UsbDevice;

use crate::{
    descriptors::Configuration,
    platform::webusb::device::{extract_decriptors, extract_string},
    BusInfo, DeviceInfo, Error, InterfaceInfo,
};

pub async fn list_devices() -> Result<impl Iterator<Item = DeviceInfo>, Error> {
    async fn inner() -> Result<Vec<DeviceInfo>, Error> {
        let window = web_sys::window().unwrap();
        let navigator = window.navigator();
        let usb = navigator.usb();
        let devices = JsFuture::from(usb.get_devices()).await.unwrap();

        let devices: Array = JsCast::unchecked_from_js(devices);

        let mut result = vec![];
        for device in devices {
            let device: UsbDevice = JsCast::unchecked_from_js(device);
            JsFuture::from(device.open()).await.unwrap();

            let device_info = device_to_info(device.clone()).await?;
            result.push(device_info);
            JsFuture::from(device.close()).await.unwrap();
        }

        Ok(result)
    }

    Ok(inner().await.unwrap().into_iter())
}

pub fn list_buses() -> Result<impl Iterator<Item = BusInfo>, Error> {
    Ok(vec![].into_iter())
}

pub(crate) async fn device_to_info(device: UsbDevice) -> Result<DeviceInfo, Error> {
    Ok(DeviceInfo {
        bus_id: "webusb".to_string(),
        device_address: 0,
        vendor_id: device.vendor_id(),
        product_id: device.product_id(),
        device_version: ((device.usb_version_major() as u16) << 8)
            | device.usb_version_minor() as u16,
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
                let configuration = Configuration::new(&descriptor);
                for interface_group in configuration.interfaces() {
                    let alternate = interface_group.first_alt_setting();
                    let interface_string = if let Some(id) = alternate.string_index() {
                        Some(extract_string(&device, id as u16).await)
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
        max_packet_size_0: 255,
        device: device.clone(),
    })
}
