use std::{sync::Arc, time::Duration};

use wasm_bindgen_futures::{js_sys::Array, wasm_bindgen::JsCast, JsFuture};
use web_sys::{
    js_sys::Uint8Array, UsbControlTransferParameters, UsbDevice, UsbInTransferResult,
    UsbOutTransferResult,
};

use crate::{
    descriptors::{validate_config_descriptor, DESCRIPTOR_TYPE_CONFIGURATION},
    transfer::{web_to_nusb_status, Control, EndpointType, TransferError, TransferHandle},
    DeviceInfo, Error,
};

#[derive(Clone)]
pub(crate) struct WebusbDevice {
    pub device: UsbDevice,
    config_descriptors: Vec<Vec<u8>>,
}

/// SAFETY: This is NOT safe at all.
unsafe impl Sync for WebusbDevice {}
unsafe impl Send for WebusbDevice {}

impl WebusbDevice {
    pub(crate) async fn from_device_info(d: &DeviceInfo) -> Result<Arc<WebusbDevice>, Error> {
        let window = web_sys::window().unwrap();
        let navigator = window.navigator();
        let usb = navigator.usb();
        let devices = JsFuture::from(usb.get_devices()).await.unwrap();
        let devices: Array = JsCast::unchecked_from_js(devices);

        for device in devices {
            let device: UsbDevice = JsCast::unchecked_from_js(device);
            if device.eq(&d.device) {
                JsFuture::from(device.open()).await.unwrap();

                let config_descriptors = extract_decriptors(&device).await?;

                #[allow(clippy::arc_with_non_send_sync)]
                return Ok(Arc::new(Self {
                    device,
                    config_descriptors,
                }));
            }
        }
        Err(Error::other("device not found"))
    }

    pub(crate) fn configuration_descriptors(&self) -> impl Iterator<Item = &[u8]> {
        self.config_descriptors.iter().map(|d| &d[..])
    }

    pub(crate) fn active_configuration_value(&self) -> u8 {
        self.device
            .configuration()
            .map(|c| c.configuration_value())
            .unwrap_or_default()
    }

    pub(crate) async fn set_configuration(&self, configuration: u8) -> Result<(), Error> {
        JsFuture::from(self.device.select_configuration(configuration))
            .await
            .map_err(|e| {
                Error::other(
                    e.as_string()
                        .unwrap_or_else(|| "No further error clarification available".into()),
                )
            })
            .map(|_| ())
    }

    pub(crate) async fn reset(&self) -> Result<(), Error> {
        JsFuture::from(self.device.reset())
            .await
            .map_err(|e| {
                Error::other(
                    e.as_string()
                        .unwrap_or_else(|| "No further error clarification available".into()),
                )
            })
            .map(|_| ())
    }

    pub(crate) fn make_control_transfer(&self) -> TransferHandle<super::TransferData> {
        TransferHandle::new(super::TransferData::new(
            self.clone(),
            0,
            EndpointType::Control,
        ))
    }

    pub(crate) async fn claim_interface(
        &self,
        interface_number: u8,
    ) -> Result<Arc<WebusbInterface>, Error> {
        JsFuture::from(self.device.claim_interface(interface_number))
            .await
            .unwrap();

        #[allow(clippy::arc_with_non_send_sync)]
        Ok(Arc::new(WebusbInterface {
            interface_number,
            device: self.clone(),
        }))
    }

    pub(crate) async fn detach_and_claim_interface(
        &self,
        interface_number: u8,
    ) -> Result<Arc<WebusbInterface>, Error> {
        self.claim_interface(interface_number).await
    }

    pub async fn get_descriptor(
        &self,
        desc_type: u8,
        desc_index: u8,
        language_id: u16,
        timeout: Duration,
    ) -> Result<Vec<u8>, Error> {
        get_descriptor(&self.device, desc_type, desc_index, language_id, timeout).await
    }
}

pub async fn extract_decriptors(device: &UsbDevice) -> Result<Vec<Vec<u8>>, Error> {
    let num_configurations = device.configurations().length() as usize;
    let mut config_descriptors = Vec::with_capacity(num_configurations);

    for i in 0..num_configurations {
        let language_id = 0;
        let desc_type = DESCRIPTOR_TYPE_CONFIGURATION;
        let desc_index = i as u8;
        let data = get_descriptor(
            device,
            desc_type,
            desc_index,
            language_id,
            Duration::from_millis(500),
        )
        .await?;
        if validate_config_descriptor(&data).is_some() {
            config_descriptors.push(data)
        }
    }
    Ok(config_descriptors)
}

