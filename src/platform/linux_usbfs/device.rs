use std::{
    collections::{BTreeMap, VecDeque},
    ffi::c_void,
    fs::File,
    io::{ErrorKind, Read, Seek},
    mem::ManuallyDrop,
    path::PathBuf,
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
    fs::{Mode, OFlags, Timespec},
    io::Errno,
    time::{timerfd_create, timerfd_settime, Itimerspec, TimerfdFlags, TimerfdTimerFlags},
};
use slab::Slab;

use super::{
    errno_to_transfer_error, events,
    usbfs::{self, Urb},
    SysfsPath, TransferData,
};
use crate::{
    bitset::EndpointBitSet,
    descriptors::{
        parse_concatenated_config_descriptors, ConfigurationDescriptor, DeviceDescriptor,
        EndpointDescriptor, TransferType, DESCRIPTOR_LEN_DEVICE,
    },
    device::ClaimEndpointError,
    maybe_future::{blocking::Blocking, MaybeFuture},
    transfer::{
        internal::{
            notify_completion, take_completed_from_queue, Idle, Notify, Pending, TransferFuture,
        },
        request_type, Buffer, Completion, ControlIn, ControlOut, ControlType, Direction, Recipient,
        TransferError,
    },
    DeviceInfo, Error, Speed,
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

    sysfs: Option<SysfsPath>,
    active_config: AtomicU8,

    timerfd: OwnedFd,
    timeouts: Mutex<BTreeMap<TimeoutEntry, ()>>,
}

impl LinuxDevice {
    pub(crate) fn from_device_info(
        d: &DeviceInfo,
    ) -> impl MaybeFuture<Output = Result<Arc<LinuxDevice>, Error>> {
        let busnum = d.busnum();
        let devnum = d.device_address();
        let sysfs_path = d.path.clone();

        Blocking::new(move || {
            let active_config = sysfs_path.read_attr("bConfigurationValue")?;
            let path = PathBuf::from(format!("/dev/bus/usb/{busnum:03}/{devnum:03}"));
            let fd = rustix::fs::open(&path, OFlags::RDWR | OFlags::CLOEXEC, Mode::empty())
                .inspect_err(|e| warn!("Failed to open device {path:?}: {e}"))?;
            Self::create_inner(fd, Some(sysfs_path), Some(active_config))
        })
    }

    pub(crate) fn from_fd(
        fd: OwnedFd,
    ) -> impl MaybeFuture<Output = Result<Arc<LinuxDevice>, Error>> {
        Blocking::new(move || {
            debug!("Wrapping fd {} as usbfs device", fd.as_raw_fd());
            Self::create_inner(fd, None, None)
        })
    }

