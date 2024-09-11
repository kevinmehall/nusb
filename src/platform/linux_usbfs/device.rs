use std::io::ErrorKind;
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
    pub(crate) fn from_device_info(d: &DeviceInfo) -> Result<Arc<LinuxDevice>, Error> {
        let busnum = d.busnum();
        let devnum = d.device_address();
        let active_config = d.path.read_attr("bConfigurationValue")?;

        let path = PathBuf::from(format!("/dev/bus/usb/{busnum:03}/{devnum:03}"));
        let fd = rustix::fs::open(&path, OFlags::RDWR | OFlags::CLOEXEC, Mode::empty())
            .inspect_err(|e| warn!("Failed to open device {path:?}: {e}"))?;

        Self::create_inner(fd, Some(d.path.clone()), active_config, busnum, devnum)
    }

    pub(crate) fn from_fd(fd: OwnedFd) -> Result<Arc<LinuxDevice>, Error> {
        log::debug!("Wrapping fd {} as usbfs device", fd.as_raw_fd());

        // NOTE: must be called from the same thread that gave us the FD on android, otherwise this fails
        let proc_path = PathBuf::from(format!("/proc/self/fd/{}", fd.as_raw_fd()));

        log::debug!("Reading link: {proc_path:?}");
        let fd_path = std::fs::read_link(&proc_path)?;

        // TODO: should we switch to regex or regex-lite for this?
        let prefix = "/dev/bus/usb/";
        let Ok(dev_num_bus_num) = fd_path.strip_prefix(prefix) else {
            log::error!(
                "Usb device path does not start with required prefix `{prefix}`: {}",
                fd_path.to_string_lossy()
            );
            return Err(ErrorKind::Other.into());
        };
        let Some(dev_num_bus_num) = dev_num_bus_num.to_str() else {
            log::error!("End of usb device path is not UTF-8!: {dev_num_bus_num:?}",);
            return Err(ErrorKind::Other.into());
        };

        let mut split = dev_num_bus_num.split("/");
        let Some(bus_str) = split.next() else {
            log::error!("Failed to split usbfs device path: {}", dev_num_bus_num);
            return Err(ErrorKind::Other.into());
        };
        let Some(dev_str) = split.next() else {
            log::error!("Failed to split usbfs device path: {}", dev_num_bus_num);
            return Err(ErrorKind::Other.into());
        };

        let Ok(busnum) = bus_str.parse::<u8>() else {
            log::error!("Usb bus number failed to parse: {bus_str}",);
            return Err(ErrorKind::Other.into());
        };
        let Ok(devnum) = dev_str.parse::<u8>() else {
            log::error!("Usb device number failed to parse: {dev_str}",);
            return Err(ErrorKind::Other.into());
        };

        const LIBUSB_REQUEST_GET_CONFIGURATION: u8 = 0x08;

        let mut dst = [0u8; 1];

        let control = Control {
            control_type: ControlType::Standard,
            recipient: Recipient::Device,
            request: LIBUSB_REQUEST_GET_CONFIGURATION,
            value: 0,
            index: 0,
        };

        // TODO: what to do about blocking here?
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

        // TODO: Could add some more logic here to support buggy devices, similar to what libusb does
        // See: https://github.com/libusb/libusb/blob/467b6a8896daea3d104958bf0887312c5d14d150/libusb/os/linux_usbfs.c#L865
        let n = r.map_err(errno_to_transfer_error)?;
        if n != dst.len() {
            log::error!("Failed to read descriptor");
            return Err(ErrorKind::Other.into());
        }

        let active_config = dst[0];
        log::info!("Got active config: {active_config}");
        Self::create_inner(fd, None, active_config, busnum, devnum)
    }

    pub(crate) fn create_inner(
        fd: OwnedFd,
        sysfs: Option<SysfsPath>,
        active_config: u8,
        busnum: u8,
        devnum: u8,
    ) -> Result<Arc<LinuxDevice>, Error> {
        log::info!("reading descriptors");
        let descriptors = {
            let mut file = unsafe { ManuallyDrop::new(File::from_raw_fd(fd.as_raw_fd())) };
            let mut buf = Vec::new();
            file.read_to_end(&mut buf)?;
            buf
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
            debug!(
                "Opened device bus={busnum} addr={devnum} with id {}",
                arc.events_id
            );
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

    pub(crate) fn set_configuration(&self, configuration: u8) -> Result<(), Error> {
        usbfs::set_configuration(&self.fd, configuration)?;
        self.active_config.store(configuration, Ordering::SeqCst);
        Ok(())
    }

    pub(crate) fn reset(&self) -> Result<(), Error> {
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

    pub(crate) fn claim_interface(
        self: &Arc<Self>,
        interface_number: u8,
    ) -> Result<Arc<LinuxInterface>, Error> {
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
            device: self.clone(),
            interface_number,
            reattach: false,
        }))
    }

    pub(crate) fn detach_and_claim_interface(
        self: &Arc<Self>,
        interface_number: u8,
    ) -> Result<Arc<LinuxInterface>, Error> {
        usbfs::detach_and_claim_interface(&self.fd, interface_number)?;
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

    pub fn set_alt_setting(&self, alt_setting: u8) -> Result<(), Error> {
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

    pub fn clear_halt(&self, endpoint: u8) -> Result<(), Error> {
        debug!("Clear halt, endpoint {endpoint:02x}");
        Ok(usbfs::clear_halt(&self.device.fd, endpoint)?)
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
