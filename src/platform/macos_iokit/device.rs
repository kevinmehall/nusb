//! macOS (IOKit) device backend.
//!
//! # Testing the kernel-driver capture / detach paths
//!
//! The capture operations ([`MacDevice::set_configuration_captured`],
//! [`MacDevice::reset_captured`], [`MacDevice::attach_kernel_driver`]) detach
//! kernel drivers and are privileged. There are two ways to be allowed:
//!
//! * **Root** (e.g. under `sudo`): always works, no entitlement needed.
//! * **The `com.apple.vm.device-access` entitlement**: the non-root path;
//!   `IOServiceAuthorize` then grants the entitled process access.
//!
//! Two things trip up local testing of the *entitlement* path:
//!
//! 1. **Run from a code-signed `.app` bundle, not a bare CLI binary.** The
//!    authorization is bound to an application identity; a plain executable has
//!    none, so `IOServiceAuthorize` returns `kIOReturnAborted` with no prompt.
//!    Wrapping the binary in a minimal `.app` (with a `CFBundleIdentifier`) and
//!    code-signing that bundle with the entitlement resolves it.
//!
//! 2. **The entitlement has to actually be honored by the OS.** Apple only
//!    grants `com.apple.vm.device-access` to approved developer accounts (it is
//!    not self-serve). For local development you can instead relax AMFI's code
//!    signing enforcement so an ad-hoc signature's embedded entitlement is
//!    trusted: disable SIP (`csrutil disable`) and set the
//!    `amfi_get_out_of_my_way=1` boot-arg, then reboot. This is a development-
//!    only measure -- undo it (re-enable SIP, clear the boot-arg) afterward.

