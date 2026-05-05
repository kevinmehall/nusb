use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::Duration,
};

pub use private::UniqueUsbDevice;
use wasm_bindgen_futures::{
    js_sys::{Array, Promise},
    wasm_bindgen::{JsCast, JsValue},
    JsFuture,
};
use web_sys::{
    js_sys::Uint8Array, UsbControlTransferParameters, UsbDevice, UsbInTransferResult,
    UsbOutTransferResult,
};

use crate::{
    bitset::EndpointBitSet,
    descriptors::{
        ConfigurationDescriptor, DeviceDescriptor, EndpointDescriptor,
        DESCRIPTOR_TYPE_CONFIGURATION,
    },
    transfer::{Buffer, Completion, ControlIn, ControlOut, Direction, TransferError},
    DeviceInfo, Error, ErrorKind, MaybeFuture, Speed,
};

use super::{
    js_value_to_error, js_value_to_transfer_error, webusb_status_to_nusb_transfer_error, WebFuture,
};

pub mod private {
    use std::ops::{Deref, DerefMut};

    use web_sys::UsbDevice;

    pub struct UniqueUsbDevice(UsbDevice);

    impl Deref for UniqueUsbDevice {
        type Target = UsbDevice;

        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }

    impl DerefMut for UniqueUsbDevice {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.0
        }
    }

    impl UniqueUsbDevice {
        pub fn new(device: UsbDevice) -> Self {
            Self(device)
        }
    }
}

#[derive(Clone)]
pub(crate) struct WebusbDevice {
    pub device: Arc<UniqueUsbDevice>,
    config_descriptors: Vec<Vec<u8>>,
    speed: Option<Speed>,
}

impl WebusbDevice {
    pub(crate) fn from_device_info(
        d: &DeviceInfo,
    ) -> impl MaybeFuture<Output = Result<Arc<WebusbDevice>, Error>> {
        let target_device = d.device.clone();
        let speed = d.speed;
        WebFuture(async move {
            let usb = super::usb()?;
            let devices = JsFuture::from(usb.get_devices())
                .await
                .map_err(js_value_to_error)?;
            let devices: Array = JsCast::unchecked_from_js(devices.into());

            for device in devices {
                let device: UsbDevice = JsCast::unchecked_from_js(device);
                if device.eq(&target_device) {
                    JsFuture::from(device.open())
                        .await
                        .map_err(js_value_to_error)?;

                    let config_descriptors = extract_decriptors(&device).await?;

                    #[allow(clippy::arc_with_non_send_sync)]
                    return Ok(Arc::new(Self {
                        device: Arc::new(UniqueUsbDevice::new(device)),
                        config_descriptors,
                        speed,
                    }));
                }
            }
            Err(Error::new(ErrorKind::NotFound, "device not found"))
        })
    }

    pub(crate) fn device_descriptor(&self) -> DeviceDescriptor {
        DeviceDescriptor::new(&self.config_descriptors[0]).unwrap()
    }

    pub(crate) fn speed(&self) -> Option<Speed> {
        self.speed
    }

    pub(crate) fn configuration_descriptors(
        &self,
    ) -> impl Iterator<Item = ConfigurationDescriptor> {
        self.config_descriptors
            .iter()
            .map(|d| ConfigurationDescriptor::new(&d[..]).unwrap())
    }

    pub(crate) fn active_configuration_value(&self) -> u8 {
        self.device
            .configuration()
            .map(|c| c.configuration_value())
            .unwrap_or_default()
    }

    pub(crate) fn set_configuration(
        self: Arc<Self>,
        configuration: u8,
    ) -> impl MaybeFuture<Output = Result<(), Error>> {
        WebFuture(async move {
            JsFuture::from(self.device.select_configuration(configuration))
                .await
                .map_err(js_value_to_error)
                .map(|_| ())
        })
    }

    pub(crate) fn reset(self: Arc<Self>) -> impl MaybeFuture<Output = Result<(), Error>> {
        WebFuture(async move {
            JsFuture::from(self.device.reset())
                .await
                .map_err(js_value_to_error)
                .map(|_| ())
        })
    }

    pub(crate) fn claim_interface(
        self: Arc<Self>,
        interface_number: u8,
    ) -> impl MaybeFuture<Output = Result<Arc<WebusbInterface>, Error>> {
        WebFuture(async move {
            JsFuture::from(self.device.claim_interface(interface_number))
                .await
                .map_err(js_value_to_error)?;

            #[allow(clippy::arc_with_non_send_sync)]
            Ok(Arc::new(WebusbInterface {
                state: Arc::new(Mutex::new(InterfaceState {
                    alt_setting: 0,
                    endpoints_used: Default::default(),
                })),
                interface_number,
                device: self.clone(),
            }))
        })
    }

