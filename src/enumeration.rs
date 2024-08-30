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

    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub(crate) busnum: u8,

    #[cfg(target_os = "windows")]
    pub(crate) instance_id: OsString,

    #[cfg(target_os = "windows")]
    pub(crate) location_paths: Vec<OsString>,

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

    pub(crate) bus_id: String,
    pub(crate) device_address: u8,
    pub(crate) port_chain: Vec<u8>,

    pub(crate) vendor_id: u16,
    pub(crate) product_id: u16,
    pub(crate) device_version: u16,

    pub(crate) class: u8,
    pub(crate) subclass: u8,
    pub(crate) protocol: u8,

    pub(crate) max_packet_size_0: u8,

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
                bus: self.busnum,
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
    #[cfg(target_os = "linux")]
    pub fn sysfs_path(&self) -> &std::path::Path {
        &self.path.0
    }

    /// *(Linux-only)* Bus number.
    ///
    /// On Linux, the `bus_id` is an integer and this provides the value as `u8`.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub fn busnum(&self) -> u8 {
        self.busnum
    }

    /// *(Windows-only)* Instance ID path of this device
    #[cfg(target_os = "windows")]
    pub fn instance_id(&self) -> &OsStr {
        &self.instance_id
    }

    /// *(Windows-only)* Location paths property
    #[cfg(target_os = "windows")]
    pub fn location_paths(&self) -> &[OsString] {
        &self.location_paths
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

    /// Path of port numbers identifying the port where the device is connected.
    ///
    /// Together with the bus ID, it identifies a physical port. The path is
    ///  expected to remain stable across device insertions or reboots.
    ///
    /// Since USB SuperSpeed is a separate topology from USB 2.0 speeds, a
    /// physical port may be identified differently depending on speed.
    pub fn port_chain(&self) -> &[u8] {
        &self.port_chain
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

    /// Identifier for the bus / host controller where the device is connected.
    pub fn bus_id(&self) -> &str {
        &self.bus_id
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

    /// Maximum packet size for endpoint zero.
    #[doc(alias = "bMaxPacketSize0")]
    pub fn max_packet_size_0(&self) -> u8 {
        self.max_packet_size_0
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

        s.field("bus_id", &self.bus_id)
            .field("device_address", &self.device_address)
            .field("port_chain", &format_args!("{:?}", self.port_chain))
            .field("vendor_id", &format_args!("0x{:04X}", self.vendor_id))
            .field("product_id", &format_args!("0x{:04X}", self.product_id))
            .field(
                "device_version",
                &format_args!("0x{:04X}", self.device_version),
            )
            .field("class", &format_args!("0x{:02X}", self.class))
            .field("subclass", &format_args!("0x{:02X}", self.subclass))
            .field("protocol", &format_args!("0x{:02X}", self.protocol))
            .field("max_packet_size_0", &self.max_packet_size_0)
            .field("speed", &self.speed)
            .field("manufacturer_string", &self.manufacturer_string)
            .field("product_string", &self.product_string)
            .field("serial_number", &self.serial_number);

        #[cfg(target_os = "linux")]
        {
            s.field("sysfs_path", &self.path);
        }
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            s.field("busnum", &self.busnum);
        }

        #[cfg(target_os = "windows")]
        {
            s.field("instance_id", &self.instance_id);
            s.field("parent_instance_id", &self.parent_instance_id);
            s.field("location_paths", &self.location_paths);
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

/// PCI device information for host controllers
#[derive(Clone)]
pub struct PciInfo {
    #[cfg(target_os = "linux")]
    pub(crate) path: SysfsPath,

    #[cfg(target_os = "windows")]
    pub(crate) instance_id: OsString,
    /// PCI vendor ID
    pub(crate) vendor_id: u16,
    /// PCI device ID
    pub(crate) device_id: u16,
    /// PCI hardware revision
    pub(crate) revision: Option<u16>,
    /// PCI subsystem vendor ID
    pub(crate) subsystem_vendor_id: Option<u16>,
    /// PCI subsystem device ID
    pub(crate) subsystem_device_id: Option<u16>,
}

impl PciInfo {
    /// *(Linux-only)* Sysfs path for the PCI device.
    #[cfg(target_os = "linux")]
    pub fn sysfs_path(&self) -> &std::path::Path {
        &self.path.0
    }

    /// *(Windows-only)* Instance ID path of this device
    #[cfg(target_os = "windows")]
    pub fn instance_id(&self) -> &OsStr {
        &self.instance_id
    }

    /// PCI vendor ID
    pub fn vendor_id(&self) -> u16 {
        self.vendor_id
    }

    /// PCI device ID
    pub fn device_id(&self) -> u16 {
        self.device_id
    }

    /// PCI hardware revision
    pub fn revision(&self) -> Option<u16> {
        self.revision
    }

    /// PCI subsystem vendor ID
    pub fn subsystem_vendor_id(&self) -> Option<u16> {
        self.subsystem_vendor_id
    }

    /// PCI subsystem device ID
    pub fn subsystem_device_id(&self) -> Option<u16> {
        self.subsystem_device_id
    }
}

impl std::fmt::Debug for PciInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut s = f.debug_struct("PciInfo");

        s.field("vendor_id", &format_args!("0x{:04X}", self.vendor_id))
            .field("device_id", &format_args!("0x{:04X}", self.device_id))
            .field("revision", &self.revision)
            .field("subsystem_vendor_id", &self.subsystem_vendor_id)
            .field("subsystem_device_id", &self.subsystem_device_id);

        #[cfg(target_os = "linux")]
        {
            s.field("sysfs_path", &self.path);
        }

        #[cfg(target_os = "windows")]
        {
            s.field("instance_id", &self.instance_id);
        }

        s.finish()
    }
}

/// USB host controller type
#[derive(Copy, Clone, Eq, PartialOrd, Ord, PartialEq, Hash, Debug)]
#[non_exhaustive]
pub enum UsbController {
    /// xHCI controller (USB 3.0+)
    XHCI,

    /// EHCI controller (USB 2.0)
    EHCI,

    /// OHCI controller (USB 1.1)
    OHCI,

    /// VHCI controller (virtual internal USB)
    VHCI,
}

impl UsbController {
    #[allow(dead_code)] // not used on all platforms
    pub(crate) fn from_str(s: &str) -> Option<Self> {
        match s
            .find("HCI")
            .filter(|i| *i > 0)
            .and_then(|i| s.bytes().nth(i - 1))
        {
            Some(b'x') | Some(b'X') => Some(UsbController::XHCI),
            Some(b'e') | Some(b'E') => Some(UsbController::EHCI),
            Some(b'o') | Some(b'O') => Some(UsbController::OHCI),
            Some(b'v') | Some(b'V') => Some(UsbController::VHCI),
            _ => None,
        }
    }
}

/// Information about a system USB bus. Aims to be a more useful and portable version of the Linux root hub device.
///
/// Platform-specific fields:
/// * Linux: `path`, `busnum`, `root_hub`
/// * Windows: `instance_id`, `location_paths`, `devinst`, `driver`
/// * macOS: `registry_id`, `location_id`, `name`, `driver`
pub struct BusInfo {
    #[cfg(target_os = "linux")]
    pub(crate) path: SysfsPath,

    /// The phony root hub device
    #[cfg(target_os = "linux")]
    pub(crate) root_hub: DeviceInfo,

    #[cfg(target_os = "linux")]
    pub(crate) busnum: u8,

    #[cfg(target_os = "windows")]
    pub(crate) instance_id: OsString,

    #[cfg(target_os = "windows")]
    pub(crate) location_paths: Vec<OsString>,

    #[cfg(target_os = "windows")]
    pub(crate) devinst: crate::platform::DevInst,

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    pub(crate) driver: Option<String>,

    #[cfg(target_os = "macos")]
    pub(crate) registry_id: u64,

    #[cfg(target_os = "macos")]
    pub(crate) location_id: u32,

    #[cfg(target_os = "macos")]
    pub(crate) name: Option<String>,

    /// System ID for the bus
    pub(crate) bus_id: String,

    /// Optional PCI information if the bus is connected to a PCI Host Controller
    pub(crate) pci_info: Option<PciInfo>,

    /// Detected USB controller type
    pub(crate) controller: Option<UsbController>,

    /// System provider class name for the bus
    pub(crate) provider_class: Option<String>,
    /// System class name for the bus
    pub(crate) class_name: Option<String>,
}

impl BusInfo {
    /// Opaque identifier for the device.
    pub fn id(&self) -> DeviceId {
        #[cfg(target_os = "windows")]
        {
            DeviceId(self.devinst)
        }

        #[cfg(target_os = "linux")]
        {
            self.root_hub.id()
        }

        #[cfg(target_os = "macos")]
        {
            DeviceId(self.registry_id)
        }
    }

    /// *(Linux-only)* Sysfs path for the bus.
    #[doc(hidden)]
    #[deprecated = "use `sysfs_path()` instead"]
    #[cfg(target_os = "linux")]
    pub fn path(&self) -> &SysfsPath {
        &self.path
    }

    /// *(Linux-only)* Sysfs path for the bus.
    #[cfg(target_os = "linux")]
    pub fn sysfs_path(&self) -> &std::path::Path {
        &self.path.0
    }

    /// *(Linux-only)* Bus number.
    ///
    /// On Linux, the `bus_id` is an integer and this provides the value as `u8`.
    #[cfg(target_os = "linux")]
    pub fn busnum(&self) -> u8 {
        self.busnum
    }

    /// *(Linux-only)* The root hub [`DeviceInfo`] representing the bus.
    #[cfg(target_os = "linux")]
    pub fn root_hub(&self) -> &DeviceInfo {
        &self.root_hub
    }

    /// *(Windows-only)* Instance ID path of this device
    #[cfg(target_os = "windows")]
    pub fn instance_id(&self) -> &OsStr {
        &self.instance_id
    }

    /// *(Windows-only)* Location paths property
    #[cfg(target_os = "windows")]
    pub fn location_paths(&self) -> &[OsString] {
        &self.location_paths
    }

    /// *(Windows/macOS-only)* Driver associated with the device as a whole
    #[cfg(any(target_os = "windows", target_os = "macos"))]
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

    /// Name of the bus
    #[cfg(target_os = "macos")]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Identifier for the bus
    pub fn bus_id(&self) -> &str {
        &self.bus_id
    }

    /// Optional PCI information if the bus is connected to a PCI Host Controller
    pub fn pci_info(&self) -> Option<&PciInfo> {
        self.pci_info.as_ref()
    }

    /// Detected USB controller type
    pub fn controller(&self) -> Option<UsbController> {
        self.controller
    }

    /// System provider class name for the bus
    pub fn provider_class(&self) -> Option<&str> {
        self.provider_class.as_deref()
    }

    /// System class name for the bus
    pub fn class_name(&self) -> Option<&str> {
        self.class_name.as_deref()
    }
}

impl std::fmt::Debug for BusInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut s = f.debug_struct("BusInfo");

        #[cfg(target_os = "linux")]
        {
            s.field("sysfs_path", &self.path);
            s.field("busnum", &self.busnum);
        }

        #[cfg(target_os = "windows")]
        {
            s.field("instance_id", &self.instance_id);
            s.field("location_paths", &self.location_paths);
            s.field("driver", &self.driver);
        }

        #[cfg(target_os = "macos")]
        {
            s.field("location_id", &format_args!("0x{:08X}", self.location_id));
            s.field(
                "registry_entry_id",
                &format_args!("0x{:08X}", self.registry_id),
            );
            s.field("name", &self.name);
            s.field("driver", &self.driver);
        }

        s.field("bus_id", &self.bus_id)
            .field("pci_info", &self.pci_info)
            .field("controller", &self.controller)
            .field("provider_class", &self.provider_class)
            .field("class_name", &self.class_name);

        s.finish()
    }
}
