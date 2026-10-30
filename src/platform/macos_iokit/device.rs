use std::{
    collections::VecDeque,
    ffi::c_void,
    sync::{
        atomic::{AtomicU8, AtomicUsize, Ordering},
        Arc, Mutex, MutexGuard, PoisonError,
    },
    task::{Context, Poll},
    time::Duration,
};

use io_kit_sys::ret::{
    kIOReturnBadArgument, kIOReturnBusy, kIOReturnExclusiveAccess, kIOReturnNoDevice,
    kIOReturnNotFound, kIOReturnNotOpen, kIOReturnNotPermitted, kIOReturnNotPrivileged,
    kIOReturnSuccess, kIOReturnUnsupported, IOReturn,
};
use log::{debug, error};

use crate::{
    bitset::EndpointBitSet,
    descriptors::{ConfigurationDescriptor, DeviceDescriptor, EndpointDescriptor},
    maybe_future::blocking::Blocking,
    transfer::{
        internal::{
            notify_completion, take_completed_from_queue, Idle, Notify, Pending, TransferFuture,
        },
        Buffer, Completion, ControlIn, ControlOut, Direction, TransferError,
    },
    DeviceInfo, Error, ErrorKind, MaybeFuture, Speed,
};

use super::{
    enumeration::{device_descriptor_from_fields, get_integer_property, service_by_registry_id},
    events::{add_event_source, EventRegistration},
    iokit::{call_iokit_function, IoService},
    iokit_c::IOUSBDevRequestTO,
    iokit_usb::{IoKitDevice, IoKitInterface},
    TransferData,
};

struct DeviceConnection {
    _event_registration: EventRegistration,
    device: IoKitDevice,
    is_open_exclusive: Mutex<bool>,
    captured: bool,
}

impl DeviceConnection {
    fn new(service: &IoService, capture: bool) -> Result<Self, Error> {
        if capture && unsafe { libc::geteuid() } != 0 {
            service.authorize().map_err(|e| {
                let kind = if e == kIOReturnNoDevice {
                    ErrorKind::Disconnected
                } else {
                    ErrorKind::PermissionDenied
                };
                Error::new_os(
                    kind,
                    "failed to authorize USB device; capture requires root or the com.apple.vm.device-access entitlement",
                    e,
                )
            })?;
        }

        // IOKit caches authorization when the user client is created.
        let device = IoKitDevice::new(service)?;
        let event_source = device
            .create_async_event_source()
            .map_err(|e| iokit_error(e, "failed to create async event source").log_error())?;

        let mut connection = Self {
            _event_registration: add_event_source(event_source),
            device,
            is_open_exclusive: Mutex::new(false),
            captured: false,
        };
        if capture {
            connection
                .device
                .reenumerate_capture()
                .map_err(|e| iokit_error(e, "failed to capture USB device"))?;
            connection.captured = true;
            connection
                .open()
                .map_err(|e| iokit_error(e, "failed to reopen USB device after capture"))?;
        } else if let Err(err) = connection.open() {
            log::debug!("Could not open device for exclusive access: 0x{err:08x}");
        }
        Ok(connection)
    }

    fn open(&self) -> Result<(), IOReturn> {
        let mut is_open_exclusive = self.is_open_exclusive.lock().unwrap();
        if !*is_open_exclusive {
            self.device.open()?;
            *is_open_exclusive = true;
        }
        Ok(())
    }

    fn release_capture(&mut self) -> Result<(), Error> {
        self.device
            .reenumerate_release()
            .map_err(|e| iokit_error(e, "failed to release captured USB device"))?;
        self.captured = false;
        *self
            .is_open_exclusive
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner) = false;
        Ok(())
    }
}

impl Drop for DeviceConnection {
    fn drop(&mut self) {
        if self.captured {
            if let Err(err) = self.release_capture() {
                log::error!("Failed to release captured device: {err}");
            }
        }
        if *self
            .is_open_exclusive
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
        {
            if let Err(err) = self.device.close() {
                // Harmless: releasing the user client when `device` drops closes the session.
                log::debug!("Failed to close device: {err:x}");
            }
        }
    }
}

pub(crate) struct MacDevice {
    connection: DeviceConnection,
    operation_lock: Mutex<()>,
    device_descriptor: DeviceDescriptor,
    config_descriptors: Vec<Vec<u8>>,
    speed: Option<Speed>,
    active_config: AtomicU8,
    claimed_interfaces: AtomicUsize,
}

// `get_configuration` does IO, so avoid it in the common case that:
//    * the device has a single configuration
//    * the device has at least one interface, indicating that it is configured
fn guess_active_config(configs: &[Vec<u8>], dev: &IoKitDevice) -> Option<u8> {
    if configs.len() != 1 {
        return None;
    }
    let mut intf = dev.create_interface_iterator().ok()?;
    intf.next()?;
    configs[0].get(5).copied() // get bConfigurationValue from descriptor
}

