use std::ffi::c_void;

use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::{
    js_sys::{Object, Uint8Array},
    wasm_bindgen::JsCast,
    UsbControlTransferParameters, UsbInTransferResult, UsbOutTransferResult, UsbTransferStatus,
};

use crate::transfer::{
    notify_completion, web_to_nusb_status, Completion, ControlIn, ControlOut, EndpointType,
    PlatformSubmit, PlatformTransfer, RequestBuffer, ResponseBuffer, TransferInner,
};

pub struct TransferData {
    device: super::Device,
    endpoint: u8,
    ep_type: EndpointType,
    written_bytes: usize,
    status: UsbTransferStatus,
    data: Vec<u8>,
}

impl TransferData {
    pub(crate) fn new(device: super::Device, endpoint: u8, ep_type: EndpointType) -> Self {
        Self {
            device,
            endpoint,
            ep_type,
            written_bytes: 0,
            status: UsbTransferStatus::Ok,
            data: vec![],
        }
    }
}

impl TransferData {
    fn store_and_notify(
        data: Vec<u8>,
        status: UsbTransferStatus,
        written_bytes: usize,
        user_data: *mut c_void,
    ) {
        unsafe {
            let transfer = user_data as *mut TransferInner<TransferData>;
            let t = (*transfer).platform_data();

            t.data = data;
            t.status = status;
            t.written_bytes = written_bytes;
            notify_completion::<TransferData>(user_data);
        }
    }
}

impl PlatformTransfer for TransferData {}

impl PlatformSubmit<Vec<u8>> for TransferData {
    unsafe fn submit(&mut self, data: Vec<u8>, user_data: *mut c_void) {
        let device = self.device.clone();
        let ep_type = self.ep_type;
        let endpoint_number = self.endpoint;
        spawn_local(async move {
            let (written_bytes, status) = match ep_type {
                EndpointType::Control => {
                    panic!("Control is unsupported for submit");
                }
                EndpointType::Isochronous => {
                    panic!("Isochronous is unsupported for submit");
                }
                EndpointType::Bulk | EndpointType::Interrupt => {
                    let array = Uint8Array::from(data.as_slice());
                    let array_obj = Object::try_from(&array).unwrap();

                    let result = JsFuture::from(
                        device
                            .device
                            .transfer_out_with_buffer_source(endpoint_number, array_obj)
                            .unwrap(),
                    )
                    .await
                    .unwrap();

                    let transfer_result: UsbOutTransferResult = JsCast::unchecked_from_js(result);
                    (
                        transfer_result.bytes_written() as usize,
                        transfer_result.status(),
                    )
                }
            };

            Self::store_and_notify(data, status, written_bytes, user_data);
        });
    }

    unsafe fn take_completed(&mut self) -> Completion<ResponseBuffer> {
        let data = ResponseBuffer::from_vec(self.data.clone(), self.written_bytes);
        let status = self.status;
        Completion {
            data,
            status: web_to_nusb_status(status),
        }
    }
}

impl PlatformSubmit<RequestBuffer> for TransferData {
    unsafe fn submit(&mut self, data: RequestBuffer, user_data: *mut c_void) {
        let device = self.device.clone();
        let ep_type = self.ep_type;
        let endpoint_number = self.endpoint & (!0x80);
        let (mut data, len) = data.into_vec();
        spawn_local(async move {
            let status = match ep_type {
                EndpointType::Control => {
                    panic!("Control is unsupported for submit");
                }
                EndpointType::Isochronous => {
                    panic!("Isochronous is unsupported for submit");
                }
                EndpointType::Bulk | EndpointType::Interrupt => {
                    let result =
                        JsFuture::from(device.device.transfer_in(endpoint_number, len as u32))
                            .await
                            .unwrap();

                    let transfer_result: UsbInTransferResult = JsCast::unchecked_from_js(result);
                    let received_data = Uint8Array::new(&transfer_result.data().unwrap().buffer());
                    data.resize(received_data.length() as usize, 0);
                    received_data.copy_to(&mut data[..received_data.length() as usize]);
                    transfer_result.status()
                }
            };

            Self::store_and_notify(data, status, len, user_data);
        });
    }

    unsafe fn take_completed(&mut self) -> Completion<Vec<u8>> {
        let data = self.data.clone();
        let status = self.status;
        Completion {
            data,
            status: web_to_nusb_status(status),
        }
    }
}

impl PlatformSubmit<ControlIn> for TransferData {
    unsafe fn submit(&mut self, data: ControlIn, user_data: *mut c_void) {
        let device = self.device.clone();
        let ep_type = self.ep_type;
        spawn_local(async move {
            let (status, data) = match ep_type {
                EndpointType::Control => {
                    panic!("Control is unsupported for submit");
                }
                EndpointType::Isochronous => {
                    panic!("Isochronous is unsupported for submit");
                }
                EndpointType::Bulk | EndpointType::Interrupt => {
                    let setup = UsbControlTransferParameters::new(
                        data.index,
                        data.recipient.into(),
                        data.request,
                        data.control_type.into(),
                        data.value,
                    );
                    let result =
                        JsFuture::from(device.device.control_transfer_in(&setup, data.length))
                            .await
                            .unwrap();

                    let transfer_result: UsbInTransferResult = JsCast::unchecked_from_js(result);
                    let received_data = Uint8Array::new(&transfer_result.data().unwrap().buffer());
                    let data = received_data.to_vec();

                    (transfer_result.status(), data)
                }
            };

            let length = data.len();
            Self::store_and_notify(data, status, length, user_data);
        });
    }

    unsafe fn take_completed(&mut self) -> Completion<Vec<u8>> {
        let data = self.data.clone();
        let status = self.status;
        Completion {
            data,
            status: web_to_nusb_status(status),
        }
    }
}

impl PlatformSubmit<ControlOut<'_>> for TransferData {
    unsafe fn submit(&mut self, data: ControlOut, user_data: *mut c_void) {
        let device = self.device.clone();
        let ep_type = self.ep_type;
        let bytes_to_send = data.data.to_vec();
        spawn_local(async move {
            let (written_bytes, status) = match ep_type {
                EndpointType::Control => {
                    panic!("Control is unsupported for submit");
                }
                EndpointType::Isochronous => {
                    panic!("Isochronous is unsupported for submit");
                }
                EndpointType::Bulk | EndpointType::Interrupt => {
                    let setup = UsbControlTransferParameters::new(
                        data.index,
                        data.recipient.into(),
                        data.request,
                        data.control_type.into(),
                        data.value,
                    );
                    let array = Uint8Array::from(bytes_to_send.as_slice());
                    let array_obj = Object::try_from(&array).unwrap();
                    let result = JsFuture::from(
                        device
                            .device
                            .control_transfer_out_with_buffer_source(&setup, array_obj)
                            .unwrap(),
                    )
                    .await
                    .unwrap();

                    let transfer_result: UsbOutTransferResult = JsCast::unchecked_from_js(result);
                    (
                        transfer_result.bytes_written() as usize,
                        transfer_result.status(),
                    )
                }
            };

            Self::store_and_notify(bytes_to_send, status, written_bytes, user_data);
        });
    }

    unsafe fn take_completed(&mut self) -> Completion<ResponseBuffer> {
        let data = ResponseBuffer::from_vec(self.data.clone(), self.written_bytes);
        let status = self.status;
        Completion {
            data,
            status: web_to_nusb_status(status),
        }
    }
}
