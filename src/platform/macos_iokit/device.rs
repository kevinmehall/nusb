use std::{
    collections::VecDeque,
    ffi::c_void,
    io::ErrorKind,
    mem::{self, ManuallyDrop},
    sync::{
        atomic::{AtomicU8, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    task::{Context, Poll},
    time::Duration,
};

use io_kit_sys::ret::{kIOReturnSuccess, IOReturn};
use log::{debug, error};

use crate::{
    bitset::EndpointBitSet,
    descriptors::{ConfigurationDescriptor, DeviceDescriptor, EndpointDescriptor},
    device::ClaimEndpointError,
    maybe_future::blocking::Blocking,
    transfer::{
        internal::{
            notify_completion, take_completed_from_queue, Idle, Notify, Pending, TransferFuture,
        },
        Allocator, Buffer, ControlIn, ControlOut, Direction, TransferError,
    },
    Completion, DeviceInfo, Error, MaybeFuture, Speed,
};

use super::{
    enumeration::{device_descriptor_from_fields, get_integer_property, service_by_registry_id},
    events::{add_event_source, EventRegistration},
    iokit::{call_iokit_function, check_iokit_return},
    iokit_c::IOUSBDevRequestTO,
    iokit_usb::{IoKitDevice, IoKitInterface},
    TransferData,
};

pub(crate) struct MacDevice {
    _event_registration: EventRegistration,
    pub(super) device: IoKitDevice,
    device_descriptor: DeviceDescriptor,
    speed: Option<Speed>,
    active_config: AtomicU8,
    is_open_exclusive: Mutex<bool>,
    claimed_interfaces: AtomicUsize,
}

// `get_configuration` does IO, so avoid it in the common case that:
//    * the device has a single configuration
//    * the device has at least one interface, indicating that it is configured
fn guess_active_config(dev: &IoKitDevice) -> Option<u8> {
    if dev.get_number_of_configurations().unwrap_or(0) != 1 {
        return None;
    }
    let mut intf = dev.create_interface_iterator().ok()?;
    intf.next()?;
    let config_desc = dev.get_configuration_descriptor(0).ok()?;
    config_desc.get(5).copied() // get bConfigurationValue from descriptor
}

impl MacDevice {
    pub(crate) fn from_device_info(
        d: &DeviceInfo,
    ) -> impl MaybeFuture<Output = Result<Arc<MacDevice>, Error>> {
        let registry_id = d.registry_id;
        let speed = d.speed;
        Blocking::new(move || {
            log::info!("Opening device from registry id {}", registry_id);
            let service = service_by_registry_id(registry_id)?;
            let device = IoKitDevice::new(&service)?;
            let _event_registration = add_event_source(device.create_async_event_source()?);

            let opened = match unsafe { call_iokit_function!(device.raw, USBDeviceOpen()) } {
                io_kit_sys::ret::kIOReturnSuccess => true,
                err => {
                    // Most methods don't require USBDeviceOpen() so this can be ignored
                    // to allow different processes to open different interfaces.
                    log::debug!("Could not open device for exclusive access: {err:x}");
                    false
                }
            };

            let device_descriptor = device_descriptor_from_fields(&service).ok_or_else(|| {
                Error::new(
                    ErrorKind::Other,
                    "could not read properties for device descriptor",
                )
            })?;

            let active_config = if let Some(active_config) = guess_active_config(&device) {
                log::debug!("Active config from single descriptor is {}", active_config);
                active_config
            } else {
                let res = device.get_configuration();
                log::debug!("Active config from request is {:?}", res);
                res.unwrap_or(0)
            };

            Ok(Arc::new(MacDevice {
                _event_registration,
                device,
                device_descriptor,
                speed,
                active_config: AtomicU8::new(active_config),
                is_open_exclusive: Mutex::new(opened),
                claimed_interfaces: AtomicUsize::new(0),
            }))
        })
    }

    pub(crate) fn device_descriptor(&self) -> DeviceDescriptor {
        self.device_descriptor.clone()
    }

    pub(crate) fn speed(&self) -> Option<Speed> {
        self.speed
    }

    pub(crate) fn active_configuration_value(&self) -> u8 {
        self.active_config.load(Ordering::SeqCst)
    }

    pub(crate) fn configuration_descriptors(
        &self,
    ) -> impl Iterator<Item = ConfigurationDescriptor> {
        let num_configs = self.device.get_number_of_configurations().unwrap_or(0);
        (0..num_configs)
            .flat_map(|i| self.device.get_configuration_descriptor(i).ok())
            .flat_map(ConfigurationDescriptor::new)
    }

    fn require_open_exclusive(&self) -> Result<(), Error> {
        let mut state = self.is_open_exclusive.lock().unwrap();
        if *state == false {
            unsafe { check_iokit_return(call_iokit_function!(self.device.raw, USBDeviceOpen()))? };
            *state = true;
        }

        if self.claimed_interfaces.load(Ordering::Relaxed) != 0 {
            return Err(Error::new(
                ErrorKind::Other,
                "cannot perform this operation while interfaces are claimed",
            ));
        }

        Ok(())
    }

    pub(crate) fn set_configuration(
        self: Arc<Self>,
        configuration: u8,
    ) -> impl MaybeFuture<Output = Result<(), Error>> {
        Blocking::new(move || {
            self.require_open_exclusive()?;
            unsafe {
                check_iokit_return(call_iokit_function!(
                    self.device.raw,
                    SetConfiguration(configuration)
                ))?
            }
            log::debug!("Set configuration {configuration}");
            self.active_config.store(configuration, Ordering::SeqCst);
            Ok(())
        })
    }

    pub(crate) fn reset(self: Arc<Self>) -> impl MaybeFuture<Output = Result<(), Error>> {
        Blocking::new(move || {
            self.require_open_exclusive()?;
            unsafe {
                check_iokit_return(call_iokit_function!(
                    self.device.raw,
                    USBDeviceReEnumerate(0)
                ))
            }
        })
    }

    pub(crate) fn claim_interface(
        self: Arc<Self>,
        interface_number: u8,
    ) -> impl MaybeFuture<Output = Result<Arc<MacInterface>, Error>> {
        Blocking::new(move || {
            let intf_service = self
                .device
                .create_interface_iterator()?
                .find(|io_service| {
                    let current_number = get_integer_property(io_service, "bInterfaceNumber");
                    let found = current_number == Some(interface_number as i64);
                    debug!(
                        "Looking for interface to claim [n={interface_number}], examining interface [n={}]{}",
                        current_number.map(|n| n.to_string()).unwrap_or_else(|| "unknown".to_string()),
                        found.then(|| " (found)").unwrap_or("")
                    );
                    found
                })
                .ok_or(Error::new(ErrorKind::NotFound, "interface not found"))?;

            let mut interface = IoKitInterface::new(intf_service)?;
            let _event_registration = add_event_source(interface.create_async_event_source()?);

            interface.open()?;
            self.claimed_interfaces.fetch_add(1, Ordering::Acquire);

            Ok(Arc::new(MacInterface {
                device: self.clone(),
                interface_number,
                interface,
                state: Mutex::new(InterfaceState::default()),
                _event_registration,
            }))
        })
    }

    pub(crate) fn detach_and_claim_interface(
        self: Arc<Self>,
        interface: u8,
    ) -> impl MaybeFuture<Output = Result<Arc<MacInterface>, Error>> {
        self.claim_interface(interface)
    }

    pub fn control_in(
        self: &Arc<Self>,
        data: ControlIn,
        timeout: Duration,
    ) -> impl MaybeFuture<Output = Result<Vec<u8>, TransferError>> {
        let timeout = timeout.as_millis().try_into().expect("timeout too long");
        let mut v = ManuallyDrop::new(Vec::with_capacity(data.length as usize));
        let t = unsafe {
            TransferData::from_raw(v.as_mut_ptr(), data.length as u32, v.capacity() as u32)
        };

        let req = IOUSBDevRequestTO {
            bmRequestType: data.request_type(),
            bRequest: data.request,
            wValue: data.value,
            wIndex: data.index,
            wLength: data.length,
            pData: t.buf as *mut c_void,
            wLenDone: 0,
            completionTimeout: timeout,
            noDataTimeout: timeout,
        };

        TransferFuture::new(t, |t| self.submit_control(Direction::In, t, req)).map(|t| {
            t.status()?;
            let t = ManuallyDrop::new(t);
            Ok(unsafe { Vec::from_raw_parts(t.buf, t.actual_len as usize, t.capacity as usize) })
        })
    }

    pub fn control_out(
        self: &Arc<Self>,
        data: ControlOut,
        timeout: Duration,
    ) -> impl MaybeFuture<Output = Result<(), TransferError>> {
        let timeout = timeout.as_millis().try_into().expect("timeout too long");
        let mut v = ManuallyDrop::new(data.data.to_vec());
        let t =
            unsafe { TransferData::from_raw(v.as_mut_ptr(), v.len() as u32, v.capacity() as u32) };

        let req = IOUSBDevRequestTO {
            bmRequestType: data.request_type(),
            bRequest: data.request,
            wValue: data.value,
            wIndex: data.index,
            wLength: u16::try_from(data.data.len()).expect("request too long"),
            pData: t.buf as *mut c_void,
            wLenDone: 0,
            completionTimeout: timeout,
            noDataTimeout: timeout,
        };

        TransferFuture::new(t, |t| self.submit_control(Direction::Out, t, req)).map(|t| {
            t.status()?;
            Ok(())
        })
    }

    fn submit_control(
        &self,
        dir: Direction,
        mut t: Idle<TransferData>,
        mut req: IOUSBDevRequestTO,
    ) -> Pending<TransferData> {
        t.actual_len = 0;
        assert!(req.pData == t.buf.cast());

        let t = t.pre_submit();
        let ptr = t.as_ptr();

        let res = unsafe {
            call_iokit_function!(
                self.device.raw,
                DeviceRequestAsyncTO(&mut req, transfer_callback, ptr as *mut c_void)
            )
        };

        if res == kIOReturnSuccess {
            debug!("Submitted control {dir:?} {ptr:?}");
        } else {
            error!("Failed to submit control {dir:?} {ptr:?}: {res:x}");
            unsafe {
                // Complete the transfer in the place of the callback
                (*ptr).status = res;
                notify_completion::<super::TransferData>(ptr);
            }
        }

        t
    }
}

impl Drop for MacDevice {
    fn drop(&mut self) {
        if *self.is_open_exclusive.get_mut().unwrap() {
            match unsafe { call_iokit_function!(self.device.raw, USBDeviceClose()) } {
                io_kit_sys::ret::kIOReturnSuccess => {}
                err => log::debug!("Failed to close device: {err:x}"),
            };
        }
    }
}

pub(crate) struct MacInterface {
    pub(crate) interface_number: u8,
    _event_registration: EventRegistration,
    pub(crate) interface: IoKitInterface,
    pub(crate) device: Arc<MacDevice>,
    state: Mutex<InterfaceState>,
}

#[derive(Default)]
struct InterfaceState {
    alt_setting: u8,
    endpoints_used: EndpointBitSet,
}

impl MacInterface {
    pub fn set_alt_setting(
        self: Arc<Self>,
        alt_setting: u8,
    ) -> impl MaybeFuture<Output = Result<(), Error>> {
        Blocking::new(move || {
            let mut state = self.state.lock().unwrap();

            if !state.endpoints_used.is_empty() {
                // TODO: Use ErrorKind::ResourceBusy once compatible with MSRV

                return Err(Error::new(
                    ErrorKind::Other,
                    "must drop endpoints before changing alt setting",
                ));
            }

            unsafe {
                check_iokit_return(call_iokit_function!(
                    self.interface.raw,
                    SetAlternateInterface(alt_setting)
                ))?;
            }

            debug!(
                "Set interface {} alt setting to {alt_setting}",
                self.interface_number
            );

            state.alt_setting = alt_setting;

            Ok(())
        })
    }

    pub fn get_alt_setting(&self) -> u8 {
        self.state.lock().unwrap().alt_setting
    }

    pub fn control_in(
        self: &Arc<Self>,
        data: ControlIn,
        timeout: Duration,
    ) -> impl MaybeFuture<Output = Result<Vec<u8>, TransferError>> {
        self.device.control_in(data, timeout)
    }

    pub fn control_out(
        self: &Arc<Self>,
        data: ControlOut,
        timeout: Duration,
    ) -> impl MaybeFuture<Output = Result<(), TransferError>> {
        self.device.control_out(data, timeout)
    }

    pub fn endpoint(
        self: &Arc<Self>,
        descriptor: EndpointDescriptor,
    ) -> Result<MacEndpoint, ClaimEndpointError> {
        let address = descriptor.address();
        let max_packet_size = descriptor.max_packet_size();

        let mut state = self.state.lock().unwrap();

        let Some(pipe_ref) = self.interface.find_pipe_ref(address) else {
            debug!("Endpoint {address:02X} not found in iokit");
            return Err(ClaimEndpointError::InvalidAddress);
        };

        if state.endpoints_used.is_set(address) {
            return Err(ClaimEndpointError::Busy);
        }
        state.endpoints_used.set(address);

        Ok(MacEndpoint {
            inner: Arc::new(EndpointInner {
                pipe_ref,
                address,
                interface: self.clone(),
                notify: Notify::new(),
            }),
            max_packet_size,
            pending: VecDeque::new(),
            idle_transfer: None,
        })
    }
}

impl Drop for MacInterface {
    fn drop(&mut self) {
        if let Err(err) = self.interface.close() {
            error!("Failed to close interface: {err}")
        }
        self.device
            .claimed_interfaces
            .fetch_sub(1, Ordering::Release);
    }
}

pub(crate) struct MacEndpoint {
    inner: Arc<EndpointInner>,
    pub(crate) max_packet_size: usize,

    /// A queue of pending transfers, expected to complete in order
    pending: VecDeque<Pending<TransferData>>,

    idle_transfer: Option<Idle<TransferData>>,
}

struct EndpointInner {
    interface: Arc<MacInterface>,
    pipe_ref: u8,
    address: u8,
    notify: Notify,
}

impl MacEndpoint {
    pub(crate) fn endpoint_address(&self) -> u8 {
        self.inner.address
    }

    pub(crate) fn pending(&self) -> usize {
        self.pending.len()
    }

    pub(crate) fn cancel_all(&mut self) {
        let r = unsafe {
            call_iokit_function!(
                self.inner.interface.interface.raw,
                AbortPipe(self.inner.pipe_ref)
            )
        };
        debug!(
            "Cancelled all transfers on endpoint {ep:02x}. status={r:x}",
            ep = self.inner.address
        );
    }

    pub(crate) fn submit(&mut self, buffer: Buffer) {
        let mut transfer = self
            .idle_transfer
            .take()
            .unwrap_or_else(|| Idle::new(self.inner.clone(), super::TransferData::new()));

        let buffer = ManuallyDrop::new(buffer);
        let endpoint = self.inner.address;
        let dir = Direction::from_address(endpoint);
        let pipe_ref = self.inner.pipe_ref;

        transfer.buf = buffer.ptr;
        transfer.capacity = buffer.capacity;
        transfer.actual_len = 0;
        let req_len = match dir {
            Direction::Out => buffer.len,
            Direction::In => buffer.transfer_len,
        };
        transfer.requested_len = req_len;

        let transfer = transfer.pre_submit();
        let ptr = transfer.as_ptr();

        let res = unsafe {
            match dir {
                Direction::Out => call_iokit_function!(
                    self.inner.interface.interface.raw,
                    WritePipeAsync(
                        pipe_ref,
                        buffer.ptr as *mut c_void,
                        buffer.len,
                        transfer_callback,
                        ptr as *mut c_void
                    )
                ),
                Direction::In => call_iokit_function!(
                    self.inner.interface.interface.raw,
                    ReadPipeAsync(
                        pipe_ref,
                        buffer.ptr as *mut c_void,
                        buffer.transfer_len,
                        transfer_callback,
                        ptr as *mut c_void
                    )
                ),
            }
        };

        if res == kIOReturnSuccess {
            debug!(
                "Submitted {dir:?} transfer {ptr:?} of len {req_len} on endpoint {endpoint:02X}"
            );
        } else {
            error!("Failed to submit transfer {ptr:?} of len {req_len} on endpoint {endpoint:02X}: {res:x}");
            unsafe {
                // Complete the transfer in the place of the callback
                (*ptr).status = res;
                notify_completion::<super::TransferData>(ptr);
            }
        }

        self.pending.push_back(transfer);
    }

    pub(crate) fn poll_next_complete(&mut self, cx: &mut Context) -> Poll<Completion> {
        self.inner.notify.subscribe(cx);
        if let Some(mut transfer) = take_completed_from_queue(&mut self.pending) {
            let status = transfer.status();

            let mut empty = ManuallyDrop::new(Vec::new());
            let ptr = mem::replace(&mut transfer.buf, empty.as_mut_ptr());
            let capacity = mem::replace(&mut transfer.capacity, 0);
            let (len, transfer_len) = match Direction::from_address(self.inner.address) {
                Direction::Out => (transfer.requested_len, transfer.actual_len),
                Direction::In => (transfer.actual_len, transfer.requested_len),
            };
            transfer.requested_len = 0;
            transfer.actual_len = 0;
            self.idle_transfer = Some(transfer);

            let data = Buffer {
                ptr,
                len,
                transfer_len,
                capacity,
                allocator: Allocator::Default,
            };

            Poll::Ready(Completion { status, data })
        } else {
            Poll::Pending
        }
    }

    pub(crate) fn clear_halt(&mut self) -> impl MaybeFuture<Output = Result<(), Error>> {
        let inner = self.inner.clone();
        Blocking::new(move || {
            debug!("Clear halt, endpoint {:02x}", inner.address);

            unsafe {
                check_iokit_return(call_iokit_function!(
                    inner.interface.interface.raw,
                    ClearPipeStallBothEnds(inner.pipe_ref)
                ))
            }
        })
    }
}

impl Drop for MacEndpoint {
    fn drop(&mut self) {
        self.cancel_all();
    }
}

impl AsRef<Notify> for EndpointInner {
    fn as_ref(&self) -> &Notify {
        &self.notify
    }
}

impl Drop for EndpointInner {
    fn drop(&mut self) {
        let mut state = self.interface.state.lock().unwrap();
        state.endpoints_used.clear(self.address);
    }
}

extern "C" fn transfer_callback(refcon: *mut c_void, result: IOReturn, len: *mut c_void) {
    let len = len as u32;
    let transfer: *mut TransferData = refcon.cast();
    debug!("Completion for transfer {transfer:?}, status={result:x}, len={len}");

    unsafe {
        (*transfer).actual_len = len;
        (*transfer).status = result;
        notify_completion::<TransferData>(transfer)
    }
}
