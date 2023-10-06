use std::{
    future::Future,
    marker::PhantomData,
    task::{Context, Poll},
};

use crate::platform;

mod buffer;
pub use buffer::{RequestBuffer, ResponseBuffer};

mod control;
pub(crate) use control::SETUP_PACKET_SIZE;
pub use control::{ControlIn, ControlOut, ControlType, Direction, Recipient};

mod internal;
pub(crate) use internal::{
    notify_completion, PlatformSubmit, PlatformTransfer, TransferHandle, TransferRequest,
};

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum EndpointType {
    Control = 0,
    Isochronous = 1,
    Bulk = 2,
    Interrupt = 3,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum TransferStatus {
    Complete,
    Cancelled,
    Stall,
    Disconnected,
    Fault,
    UnknownError,
}

#[derive(Debug, Clone)]
pub struct Completion<T> {
    pub data: T,
    pub status: TransferStatus,
}

pub struct TransferFuture<D: TransferRequest> {
    transfer: TransferHandle<platform::TransferData>,
    ty: PhantomData<D::Response>,
}

impl<D: TransferRequest> TransferFuture<D> {
    pub(crate) fn new(transfer: TransferHandle<platform::TransferData>) -> TransferFuture<D> {
        TransferFuture {
            transfer,
            ty: PhantomData,
        }
    }
}

impl<D: TransferRequest> Future for TransferFuture<D>
where
    platform::TransferData: PlatformSubmit<D>,
    D::Response: Unpin,
{
    type Output = Completion<D::Response>;

    fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.as_mut().transfer.poll_completion::<D>(cx)
    }
}
