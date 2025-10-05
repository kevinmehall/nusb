#[cfg(target_os = "windows")]
use std::ffi::{OsStr, OsString};

#[cfg(target_os = "android")]
use std::sync::{Arc, OnceLock};

#[cfg(any(target_os = "linux"))]
use crate::platform::SysfsPath;

use crate::{Device, Error, MaybeFuture};

/// Opaque device identifier.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Hash)]
pub struct DeviceId(pub(crate) crate::platform::DeviceId);

/// Information about a device that can be obtained without opening it.
///
/// `DeviceInfo` is returned by [`list_devices`][crate::list_devices].
///
/// ### Platform-specific notes
///
/// * Some fields are platform-specific
///     * Linux: `sysfs_path`, `busnum`
///     * Windows: `instance_id`, `parent_instance_id`, `port_number`, `driver`
///     * macOS: `registry_id`, `location_id`
///     * Android: `port_chain`, `device_version`, `bus_id`, `device_address`, `speed` are unavailable
#[derive(Clone)]
pub struct DeviceInfo {
    #[cfg(target_os = "linux")]
    pub(crate) path: SysfsPath,

    #[cfg(target_os = "linux")]
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

    #[cfg(target_os = "android")]
    pub(crate) jni_global_ref: crate::platform::JniGlobalRef,

    #[cfg(target_os = "macos")]
    pub(crate) registry_id: u64,

    #[cfg(target_os = "macos")]
    pub(crate) location_id: u32,

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    pub(crate) bus_id: String,

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows",))]
    pub(crate) device_address: u8,

    #[cfg(target_os = "android")]
    pub(crate) device_id: i32,

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    pub(crate) port_chain: Vec<u8>,

    pub(crate) vendor_id: u16,
    pub(crate) product_id: u16,

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    pub(crate) device_version: u16,

    pub(crate) usb_version: u16,
    pub(crate) class: u8,
    pub(crate) subclass: u8,
    pub(crate) protocol: u8,

    pub(crate) speed: Option<Speed>,

    pub(crate) manufacturer_string: Option<String>,
    pub(crate) product_string: Option<String>,

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    pub(crate) serial_number: Option<String>,

    #[cfg(target_os = "android")]
    pub(crate) serial_number: Arc<OnceLock<String>>,

    pub(crate) interfaces: Vec<InterfaceInfo>,
}

impl DeviceInfo {
    /// Opaque identifier for the device.
    pub fn id(&self) -> DeviceId {
        #[cfg(target_os = "windows")]
        {
            DeviceId(self.devinst)
        }

        #[cfg(target_os = "linux")]
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

        #[cfg(target_os = "android")]
        {
            DeviceId(self.device_id)
        }
    }

    /// *(Linux-only)* Sysfs path for the device.
    #[cfg(target_os = "linux")]
    pub fn sysfs_path(&self) -> &std::path::Path {
        &self.path.0
    }

    /// *(Linux-only)* Bus number.
    ///
    /// On Linux, the `bus_id` is an integer and this provides the value as `u8`.
    #[cfg(any(target_os = "linux"))]
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

    /// *(Not available on Android)* Path of port numbers identifying the port where
    /// the device is connected.
    ///
    /// Together with the bus ID, it identifies a physical port. The path is
    /// expected to remain stable across device insertions or reboots.
    ///
    /// Since USB SuperSpeed is a separate topology from USB 2.0 speeds, a
    /// physical port may be identified differently depending on speed.
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
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

    /// *(Not available on Android)* Identifier for the bus / host controller where the device is connected.
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    pub fn bus_id(&self) -> &str {
        &self.bus_id
    }

    /// *(Not available on Android)* Number identifying the device within the bus.
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
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

    /// *(Not available on Android)* The device version, normally encoded as BCD, from the `bcdDevice`
    /// device descriptor field.
    #[doc(alias = "bcdDevice")]
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
    pub fn device_version(&self) -> u16 {
        self.device_version
    }

    /// Encoded version of the USB specification, from the `bcdUSB` device descriptor field.
    #[doc(alias = "bcdUSB")]
    pub fn usb_version(&self) -> u16 {
        self.usb_version
    }

    /// Code identifying the [standard device
    /// class](https://www.usb.org/defined-class-codes), from the `bDeviceClass`
    /// device descriptor field.
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

