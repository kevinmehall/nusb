//! Transfer-related types.
//!
//! Use the methods on an [`Interface`][`super::Interface`] and
//! [`Endpoint`][`super::Endpoint`] to perform transfers.

use std::{fmt::Display, io};

mod control;
#[allow(unused)]
pub(crate) use control::{request_type, SETUP_PACKET_SIZE};
pub use control::{ControlIn, ControlOut, ControlType, Direction, Recipient};

mod buffer;
pub(crate) use buffer::Allocator;
pub use buffer::Buffer;

pub(crate) mod internal;

use crate::{descriptors::TransferType, platform};

/// Transfer error.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum TransferError {
    /// Transfer was cancelled or timed out.
    Cancelled,

    /// Endpoint in a STALL condition.
    ///
    /// This is used by the device to signal that an error occurred. For bulk
    /// and interrupt endpoints, the stall condition can be cleared with
    /// [`Endpoint::clear_halt`][crate::Endpoint::clear_halt]. For control
    /// requests, the stall is automatically cleared when another request is
    /// submitted.
    Stall,

    /// Device disconnected.
    Disconnected,

    /// Hardware issue or protocol violation.
    Fault,

    /// The request has an invalid argument or is not supported by this OS.
    InvalidArgument,

    /// Unknown or OS-specific error.
    ///
    /// It won't be considered a breaking change to map unhandled errors from
    /// `Unknown` to one of the above variants. If you are matching on the
    /// OS-specific code because an error is not correctly mapped, please open
    /// an issue or pull request.
    Unknown(u32),
}

impl Display for TransferError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransferError::Cancelled => write!(f, "transfer was cancelled"),
            TransferError::Stall => write!(f, "endpoint stalled"),
            TransferError::Disconnected => write!(f, "device disconnected"),
            TransferError::Fault => write!(f, "hardware fault or protocol violation"),
            TransferError::InvalidArgument => write!(f, "invalid or unsupported argument"),
            TransferError::Unknown(e) => {
                write!(f, "unknown (")?;
                platform::format_os_error_code(f, *e)?;
                write!(f, ")")
            }
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
            TransferError::Fault => io::Error::other(value),
            TransferError::InvalidArgument => io::Error::new(io::ErrorKind::InvalidInput, value),
            TransferError::Unknown(_) => io::Error::other(value),
        }
    }
}

mod private {
    pub trait Sealed {}
}

/// Type-level endpoint direction
pub trait EndpointDirection: private::Sealed + Send + Sync {
    /// Runtime direction value
    const DIR: Direction;
}

/// Type-level endpoint direction: device-to-host
pub enum In {}
impl private::Sealed for In {}
impl EndpointDirection for In {
    const DIR: Direction = Direction::In;
}

/// Type-level endpoint direction: host-to-device
pub enum Out {}
impl private::Sealed for Out {}
impl EndpointDirection for Out {
    const DIR: Direction = Direction::Out;
}

/// Type-level endpoint direction
pub trait EndpointType: private::Sealed + Send + Sync + Unpin {
    /// Runtime direction value
    const TYPE: TransferType;
}

/// EndpointType for Bulk and interrupt endpoints.
pub trait BulkOrInterrupt: EndpointType {}

/// Type-level endpoint type: Bulk
pub enum Bulk {}
impl private::Sealed for Bulk {}
impl EndpointType for Bulk {
    const TYPE: TransferType = TransferType::Bulk;
}
impl BulkOrInterrupt for Bulk {}

/// Type-level endpoint type: Interrupt
pub enum Interrupt {}
impl private::Sealed for Interrupt {}
impl EndpointType for Interrupt {
    const TYPE: TransferType = TransferType::Interrupt;
}
impl BulkOrInterrupt for Interrupt {}

/// A completed transfer returned from [`Endpoint::next_complete`][`crate::Endpoint::next_complete`].
///
/// A transfer can partially complete even in the case of failure or
/// cancellation, thus the [`actual_len`][`Self::actual_len`] may be nonzero
/// even if the [`status`][`Self::status`] is an error.
#[derive(Debug)]
pub struct Completion {
    /// The transfer buffer.
    pub buffer: Buffer,

    /// The number of bytes transferred.
    pub actual_len: usize,

    /// Status of the transfer.
    pub status: Result<(), TransferError>,
}

impl Completion {
    /// Ignore any partial completion, turning `self` into a `Result` containing
    /// either the completed buffer for a successful transfer or a
    /// `TransferError`.
    pub fn into_result(self) -> Result<Buffer, TransferError> {
        self.status.map(|()| self.buffer)
    }
}
