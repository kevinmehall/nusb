use std::{fs::File, io::Read, mem::ManuallyDrop, path::PathBuf, sync::Arc};

use log::{debug, error};
use rustix::{
    fd::{AsRawFd, FromRawFd, OwnedFd},
    fs::{Mode, OFlags},
    io::Errno,
};

use super::{
    events,
    usbfs::{self, Urb},
};
use crate::{
    descriptors::{parse_concatenated_config_descriptors, DESCRIPTOR_LEN_DEVICE},
    transfer::{notify_completion, EndpointType, TransferHandle},
    DeviceInfo, Error,
};

pub(crate) struct LinuxDevice {
    fd: OwnedFd,
    events_id: usize,

    /// Read from the fd, consists of device descriptor followed by configuration descriptors
    descriptors: Vec<u8>,
}

impl LinuxDevice {
    pub(crate) fn from_device_info(d: &DeviceInfo) -> Result<Arc<LinuxDevice>, Error> {
        Self::open(d.bus_number(), d.device_address())
    }

    pub(crate) fn open(busnum: u8, devnum: u8) -> Result<Arc<LinuxDevice>, Error> {
        let path = PathBuf::from(format!("/dev/bus/usb/{busnum:03}/{devnum:03}"));
        debug!("Opening usbfs device {}", path.display());
        let fd = rustix::fs::open(path, OFlags::RDWR | OFlags::CLOEXEC, Mode::empty())?;

        let descriptors = {
            let mut file = unsafe { ManuallyDrop::new(File::from_raw_fd(fd.as_raw_fd())) };
            let mut buf = Vec::new();
            file.read_to_end(&mut buf)?;
            buf
        };

        // because there's no Arc::try_new_cyclic
        let mut events_err = None;
        let arc = Arc::new_cyclic(|weak| {
            let res = events::register(&fd, weak.clone());
            let events_id = *res.as_ref().unwrap_or(&usize::MAX);
            events_err = res.err();
            LinuxDevice {
                fd,
                events_id,
                descriptors,
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
                events::unregister_fd(&self.fd);
            }
            Err(e) => {
                error!("Unexpected error {e} from REAPURBNDELAY");
            }
        }
    }

    pub(crate) fn configuration_descriptors(&self) -> impl Iterator<Item = &[u8]> {
        parse_concatenated_config_descriptors(&self.descriptors[DESCRIPTOR_LEN_DEVICE as usize..])
    }

    pub(crate) fn set_configuration(&self, configuration: u8) -> Result<(), Error> {
        usbfs::set_configuration(&self.fd, configuration)?;
        Ok(())
    }

    pub(crate) fn reset(&self) -> Result<(), Error> {
        usbfs::reset(&self.fd)?;
        Ok(())
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
        interface: u8,
    ) -> Result<Arc<LinuxInterface>, Error> {
        usbfs::claim_interface(&self.fd, interface)?;
        debug!(
            "Claimed interface {interface} on device id {dev}",
            dev = self.events_id
        );
        Ok(Arc::new(LinuxInterface {
            device: self.clone(),
            interface,
        }))
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
        events::unregister(&self.fd, self.events_id)
    }
}

pub(crate) struct LinuxInterface {
    pub(crate) interface: u8,
    pub(crate) device: Arc<LinuxDevice>,
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

    pub fn set_alt_setting(&self, alt_setting: u8) -> Result<(), Error> {
        debug!(
            "Set interface {} alt setting to {alt_setting}",
            self.interface
        );
        Ok(usbfs::set_interface(
            &self.device.fd,
            self.interface,
            alt_setting,
        )?)
    }
}

impl Drop for LinuxInterface {
    fn drop(&mut self) {
        let res = usbfs::release_interface(&self.device.fd, self.interface);
        debug!(
            "Released interface {} on device {}: {res:?}",
            self.interface, self.device.events_id
        );
    }
}