    pub(crate) fn create_inner(
        fd: OwnedFd,
        sysfs: Option<SysfsPath>,
        active_config: Option<u8>,
    ) -> Result<Arc<LinuxDevice>, Error> {
        let descriptors = {
            let mut file = unsafe { ManuallyDrop::new(File::from_raw_fd(fd.as_raw_fd())) };
            // NOTE: Seek required on android
            file.seek(std::io::SeekFrom::Start(0))?;
            let mut buf = Vec::new();
            file.read_to_end(&mut buf)?;
            buf
        };

        let Some(_) = DeviceDescriptor::new(&descriptors) else {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "invalid device descriptor",
            ));
        };

        let active_config = if let Some(active_config) = active_config {
            active_config
        } else {
            Self::get_config(&descriptors, &fd)?
        };

        let timerfd = timerfd_create(
            rustix::time::TimerfdClockId::Monotonic,
            TimerfdFlags::CLOEXEC | TimerfdFlags::NONBLOCK,
        )
        .inspect_err(|e| log::error!("Failed to create timerfd: {e}"))?;

        let arc = Arc::new_cyclic(|weak| {
            let events_id = DEVICES.lock().unwrap().insert(weak.clone());
            LinuxDevice {
                fd,
                events_id,
                descriptors,
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
        if let Some(sysfs) = self.sysfs.as_ref() {
            match sysfs.read_attr("bConfigurationValue") {
                Ok(v) => {
                    self.active_config.store(v, Ordering::SeqCst);
                    return v;
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
            usbfs::set_configuration(&self.fd, configuration)?;
            self.active_config.store(configuration, Ordering::SeqCst);
            Ok(())
        })
    }

    pub(crate) fn reset(self: Arc<Self>) -> impl MaybeFuture<Output = Result<(), Error>> {
        Blocking::new(move || {
            usbfs::reset(&self.fd)?;
            Ok(())
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

    pub(crate) fn claim_interface(
        self: Arc<Self>,
        interface_number: u8,
    ) -> impl MaybeFuture<Output = Result<Arc<LinuxInterface>, Error>> {
        Blocking::new(move || {
            usbfs::claim_interface(&self.fd, interface_number).inspect_err(|e| {
                warn!(
                    "Failed to claim interface {interface_number} on device id {dev}: {e}",
                    dev = self.events_id
                )
            })?;
            debug!(
                "Claimed interface {interface_number} on device id {dev}",
                dev = self.events_id
            );
            Ok(Arc::new(LinuxInterface {
                device: self,
                interface_number,
                reattach: false,
                state: Mutex::new(Default::default()),
            }))
        })
    }

    pub(crate) fn detach_and_claim_interface(
        self: Arc<Self>,
        interface_number: u8,
    ) -> impl MaybeFuture<Output = Result<Arc<LinuxInterface>, Error>> {
        Blocking::new(move || {
            usbfs::detach_and_claim_interface(&self.fd, interface_number)?;
            debug!(
                "Detached and claimed interface {interface_number} on device id {dev}",
                dev = self.events_id
            );
            Ok(Arc::new(LinuxInterface {
                device: self,
                interface_number,
                reattach: true,
                state: Mutex::new(Default::default()),
            }))
        })
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn detach_kernel_driver(
        self: &Arc<Self>,
        interface_number: u8,
    ) -> Result<(), Error> {
        usbfs::detach_kernel_driver(&self.fd, interface_number).map_err(|e| e.into())
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn attach_kernel_driver(
        self: &Arc<Self>,
        interface_number: u8,
    ) -> Result<(), Error> {
        usbfs::attach_kernel_driver(&self.fd, interface_number).map_err(|e| e.into())
    }

    pub(crate) fn submit(&self, transfer: Idle<TransferData>) -> Pending<TransferData> {
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
                debug!("Failed to submit URB {urb:?} on ep {ep:x}: {e} {u:?}");
                u.actual_length = 0;
                u.status = e.raw_os_error();
                notify_completion::<super::TransferData>(pending.as_ptr().cast());
            } else {
                debug!("Submitted URB {urb:?} on ep {ep:x}");
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
            }
        }
    }

    fn get_config(descriptors: &[u8], fd: &OwnedFd) -> Result<u8, Error> {
        const REQUEST_GET_CONFIGURATION: u8 = 0x08;

        let mut dst = [0u8; 1];
        let r = usbfs::control(
            &fd,
            usbfs::CtrlTransfer {
                bRequestType: request_type(Direction::In, ControlType::Standard, Recipient::Device),
                bRequest: REQUEST_GET_CONFIGURATION,
                wValue: 0,
                wIndex: 0,
                wLength: dst.len() as u16,
                timeout: Duration::from_millis(50)
                    .as_millis()
                    .try_into()
                    .expect("timeout must fit in u32 ms"),
                data: &mut dst[0] as *mut u8 as *mut c_void,
            },
        );

        match r {
            Ok(n) => {
                if n == dst.len() {
                    let active_config = dst[0];
                    debug!("Obtained active configuration for fd {} from GET_CONFIGURATION request: {active_config}", fd.as_raw_fd());
                    return Ok(active_config);
                } else {
                    warn!("GET_CONFIGURATION request returned incorrect length: {n}, expected 1",);
                }
            }
            Err(e) => {
                warn!(
                    "GET_CONFIGURATION request failed: {:?}",
                    errno_to_transfer_error(e)
                );
            }
        }

        if descriptors.len() < DESCRIPTOR_LEN_DEVICE as usize {
            warn!(
                "Descriptors for device fd {} too short to use fallback configuration",
                fd.as_raw_fd()
            );
            return Err(ErrorKind::Other.into());
        }

        // Assume the current configuration is the first one
        // See: https://github.com/libusb/libusb/blob/467b6a8896daea3d104958bf0887312c5d14d150/libusb/os/linux_usbfs.c#L865
        let mut descriptors =
            parse_concatenated_config_descriptors(&descriptors[DESCRIPTOR_LEN_DEVICE as usize..]);
        if let Some(config) = descriptors.next() {
            return Ok(config.configuration_value());
        }

        error!(
            "No available configurations for device fd {}",
            fd.as_raw_fd()
        );
        return Err(ErrorKind::Other.into());
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
                // TODO: Use ErrorKind::ResourceBusy once compatible with MSRV
                return Err(Error::new(
                    ErrorKind::Other,
                    "must drop endpoints before changing alt setting",
                ));
            }
            debug!(
                "Set interface {} alt setting to {alt_setting}",
                self.interface_number
            );
            usbfs::set_interface(&self.device.fd, self.interface_number, alt_setting)?;
            state.alt_setting = alt_setting;
            Ok(())
        })
    }

    pub fn endpoint(
        self: &Arc<Self>,
        descriptor: EndpointDescriptor,
    ) -> Result<LinuxEndpoint, ClaimEndpointError> {
        let address = descriptor.address();
        let ep_type = descriptor.transfer_type();
        let max_packet_size = descriptor.max_packet_size();

        let mut state = self.state.lock().unwrap();

        if state.endpoints.is_set(address) {
            return Err(ClaimEndpointError::Busy);
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

    pub(crate) fn submit(&mut self, data: Buffer) {
        let mut transfer = self.idle_transfer.take().unwrap_or_else(|| {
            Idle::new(
                self.inner.clone(),
                super::TransferData::new(self.inner.address, self.inner.ep_type),
            )
        });
        transfer.set_buffer(data);
        self.pending
            .push_back(self.inner.interface.device.submit(transfer));
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
            Ok(usbfs::clear_halt(&inner.interface.device.fd, endpoint)?)
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
        state.endpoints.clear(self.address);
    }
}
