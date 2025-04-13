//! Transfer-related types.
//!
//! Use the methods on an [`Interface`][`super::Interface`] to make individual
//! transfers or obtain a [`Queue`] to manage multiple transfers.

use std::{fmt::Display, io};

mod control;
#[allow(unused)]
pub(crate) use control::{request_type, SETUP_PACKET_SIZE};
pub use control::{ControlIn, ControlOut, ControlType, Direction, Recipient};

pub(crate) mod internal;

use crate::descriptors::TransferType;

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
pub trait EndpointType: private::Sealed + Send + Sync {
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

pub use crate::device::{Completion, Request};
