#[cfg(target_os = "windows")]
use std::ffi::{OsStr, OsString};

#[cfg(any(target_os = "linux", target_os = "android"))]
use crate::platform::SysfsPath;

use crate::{Device, Error};

/// Opaque device identifier
#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub struct DeviceId(pub(crate) crate::platform::DeviceId);

/// Information about a device that can be obtained without opening it.
///
/// Found in the results of [`crate::list_devices`].
///
/// ### Platform-specific notes
///
/// * Some fields are platform-specific
///     * Linux: `sysfs_path`
///     * Windows: `instance_id`, `parent_instance_id`, `port_number`, `driver`
///     * macOS: `registry_id`, `location_id`
#[derive(Clone)]
pub struct DeviceInfo {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub(crate) path: SysfsPath,

    #[cfg(target_os = "windows")]
    pub(crate) instance_id: OsString,

    #[cfg(target_os = "windows")]
    pub(crate) parent_instance_id: OsString,

    #[cfg(target_os = "windows")]
    pub(crate) port_number: u32,

    #[cfg(target_os = "windows")]
    pub(crate) devinst: crate::platform::DevInst,

    #[cfg(target_os = "windows")]
    pub(crate) driver: Option<String>,

    #[cfg(target_os = "macos")]
    pub(crate) registry_id: u64,

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

    pub(crate) interfaces: Vec<InterfaceInfo>,
}

impl DeviceInfo {
    /// Opaque identifier for the device.
    pub fn id(&self) -> DeviceId {
        #[cfg(target_os = "windows")]
        {
            DeviceId(self.devinst)
        }

        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            DeviceId(crate::platform::DeviceId {
                bus: self.bus_number,
                addr: self.device_address,
            })
        }

        #[cfg(target_os = "macos")]
        {
            DeviceId(self.registry_id)
        }
    }

    /// *(Linux-only)* Sysfs path for the device.
    #[doc(hidden)]
    #[deprecated = "use `sysfs_path()` instead"]
    #[cfg(target_os = "linux")]
    pub fn path(&self) -> &SysfsPath {
        &self.path
    }

    /// *(Linux-only)* Sysfs path for the device.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub fn sysfs_path(&self) -> &std::path::Path {
        &self.path.0
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

    /// *(macOS-only)* IOKit [Registry Entry ID](https://developer.apple.com/documentation/iokit/1514719-ioregistryentrygetregistryentryi?language=objc)
    #[cfg(target_os = "macos")]
    pub fn registry_entry_id(&self) -> u64 {
        self.registry_id
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

    /// Manufacturer string, if available without device IO.
    ///
    /// ### Platform-specific notes
    ///  * Windows: Windows does not cache the manufacturer string, and
    ///    this will return `None` regardless of whether a descriptor exists.
    #[doc(alias = "iManufacturer")]
    pub fn manufacturer_string(&self) -> Option<&str> {
        self.manufacturer_string.as_deref()
    }

    /// Product string, if available without device IO.
    #[doc(alias = "iProduct")]
    pub fn product_string(&self) -> Option<&str> {
        self.product_string.as_deref()
    }

    /// Serial number string, if available without device IO.
    ///
    /// ### Platform-specific notes
    /// * On Windows, this comes from a case-insensitive instance ID and may
    /// have been converted to upper case from the descriptor string. It is
    /// recommended to use a [case-insensitive
    /// comparison][str::eq_ignore_ascii_case] when matching a device.
    #[doc(alias = "iSerial")]
    pub fn serial_number(&self) -> Option<&str> {
        self.serial_number.as_deref()
    }

    /// Iterator over the device's interfaces.
    ///
    /// This returns summary information about the interfaces in the device's
    /// active configuration for the purposes of matching devices prior to
    /// opening them.
    ///
    /// Additional information about interfaces can be found in the
    /// configuration descriptor after opening the device by calling
    /// [`Device::active_configuration`].
    ///
    /// ### Platform-specific notes:
    ///   * Windows: this is only available for composite devices bound to the
    ///     `usbccgp` driver, and will be empty if the entire device is bound to
    ///     a specific driver.
    ///   * Windows: When interfaces are grouped by an interface
    ///     association descriptor, this returns details from the interface
    ///     association descriptor and does not include each of the associated
    ///     interfaces.
    pub fn interfaces(&self) -> impl Iterator<Item = &InterfaceInfo> {
        self.interfaces.iter()
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
            .field("class", &format_args!("0x{:02X}", self.class))
            .field("subclass", &format_args!("0x{:02X}", self.subclass))
            .field("protocol", &format_args!("0x{:02X}", self.protocol))
            .field("speed", &self.speed)
            .field("manufacturer_string", &self.manufacturer_string)
            .field("product_string", &self.product_string)
            .field("serial_number", &self.serial_number);

        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            s.field("sysfs_path", &self.path);
        }

        #[cfg(target_os = "windows")]
        {
            s.field("instance_id", &self.instance_id);
            s.field("parent_instance_id", &self.parent_instance_id);
            s.field("port_number", &self.port_number);
            s.field("driver", &self.driver);
        }

        #[cfg(target_os = "macos")]
        {
            s.field("location_id", &format_args!("0x{:08X}", self.location_id));
            s.field(
                "registry_entry_id",
                &format_args!("0x{:08X}", self.registry_id),
            );
        }

        s.field("interfaces", &self.interfaces);

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
    #[allow(dead_code)] // not used on all platforms
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

/// Summary information about a device's interface, available before opening a device.
#[derive(Clone)]
pub struct InterfaceInfo {
    pub(crate) interface_number: u8,
    pub(crate) class: u8,
    pub(crate) subclass: u8,
    pub(crate) protocol: u8,
    pub(crate) interface_string: Option<String>,
}

impl InterfaceInfo {
    /// Identifier for the interface from the `bInterfaceNumber` descriptor field.
    pub fn interface_number(&self) -> u8 {
        self.interface_number
    }

    /// Code identifying the standard interface class, from the `bInterfaceClass` interface descriptor field.
    pub fn class(&self) -> u8 {
        self.class
    }

    /// Standard subclass, from the `bInterfaceSubClass` interface descriptor field.
    pub fn subclass(&self) -> u8 {
        self.subclass
    }

    /// Standard protocol, from the `bInterfaceProtocol` interface descriptor field.
    pub fn protocol(&self) -> u8 {
        self.protocol
    }

    /// Interface string descriptor value as cached by the OS.
    pub fn interface_string(&self) -> Option<&str> {
        self.interface_string.as_deref()
    }
}

// Not derived so that we can format some fields in hex
impl std::fmt::Debug for InterfaceInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InterfaceInfo")
            .field("interface_number", &self.interface_number)
            .field("class", &format_args!("0x{:02X}", self.class))
            .field("subclass", &format_args!("0x{:02X}", self.subclass))
            .field("protocol", &format_args!("0x{:02X}", self.protocol))
            .field("interface_string", &self.interface_string)
            .finish()
    }
}
