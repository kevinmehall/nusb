use std::{
    collections::VecDeque,
    io::{Error, ErrorKind},
    mem::ManuallyDrop,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::Duration,
};

pub use private::UniqueUsbDevice;
use wasm_bindgen_futures::{js_sys::Array, spawn_local, wasm_bindgen::JsCast, JsFuture};
use web_sys::{
    js_sys::{Object, Uint8Array},
    UsbControlTransferParameters, UsbDevice, UsbInTransferResult, UsbOutTransferResult,
};

use crate::{
    bitset::EndpointBitSet,
    descriptors::{
        ConfigurationDescriptor, DeviceDescriptor, EndpointDescriptor,
        DESCRIPTOR_TYPE_CONFIGURATION,
    },
    maybe_future::future::ActualFuture,
    transfer::{
        internal::{notify_completion, take_completed_from_queue, Idle, Notify, Pending},
        Buffer, Completion, ControlIn, ControlOut, Direction, TransferError,
    },
    ClaimEndpointError, DeviceInfo, MaybeFuture, Speed,
};

use super::{
    js_value_to_io_error, js_value_to_transfer_error, webusb_status_to_nusb_transfer_error,
    TransferData,
};

pub mod private {
    use std::ops::{Deref, DerefMut};

    use web_sys::UsbDevice;

    pub struct UniqueUsbDevice(UsbDevice);

