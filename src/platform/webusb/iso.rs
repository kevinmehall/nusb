use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use wasm_bindgen_futures::{
    js_sys::{Number, Promise, Uint8Array},
    wasm_bindgen::{JsCast, JsValue},
    JsFuture,
};
use web_sys::{
    UsbDevice, UsbIsochronousInTransferPacket, UsbIsochronousInTransferResult,
    UsbIsochronousOutTransferPacket, UsbIsochronousOutTransferResult,
};

use crate::transfer::{
    Direction, IsoCompletion, IsoPacketResult, IsoPacketStatus, IsoTransfer, TransferError,
};

use super::{js_value_to_transfer_error, webusb_status_to_nusb_transfer_error};

/// One queued isochronous Promise and its reusable Rust allocation.
pub(super) struct PendingIso {
    future: JsFuture<JsValue>,
    transfer: IsoTransfer,
    direction: Direction,
}

impl PendingIso {
    pub(super) fn submit(
        device: &UsbDevice,
        address: u8,
        transfer: IsoTransfer,
    ) -> Result<Self, (IsoTransfer, TransferError)> {
        let packet_size = match u32::try_from(transfer.layout().packet_size()) {
            Ok(value) => value,
            Err(_) => return Err((transfer, TransferError::InvalidArgument)),
        };
        let packet_lengths = vec![Number::from(packet_size); transfer.layout().packet_count()];
        let direction = Direction::from_address(address);
        let endpoint_number = address & 0x7f;

        let promise: Promise<JsValue> = match direction {
            Direction::In => device
                .isochronous_transfer_in(endpoint_number, &packet_lengths)
                .unchecked_into(),
            Direction::Out => {
                // WebUSB copies the BufferSource before returning. Avoid exposing
                // Wasm shared linear memory directly to the browser API.
                let array = Uint8Array::from(&transfer.buffer[..]);
                match device.isochronous_transfer_out_with_buffer_source(
                    endpoint_number,
                    array.unchecked_ref(),
                    &packet_lengths,
                ) {
                    Ok(promise) => promise.unchecked_into(),
                    Err(error) => {
                        return Err((transfer, js_value_to_transfer_error(error)));
                    }
                }
            }
        };

        Ok(Self {
            future: JsFuture::from(promise),
            transfer,
            direction,
        })
    }

    pub(super) fn poll(&mut self, cx: &mut Context<'_>) -> Poll<Result<JsValue, JsValue>> {
        Pin::new(&mut self.future).poll(cx)
    }

    pub(super) fn complete(self, result: Result<JsValue, JsValue>) -> IsoCompletion {
        complete_iso_transfer(self.transfer, self.direction, result)
    }
}

fn complete_iso_transfer(
    mut transfer: IsoTransfer,
    direction: Direction,
    result: Result<JsValue, JsValue>,
) -> IsoCompletion {
    let result = match result {
        Ok(result) => result,
        Err(error) => {
            return IsoCompletion::from_completed_transfer(
                transfer,
                Err(js_value_to_transfer_error(error)),
            );
        }
    };

    let packet_size = transfer.layout().packet_size();
    let packet_count = transfer.layout().packet_count();
    let mut overall_status = Ok(());

    match direction {
        Direction::In => {
            let result: UsbIsochronousInTransferResult = result.unchecked_into();
            let packets = result.packets();
            if packets.length() as usize != packet_count {
                overall_status = Err(TransferError::Fault);
            }

            for index in 0..packet_count.min(packets.length() as usize) {
                let packet: UsbIsochronousInTransferPacket =
                    packets.get(index as u32).unchecked_into();
                let mut status = webusb_packet_status(packet.status());
                let mut actual_len = 0;

                if status == IsoPacketStatus::Success {
                    if let Some(data) = packet.data() {
                        actual_len = data.byte_length();
                        if actual_len <= packet_size {
                            let received = Uint8Array::new_with_byte_offset_and_length(
                                &data.buffer(),
                                data.byte_offset() as u32,
                                actual_len as u32,
                            );
                            received
                                .copy_to(&mut transfer.packet_mut(index).unwrap()[..actual_len]);
                        } else {
                            actual_len = 0;
                            status = IsoPacketStatus::Error(TransferError::Fault);
                            overall_status = Err(TransferError::Fault);
                        }
                    } else {
                        status = IsoPacketStatus::Error(TransferError::Fault);
                        overall_status = Err(TransferError::Fault);
                    }
                }

                transfer.packet_results_mut()[index] =
                    IsoPacketResult::new(packet_size, actual_len, status, None);
            }
        }
        Direction::Out => {
            let result: UsbIsochronousOutTransferResult = result.unchecked_into();
            let packets = result.packets();
            if packets.length() as usize != packet_count {
                overall_status = Err(TransferError::Fault);
            }

            for index in 0..packet_count.min(packets.length() as usize) {
                let packet: UsbIsochronousOutTransferPacket =
                    packets.get(index as u32).unchecked_into();
                let mut actual_len = packet.bytes_written() as usize;
                let mut status = webusb_packet_status(packet.status());
                if actual_len > packet_size {
                    actual_len = 0;
                    status = IsoPacketStatus::Error(TransferError::Fault);
                    overall_status = Err(TransferError::Fault);
                }
                transfer.packet_results_mut()[index] =
                    IsoPacketResult::new(packet_size, actual_len, status, None);
            }
        }
    }

    IsoCompletion::from_completed_transfer(transfer, overall_status)
}