#[allow(non_upper_case_globals)]
fn iokit_error(code: IOReturn, message: &'static str) -> Error {
    let kind = match code {
        kIOReturnNoDevice | kIOReturnNotOpen => ErrorKind::Disconnected,
        kIOReturnBusy | kIOReturnExclusiveAccess => ErrorKind::Busy,
        kIOReturnNotPermitted | kIOReturnNotPrivileged => ErrorKind::PermissionDenied,
        kIOReturnNotFound => ErrorKind::NotFound,
        kIOReturnUnsupported => ErrorKind::Unsupported,
        _ => ErrorKind::Other,
    };

    Error::new_os(kind, message, code)
}

impl MacDevice {
    pub(crate) fn from_device_info(
        d: &DeviceInfo,
    ) -> impl MaybeFuture<Output = Result<Arc<MacDevice>, Error>> {
        Self::open(d.registry_id, d.speed, false)
    }

    /// Capture the entire device before returning a handle; requires root or device-access rights.
    pub(crate) fn from_device_info_captured(
        d: &DeviceInfo,
    ) -> impl MaybeFuture<Output = Result<Arc<MacDevice>, Error>> {
        Self::open(d.registry_id, d.speed, true)
    }

    fn open(
        registry_id: u64,
        speed: Option<Speed>,
        capture: bool,
    ) -> impl MaybeFuture<Output = Result<Arc<MacDevice>, Error>> {
        Blocking::new(move || {
            log::info!("Opening device from registry id {}", registry_id);
            let service = service_by_registry_id(registry_id)?;
            let connection = DeviceConnection::new(&service, capture)?;

            let device_descriptor = device_descriptor_from_fields(&service).ok_or_else(|| {
                Error::new(
                    ErrorKind::Other,
                    "could not read properties for device descriptor",
                )
            })?;

            let num_configs = connection
                .device
                .get_number_of_configurations()
                .map_err(|e| iokit_error(e, "failed to get number of configurations"))?;

            let config_descriptors: Vec<Vec<u8>> = (0..num_configs)
                .flat_map(|i| {
                    let d = connection
                        .device
                        .get_configuration_descriptor(i)
                        .inspect_err(|e| {
                            log::warn!("failed to get configuration descriptor {i}: {e}");
                        })
                        .ok()?;

                    ConfigurationDescriptor::new(&d).is_some().then_some(d)
                })
                .map(|desc| desc.to_owned())
                .collect();

            let active_config = if let Some(active_config) =
                guess_active_config(&config_descriptors, &connection.device)
            {
                log::debug!("Active config from single descriptor is {}", active_config);
                active_config
            } else {
                let res = connection.device.get_configuration();
                log::debug!("Active config from request is {:?}", res);
                res.unwrap_or(0)
            };

            Ok(Arc::new(MacDevice {
                connection,
                operation_lock: Mutex::new(()),
                device_descriptor,
                config_descriptors,
                speed,
                active_config: AtomicU8::new(active_config),
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
    ) -> impl Iterator<Item = ConfigurationDescriptor<'_>> {
        self.config_descriptors
            .iter()
            .map(|d| ConfigurationDescriptor::new_unchecked(&d[..]))
    }

    /// Release capture only when no other device, interface, or transfer retains this handle.
    pub(crate) fn release_capture(self: Arc<Self>) -> impl MaybeFuture<Output = Result<(), Error>> {
        Blocking::new(move || {
            if !self.connection.captured {
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    "device was not opened with capture",
                ));
            }
            // Interfaces and pending control transfers retain this same Arc.
            let mut device = Arc::try_unwrap(self).map_err(|_| {
                Error::new(
                    ErrorKind::Busy,
                    "device still has live handles or transfers",
                )
            })?;
            device.connection.release_capture()
        })
    }

    fn require_open_exclusive(&self) -> Result<MutexGuard<'_, ()>, Error> {
        let guard = self.operation_lock.lock().unwrap();
        if self.claimed_interfaces.load(Ordering::Acquire) != 0 {
            return Err(Error::new(
                ErrorKind::Busy,
                "cannot perform this operation while interfaces are claimed",
            ));
        }
        self.connection
            .open()
            .map_err(|e| iokit_error(e, "could not open device for exclusive access"))?;
        Ok(guard)
    }

    pub(crate) fn set_configuration(
        self: Arc<Self>,
        configuration: u8,
    ) -> impl MaybeFuture<Output = Result<(), Error>> {
        Blocking::new(move || {
            let _guard = self.require_open_exclusive()?;
            self.connection
                .device
                .set_configuration(configuration)
                .map_err(|e| iokit_error(e, "failed to set configuration"))?;
            log::debug!("Set configuration {configuration}");
            self.active_config.store(configuration, Ordering::SeqCst);
            Ok(())
        })
    }

    pub(crate) fn reset(self: Arc<Self>) -> impl MaybeFuture<Output = Result<(), Error>> {
        Blocking::new(move || {
            let _guard = self.require_open_exclusive()?;
            self.connection
                .device
                .reset()
                .map_err(|e| iokit_error(e, "failed to reset device"))
        })
    }

    pub(crate) fn claim_interface(
        self: Arc<Self>,
        interface_number: u8,
    ) -> impl MaybeFuture<Output = Result<Arc<MacInterface>, Error>> {
        Blocking::new(move || {
            let _guard = self.operation_lock.lock().unwrap();
            let intf_service = self
                .connection
                .device
                .create_interface_iterator()
                .map_err(|e| iokit_error(e, "failed to create interface iterator"))?
                .find(|io_service| {
                    get_integer_property(io_service, "bInterfaceNumber")
                        == Some(interface_number as i64)
                })
                .ok_or(Error::new(ErrorKind::NotFound, "interface not found"))?;

            let mut interface = IoKitInterface::new(intf_service)?;
            let source = interface
                .create_async_event_source()
                .map_err(|e| iokit_error(e, "failed to create async event source").log_error())?;
            let _event_registration = add_event_source(source);

            interface
                .open()
                .map_err(|e| iokit_error(e, "failed to open interface"))?;
            self.claimed_interfaces.fetch_add(1, Ordering::Release);

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
        self: Arc<Self>,
        data: ControlIn,
        timeout: Duration,
    ) -> impl MaybeFuture<Output = Result<Vec<u8>, TransferError>> {
        let timeout = timeout.as_millis().try_into().expect("timeout too long");

        let mut t = TransferData::new();
        t.put_buffer(Buffer::new(data.length as usize), Direction::In);

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

        TransferFuture::new(self, t, |dev, t| dev.submit_control(Direction::In, t, req)).map(
            |mut t| {
                let c = unsafe { t.take_completion(Direction::In) };
                c.status?;
                Ok(c.buffer.into_vec())
            },
        )
    }

    pub fn control_out(
        self: Arc<Self>,
        data: ControlOut,
        timeout: Duration,
    ) -> impl MaybeFuture<Output = Result<(), TransferError>> {
        let timeout = timeout.as_millis().try_into().expect("timeout too long");

        let mut t = TransferData::new();
        t.put_buffer(Buffer::from(data.data), Direction::Out);

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

        TransferFuture::new(self, t, |dev, t| dev.submit_control(Direction::Out, t, req)).map(
            |mut t| {
                let c = unsafe { t.take_completion(Direction::Out) };
                c.status
            },
        )
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
                self.connection.device.raw,
                DeviceRequestAsyncTO(&mut req, Some(transfer_callback), ptr as *mut c_void)
            )
        };

        if res == kIOReturnSuccess {
            debug!("Submitted control {dir:?} {ptr:?}");
        } else {
            error!("Failed to submit control {dir:?} {ptr:?}: {res:x}");
            unsafe {
                // Complete the transfer in the place of the callback
                (*ptr).status = res;
                notify_completion::<TransferData>(ptr);
            }
        }

        t
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
                return Err(Error::new(
                    ErrorKind::Busy,
                    "can't change alternate setting while endpoints are in use",
                ));
            }

            self.interface
                .set_alternate_interface(alt_setting)
                .map_err(|e| iokit_error(e, "failed to set alternate interface"))?;

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
        self.device.clone().control_in(data, timeout)
    }