    // TODO: How do I best make this Send/Sync?
    // Maybe store an atomic count on the JS Object and get that?
    // Probably needs a global lock too to make sure not two people write to it.
    // If that's the case, we can probably store the open devices in a global mutex.
    // Singleton is probably ok here because devices should be uniquely accessible?
    unsafe impl Send for UniqueUsbDevice {}
    unsafe impl Sync for UniqueUsbDevice {}

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
    ) -> impl MaybeFuture<Output = Result<Arc<WebusbDevice>, std::io::Error>> {
        let target_device = d.device.clone();
        let speed = d.speed;
        ActualFuture::new(async move {
            let usb = super::usb()?;
            let devices = JsFuture::from(usb.get_devices())
                .await
                .map_err(js_value_to_io_error)?;
            let devices: Array = JsCast::unchecked_from_js(devices);

            for device in devices {
                let device: UsbDevice = JsCast::unchecked_from_js(device);
                if device.eq(&target_device) {
                    JsFuture::from(device.open())
                        .await
                        .map_err(js_value_to_io_error)?;

                    let config_descriptors = extract_decriptors(&device).await?;

                    #[allow(clippy::arc_with_non_send_sync)]
                    return Ok(Arc::new(Self {
                        // TODO: Check that we only open this once.
                        device: Arc::new(UniqueUsbDevice::new(device)),
                        config_descriptors,
                        speed,
                    }));
                }
            }
            Err(Error::other("device not found"))
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
        ActualFuture::new(async move {
            JsFuture::from(self.device.select_configuration(configuration))
                .await
                .map_err(|e| {
                    Error::other(
                        e.as_string()
                            .unwrap_or_else(|| "No further error clarification available".into()),
                    )
                })
                .map(|_| ())
        })
    }

    pub(crate) fn reset(self: Arc<Self>) -> impl MaybeFuture<Output = Result<(), Error>> {
        ActualFuture::new(async move {
            JsFuture::from(self.device.reset())
                .await
                .map_err(|e| {
                    Error::other(
                        e.as_string()
                            .unwrap_or_else(|| "No further error clarification available".into()),
                    )
                })
                .map(|_| ())
        })
    }

    pub(crate) fn claim_interface(
        self: Arc<Self>,
        interface_number: u8,
    ) -> impl MaybeFuture<Output = Result<Arc<WebusbInterface>, Error>> {
        ActualFuture::new(async move {
            JsFuture::from(self.device.claim_interface(interface_number))
                .await
                .map_err(js_value_to_io_error)?;

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
        .map_err(js_value_to_io_error)?;
    let res: UsbInTransferResult = JsCast::unchecked_from_js(res);
    Ok(Uint8Array::new(&res.data().expect("a data buffer").buffer()).to_vec())
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
        .map_err(js_value_to_io_error)?;
    let res: UsbInTransferResult = JsCast::unchecked_from_js(res);
    let mut data = Uint8Array::new(&res.data().expect("a data buffer").buffer()).to_vec();

    String::from_utf16(
        &data
            .drain(2..data[0] as usize)
            .collect::<Vec<_>>()
            .chunks(2)
            .map(|c| ((c[1] as u16) << 8) | c[0] as u16)
            .collect::<Vec<_>>(),
    )
    .map_err(|_| Error::other("invalid utf16"))
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
        ActualFuture::new(async move {
            {
                // TODO: We need an async mutex to lock for the whole duration.
                let state = self.state.lock().unwrap();

                if !state.endpoints_used.is_empty() {
                    // TODO: Use ErrorKind::ResourceBusy once compatible with MSRV

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
            .map_err(|e| {
                Error::other(
                    e.as_string()
                        .unwrap_or_else(|| "No further error clarification available".into()),
                )
            })
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
        .map_err(|e| {
            Error::other(
                e.as_string()
                    .unwrap_or_else(|| "No further error clarification available".into()),
            )
        })
        .map(|_| ())
    }

    #[allow(dead_code)]
    pub fn control_in(
        self: Arc<Self>,
        control: ControlIn,
        _timeout: Duration,
    ) -> impl MaybeFuture<Output = Result<Vec<u8>, TransferError>> {
        ActualFuture::new(async move {
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
            let res: UsbInTransferResult = JsCast::unchecked_from_js(res);
            let array = Uint8Array::new(&res.data().expect("a data buffer").buffer());

            Ok(array.to_vec())
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

        ActualFuture::new(async move {
            let res = JsFuture::from(
                self.device
                    .device
                    .control_transfer_out_with_u8_slice(&setup, &mut data)
                    .map_err(js_value_to_transfer_error)?,
            )
            .await
            .map_err(js_value_to_transfer_error)?;
            let res: UsbOutTransferResult = JsCast::unchecked_from_js(res);

            webusb_status_to_nusb_transfer_error(res.status())
        })
    }

    pub fn endpoint(
        self: &Arc<Self>,
        descriptor: EndpointDescriptor,
    ) -> Result<WebusbEndpoint, ClaimEndpointError> {
        let address = descriptor.address();
        let max_packet_size = descriptor.max_packet_size();

        Ok(WebusbEndpoint {
            inner: Arc::new(EndpointInner {
                address,
                notify: Arc::new(Notify::new()),
                interface: self.clone(),
            }),
            max_packet_size,
            pending: VecDeque::new(),
            idle_transfer: None,
        })
    }
}

pub(crate) struct WebusbEndpoint {
    inner: Arc<EndpointInner>,

    pub(crate) max_packet_size: usize,

    /// A queue of pending transfers, expected to complete in order
    pending: VecDeque<Pending<super::TransferData>>,

    idle_transfer: Option<Idle<super::TransferData>>,
}

struct EndpointInner {
    interface: Arc<WebusbInterface>,
    address: u8,
    notify: Arc<Notify>,
}

impl WebusbEndpoint {
    pub(crate) fn endpoint_address(&self) -> u8 {
        self.inner.address
    }

    pub(crate) fn pending(&self) -> usize {
        self.pending.len()
    }

    pub(crate) fn cancel_all(&mut self) {
        // Cancel transfers in reverse order to ensure subsequent transfers
        // can't complete out of order while we're going through them.
        // TODO: Implement cancelling.
        // for transfer in self.pending.iter_mut().rev() {
        //     self.inner.interface.device.device.cancel(transfer);
        // }
    }

    pub(crate) fn submit(&mut self, buffer: Buffer) {
        let transfer = self
            .idle_transfer
            .take()
            .unwrap_or_else(|| Idle::new(self.inner.notify.clone(), super::TransferData::new()));

        let buffer = ManuallyDrop::new(buffer);

        let address = self.inner.address;
        let dir = Direction::from_address(self.inner.address);

        let transfer = transfer.pre_submit();
        let ptr = transfer.as_ptr();

        let device = self.inner.interface.clone();

        spawn_local(async move {
            match dir {
                Direction::Out => {
                    let data = buffer.to_vec();
                    let array = Uint8Array::from(data.as_slice());
                    let array_obj = Object::try_from(&array).expect("an object");
                    let endpoint_number = address;

                    let result = JsFuture::from(
                        device
                            .device
                            .device
                            .transfer_out_with_buffer_source(endpoint_number, array_obj)
                            .expect("transfers are possible"),
                    )
                    .await
                    .expect("transfers don't fail");

                    let transfer_result: UsbOutTransferResult = JsCast::unchecked_from_js(result);

                    unsafe {
                        (*ptr).status = transfer_result.status();
                        (*ptr).actual_len = transfer_result.bytes_written();
                        (*ptr).actual_len = data.len() as u32;
                        notify_completion::<TransferData>(ptr)
                    }
                }
                Direction::In => {
                    let endpoint_number = address & (!0x80);
                    let mut data = buffer.to_vec();
                    let len = data.len() as u32;
                    let result =
                        JsFuture::from(device.device.device.transfer_in(endpoint_number, len))
                            .await
                            .expect("transfers are possible");

                    let transfer_result: UsbInTransferResult = JsCast::unchecked_from_js(result);
                    let received_data = Uint8Array::new(
                        &transfer_result
                            .data()
                            .expect("a data buffer is present")
                            .buffer(),
                    );
                    data.resize(received_data.length() as usize, 0);
                    received_data.copy_to(&mut data[..received_data.length() as usize]);

                    unsafe {
                        (*ptr).status = transfer_result.status();
                        (*ptr).actual_len = len;
                        (*ptr).requested_len = len;
                        notify_completion::<TransferData>(ptr)
                    }
                }
            }
        });
        self.pending.push_back(transfer);
    }

    pub(crate) fn poll_next_complete(&mut self, cx: &mut Context) -> Poll<Completion> {
        self.inner.notify.subscribe(cx);
        let dir = Direction::from_address(self.inner.address);
        if let Some(mut transfer) = take_completed_from_queue(&mut self.pending) {
            let completion = unsafe { transfer.take_completion(dir) };
            self.idle_transfer = Some(transfer);
            Poll::Ready(completion)
        } else {
            Poll::Pending
        }
    }

    pub(crate) fn clear_halt(&self) -> impl MaybeFuture<Output = Result<(), Error>> {
        let device = self.inner.interface.device.clone();
        let endpoint = self.inner.address;
        let endpoint_in = endpoint & 0x80 != 0;
        ActualFuture::new(async move {
            JsFuture::from(device.device.clear_halt(
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
        })
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