fn webusb_packet_status(status: web_sys::UsbTransferStatus) -> IsoPacketStatus {
    match webusb_status_to_nusb_transfer_error(status) {
        Ok(()) => IsoPacketStatus::Success,
        Err(error) => IsoPacketStatus::Error(error),
    }
}

#[cfg(test)]
mod tests {
    use wasm_bindgen_futures::{
        js_sys,
        wasm_bindgen::{JsCast, JsValue},
    };
    use wasm_bindgen_test::wasm_bindgen_test;
    use web_sys::{UsbDevice, UsbIsochronousInTransferResult, UsbIsochronousOutTransferResult};

    use super::{complete_iso_transfer, PendingIso};
    use crate::transfer::{
        Buffer, Direction, IsoLayout, IsoPacketStatus, IsoTransfer, TransferError,
    };

    fn set(object: &js_sys::Object, key: &str, value: &JsValue) {
        js_sys::Reflect::set(object, &JsValue::from_str(key), value).unwrap();
    }

    fn packet(status: &str, data: Option<&js_sys::DataView>) -> js_sys::Object {
        let packet = js_sys::Object::new();
        set(&packet, "status", &JsValue::from_str(status));
        if let Some(data) = data {
            set(&packet, "data", data);
        }
        packet
    }

    fn in_result(packets: &[js_sys::Object]) -> UsbIsochronousInTransferResult {
        let array = js_sys::Array::new();
        for packet in packets {
            array.push(packet);
        }
        let result = js_sys::Object::new();
        set(&result, "packets", &array);
        result.unchecked_into()
    }

    fn out_packet(status: &str, bytes_written: u32) -> js_sys::Object {
        let packet = js_sys::Object::new();
        set(&packet, "status", &JsValue::from_str(status));
        set(
            &packet,
            "bytesWritten",
            &JsValue::from_f64(bytes_written.into()),
        );
        packet
    }

    fn out_result(packets: &[js_sys::Object]) -> UsbIsochronousOutTransferResult {
        let array = js_sys::Array::new();
        for packet in packets {
            array.push(packet);
        }
        let result = js_sys::Object::new();
        set(&result, "packets", &array);
        result.unchecked_into()
    }

    fn transfer(packet_count: usize, packet_size: usize) -> IsoTransfer {
        let layout = IsoLayout::new(packet_count, packet_size).unwrap();
        IsoTransfer::new(1, layout, Buffer::new(layout.total_len()))
    }

    #[wasm_bindgen_test]
    fn sparse_in_packets_stay_in_their_slots() {
        let bytes = js_sys::Uint8Array::from(&[1, 2, 3, 0, 0, 0, 0, 0, 9, 8, 7, 6][..]);
        let buffer = bytes.buffer();
        let first = js_sys::DataView::new(&buffer, 0, 3);
        let third = js_sys::DataView::new(&buffer, 8, 4);
        let result = in_result(&[
            packet("ok", Some(&first)),
            packet("stall", None),
            packet("ok", Some(&third)),
        ]);

        let completion = complete_iso_transfer(transfer(3, 4), Direction::In, Ok(result.into()));

        assert_eq!(completion.status(), Ok(()));
        let packets: Vec<_> = completion.packets().collect();
        assert_eq!(packets[0].data(), &[1, 2, 3]);
        assert_eq!(
            packets[1].status(),
            IsoPacketStatus::Error(TransferError::Stall)
        );
        assert!(packets[1].data().is_empty());
        assert_eq!(packets[2].data(), &[9, 8, 7, 6]);
    }

