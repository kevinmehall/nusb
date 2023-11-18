#[cfg(target_os = "windows")]
use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
};

#[cfg(target_os = "linux")]
use crate::platform::SysfsPath;

use crate::{Device, Error};

/// Information about a device that can be obtained without opening it.
///
/// Found in the results of [`crate::list_devices`].
///
/// ### Platform-specific notes
///
/// * Some fields are platform-specific
///     * Linux: `path`
///     * Windows: `instance_id`, `parent_instance_id`, `port_number`, `driver`
#[derive(Clone)]
pub struct DeviceInfo {
    #[cfg(target_os = "linux")]
    pub(crate) path: SysfsPath,

    #[cfg(target_os = "windows")]
    pub(crate) instance_id: OsString,

    #[cfg(target_os = "windows")]
    pub(crate) parent_instance_id: OsString,

    #[cfg(target_os = "windows")]
    pub(crate) port_number: u32,

    #[cfg(target_os = "windows")]
    pub(crate) driver: Option<String>,

    #[cfg(target_os = "windows")]
    pub(crate) interfaces: HashMap<u8, OsString>,

    #[cfg(target_os = "macos")]
    pub(crate) location_id: u32,

    pub(crate) bus_number: u8,
    pub(crate) device_address: u8,

    pub(crate) vendor_id: u16,
    pub(crate) product_id: u16,
    pub(crate) device_version: u16,

    pub(crate) class: u8,
    pub(crate) subclass: u8,
    pub(crate) protocol: u8,

    pub(crate) speed: Option<Speed>,

    pub(crate) manufacturer_string: Option<String>,
    pub(crate) product_string: Option<String>,
    pub(crate) serial_number: Option<String>,
}

impl DeviceInfo {
    /// *(Linux-only)* Sysfs path for the device.
    #[cfg(target_os = "linux")]
    pub fn path(&self) -> &SysfsPath {
        &self.path
    }

    /// *(Windows-only)* Instance ID path of this device
    #[cfg(target_os = "windows")]
    pub fn instance_id(&self) -> &OsStr {
        &self.instance_id
    }

    /// *(Windows-only)* Instance ID path of the parent hub
    #[cfg(target_os = "windows")]
    pub fn parent_instance_id(&self) -> &OsStr {
        &self.parent_instance_id
    }

    /// *(Windows-only)* Port number
    #[cfg(target_os = "windows")]
    pub fn port_number(&self) -> u32 {
        self.port_number
    }

    /// *(Windows-only)* Driver associated with the device as a whole
    #[cfg(target_os = "windows")]
    pub fn driver(&self) -> Option<&str> {
        self.driver.as_deref()
    }

    /// *(macOS-only)* IOKit Location ID
    #[cfg(target_os = "macos")]
    pub fn location_id(&self) -> u32 {
        self.location_id
    }

    /// Number identifying the bus / host controller where the device is connected.
    pub fn bus_number(&self) -> u8 {
        self.bus_number
    }

    /// Number identifying the device within the bus.
    pub fn device_address(&self) -> u8 {
        self.device_address
    }

    /// The 16-bit number identifying the device's vendor, from the `idVendor` device descriptor field.
    #[doc(alias = "idVendor")]
    pub fn vendor_id(&self) -> u16 {
        self.vendor_id
    }

    /// The 16-bit number identifying the product, from the `idProduct` device descriptor field.
    #[doc(alias = "idProduct")]
    pub fn product_id(&self) -> u16 {
        self.product_id
    }

    /// The device version, normally encoded as BCD, from the `bcdDevice` device descriptor field.
    #[doc(alias = "bcdDevice")]
    pub fn device_version(&self) -> u16 {
        self.device_version
    }

    /// Code identifying the standard device class, from the `bDeviceClass` device descriptor field.
    ///
    /// `0x00`: specified at the interface level.\
    /// `0xFF`: vendor-defined.
    #[doc(alias = "bDeviceClass")]
    pub fn class(&self) -> u8 {
        self.class
    }

    /// Standard subclass, from the `bDeviceSubClass` device descriptor field.
    #[doc(alias = "bDeviceSubClass")]
    pub fn subclass(&self) -> u8 {
        self.subclass
    }

    /// Standard protocol, from the `bDeviceProtocol` device descriptor field.
    #[doc(alias = "bDeviceProtocol")]
    pub fn protocol(&self) -> u8 {
        self.protocol
    }

    /// Connection speed
    pub fn speed(&self) -> Option<Speed> {
        self.speed
    }

    /// Manufacturer string, if available without device IO
    #[doc(alias = "iManufacturer")]
    pub fn manufacturer_string(&self) -> Option<&str> {
        self.manufacturer_string.as_deref()
    }

    /// Product string, if available without device IO
    #[doc(alias = "iProduct")]
    pub fn product_string(&self) -> Option<&str> {
        self.product_string.as_deref()
    }

    /// Serial number string, if available without device IO
    #[doc(alias = "iSerial")]
    pub fn serial_number(&self) -> Option<&str> {
        self.serial_number.as_deref()
    }

    /// Open the device
    pub fn open(&self) -> Result<Device, Error> {
        Device::open(self)
    }
}

// Not derived so that we can format some fields in hex
impl std::fmt::Debug for DeviceInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut s = f.debug_struct("DeviceInfo");

        s.field("bus_number", &self.bus_number)
            .field("device_address", &self.device_address)
            .field("vendor_id", &format_args!("0x{:04X}", self.vendor_id))
            .field("product_id", &format_args!("0x{:04X}", self.product_id))
            .field(
                "device_version",
                &format_args!("0x{:04X}", self.device_version),
            )
            .field("class", &self.class)
            .field("subclass", &self.subclass)
            .field("protocol", &self.protocol)
            .field("speed", &self.speed)
            .field("manufacturer_string", &self.manufacturer_string)
            .field("product_string", &self.product_string)
            .field("serial_number", &self.serial_number);

        #[cfg(target_os = "linux")]
        {
            s.field("path", &self.path);
        }

        #[cfg(target_os = "windows")]
        {
            s.field("instance_id", &self.instance_id)
                .field("parent_instance_id", &self.parent_instance_id)
                .field("port_number", &self.port_number)
                .field("driver", &self.driver)
                .field("interfaces", &self.interfaces);
        }

        s.finish()
    }
}

/// USB connection speed
#[derive(Copy, Clone, Eq, PartialOrd, Ord, PartialEq, Hash, Debug)]
#[non_exhaustive]
pub enum Speed {
    /// Low speed (1.5 Mbit)
    Low,

    /// Full speed (12 Mbit)
    Full,

    /// High speed (480 Mbit)
    High,

    /// Super speed (5000 Mbit)
    Super,

    /// Super speed (10000 Mbit)
    SuperPlus,
}

impl Speed {
    pub(crate) fn from_str(s: &str) -> Option<Self> {
        match s {
            "low" | "1.5" => Some(Speed::Low),
            "full" | "12" => Some(Speed::Full),
            "high" | "480" => Some(Speed::High),
            "super" | "5000" => Some(Speed::Super),
            "super+" | "10000" => Some(Speed::SuperPlus),
            _ => None,
        }
    }
}
