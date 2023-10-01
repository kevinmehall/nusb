use std::io;

pub mod platform;
pub use platform::list_devices;

mod control;
pub use control::{ControlIn, ControlOut, ControlType, Direction, Recipient};

mod enumeration;
pub use enumeration::{DeviceInfo, Speed, UnknownValue};

mod device;
use device::Device;

mod transfer;
pub use transfer::{Completion, EndpointType, TransferFuture, TransferStatus};

mod transfer_internal;

pub type Error = io::Error;