    pub fn control_out(
        self: &Arc<Self>,
        data: ControlOut,
        timeout: Duration,
    ) -> impl MaybeFuture<Output = Result<(), TransferError>> {
        self.device.clone().control_out(data, timeout)
    }

    pub fn endpoint(
        self: &Arc<Self>,
        descriptor: EndpointDescriptor,
    ) -> Result<MacEndpoint, Error> {
        let address = descriptor.address();
        let max_packet_size = descriptor.max_packet_size();

        let mut state = self.state.lock().unwrap();

        let Some(pipe_ref) = self.interface.find_pipe_ref(address) else {
            debug!("Endpoint {address:02X} not found in iokit");
            return Err(Error::new(
                ErrorKind::NotFound,
                "specified endpoint does not exist on IOKit interface",
            ));
        };

        if state.endpoints_used.is_set(address) {
            return Err(Error::new(ErrorKind::Busy, "endpoint already in use"));
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

    pub fn release(self: Arc<Self>) -> impl MaybeFuture<Output = Result<(), Error>> {
        Blocking::new(move || {
            if let Some(this) = Arc::into_inner(self) {
                // USBInterfaceClose only fails on disconnected devices, but we'll ignore
                // those errors to match the behavior of other platforms.
                drop(this);
                Ok(())
            } else {
                return Err(Error::new(ErrorKind::Busy, "interface is still in use"));
            }
        })
    }
}

impl Drop for MacInterface {
    fn drop(&mut self) {
        // Expected to fail with kIOReturnNoDevice when the device is disconnected and can be ignored
        if let Err(err) = self.interface.close() {
            debug!("Failed to close interface: {err:x}")
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

    fn make_transfer(&mut self, buffer: Buffer) -> Idle<TransferData> {
        let mut transfer = self
            .idle_transfer
            .take()
            .unwrap_or_else(|| Idle::new(self.inner.clone(), TransferData::new()));
        transfer.put_buffer(buffer, Direction::from_address(self.inner.address));
        transfer
    }

    pub(crate) fn submit(&mut self, buffer: Buffer) {
        let transfer = self.make_transfer(buffer);
        let endpoint = self.inner.address;
        let dir = Direction::from_address(endpoint);
        let req_len = transfer.requested_len;
        let buf_ptr = transfer.buf;

        let transfer = transfer.pre_submit();
        let ptr = transfer.as_ptr();

        let res = unsafe {
            match dir {
                Direction::Out => call_iokit_function!(
                    self.inner.interface.interface.raw,
                    WritePipeAsync(
                        self.inner.pipe_ref,
                        buf_ptr as *mut c_void,
                        req_len,
                        Some(transfer_callback),
                        ptr as *mut c_void
                    )
                ),
                Direction::In => call_iokit_function!(
                    self.inner.interface.interface.raw,
                    ReadPipeAsync(
                        self.inner.pipe_ref,
                        buf_ptr as *mut c_void,
                        req_len,
                        Some(transfer_callback),
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
            error!("Failed to submit {dir:?} transfer {ptr:?} of len {req_len} on endpoint {endpoint:02X}: {res:x}");
            unsafe {
                // Complete the transfer in the place of the callback
                (*ptr).status = res;
                notify_completion::<TransferData>(ptr);
            }
        }

        self.pending.push_back(transfer);
    }

    pub(crate) fn submit_err(&mut self, buffer: Buffer, err: TransferError) {
        assert_eq!(err, TransferError::InvalidArgument);
        let mut transfer = self.make_transfer(buffer);
        transfer.status = kIOReturnBadArgument;
        self.pending.push_back(transfer.simulate_complete());
    }

    pub(crate) fn poll_next_complete(&mut self, cx: &mut Context) -> Poll<Completion> {
        self.inner.notify.subscribe(cx);
        if let Some(mut transfer) = take_completed_from_queue(&mut self.pending) {
            let dir = Direction::from_address(self.inner.address);
            let completion = unsafe { transfer.take_completion(dir) };
            self.idle_transfer = Some(transfer);
            Poll::Ready(completion)
        } else {
            Poll::Pending
        }
    }

    pub(crate) fn wait_next_complete(&mut self, timeout: Duration) -> Option<Completion> {
        self.inner.notify.wait_timeout(timeout, || {
            take_completed_from_queue(&mut self.pending).map(|mut transfer| {
                let dir = Direction::from_address(self.inner.address);
                let completion = unsafe { transfer.take_completion(dir) };
                self.idle_transfer = Some(transfer);
                completion
            })
        })
    }

    pub(crate) fn clear_halt(&mut self) -> impl MaybeFuture<Output = Result<(), Error>> {
        let inner = self.inner.clone();
        Blocking::new(move || {
            debug!("Clear halt, endpoint {:02x}", inner.address);

            inner
                .interface
                .interface
                .clear_pipe_stall_both_ends(inner.pipe_ref)
                .map_err(|e| iokit_error(e, "failed to clear halt on endpoint"))
        })
    }
}

impl Drop for MacEndpoint {
    fn drop(&mut self) {
        if !self.pending.is_empty() {
            debug!(
                "Dropping endpoint {:02x} with {} pending transfers",
                self.inner.address,
                self.pending.len()
            );
            self.cancel_all();
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "smol")]
    use futures_lite::future::block_on;

    #[test]
    fn test_capture_open_missing_device() {
        let failure = MacDevice::open(u64::MAX, None, true)
            .wait()
            .err()
            .expect("a missing registry entry must not open a device");
        assert_eq!(failure.kind(), ErrorKind::NotFound);

        #[cfg(feature = "smol")]
        {
            let failure = block_on(async { MacDevice::open(u64::MAX, None, true).await })
                .err()
                .expect("asynchronous capture must report the missing registry entry");
            assert_eq!(failure.kind(), ErrorKind::NotFound);
        }
    }
}
