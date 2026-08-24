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

pub(super) struct DeviceConnection {
    _event_registration: EventRegistration,
    device: IoKitDevice,
    is_open_exclusive: Mutex<bool>,
}

impl DeviceConnection {
    fn new(service: &IoService) -> Result<Arc<Self>, Error> {
        let device = IoKitDevice::new(service)?;
        let event_source = device
            .create_async_event_source()
            .map_err(|e| iokit_error(e, "failed to create async event source").log_error())?;

        Ok(Arc::new(Self {
            _event_registration: add_event_source(event_source),
            device,
            is_open_exclusive: Mutex::new(false),
        }))
    }

    fn open(&self) -> Result<(), IOReturn> {
        let mut is_open_exclusive = self.is_open_exclusive.lock().unwrap();
        if !*is_open_exclusive {
            self.device.open()?;
            *is_open_exclusive = true;
        }
        Ok(())
    }

    fn mark_closed(&self) {
        *self.is_open_exclusive.lock().unwrap() = false;
    }
}

impl Drop for DeviceConnection {
    fn drop(&mut self) {
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

struct DeviceState {
    connection: Arc<DeviceConnection>,
    captured: bool,
}

pub(crate) struct MacDevice {
    registry_id: u64,
    state: Mutex<DeviceState>,
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

fn validate_capture_interface(
    configs: &[Vec<u8>],
    active_config: u8,
    interface: u8,
) -> Result<(), Error> {
    let config = configs
        .iter()
        .map(|d| ConfigurationDescriptor::new_unchecked(d))
        .find(|d| d.configuration_value() == active_config)
        .ok_or_else(|| Error::new(ErrorKind::NotFound, "active configuration not found"))?;

    if !config
        .interface_alt_settings()
        .any(|d| d.interface_number() == interface)
    {
        return Err(Error::new(ErrorKind::NotFound, "interface not found"));
    }

    Ok(())
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
        let registry_id = d.registry_id;
        let speed = d.speed;
        Blocking::new(move || {
            log::info!("Opening device from registry id {}", registry_id);
            let service = service_by_registry_id(registry_id)?;
            let connection = DeviceConnection::new(&service)?;

            if let Err(err) = connection.open() {
                log::debug!("Could not open device for exclusive access: 0x{err:08x}");
            }

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
                registry_id,
                state: Mutex::new(DeviceState {
                    connection,
                    captured: false,
                }),
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

    /// Capture the device (device-wide): terminate its kernel drivers so an interface claim can
    /// succeed. Needs root or the `com.apple.vm.device-access` entitlement.
    pub(crate) fn detach_kernel_driver(&self, interface: u8) -> Result<(), Error> {
        let mut state = self.state.lock().unwrap();

        validate_capture_interface(
            &self.config_descriptors,
            self.active_config.load(Ordering::SeqCst),
            interface,
        )?;

        if self.claimed_interfaces.load(Ordering::Acquire) != 0 {
            return Err(Error::new(
                ErrorKind::Busy,
                "cannot capture device while interfaces are claimed",
            ));
        }

        // Capture is device-wide, so repeated detach requests do not need another disruptive
        // re-enumeration.
        if state.captured {
            return Ok(());
        }

        // The service is gone once a release has re-enumerated the device.
        let service = service_by_registry_id(self.registry_id).map_err(|e| match e.kind() {
            ErrorKind::NotFound => Error::new(ErrorKind::Disconnected, "device disconnected"),
            _ => e,
        })?;
        if unsafe { libc::geteuid() } != 0 {
            service.authorize().map_err(|e| {
                iokit_error(
                    e,
                    "failed to authorize USB device; capture requires root or the com.apple.vm.device-access entitlement",
                )
            })?;
        }

        // Authorization is cached when the user client starts, so build a fresh user client after
        // IOServiceAuthorize. Creating its event source before capture also keeps callbacks valid
        // through the close-and-reopen performed by capture.
        let new_connection = DeviceConnection::new(&service)?;
        new_connection
            .device
            .reenumerate_capture()
            .map_err(|e| iokit_error(e, "failed to capture USB device"))?;

        match new_connection.open() {
            Ok(()) => {
                state.connection = new_connection;
                state.captured = true;
                Ok(())
            }
            Err(open_error) => {
                // The capture itself succeeded, so update the active connection even when the
                // reopen fails. If rollback also fails, preserve the captured state so a later
                // attach_kernel_driver call can retry the release.
                match new_connection.device.reenumerate_release() {
                    Ok(()) => {
                        new_connection.mark_closed();
                        state.connection = new_connection;
                        state.captured = false;
                        Err(iokit_error(
                            open_error,
                            "failed to reopen USB device after capture",
                        ))
                    }
                    Err(release_error) => {
                        log::error!(
                            "Failed to reopen captured device: 0x{open_error:08x}; rollback also failed: 0x{release_error:08x}"
                        );
                        state.connection = new_connection;
                        state.captured = true;
                        Err(iokit_error(
                            release_error,
                            "failed to release USB device after capture setup failed",
                        ))
                    }
                }
            }
        }
    }

    /// Release the device-wide capture. The interface number is intentionally ignored on macOS.
    pub(crate) fn attach_kernel_driver(&self, _interface: u8) -> Result<(), Error> {
        let mut state = self.state.lock().unwrap();

        if self.claimed_interfaces.load(Ordering::Acquire) != 0 {
            return Err(Error::new(
                ErrorKind::Busy,
                "cannot release captured device while interfaces are claimed",
            ));
        }

        if !state.captured {
            return Err(Error::new(ErrorKind::Busy, "USB device is not captured"));
        }

        state
            .connection
            .device
            .reenumerate_release()
            .map_err(|e| iokit_error(e, "failed to release captured USB device"))?;
        state.connection.mark_closed();
        state.captured = false;
        Ok(())
    }

    fn require_open_exclusive(&self) -> Result<MutexGuard<'_, DeviceState>, Error> {
        let state = self.state.lock().unwrap();

        if self.claimed_interfaces.load(Ordering::Acquire) != 0 {
            return Err(Error::new(
                ErrorKind::Busy,
                "cannot perform this operation while interfaces are claimed",
            ));
        }

        state
            .connection
            .open()
            .map_err(|e| iokit_error(e, "could not open device for exclusive access"))?;

        Ok(state)
    }

    pub(crate) fn set_configuration(
        self: Arc<Self>,
        configuration: u8,
    ) -> impl MaybeFuture<Output = Result<(), Error>> {
        Blocking::new(move || {
            let state = self.require_open_exclusive()?;
            state
                .connection
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
            let state = self.require_open_exclusive()?;
            state
                .connection
                .device
                .reset()
                .map_err(|e| iokit_error(e, "failed to reset device"))
        })
    }

    pub(crate) fn claim_interface(
        self: Arc<Self>,
        interface_number: u8,
    ) -> impl MaybeFuture<Output = Result<Arc<MacInterface>, Error>> {
        self.claim_interface_inner(interface_number, false)
    }

    /// Claim an interface, `seize`ing it from a kernel driver if set.
    fn claim_interface_inner(
        self: Arc<Self>,
        interface_number: u8,
        seize: bool,
    ) -> impl MaybeFuture<Output = Result<Arc<MacInterface>, Error>> {
        Blocking::new(move || {
            // Holding the device state lock through the successful open and claim count update
            // prevents capture/release from racing the creation of a live interface.
            let state = self.state.lock().unwrap();
            let intf_service = state
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

            let opened = if seize {
                interface.open_seize()
            } else {
                interface.open()
            };
            opened.map_err(|e| iokit_error(e, "failed to open interface"))?;
            self.claimed_interfaces.fetch_add(1, Ordering::Release);
            drop(state);

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
        self.claim_interface_inner(interface, true)
    }

    pub fn control_in(
        self: Arc<Self>,
        data: ControlIn,
        timeout: Duration,
    ) -> impl MaybeFuture<Output = Result<Vec<u8>, TransferError>> {
        let timeout = timeout.as_millis().try_into().expect("timeout too long");

        let state = self.state.lock().unwrap();
        let connection = state.connection.clone();
        let mut t = TransferData::new();
        t.pin_device_connection(connection.clone());
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

        let transfer = TransferFuture::new(t, move |t| {
            Self::submit_control(&connection, Direction::In, t, req)
        });
        drop(state);

        transfer.map(move |mut t| {
            let c = unsafe { t.take_completion(Direction::In) };
            c.status?;
            Ok(c.buffer.into_vec())
        })
    }

    pub fn control_out(
        self: Arc<Self>,
        data: ControlOut,
        timeout: Duration,
    ) -> impl MaybeFuture<Output = Result<(), TransferError>> {
        let timeout = timeout.as_millis().try_into().expect("timeout too long");

        let state = self.state.lock().unwrap();
        let connection = state.connection.clone();
        let mut t = TransferData::new();
        t.pin_device_connection(connection.clone());
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

        let transfer = TransferFuture::new(t, move |t| {
            Self::submit_control(&connection, Direction::Out, t, req)
        });
        drop(state);

        transfer.map(move |mut t| {
            let c = unsafe { t.take_completion(Direction::Out) };
            c.status
        })
    }

    fn submit_control(
        connection: &DeviceConnection,
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
                connection.device.raw,
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

    fn configuration(value: u8, interface: u8) -> Vec<u8> {
        vec![
            9, 2, 18, 0, 1, value, 0, 0x80, 50, 9, 4, interface, 0, 0, 0xff, 0, 0, 0,
        ]
    }

    #[test]
    fn test_capture_validation_uses_only_the_active_configuration() {
        let configs = vec![configuration(1, 1), configuration(2, 2)];

        assert!(validate_capture_interface(&configs, 2, 2).is_ok());

        let inactive_interface = validate_capture_interface(&configs, 2, 1).unwrap_err();
        assert_eq!(
            inactive_interface.kind(),
            ErrorKind::NotFound,
            "an interface that exists only in an inactive configuration must be rejected"
        );
    }
}
