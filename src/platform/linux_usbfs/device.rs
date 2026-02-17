use std::{
    collections::{BTreeMap, VecDeque},
    ffi::c_void,
    fs::File,
    io::{Read, Seek},
    mem::ManuallyDrop,
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc, Mutex, MutexGuard, Weak,
    },
    task::{Context, Poll},
    time::{Duration, Instant},
};

use log::{debug, error, warn};
use rustix::{
    event::epoll::EventFlags,
    fd::{AsFd, AsRawFd, FromRawFd, OwnedFd},
    fs::Timespec,
    io::Errno,
    time::{timerfd_create, timerfd_settime, Itimerspec, TimerfdFlags, TimerfdTimerFlags},
};
use slab::Slab;

use super::{
    errno_to_transfer_error, events,
    usbfs::{self, Urb},
    TransferData,
};

#[cfg(not(target_os = "android"))]
use super::{
    enumeration::{SysfsError, SysfsErrorKind},
    SysfsPath,
};

use crate::{
    bitset::EndpointBitSet,
    descriptors::{
        parse_concatenated_config_descriptors, ConfigurationDescriptor, DeviceDescriptor,
        EndpointDescriptor, TransferType, DESCRIPTOR_LEN_DEVICE,
    },
    maybe_future::{blocking::Blocking, MaybeFuture},
    transfer::{
        internal::{
            notify_completion, take_completed_from_queue, Idle, Notify, Pending, TransferFuture,
        },
        request_type, Buffer, Completion, ControlIn, ControlOut, ControlType, Direction, Recipient,
        TransferError,
    },
    DeviceInfo, Error, ErrorKind, Speed,
};

#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct TimeoutEntry {
    deadline: Instant,
    urb: *mut Urb,
}

unsafe impl Send for TimeoutEntry {}
unsafe impl Sync for TimeoutEntry {}

static DEVICES: Mutex<Slab<Weak<LinuxDevice>>> = Mutex::new(Slab::new());

pub(crate) struct LinuxDevice {
    fd: OwnedFd,
    events_id: usize,

    /// Read from the fd, consists of device descriptor followed by configuration descriptors
    descriptors: Vec<u8>,

    #[cfg(not(target_os = "android"))]
    sysfs: Option<SysfsPath>,

    active_config: AtomicU8,

    timerfd: OwnedFd,
    timeouts: Mutex<BTreeMap<TimeoutEntry, ()>>,
}

impl LinuxDevice {
    #[cfg(not(target_os = "android"))]
    pub(crate) fn from_device_info(
        d: &DeviceInfo,
    ) -> impl MaybeFuture<Output = Result<Arc<LinuxDevice>, Error>> {
        use rustix::fs::{Mode, OFlags};

        let busnum = d.busnum();
        let devnum = d.device_address();
        let sysfs_path = d.path.clone();

        Blocking::new(move || {
            let path = std::path::PathBuf::from(format!("/dev/bus/usb/{busnum:03}/{devnum:03}"));
            let fd = rustix::fs::open(&path, OFlags::RDWR | OFlags::CLOEXEC, Mode::empty())
                .map_err(|e| {
                    match e {
                        Errno::NOENT => {
                            Error::new_os(ErrorKind::Disconnected, "device not found", e)
                        }
                        Errno::PERM => {
                            Error::new_os(ErrorKind::PermissionDenied, "permission denied", e)
                        }
                        e => Error::new_os(ErrorKind::Other, "failed to open device", e),
                    }
                    .log_debug()
                })?;

            Self::create_inner(fd, Some(sysfs_path))
        })
    }

    #[cfg(target_os = "android")]
    pub(crate) fn from_device_info(
        _d: &DeviceInfo,
    ) -> impl MaybeFuture<Output = Result<Arc<LinuxDevice>, Error>> {
        Blocking::new(move || unimplemented!())
    }

    pub(crate) fn from_fd(
        fd: OwnedFd,
    ) -> impl MaybeFuture<Output = Result<Arc<LinuxDevice>, Error>> {
        Blocking::new(move || {
            debug!("Wrapping fd {} as usbfs device", fd.as_raw_fd());
            Self::create_inner(
                fd,
                #[cfg(not(target_os = "android"))]
                None,
            )
        })
    }

