use std::{collections::VecDeque, sync::Arc, time::Duration};

use crate::{transfer::EndpointType, Completion, DeviceInfo, Error, Transfer};

type TransferError = Error;
type Buffer = Vec<u8>;

#[derive(Clone)]
pub struct Device {
    backend: Arc<crate::platform::Device>,
}

impl Device {
    pub(crate) fn open(d: &DeviceInfo) -> Result<Device, std::io::Error> {
        let backend = crate::platform::Device::from_device_info(d)?;
        Ok(Device { backend })
    }

    pub fn set_configuration(&self, configuration: u8) -> Result<(), Error> {
        self.backend.set_configuration(configuration)
    }

    pub fn reset(&self) -> Result<(), Error> {
        self.backend.reset()
    }

    pub fn claim_interface(&self, interface: u8) -> Result<Interface, Error> {
        let backend = self.backend.claim_interface(interface)?;
        Ok(Interface { backend })
    }
}

pub struct Interface {
    backend: Arc<crate::platform::Interface>,
}

impl Interface {
    pub fn set_alt_setting(&self) {
        todo!()
    }

    pub fn bulk_transfer(&self, endpoint: u8, buf: Vec<u8>) -> Transfer {
        let mut t = Transfer::new(self.backend.clone(), endpoint, EndpointType::Bulk);
        t.submit(buf);
        t
    }

    pub fn interrupt_transfer(&self, endpoint: u8, buf: Vec<u8>) -> Transfer {
        let mut t = Transfer::new(self.backend.clone(), endpoint, EndpointType::Interrupt);
        t.submit(buf);
        t
    }
}

struct Queue {
    pending: VecDeque<Transfer>,
    cached: Option<Transfer>,
}

impl Queue {
    /// Submit a new transfer on the endpoint.
    ///
    /// For an IN endpoint, the transfer size is set by the *capacity* of
    /// the buffer, and the length and current contents are ignored. The
    /// buffer is returned from a later call to `complete` filled with
    /// the data read from the endpoint.
    ///
    /// For an OUT endpoint, the contents of the buffer are written to
    /// the endpoint.
    pub fn submit(&mut self, buf: Buffer) -> Result<(), TransferError> {
        todo!()
    }

    /// Block waiting for the next pending transfer to complete, and return
    /// its buffer or an error status.
    ///
    /// For an IN endpoint, the returned buffer contains the data
    /// read from the device.
    ///
    /// For an OUT endpoint, the buffer is unmodified, but can be
    /// reused for another transfer.
    pub fn complete(&mut self, timeout: Option<Duration>) -> Option<Completion> {
        todo!()
    }

    /// Get the number of transfers that have been submitted with
    /// `submit` that have not yet been returned from `complete`.
    pub fn pending_transfers(&self) -> usize {
        todo!()
    }

    /// Get the number of transfers that have completed and are
    /// ready to be returned from `complete` without blocking.
    pub fn ready_transfers(&self) -> usize {
        todo!()
    }

    /// Cancel all pending transfers on the endpoint pipe.
    /// TODO: maybe this should be on the `Device` or an object separable from the `Pipe`
    /// so it can be called from another thread, and cause a blocking `complete` call to
    //// immediately return.
    fn cancel_all(&mut self) -> Result<(), TransferError> {
        todo!()
    }
}
