use std::{fmt::Display, str::FromStr};

use crate::{platform, Device, Error};

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    #[cfg(target_os = "linux")]
    pub(crate) path: crate::platform::SysfsPath,

    pub(crate) bus_number: u8,
    pub(crate) device_address: u8,

    pub(crate) vendor_id: u16,
    pub(crate) product_id: u16,

    pub(crate) device_version: u16,
    pub(crate) class: u8,
    pub(crate) subclass: u8,
    pub(crate) protocol: u8,

    pub(crate) speed: Speed,

    pub(crate) manufacturer_string: Option<String>,
    pub(crate) product_string: Option<String>,
    pub(crate) serial_number: Option<String>,
}

impl DeviceInfo {
    #[cfg(target_os = "linux")]
    pub fn path(&self) -> &platform::SysfsPath {
        &self.path
    }

    pub fn bus_number(&self) -> u8 {
        self.bus_number
    }
    pub fn device_address(&self) -> u8 {
        self.device_address
    }

    pub fn vendor_id(&self) -> u16 {
        self.vendor_id
    }
    pub fn product_id(&self) -> u16 {
        self.product_id
    }

    pub fn device_version(&self) -> u16 {
        self.device_version
    }

    pub fn class(&self) -> u8 {
        self.class
    }
    pub fn subclass(&self) -> u8 {
        self.subclass
    }
    pub fn protocol(&self) -> u8 {
        self.protocol
    }

    pub fn speed(&self) -> Speed {
        self.speed
    }

    pub fn manufacturer_string(&self) -> Option<&str> {
        self.manufacturer_string.as_deref()
    }
    pub fn product_string(&self) -> Option<&str> {
        self.product_string.as_deref()
    }
    pub fn serial_number(&self) -> Option<&str> {
        self.serial_number.as_deref()
    }

    pub fn open(&self) -> Result<Device, Error> {
        Device::open(self)
    }
}

#[derive(Copy, Clone, Eq, PartialOrd, Ord, PartialEq, Hash, Debug)]
#[non_exhaustive]
pub enum Speed {
    Low,
    Full,
    High,
    Super,
    SuperPlus,
}

#[derive(Copy, Clone, Debug)]
pub struct UnknownValue;

impl Display for UnknownValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Unknown value")
    }
}

impl std::error::Error for UnknownValue {}

impl FromStr for Speed {
    type Err = UnknownValue; //TODO

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "low" | "1.5" => Ok(Speed::Low),
            "full" | "12" => Ok(Speed::Full),
            "high" | "480" => Ok(Speed::High),
            "super" | "5000" => Ok(Speed::Super),
            "super+" | "10000" => Ok(Speed::SuperPlus),
            _ => Err(UnknownValue),
        }
    }
}