    pub(crate) fn create_inner(
        fd: OwnedFd,
        #[cfg(not(target_os = "android"))] sysfs: Option<SysfsPath>,
    ) -> Result<Arc<LinuxDevice>, Error> {
        let descriptors = read_all_from_fd(&fd).map_err(|e| {
            Error::new_io(ErrorKind::Other, "failed to read descriptors", e).log_error()
        })?;

        let Some(_) = DeviceDescriptor::new(&descriptors) else {
            return Err(Error::new(ErrorKind::Other, "invalid device descriptor"));
        };

        #[cfg(not(target_os = "android"))]
        let active_config: u8 = if let Some(sysfs) = sysfs.as_ref() {
            match sysfs.read_attr("bConfigurationValue") {
                Ok(v) => v,
                // Linux returns an empty string when the device is unconfigured.
                // We'll assume all parse errors are the empty string.
                Err(SysfsError(_, SysfsErrorKind::Parse(_))) => 0,

                Err(e) => {
                    warn!("failed to read sysfs bConfigurationValue: {e}");
                    return Err(Error::new(
                        ErrorKind::Other,
                        "failed to read sysfs bConfigurationValue",
                    ));
                }
            }
        } else {
            guess_active_configuration(&fd, &descriptors)
        };

        #[cfg(target_os = "android")]
        let active_config = guess_active_configuration(&fd, &descriptors);

        let timerfd = timerfd_create(
            rustix::time::TimerfdClockId::Monotonic,
            TimerfdFlags::CLOEXEC | TimerfdFlags::NONBLOCK,
        )
        .map_err(|e| Error::new_os(ErrorKind::Other, "failed to create timerfd", e).log_error())?;

        let arc = Arc::new_cyclic(|weak| {
            let events_id = DEVICES.lock().unwrap().insert(weak.clone());
            LinuxDevice {
                fd,
                events_id,
                descriptors,
                #[cfg(not(target_os = "android"))]
                sysfs,
                active_config: AtomicU8::new(active_config),
                timerfd,
                timeouts: Mutex::new(BTreeMap::new()),
            }
        });

        debug!(
            "Opened device fd={} with id {}",
            arc.fd.as_raw_fd(),
            arc.events_id
        );

        events::register_fd(
            arc.fd.as_fd(),
            events::Tag::Device(arc.events_id),
            EventFlags::OUT,
        )?;

        events::register_fd(
            arc.timerfd.as_fd(),
            events::Tag::DeviceTimer(arc.events_id),
            EventFlags::IN,
        )?;

        Ok(arc)
    }

    pub(crate) fn handle_usb_epoll(id: usize) {
        let device = DEVICES.lock().unwrap().get(id).and_then(|w| w.upgrade());
        if let Some(device) = device {
            device.handle_events();
        }
    }

    fn handle_events(&self) {
        debug!("Handling events for device {}", self.events_id);
        match usbfs::reap_urb_ndelay(&self.fd) {
            Ok(urb) => {
                let transfer_data: *mut TransferData = unsafe { &(*urb) }.usercontext.cast();

                {
                    let transfer = unsafe { &*transfer_data };
                    debug_assert!(transfer.urb_ptr() == urb);
                    debug!(
                        "URB {:?} for ep {:x} completed, status={} actual_length={}",
                        transfer.urb_ptr(),
                        transfer.urb().endpoint,
                        transfer.urb().status,
                        transfer.urb().actual_length
                    );

                    if let Some(deadline) = transfer.deadline {
                        let mut timeouts = self.timeouts.lock().unwrap();
                        timeouts.remove(&TimeoutEntry { deadline, urb });
                        self.update_timeouts(timeouts, Instant::now());
                    }
                };

                // SAFETY: pointer came from submit via kernel and we're now done with it
                unsafe { notify_completion::<super::TransferData>(transfer_data) }
            }
            Err(Errno::AGAIN) => {}
            Err(Errno::NODEV) => {
                debug!("Device {} disconnected", self.events_id);

                // epoll returns events continuously on a disconnected device, and REAPURB
                // only returns ENODEV after all events are received, so unregister to
                // keep the event thread from spinning because we won't receive further events.
                // The drop impl will try to unregister again, but that's ok.
                events::unregister_fd(self.fd.as_fd());
            }
            Err(e) => {
                error!("Unexpected error {e} from REAPURBNDELAY");
            }
        }
    }