pub async fn get_descriptor(
    device: &UsbDevice,
    desc_type: u8,
    desc_index: u8,
    language_id: u16,
    _timeout: Duration,
) -> Result<Vec<u8>, Error> {
    let setup = UsbControlTransferParameters::new(
        language_id,
        web_sys::UsbRecipient::Device,
        0x6, // Get descriptor: https://www.beyondlogic.org/usbnutshell/usb6.shtml#StandardDeviceRequests
        web_sys::UsbRequestType::Standard,
        ((desc_type as u16) << 8) | (desc_index as u16),
    );
    let res = wasm_bindgen_futures::JsFuture::from(device.control_transfer_in(&setup, 255))
        .await
        .unwrap();
    let res: UsbInTransferResult = JsCast::unchecked_from_js(res);
    Ok(Uint8Array::new(&res.data().unwrap().buffer()).to_vec())
}

pub async fn extract_string(device: &UsbDevice, id: u16) -> String {
    let setup = UsbControlTransferParameters::new(
        0,
        web_sys::UsbRecipient::Device,
        0x6, // Get descriptor: https://www.beyondlogic.org/usbnutshell/usb6.shtml#StandardDeviceRequests
        web_sys::UsbRequestType::Standard,
        (0x03_u16 << 8) | (id),
    );
    let res = JsFuture::from(device.control_transfer_in(&setup, 255))
        .await
        .unwrap();
    let res: UsbInTransferResult = JsCast::unchecked_from_js(res);
    let mut data = Uint8Array::new(&res.data().unwrap().buffer()).to_vec();

    String::from_utf16(
        &data
            .drain(2..data[0] as usize)
            .collect::<Vec<_>>()
            .chunks(2)
            .map(|c| ((c[1] as u16) << 8) | c[0] as u16)
            .collect::<Vec<_>>(),
    )
    .unwrap()
}

#[derive(Clone)]
pub(crate) struct WebusbInterface {
    pub interface_number: u8,
    pub(crate) device: WebusbDevice,
}

impl WebusbInterface {
    pub(crate) fn make_transfer(
        self: &Arc<Self>,
        endpoint: u8,
        ep_type: EndpointType,
    ) -> TransferHandle<super::TransferData> {
        TransferHandle::new(super::TransferData::new(
            self.device.clone(),
            endpoint,
            ep_type,
        ))
    }

    pub async fn set_alt_setting(&self, alternate_setting: u8) -> Result<(), Error> {
        JsFuture::from(
            self.device
                .device
                .select_alternate_interface(self.interface_number, alternate_setting),
        )
        .await
        .map_err(|e| {
            Error::other(
                e.as_string()
                    .unwrap_or_else(|| "No further error clarification available".into()),
            )
        })
        .map(|_| ())
    }

    pub async fn clear_halt(&self, endpoint: u8) -> Result<(), Error> {
        let endpoint_in = endpoint & 0x80 != 0;
        JsFuture::from(self.device.device.clear_halt(
            if endpoint_in {
                web_sys::UsbDirection::In
            } else {
                web_sys::UsbDirection::Out
            },
            endpoint,
        ))
        .await
        .map_err(|e| {
            Error::other(
                e.as_string()
                    .unwrap_or_else(|| "No further error clarification available".into()),
            )
        })
        .map(|_| ())
    }

    #[allow(dead_code)]
    pub async fn control_in(
        &self,
        control: Control,
        data: &mut [u8],
        _timeout: Duration,
    ) -> Result<usize, TransferError> {
        let setup = UsbControlTransferParameters::new(
            control.index,
            control.recipient.into(),
            control.request,
            control.control_type.into(),
            control.value,
        );
        let res = wasm_bindgen_futures::JsFuture::from(
            self.device.device.control_transfer_in(&setup, 255),
        )
        .await
        .unwrap();
        let res: UsbInTransferResult = JsCast::unchecked_from_js(res);
        let array = Uint8Array::new(&res.data().unwrap().buffer());
        array.copy_to(data);

        web_to_nusb_status(res.status()).map(|_| array.length() as usize)
    }

    #[allow(dead_code)]
    pub(crate) async fn control_out(
        &self,
        control: Control,
        data: &[u8],
        _timeout: Duration,
    ) -> Result<usize, TransferError> {
        let setup = UsbControlTransferParameters::new(
            control.index,
            control.recipient.into(),
            control.request,
            control.control_type.into(),
            control.value,
        );
        let mut data = data.to_vec();
        let res = wasm_bindgen_futures::JsFuture::from(
            self.device
                .device
                .control_transfer_out_with_u8_slice(&setup, &mut data)
                .unwrap(),
        )
        .await
        .unwrap();
        let res: UsbOutTransferResult = JsCast::unchecked_from_js(res);

        web_to_nusb_status(res.status()).map(|_| res.bytes_written() as usize)
    }
}
