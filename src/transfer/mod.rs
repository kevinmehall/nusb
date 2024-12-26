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
#[allow(unused)]
pub(crate) use control::SETUP_PACKET_SIZE;
pub use control::{Control, ControlIn, ControlOut, ControlType, Direction, Recipient};

mod internal;
#[cfg(target_arch = "wasm32")]
pub(crate) use internal::TransferInner;
pub(crate) use internal::{
    notify_completion, PlatformSubmit, PlatformTransfer, TransferHandle, TransferRequest,
};

/// Endpoint type.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[allow(dead_code)]
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
    ///
    /// This is used by the device to signal that an error occurred. For bulk
    /// and interrupt endpoints, the stall condition can be cleared with
    /// [`Interface::clear_halt`][crate::Interface::clear_halt]. For control
    /// requests, the stall is automatically cleared when another request is
    /// submitted.
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

#[cfg(target_arch = "wasm32")]
pub(crate) fn web_to_nusb_status(status: web_sys::UsbTransferStatus) -> Result<(), TransferError> {
    match status {
        web_sys::UsbTransferStatus::Ok => Ok(()),
        web_sys::UsbTransferStatus::Stall => Err(TransferError::Stall),
        web_sys::UsbTransferStatus::Babble => Err(TransferError::Unknown),
        _ => unreachable!(),
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
/// Use the methods on [`Interface`][super::Interface] to
/// submit an individual transfer and obtain a `TransferFuture`.
///
/// The transfer is cancelled on drop. The buffer and
/// any partially-completed data are destroyed. This means
/// that `TransferFuture` is not [cancel-safe] and cannot be used
/// in `select!{}`, When racing a `TransferFuture` with a timeout
/// you cannot tell whether data may have been partially transferred on timeout.
/// Use the [`Queue`] interface if these matter for your application.
///
/// [cancel-safe]: https://docs.rs/tokio/latest/tokio/macro.select.html#cancellation-safety
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