    pub(crate) fn handle_timer_epoll(id: usize) {
        let device = DEVICES.lock().unwrap().get(id).and_then(|w| w.upgrade());
        if let Some(device) = device {
            device.handle_timeouts();
        }
    }

    fn handle_timeouts(&self) {
        debug!("Handling timeouts for device {}", self.events_id);
        let now = Instant::now();

        rustix::io::read(self.timerfd.as_fd(), &mut [0u8; 8]).ok();

        let mut timeouts = self.timeouts.lock().unwrap();
        while let Some(entry) = timeouts.first_entry() {
            if entry.key().deadline > now {
                break;
            }

            let urb = entry.remove_entry().0.urb;

            unsafe {
                match usbfs::discard_urb(&self.fd, urb) {
                    Ok(()) => debug!("Cancelled URB {urb:?} after timeout"),
                    Err(e) => debug!("Failed to cancel timed out URB {urb:?}: {e}"),
                }
            }
        }

        self.update_timeouts(timeouts, now);
    }

    fn update_timeouts(&self, timeouts: MutexGuard<BTreeMap<TimeoutEntry, ()>>, now: Instant) {
        const TIMESPEC_ZERO: Timespec = Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };

        let next = if let Some((TimeoutEntry { deadline, .. }, _)) = timeouts.first_key_value() {
            let duration = deadline
                .checked_duration_since(now)
                .unwrap_or(Duration::from_nanos(1));
            log::debug!("Next timeout in {duration:?}");
            Timespec {
                tv_sec: duration.as_secs() as i64,
                tv_nsec: duration.subsec_nanos() as i64,
            }
        } else {
            TIMESPEC_ZERO
        };

