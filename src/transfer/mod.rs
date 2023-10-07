//! Transfer-related types.
//! 
//! Use the methods on an [`Interface`][`super::Interface`] to make individual
//! transfers or obtain a [`Queue`] to manage multiple transfers.

use std::{
    future::Future,
    marker::PhantomData,
    task::{Context, Poll},
};

use crate::platform;

mod queue;
pub use queue::Queue;

mod buffer;
pub use buffer::{RequestBuffer, ResponseBuffer};

mod control;
pub(crate) use control::SETUP_PACKET_SIZE;
pub use control::{ControlIn, ControlOut, ControlType, Direction, Recipient};

mod internal;
pub(crate) use internal::{
    notify_completion, PlatformSubmit, PlatformTransfer, TransferHandle, TransferRequest,
};

/// Endpoint type.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum EndpointType {
    Control = 0,
    Isochronous = 1,
    Bulk = 2,
    Interrupt = 3,
}

/// Transfer status.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum TransferStatus {
    /// Transfer completed successfully.
    Complete,

    /// Transfer was cancelled.
    Cancelled,

    /// Endpoint in a STALL condition.
    Stall,

    /// Device disconnected.
    Disconnected,

    /// Hardware issue or protocol violation.
    Fault,

    /// Unknown or OS-specific error.
    UnknownError,
}

/// Status and data returned on transfer completion.
///
/// A transfer can return partial data even in the case of failure or
/// cancellation, thus this is a struct containing both rather than a `Result`.
#[derive(Debug, Clone)]
pub struct Completion<T> {
    /// Returned data or buffer to re-use.
    pub data: T,

    /// Indicates successful completion or error.
    pub status: TransferStatus,
}

/// [`Future`] used to await the completion of a transfer.
///
/// The transfer is cancelled on drop. The buffer and
/// any partially-completed data are destroyed.
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
