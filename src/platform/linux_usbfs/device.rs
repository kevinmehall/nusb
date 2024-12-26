use std::io::{ErrorKind, Seek};
use std::{ffi::c_void, time::Duration};
use std::{
    fs::File,
    io::Read,
    mem::ManuallyDrop,
    path::PathBuf,
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc,
    },
};

use log::{debug, error, warn};
use rustix::event::epoll;
use rustix::fd::AsFd;
use rustix::{
    fd::{AsRawFd, FromRawFd, OwnedFd},
    fs::{Mode, OFlags},
    io::Errno,
};

use super::{
    errno_to_transfer_error, events,
    usbfs::{self, Urb},
    SysfsPath,
};
use crate::descriptors::Configuration;
use crate::platform::linux_usbfs::events::Watch;
use crate::transfer::{ControlType, Recipient};
use crate::{
    descriptors::{parse_concatenated_config_descriptors, DESCRIPTOR_LEN_DEVICE},
    transfer::{
        notify_completion, Control, Direction, EndpointType, TransferError, TransferHandle,
    },
    DeviceInfo, Error,
};

pub(crate) struct LinuxDevice {
    fd: OwnedFd,
    events_id: usize,

    /// Read from the fd, consists of device descriptor followed by configuration descriptors
    descriptors: Vec<u8>,

    sysfs: Option<SysfsPath>,
    active_config: AtomicU8,
}

impl LinuxDevice {
    pub(crate) async fn from_device_info(d: &DeviceInfo) -> Result<Arc<LinuxDevice>, Error> {
        let busnum = d.busnum();
        let devnum = d.device_address();
        let active_config = d.path.read_attr("bConfigurationValue")?;

        let path = PathBuf::from(format!("/dev/bus/usb/{busnum:03}/{devnum:03}"));
        let fd = rustix::fs::open(&path, OFlags::RDWR | OFlags::CLOEXEC, Mode::empty())
            .inspect_err(|e| warn!("Failed to open device {path:?}: {e}"))?;

        let inner = Self::create_inner(fd, Some(d.path.clone()), Some(active_config));
        if inner.is_ok() {
            debug!("Opened device bus={busnum} addr={devnum}",);
        }
        inner
    }

    pub(crate) fn from_fd(fd: OwnedFd) -> Result<Arc<LinuxDevice>, Error> {
        debug!("Wrapping fd {} as usbfs device", fd.as_raw_fd());

        Self::create_inner(fd, None, None)
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

        let active_config = if let Some(active_config) = active_config {
            active_config
        } else {
            Self::get_config(&descriptors, &fd)?
        };

        // because there's no Arc::try_new_cyclic
        let mut events_err = None;
        let arc = Arc::new_cyclic(|weak| {
            let res = events::register(
                fd.as_fd(),
                Watch::Device(weak.clone()),
                epoll::EventFlags::OUT,
            );
            let events_id = *res.as_ref().unwrap_or(&usize::MAX);
            events_err = res.err();
            if events_err.is_none() {
                debug!("Opened device fd={} with id {}", fd.as_raw_fd(), events_id,);
            }
            LinuxDevice {
                fd,
                events_id,
                descriptors,
                sysfs,
                active_config: AtomicU8::new(active_config),
            }
        });

        if let Some(err) = events_err {
            error!("Failed to initialize event loop: {err}");
            Err(err)
        } else {
            Ok(arc)
        }
    }

