use std::{
    collections::VecDeque,
    future::{poll_fn, Future},
    marker::PhantomData,
    sync::Arc,
};

use crate::{
    platform,
    transfer::{
        Completion, ControlIn, ControlOut, EndpointType, PlatformSubmit, RequestBuffer,
        TransferFuture, TransferHandle, TransferRequest,
    },
    DeviceInfo, Error,
};

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

    pub fn control_in(&self, data: ControlIn) -> TransferFuture<ControlIn> {
        let mut t = self.backend.make_transfer(0, EndpointType::Control);
        t.submit::<ControlIn>(data);
        TransferFuture::new(t)
    }

    pub fn control_out(&self, data: ControlOut) -> TransferFuture<ControlOut> {
        let mut t = self.backend.make_transfer(0, EndpointType::Control);
        t.submit::<ControlOut>(data);
        TransferFuture::new(t)
    }

    pub fn bulk_in(&self, endpoint: u8, buf: RequestBuffer) -> TransferFuture<RequestBuffer> {
        let mut t = self.backend.make_transfer(endpoint, EndpointType::Bulk);
        t.submit(buf);
        TransferFuture::new(t)
    }

    pub fn bulk_out(&self, endpoint: u8, buf: Vec<u8>) -> TransferFuture<Vec<u8>> {
        let mut t = self.backend.make_transfer(endpoint, EndpointType::Bulk);
        t.submit(buf);
        TransferFuture::new(t)
    }

    pub fn bulk_in_queue(&self, endpoint: u8) -> Queue<RequestBuffer> {
        Queue::new(self.backend.clone(), endpoint, EndpointType::Bulk)
    }

    pub fn bulk_out_queue(&self, endpoint: u8) -> Queue<Vec<u8>> {
        Queue::new(self.backend.clone(), endpoint, EndpointType::Bulk)
    }

    pub fn interrupt_in(&self, endpoint: u8, buf: RequestBuffer) -> TransferFuture<RequestBuffer> {
        let mut t = self
            .backend
            .make_transfer(endpoint, EndpointType::Interrupt);
        t.submit(buf);
        TransferFuture::new(t)
    }

    pub fn interrupt_out(&self, endpoint: u8, buf: Vec<u8>) -> TransferFuture<Vec<u8>> {
        let mut t = self
            .backend
            .make_transfer(endpoint, EndpointType::Interrupt);
        t.submit(buf);
        TransferFuture::new(t)
    }

    pub fn interrupt_in_queue(&self, endpoint: u8) -> Queue<RequestBuffer> {
        Queue::new(self.backend.clone(), endpoint, EndpointType::Interrupt)
    }

    pub fn interrupt_out_queue(&self, endpoint: u8) -> Queue<Vec<u8>> {
        Queue::new(self.backend.clone(), endpoint, EndpointType::Interrupt)
    }
}

pub struct Queue<R: TransferRequest> {
    interface: Arc<platform::Interface>,
    endpoint: u8,
    endpoint_type: EndpointType,

    /// A queue of pending transfers, expected to complete in order
    pending: VecDeque<TransferHandle<platform::TransferData>>,

    /// An idle transfer that recently completed for re-use. Limiting
    cached: Option<TransferHandle<platform::TransferData>>,

    bufs: PhantomData<R>,
}

impl<R> Queue<R>
where
    R: TransferRequest,
    platform::TransferData: PlatformSubmit<R>,
{
    fn new(
        interface: Arc<platform::Interface>,
        endpoint: u8,
        endpoint_type: EndpointType,
    ) -> Queue<R> {
        Queue {
            interface,
            endpoint,
            endpoint_type,
            pending: VecDeque::new(),
            cached: None,
            bufs: PhantomData,
        }
    }

    /// Submit a new transfer on the endpoint.
    pub fn submit(&mut self, data: R) {
        let mut transfer = self.cached.take().unwrap_or_else(|| {
            self.interface
                .make_transfer(self.endpoint, self.endpoint_type)
        });
        transfer.submit(data);
        self.pending.push_back(transfer);
    }

    /// Block waiting for the next pending transfer to complete, and return
    /// its buffer or an error status.
    ///
    /// Panics if there are no transfers pending.
    pub fn next_complete<'a>(&'a mut self) -> impl Future<Output = Completion<R::Response>> + 'a {
        poll_fn(|cx| {
            let res = self
                .pending
                .front_mut()
                .expect("queue should have pending transfers when calling next_complete")
                .poll_completion::<R>(cx);
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

impl<R: TransferRequest> Drop for Queue<R> {
    fn drop(&mut self) {
        // Cancel transfers in reverse order to ensure subsequent transfers can't complete
        // out of order while we're going through them.
        self.pending.drain(..).rev().for_each(drop)
    }
}
