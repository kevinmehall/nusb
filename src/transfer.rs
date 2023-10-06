use crate::{
    platform,
    transfer_internal::{PlatformSubmit, TransferHandle, TransferRequest},
};
use std::{
    future::Future,
    marker::PhantomData,
    task::{Context, Poll},
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