    pub(crate) fn handle_events(&self) {
        debug!("Handling events for device {}", self.events_id);
        match usbfs::reap_urb_ndelay(&self.fd) {
            Ok(urb_ptr) => {
                let user_data = {
                    let urb = unsafe { &*urb_ptr };
                    debug!(
                        "URB {:?} for ep {:x} completed, status={} actual_length={}",
                        urb_ptr, urb.endpoint, urb.status, urb.actual_length
                    );
                    urb.usercontext
                };

                // SAFETY: pointer came from submit via kernel an we're now done with it
                unsafe { notify_completion::<super::TransferData>(user_data) }
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

    pub(crate) fn configuration_descriptors(&self) -> impl Iterator<Item = &[u8]> {
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

    pub(crate) async fn set_configuration(&self, configuration: u8) -> Result<(), Error> {
        usbfs::set_configuration(&self.fd, configuration)?;
        self.active_config.store(configuration, Ordering::SeqCst);
        Ok(())
    }

    pub(crate) async fn reset(&self) -> Result<(), Error> {
        usbfs::reset(&self.fd)?;
        Ok(())
    }

    /// SAFETY: `data` must be valid for `len` bytes to read or write, depending on `Direction`
    unsafe fn control_blocking(
        &self,
        direction: Direction,
        control: Control,
        data: *mut u8,
        len: usize,
        timeout: Duration,
    ) -> Result<usize, TransferError> {
        let r = usbfs::control(
            &self.fd,
            usbfs::CtrlTransfer {
                bRequestType: control.request_type(direction),
                bRequest: control.request,
                wValue: control.value,
                wIndex: control.index,
                wLength: len.try_into().expect("length must fit in u16"),
                timeout: timeout
                    .as_millis()
                    .try_into()
                    .expect("timeout must fit in u32 ms"),
                data: data as *mut c_void,
            },
        );

        r.map_err(errno_to_transfer_error)
    }

    pub fn control_in_blocking(
        &self,
        control: Control,
        data: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, TransferError> {
        unsafe {
            self.control_blocking(
                Direction::In,
                control,
                data.as_mut_ptr(),
                data.len(),
                timeout,
            )
        }
    }

    pub fn control_out_blocking(
        &self,
        control: Control,
        data: &[u8],
        timeout: Duration,
    ) -> Result<usize, TransferError> {
        unsafe {
            self.control_blocking(
                Direction::Out,
                control,
                data.as_ptr() as *mut u8,
                data.len(),
                timeout,
            )
        }
    }

    pub(crate) fn make_control_transfer(self: &Arc<Self>) -> TransferHandle<super::TransferData> {
        TransferHandle::new(super::TransferData::new(
            self.clone(),
            None,
            0,
            EndpointType::Control,
        ))
    }

    pub(crate) async fn claim_interface(
        self: &Arc<Self>,
        interface_number: u8,
    ) -> Result<Arc<LinuxInterface>, Error> {
        usbfs::claim_interface(&self.fd, interface_number)
            .await
            .inspect_err(|e| {
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
            device: self.clone(),
            interface_number,
            reattach: false,
        }))
    }

    pub(crate) async fn detach_and_claim_interface(
        self: &Arc<Self>,
        interface_number: u8,
    ) -> Result<Arc<LinuxInterface>, Error> {
        usbfs::detach_and_claim_interface(&self.fd, interface_number).await?;
        debug!(
            "Detached and claimed interface {interface_number} on device id {dev}",
            dev = self.events_id
        );
        Ok(Arc::new(LinuxInterface {
            device: self.clone(),
            interface_number,
            reattach: true,
        }))
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

    pub(crate) unsafe fn submit_urb(&self, urb: *mut Urb) {
        let ep = unsafe { (*urb).endpoint };
        if let Err(e) = usbfs::submit_urb(&self.fd, urb) {
            // SAFETY: Transfer was not submitted. We still own the transfer
            // and can write to the URB and complete it in place of the handler.
            unsafe {
                let user_data = {
                    let u = &mut *urb;
                    debug!("Failed to submit URB {urb:?} on ep {ep:x}: {e} {u:?}");
                    u.actual_length = 0;
                    u.status = e.raw_os_error();
                    u.usercontext
                };
                notify_completion::<super::TransferData>(user_data)
            }
        } else {
            debug!("Submitted URB {urb:?} on ep {ep:x}");
        }
    }

    pub(crate) unsafe fn cancel_urb(&self, urb: *mut Urb) {
        unsafe {
            if let Err(e) = usbfs::discard_urb(&self.fd, urb) {
                debug!("Failed to cancel URB {urb:?}: {e}");
            }
        }
    }

    fn get_config(descriptors: &[u8], fd: &OwnedFd) -> Result<u8, Error> {
        const REQUEST_GET_CONFIGURATION: u8 = 0x08;

        let mut dst = [0u8; 1];

        let control = Control {
            control_type: ControlType::Standard,
            recipient: Recipient::Device,
            request: REQUEST_GET_CONFIGURATION,
            value: 0,
            index: 0,
        };

        let r = usbfs::control(
            &fd,
            usbfs::CtrlTransfer {
                bRequestType: control.request_type(Direction::In),
                bRequest: control.request,
                wValue: control.value,
                wIndex: control.index,
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
            parse_concatenated_config_descriptors(&descriptors[DESCRIPTOR_LEN_DEVICE as usize..])
                .map(Configuration::new);

        if let Some(config) = descriptors.next() {
            return Ok(config.configuration_value());
        }

        error!(
            "No available configurations for device fd {}",
            fd.as_raw_fd()
        );
        return Err(ErrorKind::Other.into());
    }
}

impl Drop for LinuxDevice {
    fn drop(&mut self) {
        debug!("Closing device {}", self.events_id);
        events::unregister(self.fd.as_fd(), self.events_id)
    }
}

pub(crate) struct LinuxInterface {
    pub(crate) interface_number: u8,
    pub(crate) device: Arc<LinuxDevice>,
    pub(crate) reattach: bool,
}

impl LinuxInterface {
    pub(crate) fn make_transfer(
        self: &Arc<Self>,
        endpoint: u8,
        ep_type: EndpointType,
    ) -> TransferHandle<super::TransferData> {
        TransferHandle::new(super::TransferData::new(
            self.device.clone(),
            Some(self.clone()),
            endpoint,
            ep_type,
        ))
    }

    pub fn control_in_blocking(
        &self,
        control: Control,
        data: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, TransferError> {
        self.device.control_in_blocking(control, data, timeout)
    }

    pub fn control_out_blocking(
        &self,
        control: Control,
        data: &[u8],
        timeout: Duration,
    ) -> Result<usize, TransferError> {
        self.device.control_out_blocking(control, data, timeout)
    }

    pub async fn set_alt_setting(&self, alt_setting: u8) -> Result<(), Error> {
        debug!(
            "Set interface {} alt setting to {alt_setting}",
            self.interface_number
        );
        Ok(usbfs::set_interface(
            &self.device.fd,
            self.interface_number,
            alt_setting,
        )?)
    }

    pub async fn clear_halt(&self, endpoint: u8) -> Result<(), Error> {
        debug!("Clear halt, endpoint {endpoint:02x}");
        Ok(usbfs::clear_halt(&self.device.fd, endpoint).await?)
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