    pub(crate) fn detach_and_claim_interface(
        self: Arc<Self>,
        interface_number: u8,
    ) -> impl MaybeFuture<Output = Result<Arc<WebusbInterface>, Error>> {
        self.claim_interface(interface_number)
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

    pub fn control_in(
        self: Arc<Self>,
        control: ControlIn,
        _timeout: Duration,
    ) -> impl MaybeFuture<Output = Result<Vec<u8>, TransferError>> {
        WebFuture(async move {
            let setup = UsbControlTransferParameters::new(
                control.index,
                control.recipient.into(),
                control.request,
                control.control_type.into(),
                control.value,
            );
            let res = JsFuture::from(self.device.control_transfer_in(&setup, control.length))
                .await
                .map_err(js_value_to_transfer_error)?;
            let res: UsbInTransferResult = JsCast::unchecked_from_js(res.into());
            let data = res.data().ok_or(TransferError::Fault)?;
            Ok(Uint8Array::new(&data.buffer()).to_vec())
        })
    }

    pub fn control_out(
        self: Arc<Self>,
        control: ControlOut<'_>,
        _timeout: Duration,
    ) -> impl MaybeFuture<Output = Result<(), TransferError>> {
        let setup = UsbControlTransferParameters::new(
            control.index,
            control.recipient.into(),
            control.request,
            control.control_type.into(),
            control.value,
        );
        let mut data = control.data.to_vec();
        WebFuture(async move {
            let res = JsFuture::from(
                self.device
                    .control_transfer_out_with_u8_slice(&setup, &mut data)
                    .map_err(js_value_to_transfer_error)?,
            )
            .await
            .map_err(js_value_to_transfer_error)?;
            let res: UsbOutTransferResult = JsCast::unchecked_from_js(res.into());
            webusb_status_to_nusb_transfer_error(res.status())
        })
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
        config_descriptors.push(data)
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
        .map_err(js_value_to_error)?;
    let res: UsbInTransferResult = JsCast::unchecked_from_js(res.into());
    let data = res
        .data()
        .ok_or_else(|| Error::new(ErrorKind::Other, "descriptor transfer returned no data"))?;
    Ok(Uint8Array::new(&data.buffer()).to_vec())
}

pub async fn extract_string(device: &UsbDevice, id: u16) -> Result<String, Error> {
    let setup = UsbControlTransferParameters::new(
        0,
        web_sys::UsbRecipient::Device,
        0x6, // Get descriptor: https://www.beyondlogic.org/usbnutshell/usb6.shtml#StandardDeviceRequests
        web_sys::UsbRequestType::Standard,
        (0x03_u16 << 8) | (id),
    );
    let res = JsFuture::from(device.control_transfer_in(&setup, 255))
        .await
        .map_err(js_value_to_error)?;
    let res: UsbInTransferResult = JsCast::unchecked_from_js(res.into());
    let mut data = Uint8Array::new(
        &res.data()
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::Other,
                    "string descriptor transfer returned no data",
                )
            })?
            .buffer(),
    )
    .to_vec();

    String::from_utf16(
        &data
            .drain(2..data[0] as usize)
            .collect::<Vec<_>>()
            .chunks(2)
            .map(|c| ((c[1] as u16) << 8) | c[0] as u16)
            .collect::<Vec<_>>(),
    )
    .map_err(|_| Error::new(ErrorKind::Other, "invalid utf16"))
}

#[derive(Clone)]
pub(crate) struct WebusbInterface {
    pub interface_number: u8,
    pub(crate) device: Arc<WebusbDevice>,
    state: Arc<Mutex<InterfaceState>>,
}

#[derive(Default)]
struct InterfaceState {
    alt_setting: u8,
    endpoints_used: EndpointBitSet,
}

impl WebusbInterface {
    pub fn set_alt_setting(
        self: Arc<Self>,
        alternate_setting: u8,
    ) -> impl MaybeFuture<Output = Result<(), Error>> {
        WebFuture(async move {
            {
                let state = self.state.lock().unwrap();
                if !state.endpoints_used.is_empty() {
                    return Err(Error::new(
                        ErrorKind::Other,
                        "must drop endpoints before changing alt setting",
                    ));
                }
            }

            JsFuture::from(
                self.device
                    .device
                    .select_alternate_interface(self.interface_number, alternate_setting),
            )
            .await
            .map_err(js_value_to_error)
            .map(|_| ())?;

            {
                let mut state = self.state.lock().unwrap();
                state.alt_setting = alternate_setting;
            }

            Ok(())
        })
    }