    #[wasm_bindgen_test]
    fn missing_packet_faults_without_fabricating_a_result() {
        let result = in_result(&[packet("ok", None)]);
        let completion = complete_iso_transfer(transfer(2, 4), Direction::In, Ok(result.into()));

        assert_eq!(completion.status(), Err(TransferError::Fault));
        let packets: Vec<_> = completion.packets().collect();
        assert_eq!(
            packets[0].status(),
            IsoPacketStatus::Error(TransferError::Fault)
        );
        assert_eq!(packets[1].status(), IsoPacketStatus::NotReported);
    }

    #[wasm_bindgen_test]
    fn oversized_packet_data_is_rejected_without_copying() {
        let bytes = js_sys::Uint8Array::from(&[1, 2, 3, 4, 5][..]);
        let view = js_sys::DataView::new(&bytes.buffer(), 0, 5);
        let result = in_result(&[packet("ok", Some(&view))]);
        let completion = complete_iso_transfer(transfer(1, 4), Direction::In, Ok(result.into()));

        assert_eq!(completion.status(), Err(TransferError::Fault));
        let packet = completion.packets().next().unwrap();
        assert_eq!(
            packet.status(),
            IsoPacketStatus::Error(TransferError::Fault)
        );
        assert!(packet.data().is_empty());
    }

    #[wasm_bindgen_test]
    fn successful_in_packet_without_data_faults_completion() {
        let result = in_result(&[packet("ok", None)]);
        let completion = complete_iso_transfer(transfer(1, 4), Direction::In, Ok(result.into()));

        assert_eq!(completion.status(), Err(TransferError::Fault));
        let packet = completion.packets().next().unwrap();
        assert_eq!(
            packet.status(),
            IsoPacketStatus::Error(TransferError::Fault)
        );
        assert!(packet.data().is_empty());
    }

    #[wasm_bindgen_test]
    fn out_packets_preserve_bytes_written_and_status() {
        let result = out_result(&[out_packet("ok", 4), out_packet("stall", 0)]);
        let completion = complete_iso_transfer(transfer(2, 4), Direction::Out, Ok(result.into()));

        assert_eq!(completion.status(), Ok(()));
        let packets: Vec<_> = completion.packets().collect();
        assert_eq!(packets[0].actual_len(), 4);
        assert_eq!(packets[0].status(), IsoPacketStatus::Success);
        assert_eq!(packets[1].actual_len(), 0);
        assert_eq!(
            packets[1].status(),
            IsoPacketStatus::Error(TransferError::Stall)
        );
    }

    #[wasm_bindgen_test]
    fn rejected_promise_leaves_all_packets_unreported() {
        let completion = complete_iso_transfer(
            transfer(2, 4),
            Direction::In,
            Err(js_sys::Error::new("transfer failed").into()),
        );

        assert_eq!(completion.status(), Err(TransferError::Fault));
        assert!(completion
            .packets()
            .all(|packet| packet.status() == IsoPacketStatus::NotReported));
    }

    #[wasm_bindgen_test]
    fn synchronous_out_exception_returns_transfer_ownership() {
        let device = js_sys::Object::new();
        let method = js_sys::Function::new_no_args("throw new TypeError('invalid transfer');");
        set(&device, "isochronousTransferOut", &method);
        let device: UsbDevice = device.unchecked_into();

        let (returned, transfer_error) = match PendingIso::submit(&device, 0x01, transfer(2, 4)) {
            Ok(_) => panic!("synchronous exception must reject submission"),
            Err(error) => error,
        };

        assert_eq!(transfer_error, TransferError::Fault);
        assert_eq!(returned.layout(), IsoLayout::new(2, 4).unwrap());
    }

    #[wasm_bindgen_test]
    async fn submitted_in_promise_completes_through_the_pending_state() {
        let bytes = js_sys::Uint8Array::new_with_length(0);
        let empty = js_sys::DataView::new(&bytes.buffer(), 0, 0);
        let result = in_result(&[packet("ok", Some(&empty)), packet("stall", None)]);
        let device = js_sys::Object::new();
        set(&device, "isoResult", &result);
        let method = js_sys::Function::new_no_args("return Promise.resolve(this.isoResult);");
        set(&device, "isochronousTransferIn", &method);
        let device: UsbDevice = device.unchecked_into();

        let mut pending = PendingIso::submit(&device, 0x81, transfer(2, 4)).unwrap();
        let result = std::future::poll_fn(|cx| pending.poll(cx)).await;
        let completion = pending.complete(result);

        assert_eq!(completion.status(), Ok(()));
        let packets: Vec<_> = completion.packets().collect();
        assert_eq!(packets[0].status(), IsoPacketStatus::Success);
        assert_eq!(
            packets[1].status(),
            IsoPacketStatus::Error(TransferError::Stall)
        );
    }
}
