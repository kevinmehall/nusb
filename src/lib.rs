use std::io;

pub mod platform;
pub use platform::list_devices;

mod enumeration;
pub use enumeration::{DeviceInfo, Speed, UnknownValue};

mod device;
use device::Device;

pub mod transfer;

pub type Error = io::Error;