    pub fn get_alt_setting(&self) -> u8 {
        self.state.lock().unwrap().alt_setting
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
        .map_err(js_value_to_error)
        .map(|_| ())
    }

    #[allow(dead_code)]
    pub fn control_in(
        self: Arc<Self>,
        control: ControlIn,
        _timeout: Duration,
    ) -> impl MaybeFuture<Output = Result<Vec<u8>, TransferError>> {
        WebFuture(async move {
            let setup = UsbControlTransferParameters::new(
                control.index,
                control.recipient.into(),
                control.request,
                control.control_type.into(),
                control.value,
            );
            let res = JsFuture::from(self.device.device.control_transfer_in(&setup, 255))
                .await
                .map_err(js_value_to_transfer_error)?;
            let res: UsbInTransferResult = JsCast::unchecked_from_js(res.into());
            let data = res.data().ok_or(TransferError::Fault)?;
            Ok(Uint8Array::new(&data.buffer()).to_vec())
        })
    }

    #[allow(dead_code)]
    pub(crate) fn control_out(
        self: Arc<Self>,
        control: ControlOut<'_>,
        _timeout: Duration,
    ) -> impl MaybeFuture<Output = Result<(), TransferError>> {
        let setup = UsbControlTransferParameters::new(
            control.index,
            control.recipient.into(),
            control.request,
            control.control_type.into(),
            control.value,
        );
        let mut data = control.data.to_vec();

        WebFuture(async move {
            let res = JsFuture::from(
                self.device
                    .device
                    .control_transfer_out_with_u8_slice(&setup, &mut data)
                    .map_err(js_value_to_transfer_error)?,
            )
            .await
            .map_err(js_value_to_transfer_error)?;
            let res: UsbOutTransferResult = JsCast::unchecked_from_js(res.into());

            webusb_status_to_nusb_transfer_error(res.status())
        })
    }

    pub fn endpoint(
        self: &Arc<Self>,
        descriptor: EndpointDescriptor,
    ) -> Result<WebusbEndpoint, Error> {
        let address = descriptor.address();
        let max_packet_size = descriptor.max_packet_size();

        let mut state = self.state.lock().unwrap();
        if state.endpoints_used.is_set(address) {
            return Err(Error::new(ErrorKind::Busy, "endpoint already in use"));
        }
        state.endpoints_used.set(address);

        Ok(WebusbEndpoint {
            inner: Arc::new(EndpointInner {
                address,
                interface: self.clone(),
            }),
            max_packet_size,
            pending: VecDeque::new(),
        })
    }
}

/// One queued transfer.
///
/// WebUSB owns its own buffers in JS land, so unlike the OS-level backends
/// we just hold the user's `Buffer` alongside the `JsFuture` for the in-flight
/// promise.
enum Pending {
    InFlight {
        future: JsFuture<JsValue>,
        buffer: Buffer,
        direction: Direction,
    },
    /// Submission failed synchronously (e.g. validation error or a thrown
    /// exception from the WebUSB API). Returned as a completion when the
    /// caller next polls.
    Failed {
        buffer: Buffer,
        error: TransferError,
    },
}

// `JsFuture` contains an `Rc<RefCell<...>>`, so it is neither `Send` nor
// `Sync`. The cross-platform `Endpoint` API requires its backend to be both
// (so users can pass endpoints into Send-bounded async runtimes, the same
// reason `wasm-bindgen` itself unsafe-impls `Send`/`Sync` for `JsValue`).
// wasm32 is single-threaded by default, and even with the threads proposal
// JS objects can't transit between worker contexts, so this is sound in
// practice.
unsafe impl Send for Pending {}
unsafe impl Sync for Pending {}

pub(crate) struct WebusbEndpoint {
    inner: Arc<EndpointInner>,

    pub(crate) max_packet_size: usize,

    /// A queue of pending transfers, expected to complete in order.
    pending: VecDeque<Pending>,
}

struct EndpointInner {
    interface: Arc<WebusbInterface>,
    address: u8,
}

impl WebusbEndpoint {
    pub(crate) fn endpoint_address(&self) -> u8 {
        self.inner.address
    }

    pub(crate) fn pending(&self) -> usize {
        self.pending.len()
    }

    pub(crate) fn cancel_all(&mut self) {
        // WebUSB does not expose a transfer cancellation API (see
        // https://github.com/WICG/webusb/issues/25), so pending transfers
        // cannot be cancelled and will run to completion on the JS event loop.
    }

