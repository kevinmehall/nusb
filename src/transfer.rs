pub use crate::platform::Transfer;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum EndpointType {
    Control,
    Interrupt,
    Bulk,
    Isochronous,
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
pub struct Completion {
    pub data: Vec<u8>,
    pub status: TransferStatus,
}