    /// *(Not available on Android)* Connection speed.
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
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
    ///  * Android: Starting from Android 10, this can not be read without permission of opening the device.
    ///    See <https://developer.android.com/about/versions/10/privacy/changes?hl=en#usb-serial>.
    #[doc(alias = "iSerial")]
    pub fn serial_number(&self) -> Option<&str> {
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
        {
            self.serial_number.as_deref()
        }
        #[cfg(target_os = "android")]
        {
            self.get_serial_number()
        }
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

    /// Opens the device.
    ///
    /// ### Platform-specific notes
    ///
    /// * On Android, `DeviceInfo::request_permission` must be called if the permission
    ///   has not been granted.
    pub fn open(&self) -> impl MaybeFuture<Output = Result<Device, Error>> {
        Device::open(self)
    }

    /// *(Android-only)* Checks if the caller has permission to access the device.
    #[cfg(target_os = "android")]
    pub fn has_permission(&self) -> Result<bool, Error> {
        crate::platform::has_permission(self)
    }

    /// *(Android-only)* Performs a permission request for the device.
    ///
    /// Returns `Ok(None)` if the permission is already granted; otherwise it returns
    /// a `PermissionRequest`.
    ///
    /// The current Android activity may be paused by `UsbManager.requestPermission()`
    /// called here, and resumed on receving result.
    ///
    /// Blocking on such a request in the UI thread or the native activity's main
    /// event thread may block forever. Please avoid blocking in such a thread if
    /// `DeviceInfo::has_permission` returns `false`. Status of the `PermissionRequest`
    /// can be checked on the activity resume event.
    #[cfg(target_os = "android")]
    pub fn request_permission(&self) -> Result<Option<crate::PermissionRequest>, Error> {
        crate::platform::request_permission(self)
    }
}

// Not derived so that we can format some fields in hex
impl std::fmt::Debug for DeviceInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut s = f.debug_struct("DeviceInfo");

        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
        s.field("bus_id", &self.bus_id)
            .field("device_address", &self.device_address)
            .field("port_chain", &format_args!("{:?}", self.port_chain));

        #[cfg(target_os = "android")]
        {
            s.field("device_id", &self.device_id);
        }

        s.field("vendor_id", &format_args!("0x{:04X}", self.vendor_id))
            .field("product_id", &format_args!("0x{:04X}", self.product_id));

        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
        s.field(
            "device_version",
            &format_args!("0x{:04X}", self.device_version),
        );

        s.field("usb_version", &format_args!("0x{:04X}", self.usb_version))
            .field("class", &format_args!("0x{:02X}", self.class))
            .field("subclass", &format_args!("0x{:02X}", self.subclass))
            .field("protocol", &format_args!("0x{:02X}", self.protocol))
            .field("speed", &self.speed)
            .field("manufacturer_string", &self.manufacturer_string)
            .field("product_string", &self.product_string)
            .field("serial_number", &self.serial_number());

