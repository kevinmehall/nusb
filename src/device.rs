use std::{
    collections::VecDeque,
    future::{poll_fn, Future},
    sync::Arc,
};

use crate::{
    control::{ControlIn, ControlOut},
    platform,
    transfer_internal::TransferHandle,
    Completion, DeviceInfo, EndpointType, Error, TransferFuture,
};

type TransferError = Error;
type Buffer = Vec<u8>;

#[derive(Clone)]
pub struct Device {
    backend: Arc<crate::platform::Device>,
}

impl Device {
    pub(crate) fn open(d: &DeviceInfo) -> Result<Device, std::io::Error> {
        let backend = platform::Device::from_device_info(d)?;
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
    backend: Arc<platform::Interface>,
}

impl Interface {
    pub fn set_alt_setting(&self) {
        todo!()
    }

    pub fn control_transfer_in(&self, data: ControlIn) -> TransferFuture<ControlIn> {
        let mut t = TransferHandle::new(self.backend.clone(), 0, EndpointType::Control);
        t.submit::<ControlIn>(data);
        TransferFuture::new(t)
    }

    pub fn control_transfer_out(&self, data: ControlOut) -> TransferFuture<ControlOut> {
        let mut t = TransferHandle::new(self.backend.clone(), 0, EndpointType::Control);
        t.submit::<ControlOut>(data);
        TransferFuture::new(t)
    }

    pub fn bulk_transfer(&self, endpoint: u8, buf: Vec<u8>) -> TransferFuture<Vec<u8>> {
        let mut t = TransferHandle::new(self.backend.clone(), endpoint, EndpointType::Bulk);
        t.submit(buf);
        TransferFuture::new(t)
    }

    pub fn bulk_queue(&self, endpoint: u8) -> Queue {
        Queue::new(self.backend.clone(), endpoint, EndpointType::Bulk)
    }

    pub fn interrupt_transfer(&self, endpoint: u8, buf: Vec<u8>) -> TransferFuture<Vec<u8>> {
        let mut t = TransferHandle::new(self.backend.clone(), endpoint, EndpointType::Interrupt);
        t.submit(buf);
        TransferFuture::new(t)
    }

    pub fn interrupt_queue(&self, endpoint: u8) -> Queue {
        Queue::new(self.backend.clone(), endpoint, EndpointType::Interrupt)
    }
}

pub struct Queue {
    interface: Arc<platform::Interface>,
    endpoint: u8,
    endpoint_type: EndpointType,

    /// A queue of pending transfers, expected to complete in order
    pending: VecDeque<TransferHandle<platform::Interface>>,

    /// An idle transfer that recently completed for re-use. Limiting
    cached: Option<TransferHandle<platform::Interface>>,
}

impl Queue {
    fn new(
        interface: Arc<platform::Interface>,
        endpoint: u8,
        endpoint_type: EndpointType,
    ) -> Queue {
        Queue {
            interface,
            endpoint,
            endpoint_type,
            pending: VecDeque::new(),
            cached: None,
        }
    }

    /// Submit a new transfer on the endpoint.
    ///
    /// For an IN endpoint, the transfer size is set by the *capacity* of
    /// the buffer, and the length and current contents are ignored. The
    /// buffer is returned from a later call to `complete` filled with
    /// the data read from the endpoint.
    ///
    /// For an OUT endpoint, the contents of the buffer are written to
    /// the endpoint.
    pub fn submit(&mut self, data: Buffer) {
        let mut transfer = self.cached.take().unwrap_or_else(|| {
            TransferHandle::new(self.interface.clone(), self.endpoint, self.endpoint_type)
        });
        transfer.submit(data);
        self.pending.push_back(transfer);
    }

    /// Block waiting for the next pending transfer to complete, and return
    /// its buffer or an error status.
    ///
    /// For an IN endpoint, the returned buffer contains the data
    /// read from the device.
    ///
    /// For an OUT endpoint, the buffer is unmodified, but can be
    /// reused for another transfer.
    ///
    /// Panics if there are no transfers pending.
    pub fn next_complete<'a>(&'a mut self) -> impl Future<Output = Completion<Vec<u8>>> + 'a {
        poll_fn(|cx| {
            let res = self
                .pending
                .front_mut()
                .expect("queue should have pending transfers when calling next_complete")
                .poll_completion::<Vec<u8>>(cx);
            if res.is_ready() {
                self.cached = self.pending.pop_front();
            }
            res
        })
    }

    /// Get the number of transfers that have been submitted with
    /// `submit` that have not yet been returned from `complete`.
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    /// Cancel all pending transfers. They will still be returned from `complete` so you can tell
    /// which were completed, partially-completed, or cancelled.
    pub fn cancel_all(&mut self) {
        // Cancel transfers in reverse order to ensure subsequent transfers can't complete
        // out of order while we're going through them.
        for transfer in self.pending.iter_mut().rev() {
            transfer.cancel();
        }
    }
}

impl Drop for Queue {
    fn drop(&mut self) {
        // Cancel transfers in reverse order to ensure subsequent transfers can't complete
        // out of order while we're going through them.
        self.pending.drain(..).rev().for_each(drop)
    }
}