use std::{
    collections::VecDeque,
    ffi::c_void,
    sync::{
        atomic::{AtomicU8, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    task::{Context, Poll},
    time::Duration,
};

use io_kit_sys::{
    kIOServiceInteractionAllowed,
    ret::{kIOReturnSuccess, IOReturn},
};
use log::{debug, error};

use crate::{
    bitset::EndpointBitSet,
    descriptors::{ConfigurationDescriptor, DeviceDescriptor, EndpointDescriptor},
    maybe_future::blocking::Blocking,
    platform::macos_iokit::iokit_c::kUSBReEnumerateCaptureDeviceMask,
    transfer::{
        internal::{
            notify_completion, take_completed_from_queue, Idle, Notify, Pending, TransferFuture,
        },
        Buffer, Completion, ControlIn, ControlOut, Direction, TransferError,
    },
    DeviceInfo, Error, ErrorKind, InterfaceDriver, MaybeFuture, Speed,
};

use super::{
    enumeration::{
        device_descriptor_from_fields, get_integer_property, interface_driver,
        service_by_registry_id,
    },
    events::{add_event_source, EventRegistration},
    iokit::{call_iokit_function, ioservice_authorize, IoService},
    iokit_c::IOUSBDevRequestTO,
    iokit_usb::{IoKitDevice, IoKitInterface},
    TransferData,
};

pub(crate) struct MacDevice {
    _event_registration: EventRegistration,
    /// The IOKit service backing this device, retained for operations (such as
    /// capturing the device) that require re-opening it after authorization.
    service: IoService,
    pub(super) device: IoKitDevice,
    device_descriptor: DeviceDescriptor,
    config_descriptors: Vec<Vec<u8>>,
    speed: Option<Speed>,
    active_config: AtomicU8,
    is_open_exclusive: Mutex<bool>,
    claimed_interfaces: AtomicUsize,
    /// A device interface that has been authorized for capture (via
    /// `IOServiceAuthorize`), cached so repeated capture operations don't
    /// re-authorize -- each `IOServiceAuthorize` prompts the user.
    capture: Mutex<Option<CaptureHandle>>,
}

/// An `IOServiceAuthorize`-authorized device interface, held so the
/// authorization carries across capture operations. Created fresh *after*
/// authorizing so IOKit's `start()` picks up the refreshed authorization.
struct CaptureHandle {
    device: IoKitDevice,
    opened: bool,
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
            let event_source = device.create_async_event_source().map_err(|e| {
                Error::new_os(ErrorKind::Other, "failed to create async event source", e)
                    .log_error()
            })?;
            let _event_registration = add_event_source(event_source);

            let opened = device
                .open()
                .inspect_err(|err| {
                    log::debug!("Could not open device for exclusive access: 0x{err:08x}");
                })
                .is_ok();

            let device_descriptor = device_descriptor_from_fields(&service).ok_or_else(|| {
                Error::new(
                    ErrorKind::Other,
                    "could not read properties for device descriptor",
                )
            })?;

            let num_configs = device.get_number_of_configurations().map_err(|e| {
                Error::new_os(
                    ErrorKind::Other,
                    "failed to get number of configurations",
                    e,
                )
            })?;

            let config_descriptors: Vec<Vec<u8>> = (0..num_configs)
                .flat_map(|i| {
                    let d = device
                        .get_configuration_descriptor(i)
                        .inspect_err(|e| {
                            log::warn!("failed to get configuration descriptor {i}: {e}");
                        })
                        .ok()?;

                    ConfigurationDescriptor::new(&d).is_some().then_some(d)
                })
                .map(|desc| desc.to_owned())
                .collect();

            let active_config =
                if let Some(active_config) = guess_active_config(&config_descriptors, &device) {
                    log::debug!("Active config from single descriptor is {}", active_config);
                    active_config
                } else {
                    let res = device.get_configuration();
                    log::debug!("Active config from request is {:?}", res);
                    res.unwrap_or(0)
                };

            Ok(Arc::new(MacDevice {
                _event_registration,
                service,
                device,
                device_descriptor,
                config_descriptors,
                speed,
                active_config: AtomicU8::new(active_config),
                is_open_exclusive: Mutex::new(opened),
                claimed_interfaces: AtomicUsize::new(0),
                capture: Mutex::new(None),
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

    fn require_open_exclusive(&self) -> Result<(), Error> {
        let mut is_open_exclusive = self.is_open_exclusive.lock().unwrap();
        if !*is_open_exclusive {
            self.device.open().map_err(|e| match e {
                io_kit_sys::ret::kIOReturnNoDevice => {
                    Error::new_os(ErrorKind::Disconnected, "device disconnected", e)
                }
                _ => Error::new_os(
                    ErrorKind::Other,
                    "could not open device for exclusive access",
                    e,
                ),
            })?;
            *is_open_exclusive = true;
        }

        if self.claimed_interfaces.load(Ordering::Relaxed) != 0 {
            return Err(Error::new(
                ErrorKind::Busy,
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
            self.device
                .set_configuration(configuration)
                .map_err(|e| match e {
                    io_kit_sys::ret::kIOReturnNoDevice => {
                        Error::new_os(ErrorKind::Disconnected, "device disconnected", e)
                    }
                    io_kit_sys::ret::kIOReturnNotFound => {
                        Error::new_os(ErrorKind::NotFound, "configuration not found", e)
                    }
                    _ => Error::new_os(ErrorKind::Other, "failed to set configuration", e),
                })?;
            log::debug!("Set configuration {configuration}");
            self.active_config.store(configuration, Ordering::SeqCst);
            Ok(())
        })
    }

    pub(crate) fn reset(self: Arc<Self>) -> impl MaybeFuture<Output = Result<(), Error>> {
        Blocking::new(move || {
            self.require_open_exclusive()?;
            self.device.reset().map_err(|e| match e {
                io_kit_sys::ret::kIOReturnNoDevice => {
                    Error::new_os(ErrorKind::Disconnected, "device disconnected", e)
                }
                _ => Error::new_os(ErrorKind::Other, "failed to reset device", e),
            })
        })
    }

    /// Set the configuration, detaching any kernel drivers from the device's
    /// interfaces ("capturing" the device) in the process.
    pub(crate) fn set_configuration_captured(
        self: Arc<Self>,
        configuration: u8,
    ) -> impl MaybeFuture<Output = Result<(), Error>> {
        Blocking::new(move || {
            // `SetConfigurationV2` operates on an open device, so the captured
            // handle must be opened first.
            self.with_capture_authorization(CaptureAccess::Open, |device| {
                // `startInterfaceMatching = false` detaches kernel drivers;
                // `issueRemoteWakeup = true` matches the plain `SetConfiguration` behavior.
                let r = unsafe {
                    call_iokit_function!(device.raw, SetConfigurationV2(configuration, false, true))
                };
                check_capture_return(r, "failed to set configuration")?;

                log::debug!("Set configuration {configuration} (captured)");
                self.active_config.store(configuration, Ordering::SeqCst);
                Ok(())
            })
        })
    }

    /// Reset the device, detaching any kernel drivers from its interfaces
    /// ("capturing" the device) as it re-enumerates.
    pub(crate) fn reset_captured(self: Arc<Self>) -> impl MaybeFuture<Output = Result<(), Error>> {
        Blocking::new(move || {
            // `USBDeviceReEnumerate` works on an unopened interface (this is how
            // libusb captures), so the captured handle need not be opened.
            self.with_capture_authorization(CaptureAccess::NoOpen, |device| {
                let r = unsafe {
                    call_iokit_function!(
                        device.raw,
                        USBDeviceReEnumerate(kUSBReEnumerateCaptureDeviceMask)
                    )
                };
                check_capture_return(r, "failed to reset device")
            })
        })
    }

    /// Run `f` with a device handle authorized to capture (detach kernel
    /// drivers from) the device. Capturing requires either running as root or
    /// the `com.apple.vm.device-access` entitlement.
    ///
    /// Root is preferred when available: it needs no authorization (and so no
    /// user prompt), even for a signed/entitled binary that would otherwise be
    /// stopped at `IOServiceAuthorize`.
    ///
    /// The entitlement path mirrors libusb's `darwin_detach_kernel_driver`: call
    /// `IOServiceAuthorize` *before* touching the device, then obtain a *fresh*
    /// device interface so IOKit's `start()` re-runs with the refreshed
    /// authorization. (Opening the device before authorizing would fail, so we
    /// can't go through `require_open_exclusive` first here.) Each
    /// `IOServiceAuthorize` prompts the user, so the authorized interface is
    /// cached in `self.capture` and reused by later capture operations.
    ///
    /// `access` governs whether the authorized handle is opened before `f`:
    /// `USBDeviceReEnumerate` works on an unopened interface, but
    /// `SetConfigurationV2` requires an open device.
    ///
    /// # Which device interface is used
    ///
    /// Capture operates on one of two interfaces to the same device, chosen by
    /// privilege, and the two are never used together:
    ///
    /// * **root** -> the existing `self.device` (already opened by
    ///   `require_open_exclusive`). No second handle is created, so there is no
    ///   risk of a conflicting second exclusive `USBDeviceOpen`.
    /// * **entitled (non-root)** -> the cached `self.capture` handle. In this
    ///   case `self.device` is not open (a non-root open of a driver-owned
    ///   device fails during enumeration), so again there is no exclusive-open
    ///   conflict.
    ///
    /// The handles do not invalidate one another. The one exception is
    /// [`MacDevice::reset_captured`], whose `USBDeviceReEnumerate` re-enumerates
    /// the device: that invalidates *every* handle we hold (`self.device`,
    /// `self.service`, and the cached capture handle) at once. Per the public
    /// `Device::reset*` contract the `Device` must then be dropped and re-opened;
    /// a capture call made afterward operates on a stale handle and fails
    /// cleanly with a disconnected error rather than misbehaving. Operations
    /// that do not re-enumerate (`set_configuration_captured`,
    /// `attach_kernel_driver`) leave the handles valid and are safe to repeat.
    fn with_capture_authorization<F, R>(&self, access: CaptureAccess, f: F) -> Result<R, Error>
    where
        F: FnOnce(&IoKitDevice) -> Result<R, Error>,
    {
        // Capturing re-enumerates/reconfigures the whole device, so it can't be
        // done while we hold interfaces claimed.
        if self.claimed_interfaces.load(Ordering::Relaxed) != 0 {
            return Err(Error::new(
                ErrorKind::Busy,
                "cannot capture the device while interfaces are claimed",
            ));
        }

        // `geteuid` is declared directly rather than pulling in the `libc` crate
        // for a single call; it's a stable libSystem symbol.
        extern "C" {
            fn geteuid() -> u32;
        }

        if unsafe { geteuid() == 0 } {
            // Root: no authorization needed. Operate on the existing interface,
            // opened exclusively as usual.
            self.require_open_exclusive()?;
            f(&self.device)
        } else if security::have_capture_entitlement() {
            let mut guard = self.capture.lock().unwrap();
            if guard.is_none() {
                check_capture_return(
                    ioservice_authorize(&self.service, kIOServiceInteractionAllowed),
                    "failed to authorize device access",
                )?;
                // Fresh interface so the authorization takes effect.
                *guard = Some(CaptureHandle {
                    device: IoKitDevice::new(&self.service)?,
                    opened: false,
                });
            }
            let handle = guard.as_mut().unwrap();
            if matches!(access, CaptureAccess::Open) && !handle.opened {
                let r = unsafe { call_iokit_function!(handle.device.raw, USBDeviceOpen()) };
                check_capture_return(r, "failed to open device for capture")?;
                handle.opened = true;
            }
            f(&handle.device)
        } else {
            // Neither root nor entitled. We must fail up front rather than
            // attempt the operation: unprivileged capture calls (e.g.
            // `USBDeviceReEnumerate` with the capture mask) don't error -- IOKit
            // returns `kIOReturnSuccess` but silently ignores the capture,
            // leaving the kernel drivers attached.
            Err(Error::new(
                ErrorKind::PermissionDenied,
                "operation not permitted (requires root or com.apple.vm.device-access entitlement)",
            ))
        }
    }

    pub(crate) fn interface_driver(
        self: Arc<Self>,
        interface_number: u8,
    ) -> Result<InterfaceDriver, Error> {
        let intf_service = find_interface_service(&self.device, interface_number)?;
        // Reads the registry live, so it reflects detach/attach done through
        // this device (and reports a captured interface as `Unbound`, not the
        // capturing process).
        Ok(interface_driver(&intf_service))
    }

    /// Re-register the kernel driver for a single interface (`RegisterDriver`).
    ///
    /// Note this is *not* the same as fully releasing a device captured by
    /// [`reset_captured`][Self::reset_captured]. `RegisterDriver` re-binds the
    /// driver to the interface, but the device stays in the "captured" mode
    /// that `USBDeviceReEnumerate(kUSBReEnumerateCaptureDeviceMask)` put it in
    /// -- so it remains reserved for privileged/authorized access and an
    /// unprivileged open keeps failing with `kIOReturnNoResources` until the
    /// capture is cleared. To fully release a captured device (re-attach its
    /// drivers *and* clear the capture, the way a physical re-plug would),
    /// re-enumerate it without the capture bit via [`reset`][Self::reset]
    /// (`USBDeviceReEnumerate(0)`), which is what libusb does on release.
    pub(crate) fn attach_kernel_driver(self: &Arc<Self>, interface_number: u8) -> Result<(), Error> {
        // `RegisterDriver` acts on the interface, and enumerating interfaces
        // does not require the device to be open.
        self.with_capture_authorization(CaptureAccess::NoOpen, |device| {
            let intf_service = find_interface_service(device, interface_number)?;
            let interface = IoKitInterface::new(intf_service)?;

            let r = unsafe { call_iokit_function!(interface.raw, RegisterDriver()) };
            check_capture_return(r, "failed to attach kernel driver")
        })
    }

    pub(crate) fn claim_interface(
        self: Arc<Self>,
        interface_number: u8,
    ) -> impl MaybeFuture<Output = Result<Arc<MacInterface>, Error>> {
        Blocking::new(move || {
            let intf_service = self
                .device
                .create_interface_iterator()
                .map_err(|e| {
                    Error::new_os(ErrorKind::Other, "failed to create interface iterator", e)
                })?
                .find(|io_service| {
                    get_integer_property(io_service, "bInterfaceNumber")
                        == Some(interface_number as i64)
                })
                .ok_or(Error::new(ErrorKind::NotFound, "interface not found"))?;

            let mut interface = IoKitInterface::new(intf_service)?;
            let source = interface.create_async_event_source().map_err(|e| {
                Error::new_os(ErrorKind::Other, "failed to create async event source", e)
                    .log_error()
            })?;
            let _event_registration = add_event_source(source);

            interface.open().map_err(|e| match e {
                io_kit_sys::ret::kIOReturnExclusiveAccess => Error::new_os(
                    ErrorKind::Busy,
                    "could not open interface for exclusive access",
                    e,
                ),
                io_kit_sys::ret::kIOReturnNoDevice => {
                    Error::new_os(ErrorKind::Disconnected, "device disconnected", e)
                }
                _ => Error::new_os(ErrorKind::Other, "failed to open interface", e),
            })?;
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

        TransferFuture::new(t, |t| self.submit_control(Direction::In, t, req)).map(move |mut t| {
            drop(self); // ensure device stays alive
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

        TransferFuture::new(t, |t| self.submit_control(Direction::Out, t, req)).map(move |mut t| {
            drop(self); // ensure device stays alive
            let c = unsafe { t.take_completion(Direction::Out) };
            c.status
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
                return Err(Error::new(
                    ErrorKind::Busy,
                    "can't change alternate setting while endpoints are in use",
                ));
            }

            self.interface
                .set_alternate_interface(alt_setting)
                .map_err(|e| match e {
                    io_kit_sys::ret::kIOReturnNoDevice => {
                        Error::new_os(ErrorKind::Disconnected, "device disconnected", e)
                    }
                    _ => Error::new_os(ErrorKind::Other, "failed to set alternate interface", e),
                })?;

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
        transfer.status = io_kit_sys::ret::kIOReturnBadArgument;
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
                .map_err(|e| match e {
                    io_kit_sys::ret::kIOReturnNoDevice => {
                        Error::new_os(ErrorKind::Disconnected, "device disconnected", e)
                    }
                    _ => Error::new_os(ErrorKind::Other, "failed to clear halt on endpoint", e),
                })
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

/// Whether a freshly authorized capture handle must be opened before use.
enum CaptureAccess {
    /// Open the device (required by `SetConfigurationV2`).
    Open,
    /// Leave the interface unopened (fine for `USBDeviceReEnumerate` and for
    /// operations that act on an interface rather than the device).
    NoOpen,
}

/// Find the IOKit service for the interface with the given `bInterfaceNumber`.
fn find_interface_service(device: &IoKitDevice, interface_number: u8) -> Result<IoService, Error> {
    device
        .create_interface_iterator()
        .map_err(|e| Error::new_os(ErrorKind::Other, "failed to create interface iterator", e))?
        .find(|io_service| {
            get_integer_property(io_service, "bInterfaceNumber") == Some(interface_number as i64)
        })
        .ok_or(Error::new(ErrorKind::NotFound, "interface not found"))
}

/// Map an `IOReturn` from a capture/detach operation to an `Error`, using
/// `message` for the fallback case.
fn check_capture_return(r: IOReturn, message: &'static str) -> Result<(), Error> {
    match r {
        io_kit_sys::ret::kIOReturnSuccess => Ok(()),
        io_kit_sys::ret::kIOReturnNotPermitted | io_kit_sys::ret::kIOReturnNotPrivileged => {
            Err(Error::new_os(
                ErrorKind::PermissionDenied,
                "operation not permitted (requires root or com.apple.vm.device-access entitlement)",
                r,
            ))
        }
        io_kit_sys::ret::kIOReturnNoDevice => {
            Err(Error::new_os(ErrorKind::Disconnected, "device disconnected", r))
        }
        io_kit_sys::ret::kIOReturnNotFound => {
            Err(Error::new_os(ErrorKind::NotFound, "not found", r))
        }
        _ => Err(Error::new_os(ErrorKind::Other, message, r)),
    }
}

pub(crate) mod security {
    use core_foundation::{
        base::{kCFAllocatorDefault, CFAllocatorRef, CFGetTypeID, CFRelease, CFTypeRef, TCFType},
        boolean::CFBoolean,
        error::CFErrorRef,
        number::CFBooleanGetValue,
        string::{CFString, CFStringRef},
    };

    #[repr(C)]
    #[derive(Debug, Copy, Clone)]
    pub struct __SecTask {
        _unused: [u8; 0],
    }
    pub type SecTaskRef = *mut __SecTask;

    #[link(name = "Security", kind = "framework")]
    extern "C" {
        pub fn SecTaskCreateFromSelf(allocator: CFAllocatorRef) -> SecTaskRef;
        pub fn SecTaskCopyValueForEntitlement(
            task: SecTaskRef,
            entitlement: CFStringRef,
            error: *mut CFErrorRef,
        ) -> CFTypeRef;
    }

    pub(crate) fn have_capture_entitlement() -> bool {
        have_entitlement("com.apple.vm.device-access")
    }

    pub fn have_entitlement(entitlement: &'static str) -> bool {
        unsafe {
            let task = SecTaskCreateFromSelf(kCFAllocatorDefault);
            if task.is_null() {
                return false;
            }

            let value = SecTaskCopyValueForEntitlement(
                task,
                CFString::from_static_string(entitlement).as_concrete_TypeRef(),
                std::ptr::null_mut(),
            );

            CFRelease(task.cast());

            let entitled = !value.is_null()
                && CFGetTypeID(value.cast()) == CFBoolean::type_id()
                && CFBooleanGetValue(value.cast());

            if !value.is_null() {
                CFRelease(value.cast());
            }

            entitled
        }
    }
}