        timerfd_settime(
            self.timerfd.as_fd(),
            TimerfdTimerFlags::empty(),
            &Itimerspec {
                it_interval: TIMESPEC_ZERO,
                it_value: next,
            },
        )
        .inspect_err(|e| {
            log::error!("Failed to set timerfd: {e}");
        })
        .ok();
    }

    pub(crate) fn device_descriptor(&self) -> DeviceDescriptor {
        DeviceDescriptor::new(&self.descriptors).unwrap()
    }

    pub(crate) fn configuration_descriptors(
        &self,
    ) -> impl Iterator<Item = ConfigurationDescriptor<'_>> {
        parse_concatenated_config_descriptors(&self.descriptors[DESCRIPTOR_LEN_DEVICE as usize..])
    }

    pub(crate) fn active_configuration_value(&self) -> u8 {
        #[cfg(not(target_os = "android"))]
        if let Some(sysfs) = self.sysfs.as_ref() {
            match sysfs.read_attr("bConfigurationValue") {
                Ok(v) => {
                    self.active_config.store(v, Ordering::SeqCst);
                    return v;
                }
                Err(SysfsError(_, SysfsErrorKind::Parse(_))) => {
                    self.active_config.store(0, Ordering::SeqCst);
                    return 0;
                }
                Err(e) => {
                    error!("Failed to read sysfs bConfigurationValue: {e}, using cached value");
                }
            }
        }
        self.active_config.load(Ordering::SeqCst)
    }

    pub(crate) fn set_configuration(
        self: Arc<Self>,
        configuration: u8,
    ) -> impl MaybeFuture<Output = Result<(), Error>> {
        Blocking::new(move || {
            usbfs::set_configuration(&self.fd, configuration).map_err(|e| match e {
                Errno::INVAL => Error::new_os(ErrorKind::NotFound, "configuration not found", e),
                Errno::BUSY => Error::new_os(ErrorKind::Busy, "device is busy", e),
                Errno::NODEV => Error::new_os(ErrorKind::Disconnected, "device disconnected", e),
                _ => Error::new_os(ErrorKind::Other, "failed to set configuration", e),
            })?;
            self.active_config.store(configuration, Ordering::SeqCst);
            Ok(())
        })
    }

    pub(crate) fn reset(self: Arc<Self>) -> impl MaybeFuture<Output = Result<(), Error>> {
        Blocking::new(move || {
            usbfs::reset(&self.fd).map_err(|e| match e {
                Errno::BUSY => Error::new_os(ErrorKind::Busy, "device is busy", e),
                Errno::NODEV => Error::new_os(ErrorKind::Disconnected, "device disconnected", e),
                _ => Error::new_os(ErrorKind::Other, "failed to reset device", e),
            })
        })
    }

    pub fn control_in(
        self: Arc<Self>,
        data: ControlIn,
        timeout: Duration,
    ) -> impl MaybeFuture<Output = Result<Vec<u8>, TransferError>> {
        let t = TransferData::new_control_in(data);
        TransferFuture::new(t, |t| self.submit_timeout(t, timeout)).map(move |t| {
            drop(self); // ensure device stays alive
            t.status()?;
            Ok(t.control_in_data().to_owned())
        })
    }

    pub fn control_out(
        self: Arc<Self>,
        data: ControlOut,
        timeout: Duration,
    ) -> impl MaybeFuture<Output = Result<(), TransferError>> {
        let t = TransferData::new_control_out(data);
        TransferFuture::new(t, |t| self.submit_timeout(t, timeout)).map(move |t| {
            drop(self); // ensure device stays alive
            t.status()?;
            Ok(())
        })
    }

    fn handle_claim_interface_result(
        self: Arc<Self>,
        interface_number: u8,
        result: Result<(), Errno>,
        reattach: bool,
    ) -> Result<Arc<LinuxInterface>, Error> {
        result.map_err(|e| {
            match e {
                Errno::INVAL => Error::new_os(ErrorKind::NotFound, "interface not found", e),
                Errno::BUSY => Error::new_os(ErrorKind::Busy, "interface is busy", e),
                Errno::NODEV => Error::new_os(ErrorKind::Disconnected, "device disconnected", e),
                _ => Error::new_os(ErrorKind::Other, "failed to claim interface", e),
            }
            .log_error()
        })?;
        debug!(
            "Claimed interface {interface_number} on device id {dev}",
            dev = self.events_id
        );
        Ok(Arc::new(LinuxInterface {
            device: self,
            interface_number,
            reattach,
            state: Mutex::new(Default::default()),
        }))
    }

    pub(crate) fn claim_interface(
        self: Arc<Self>,
        interface_number: u8,
    ) -> impl MaybeFuture<Output = Result<Arc<LinuxInterface>, Error>> {
        Blocking::new(move || {
            let result = usbfs::claim_interface(&self.fd, interface_number);
            self.handle_claim_interface_result(interface_number, result, false)
        })
    }

    pub(crate) fn detach_and_claim_interface(
        self: Arc<Self>,
        interface_number: u8,
    ) -> impl MaybeFuture<Output = Result<Arc<LinuxInterface>, Error>> {
        Blocking::new(move || {
            let result = usbfs::detach_and_claim_interface(&self.fd, interface_number);
            self.handle_claim_interface_result(interface_number, result, true)
        })
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn detach_kernel_driver(
        self: &Arc<Self>,
        interface_number: u8,
    ) -> Result<(), Error> {
        usbfs::detach_kernel_driver(&self.fd, interface_number).map_err(|e| match e {
            Errno::INVAL => Error::new_os(ErrorKind::NotFound, "interface not found", e),
            Errno::NODEV => Error::new_os(ErrorKind::Disconnected, "device disconnected", e),
            Errno::NODATA => Error::new_os(ErrorKind::Other, "no kernel driver attached", e),
            _ => Error::new_os(ErrorKind::Other, "failed to detach kernel driver", e),
        })
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn attach_kernel_driver(
        self: &Arc<Self>,
        interface_number: u8,
    ) -> Result<(), Error> {
        usbfs::attach_kernel_driver(&self.fd, interface_number).map_err(|e| match e {
            Errno::INVAL => Error::new_os(ErrorKind::NotFound, "interface not found", e),
            Errno::NODEV => Error::new_os(ErrorKind::Disconnected, "device disconnected", e),
            Errno::BUSY => Error::new_os(ErrorKind::Busy, "kernel driver already attached", e),
            _ => Error::new_os(ErrorKind::Other, "failed to attach kernel driver", e),
        })
    }

    pub(crate) fn submit(&self, transfer: Idle<TransferData>) -> Pending<TransferData> {
        let len = transfer.urb().buffer_length;
        let pending = transfer.pre_submit();
        let urb = pending.urb_ptr();

        // SAFETY: We got the urb from `Idle<TransferData>`, which always points to
        // a valid URB with valid buffers, which is not already pending
        unsafe {
            let ep = (*urb).endpoint;
            (*urb).usercontext = pending.as_ptr().cast();
            if let Err(e) = usbfs::submit_urb(&self.fd, urb) {
                // SAFETY: Transfer was not submitted. We still own the transfer
                // and can write to the URB and complete it in place of the handler.
                let u = &mut *urb;
                debug!("Failed to submit URB {urb:?}: {len} bytes on ep {ep:x}: {e} {u:?}");
                u.actual_length = 0;
                u.status = e.raw_os_error();
                notify_completion::<super::TransferData>(pending.as_ptr().cast());
            } else {
                debug!("Submitted URB {urb:?}: {len} bytes on ep {ep:x}");
            }
        };

        pending
    }

    fn submit_timeout(
        &self,
        mut transfer: Idle<TransferData>,
        timeout: Duration,
    ) -> Pending<TransferData> {
        let urb = transfer.urb_ptr();
        let now = Instant::now();
        let deadline = now + timeout;
        transfer.deadline = Some(deadline);

        // Hold the lock across `submit`, so that it can't complete before we
        // insert the timeout entry.
        let mut timeouts = self.timeouts.lock().unwrap();

        let r = self.submit(transfer);

        // This can only be false if submit failed, because we hold the timeouts lock
        // and would block the completion handler.
        if !r.is_complete() {
            timeouts.insert(TimeoutEntry { deadline, urb }, ());
            self.update_timeouts(timeouts, now);
        }

        r
    }

    pub(crate) fn cancel(&self, transfer: &mut Pending<TransferData>) {
        let urb = transfer.urb_ptr();
        unsafe {
            if let Err(e) = usbfs::discard_urb(&self.fd, urb) {
                debug!("Failed to cancel URB {urb:?}: {e}");
            } else {
                debug!("Requested cancellation of URB {urb:?}");
            }
        }
    }

    pub(crate) fn speed(&self) -> Option<Speed> {
        usbfs::get_speed(&self.fd)
            .inspect_err(|e| log::error!("USBDEVFS_GET_SPEED failed: {e}"))
            .ok()
            .and_then(|raw_speed| match raw_speed {
                1 => Some(Speed::Low),
                2 => Some(Speed::Full),
                3 => Some(Speed::High),
                // 4 is wireless USB, but we don't support it
                5 => Some(Speed::Super),
                6 => Some(Speed::SuperPlus),
                _ => None,
            })
    }
}

fn read_all_from_fd(fd: &OwnedFd) -> Result<Vec<u8>, std::io::Error> {
    let mut file = unsafe { ManuallyDrop::new(File::from_raw_fd(fd.as_raw_fd())) };
    file.seek(std::io::SeekFrom::Start(0))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    Ok(buf)
}

/// Try a request to get the active configuration or fall back to a guess.
fn guess_active_configuration(fd: &OwnedFd, descriptors: &[u8]) -> u8 {
    request_configuration(fd).unwrap_or_else(|()| {
        let mut config_descriptors =
            parse_concatenated_config_descriptors(&descriptors[DESCRIPTOR_LEN_DEVICE as usize..]);

        if let Some(config) = config_descriptors.next() {
            config.configuration_value()
        } else {
            error!("no configurations for device fd {}", fd.as_raw_fd());
            0
        }
    })
}

/// Get the active configuration with a blocking request to the device.
fn request_configuration(fd: &OwnedFd) -> Result<u8, ()> {
    const REQUEST_GET_CONFIGURATION: u8 = 0x08;

    let mut dst = [0u8; 1];
    let r = usbfs::control(
        fd,
        usbfs::CtrlTransfer {
            bRequestType: request_type(Direction::In, ControlType::Standard, Recipient::Device),
            bRequest: REQUEST_GET_CONFIGURATION,
            wValue: 0,
            wIndex: 0,
            wLength: dst.len() as u16,
            timeout: 50,
            data: &mut dst[0] as *mut u8 as *mut c_void,
        },
    );

    match r {
        Ok(1) => {
            let active_config = dst[0];
            debug!("Obtained active configuration for fd {} from GET_CONFIGURATION request: {active_config}", fd.as_raw_fd());
            Ok(active_config)
        }
        Ok(n) => {
            warn!("GET_CONFIGURATION request returned unexpected length: {n}, expected 1");
            Err(())
        }
        Err(e) => {
            warn!(
                "GET_CONFIGURATION request failed: {:?}",
                errno_to_transfer_error(e)
            );
            Err(())
        }
    }
}

impl Drop for LinuxDevice {
    fn drop(&mut self) {
        debug!("Closing device {}", self.events_id);
        events::unregister_fd(self.fd.as_fd());
        events::unregister_fd(self.timerfd.as_fd());
        DEVICES.lock().unwrap().remove(self.events_id);
    }
}

pub(crate) struct LinuxInterface {
    pub(crate) interface_number: u8,
    pub(crate) device: Arc<LinuxDevice>,
    reattach: bool,
    state: Mutex<InterfaceState>,
}

#[derive(Default)]
struct InterfaceState {
    endpoints: EndpointBitSet,
    alt_setting: u8,
}

impl LinuxInterface {
    pub fn control_in(
        &self,
        data: ControlIn,
        timeout: Duration,
    ) -> impl MaybeFuture<Output = Result<Vec<u8>, TransferError>> {
        self.device.clone().control_in(data, timeout)
    }

    pub fn control_out(
        &self,
        data: ControlOut,
        timeout: Duration,
    ) -> impl MaybeFuture<Output = Result<(), TransferError>> {
        self.device.clone().control_out(data, timeout)
    }

    pub fn get_alt_setting(&self) -> u8 {
        self.state.lock().unwrap().alt_setting
    }

    pub fn set_alt_setting(
        self: Arc<Self>,
        alt_setting: u8,
    ) -> impl MaybeFuture<Output = Result<(), Error>> {
        Blocking::new(move || {
            let mut state = self.state.lock().unwrap();
            if !state.endpoints.is_empty() {
                return Err(Error::new(
                    ErrorKind::Busy,
                    "can't change alternate setting while endpoints are in use",
                ));
            }
            usbfs::set_interface(&self.device.fd, self.interface_number, alt_setting).map_err(
                |e| match e {
                    Errno::INVAL => {
                        Error::new_os(ErrorKind::NotFound, "alternate setting not found", e)
                    }
                    Errno::NODEV => {
                        Error::new_os(ErrorKind::Disconnected, "device disconnected", e)
                    }
                    _ => Error::new_os(ErrorKind::Other, "failed to set alternate setting", e),
                },
            )?;
            debug!(
                "Set interface {} alt setting to {alt_setting}",
                self.interface_number
            );
            state.alt_setting = alt_setting;
            Ok(())
        })
    }

    pub fn endpoint(
        self: &Arc<Self>,
        descriptor: EndpointDescriptor,
    ) -> Result<LinuxEndpoint, Error> {
        let address = descriptor.address();
        let ep_type = descriptor.transfer_type();
        let max_packet_size = descriptor.max_packet_size();

        let mut state = self.state.lock().unwrap();

        if state.endpoints.is_set(address) {
            return Err(Error::new(ErrorKind::Busy, "endpoint already in use"));
        }
        state.endpoints.set(address);

        Ok(LinuxEndpoint {
            inner: Arc::new(EndpointInner {
                address,
                ep_type,
                interface: self.clone(),
                notify: Notify::new(),
            }),
            max_packet_size,
            pending: VecDeque::new(),
            idle_transfer: None,
        })
    }
}

impl Drop for LinuxInterface {
    fn drop(&mut self) {
        let res = usbfs::release_interface(&self.device.fd, self.interface_number);
        debug!(
            "Released interface {} on device {}: {res:?}",
            self.interface_number, self.device.events_id
        );

        if res.is_ok() && self.reattach {
            let res = usbfs::attach_kernel_driver(&self.device.fd, self.interface_number);
            debug!(
                "Reattached kernel drivers for interface {} on device {}: {res:?}",
                self.interface_number, self.device.events_id
            );
        }
    }
}

pub(crate) struct LinuxEndpoint {
    inner: Arc<EndpointInner>,

    pub(crate) max_packet_size: usize,

    /// A queue of pending transfers, expected to complete in order
    pending: VecDeque<Pending<super::TransferData>>,

    idle_transfer: Option<Idle<TransferData>>,
}

struct EndpointInner {
    interface: Arc<LinuxInterface>,
    address: u8,
    ep_type: TransferType,
    notify: Notify,
}

impl LinuxEndpoint {
    pub(crate) fn endpoint_address(&self) -> u8 {
        self.inner.address
    }

    pub(crate) fn pending(&self) -> usize {
        self.pending.len()
    }

    pub(crate) fn cancel_all(&mut self) {
        // Cancel transfers in reverse order to ensure subsequent transfers
        // can't complete out of order while we're going through them.
        for transfer in self.pending.iter_mut().rev() {
            self.inner.interface.device.cancel(transfer);
        }
    }

    fn get_transfer(&mut self) -> Idle<TransferData> {
        self.idle_transfer.take().unwrap_or_else(|| {
            Idle::new(
                self.inner.clone(),
                super::TransferData::new(self.inner.address, self.inner.ep_type),
            )
        })
    }

    pub(crate) fn submit(&mut self, data: Buffer) {
        let mut transfer = self.get_transfer();
        transfer.set_buffer(data);
        self.pending
            .push_back(self.inner.interface.device.submit(transfer));
    }

    pub(crate) fn submit_err(&mut self, data: Buffer, error: TransferError) {
        assert_eq!(error, TransferError::InvalidArgument);
        let mut transfer = self.get_transfer();
        transfer.set_buffer(data);
        transfer.urb_mut().status = Errno::INVAL.raw_os_error();
        self.pending.push_back(transfer.simulate_complete());
    }

    pub(crate) fn poll_next_complete(&mut self, cx: &mut Context) -> Poll<Completion> {
        self.inner.notify.subscribe(cx);
        if let Some(mut transfer) = take_completed_from_queue(&mut self.pending) {
            let completion = transfer.take_completion();
            self.idle_transfer = Some(transfer);
            Poll::Ready(completion)
        } else {
            Poll::Pending
        }
    }

    pub(crate) fn wait_next_complete(&mut self, timeout: Duration) -> Option<Completion> {
        self.inner.notify.wait_timeout(timeout, || {
            take_completed_from_queue(&mut self.pending).map(|mut transfer| {
                let completion = transfer.take_completion();
                self.idle_transfer = Some(transfer);
                completion
            })
        })
    }

    pub(crate) fn clear_halt(&self) -> impl MaybeFuture<Output = Result<(), Error>> {
        let inner = self.inner.clone();
        Blocking::new(move || {
            let endpoint = inner.address;
            debug!("Clear halt, endpoint {endpoint:02x}");
            usbfs::clear_halt(&inner.interface.device.fd, endpoint).map_err(|e| match e {
                Errno::NODEV => Error::new_os(ErrorKind::Disconnected, "device disconnected", e),
                _ => Error::new_os(ErrorKind::Other, "failed to clear halt", e),
            })
        })
    }

    pub(crate) fn allocate(&self, len: usize) -> Result<Buffer, Errno> {
        Buffer::mmap(&self.inner.interface.device.fd, len).inspect_err(|e| {
            warn!(
                "Failed to allocate zero-copy buffer of length {len} for endpoint {}: {e}",
                self.inner.address
            );
        })
    }
}

impl Drop for LinuxEndpoint {
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
        state.endpoints.clear(self.address);
    }
}
