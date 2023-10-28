//! Transfer-related types.
//!
//! Use the methods on an [`Interface`][`super::Interface`] to make individual
//! transfers or obtain a [`Queue`] to manage multiple transfers.

use std::{
    fmt::Display,
    future::Future,
    io,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointType {
    /// Control endpoint.
    Control = 0,
    /// Isochronous endpoint.
    Isochronous = 1,
    /// Bulk endpoint.
    Bulk = 2,
    /// Interrupt endpoint.
    Interrupt = 3,
}

/// Transfer error.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum TransferError {
    /// Transfer was cancelled.
    Cancelled,

    /// Endpoint in a STALL condition.
    Stall,

    /// Device disconnected.
    Disconnected,

    /// Hardware issue or protocol violation.
    Fault,

    /// Unknown or OS-specific error.
    Unknown,
}

impl Display for TransferError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransferError::Cancelled => write!(f, "transfer was cancelled"),
            TransferError::Stall => write!(f, "endpoint STALL condition"),
            TransferError::Disconnected => write!(f, "device disconnected"),
            TransferError::Fault => write!(f, "hardware fault or protocol violation"),
            TransferError::Unknown => write!(f, "unknown error"),
        }
    }
}

impl std::error::Error for TransferError {}

impl From<TransferError> for io::Error {
    fn from(value: TransferError) -> Self {
        match value {
            TransferError::Cancelled => io::Error::new(io::ErrorKind::Interrupted, value),
            TransferError::Stall => io::Error::new(io::ErrorKind::ConnectionReset, value),
            TransferError::Disconnected => io::Error::new(io::ErrorKind::ConnectionAborted, value),
            TransferError::Fault => io::Error::new(io::ErrorKind::Other, value),
            TransferError::Unknown => io::Error::new(io::ErrorKind::Other, value),
        }
    }
}

/// Status and data returned on transfer completion.
///
/// A transfer can return partial data even in the case of failure or
/// cancellation, thus this is a struct containing both `data` and `status`
/// rather than a `Result`. Use [`into_result`][`Completion::into_result`] to
/// ignore a partial transfer and get a `Result`.
#[derive(Debug, Clone)]
#[must_use]
pub struct Completion<T> {
    /// Returned data or buffer to re-use.
    pub data: T,

    /// Indicates successful completion or error.
    pub status: Result<(), TransferError>,
}

impl<T> Completion<T> {
    /// Ignore any partial completion, turning `self` into a `Result` containing
    /// either the completed buffer for a successful transfer or a
    /// `TransferError`.
    pub fn into_result(self) -> Result<T, TransferError> {
        self.status.map(|()| self.data)
    }
}

impl TryFrom<Completion<Vec<u8>>> for Vec<u8> {
    type Error = TransferError;

    fn try_from(c: Completion<Vec<u8>>) -> Result<Self, Self::Error> {
        c.into_result()
    }
}

impl TryFrom<Completion<ResponseBuffer>> for ResponseBuffer {
    type Error = TransferError;

    fn try_from(c: Completion<ResponseBuffer>) -> Result<Self, Self::Error> {
        c.into_result()
    }
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