    /// Enqueue a transfer that fails immediately with the given error.
    ///
    /// Used by the public `Endpoint::submit` to surface validation errors
    /// through the same completion queue as real transfers.
    pub(crate) fn submit_err(&mut self, buffer: Buffer, error: TransferError) {
        self.pending.push_back(Pending::Failed { buffer, error });
    }

    pub(crate) fn submit(&mut self, buffer: Buffer) {
        let address = self.inner.address;
        let direction = Direction::from_address(address);
        let endpoint_number = address & 0x7F;
        let device = &self.inner.interface.device.device;

        let promise: Promise<JsValue> = match direction {
            Direction::Out => {
                let array = Uint8Array::from(&buffer[..]);
                match device.transfer_out_with_buffer_source(endpoint_number, array.unchecked_ref())
                {
                    Ok(p) => p.unchecked_into(),
                    Err(_) => {
                        self.pending.push_back(Pending::Failed {
                            buffer,
                            error: TransferError::Fault,
                        });
                        return;
                    }
                }
            }
            Direction::In => device
                .transfer_in(endpoint_number, buffer.requested_len() as u32)
                .unchecked_into(),
        };

        self.pending.push_back(Pending::InFlight {
            future: JsFuture::from(promise),
            buffer,
            direction,
        });
    }

    pub(crate) fn poll_next_complete(&mut self, cx: &mut Context) -> Poll<Completion> {
        let Some(head) = self.pending.front_mut() else {
            return Poll::Pending;
        };

        match head {
            Pending::Failed { .. } => {
                let Some(Pending::Failed { buffer, error }) = self.pending.pop_front() else {
                    unreachable!()
                };
                Poll::Ready(Completion {
                    buffer,
                    actual_len: 0,
                    status: Err(error),
                })
            }
            Pending::InFlight { future, .. } => {
                let result = match Pin::new(future).poll(cx) {
                    Poll::Ready(r) => r,
                    Poll::Pending => return Poll::Pending,
                };
                let Some(Pending::InFlight {
                    buffer, direction, ..
                }) = self.pending.pop_front()
                else {
                    unreachable!()
                };
                Poll::Ready(complete_transfer(buffer, direction, result))
            }
        }
    }

    pub(crate) fn clear_halt(&self) -> impl MaybeFuture<Output = Result<(), Error>> {
        let device = self.inner.interface.device.clone();
        let endpoint = self.inner.address;
        let endpoint_in = endpoint & 0x80 != 0;
        WebFuture(async move {
            JsFuture::from(device.device.clear_halt(
                if endpoint_in {
                    web_sys::UsbDirection::In
                } else {
                    web_sys::UsbDirection::Out
                },
                endpoint,
            ))
            .await
            .map_err(js_value_to_error)
            .map(|_| ())
        })
    }
}

fn complete_transfer(
    mut buffer: Buffer,
    direction: Direction,
    result: Result<JsValue, JsValue>,
) -> Completion {
    let result = match result {
        Ok(r) => r,
        Err(e) => {
            return Completion {
                buffer,
                actual_len: 0,
                status: Err(js_value_to_transfer_error(e)),
            };
        }
    };

    match direction {
        Direction::Out => {
            let result: UsbOutTransferResult = JsCast::unchecked_from_js(result.into());
            // `buffer.len` is the user-supplied payload length (unchanged from
            // submit). `actual_len` is what the device acknowledged.
            Completion {
                actual_len: result.bytes_written() as usize,
                status: webusb_status_to_nusb_transfer_error(result.status()),
                buffer,
            }
        }
        Direction::In => {
            let result: UsbInTransferResult = JsCast::unchecked_from_js(result.into());
            let Some(data) = result.data() else {
                return Completion {
                    buffer,
                    actual_len: 0,
                    status: Err(TransferError::Fault),
                };
            };
            let received = Uint8Array::new(&data.buffer());
            let received_len = received.length().min(buffer.capacity);
            // Safety: the slice covers `received_len` bytes within the buffer's
            // allocation (capacity-bounded). `Buffer` derefs as `&[u8]` of
            // length `len`, so we write past `len` here.
            unsafe {
                received.copy_to(std::slice::from_raw_parts_mut(
                    buffer.ptr,
                    received_len as usize,
                ));
            }
            buffer.len = received_len;
            Completion {
                actual_len: received_len as usize,
                status: webusb_status_to_nusb_transfer_error(result.status()),
                buffer,
            }
        }
    }
}

impl Drop for WebusbEndpoint {
    fn drop(&mut self) {
        self.cancel_all();
    }
}

impl Drop for EndpointInner {
    fn drop(&mut self) {
        let mut state = self.interface.state.lock().unwrap();
        state.endpoints_used.clear(self.address);
    }
}
