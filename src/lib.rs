use std::{fmt::Display, io, str::FromStr};

pub mod platform;

use device::Device;
pub use platform::list_devices;

mod enumeration;
pub use enumeration::{DeviceInfo, Speed, UnknownValue};

mod device;

mod transfer;
pub use transfer::{Completion, Transfer, TransferStatus};

pub type Error = io::Error;