        #[cfg(target_os = "linux")]
        {
            s.field("sysfs_path", &self.path);
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

/// USB host controller type
#[derive(Copy, Clone, Eq, PartialOrd, Ord, PartialEq, Hash, Debug)]
#[non_exhaustive]
pub enum UsbControllerType {
    /// xHCI controller (USB 3.0+)
    XHCI,

    /// EHCI controller (USB 2.0)
    EHCI,

    /// OHCI controller (USB 1.1)
    OHCI,

    /// UHCI controller (USB 1.x) (proprietary interface created by Intel)
    UHCI,

    /// VHCI controller (virtual internal USB)
    VHCI,
}

impl UsbControllerType {
    #[allow(dead_code)] // not used on all platforms
    pub(crate) fn from_str(s: &str) -> Option<Self> {
        let lower_s = s.to_owned().to_ascii_lowercase();
        match lower_s
            .find("hci")
            .filter(|i| *i > 0)
            .and_then(|i| lower_s.as_bytes().get(i - 1))
        {
            Some(b'x') => Some(UsbControllerType::XHCI),
            Some(b'e') => Some(UsbControllerType::EHCI),
            Some(b'o') => Some(UsbControllerType::OHCI),
            Some(b'v') => Some(UsbControllerType::VHCI),
            Some(b'u') => Some(UsbControllerType::UHCI),
            _ => None,
        }
    }
}

/// Information about a system USB bus.
///
/// Platform-specific fields:
/// * Linux: `path`, `busnum`, `root_hub`
/// * Windows: `instance_id`, `parent_instance_id`, `location_paths`, `devinst`, `root_hub_description`
/// * macOS: `registry_id`, `location_id`, `name`, `provider_class_name`, `class_name`
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub struct BusInfo {
    #[cfg(any(target_os = "linux"))]
    pub(crate) path: SysfsPath,

    /// The phony root hub device
    #[cfg(any(target_os = "linux"))]
    pub(crate) root_hub: DeviceInfo,

    #[cfg(any(target_os = "linux"))]
    pub(crate) busnum: u8,

    #[cfg(target_os = "windows")]
    pub(crate) instance_id: OsString,

    #[cfg(target_os = "windows")]
    pub(crate) location_paths: Vec<OsString>,

    #[cfg(target_os = "windows")]
    pub(crate) devinst: crate::platform::DevInst,

    #[cfg(target_os = "windows")]
    pub(crate) root_hub_description: String,

    #[cfg(target_os = "windows")]
    pub(crate) parent_instance_id: OsString,

    #[cfg(target_os = "macos")]
    pub(crate) registry_id: u64,

    #[cfg(target_os = "macos")]
    pub(crate) location_id: u32,

    #[cfg(target_os = "macos")]
    pub(crate) provider_class_name: String,

    #[cfg(target_os = "macos")]
    pub(crate) class_name: String,

    #[cfg(target_os = "macos")]
    pub(crate) name: Option<String>,

    pub(crate) driver: Option<String>,

    /// System ID for the bus
    pub(crate) bus_id: String,

    /// Detected USB controller type
    pub(crate) controller_type: Option<UsbControllerType>,
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
impl BusInfo {
    /// *(Linux-only)* Sysfs path for the bus.
    #[cfg(any(target_os = "linux"))]
    pub fn sysfs_path(&self) -> &std::path::Path {
        &self.path.0
    }

    /// *(Linux-only)* Bus number.
    ///
    /// On Linux, the `bus_id` is an integer and this provides the value as `u8`.
    #[cfg(any(target_os = "linux"))]
    pub fn busnum(&self) -> u8 {
        self.busnum
    }

    /// *(Linux-only)* The root hub [`DeviceInfo`] representing the bus.
    #[cfg(any(target_os = "linux"))]
    pub fn root_hub(&self) -> &DeviceInfo {
        &self.root_hub
    }

    /// *(Windows-only)* Instance ID path of this device
    #[cfg(target_os = "windows")]
    pub fn instance_id(&self) -> &OsStr {
        &self.instance_id
    }

    /// *(Windows-only)* Instance ID path of the parent device
    #[cfg(target_os = "windows")]
    pub fn parent_instance_id(&self) -> &OsStr {
        &self.parent_instance_id
    }

    /// *(Windows-only)* Location paths property
    #[cfg(target_os = "windows")]
    pub fn location_paths(&self) -> &[OsString] {
        &self.location_paths
    }

    /// *(Windows-only)* Device Instance ID
    #[cfg(target_os = "windows")]
    pub fn devinst(&self) -> crate::platform::DevInst {
        self.devinst
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

    /// *(macOS-only)* IOKit provider class name
    #[cfg(target_os = "macos")]
    pub fn provider_class_name(&self) -> &str {
        &self.provider_class_name
    }

    /// *(macOS-only)* IOKit class name
    #[cfg(target_os = "macos")]
    pub fn class_name(&self) -> &str {
        &self.class_name
    }

    /// *(macOS-only)* Name of the bus
    #[cfg(target_os = "macos")]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Driver associated with the bus
    pub fn driver(&self) -> Option<&str> {
        self.driver.as_deref()
    }

    /// Identifier for the bus
    pub fn bus_id(&self) -> &str {
        &self.bus_id
    }

    /// Detected USB controller type
    ///
    /// None means the controller type could not be determined.
    pub fn controller_type(&self) -> Option<UsbControllerType> {
        self.controller_type
    }

    /// System name of the bus
    ///
    /// ### Platform-specific notes
    ///
    /// * Linux: The root hub product string.
    /// * macOS: The [IONameMatched](https://developer.apple.com/documentation/bundleresources/information_property_list/ionamematch) key of the IOService entry.
    /// * Windows: Description field of the root hub device. How the bus will appear in Device Manager.
    pub fn system_name(&self) -> Option<&str> {
        #[cfg(any(target_os = "linux"))]
        {
            self.root_hub.product_string()
        }

        #[cfg(target_os = "windows")]
        {
            Some(&self.root_hub_description)
        }

        #[cfg(target_os = "macos")]
        {
            self.name.as_deref()
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
impl std::fmt::Debug for BusInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut s = f.debug_struct("BusInfo");

        #[cfg(any(target_os = "linux"))]
        {
            s.field("sysfs_path", &self.path);
            s.field("busnum", &self.busnum);
        }

        #[cfg(target_os = "windows")]
        {
            s.field("instance_id", &self.instance_id);
            s.field("parent_instance_id", &self.parent_instance_id);
            s.field("location_paths", &self.location_paths);
        }

        #[cfg(target_os = "macos")]
        {
            s.field("location_id", &format_args!("0x{:08X}", self.location_id));
            s.field(
                "registry_entry_id",
                &format_args!("0x{:08X}", self.registry_id),
            );
            s.field("class_name", &self.class_name);
            s.field("provider_class_name", &self.provider_class_name);
        }

        s.field("bus_id", &self.bus_id)
            .field("system_name", &self.system_name())
            .field("controller_type", &self.controller_type)
            .field("driver", &self.driver);

        s.finish()
    }
}
