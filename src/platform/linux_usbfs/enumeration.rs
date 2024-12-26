use std::fs;
use std::io;
use std::num::ParseIntError;
use std::path::PathBuf;
use std::str::FromStr;

use log::debug;
use log::warn;

use crate::enumeration::InterfaceInfo;
use crate::{BusInfo, DeviceInfo, Error, Speed, UsbControllerType};

#[derive(Debug, Clone)]
pub struct SysfsPath(pub(crate) PathBuf);

#[derive(Debug)]
pub struct SysfsError(PathBuf, SysfsErrorKind);

#[derive(Debug)]
enum SysfsErrorKind {
    Io(io::Error),
    Parse(String),
}

impl std::fmt::Display for SysfsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "failed to read sysfs attribute {}: ", self.0.display())?;
        match &self.1 {
            SysfsErrorKind::Io(e) => write!(f, "{e}"),
            SysfsErrorKind::Parse(v) => write!(f, "couldn't parse value {:?}", v.trim()),
        }
    }
}

impl std::error::Error for SysfsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self.1 {
            SysfsErrorKind::Io(ref e) => Some(e),
            _ => None,
        }
    }
}

impl From<SysfsError> for io::Error {
    fn from(value: SysfsError) -> Self {
        io::Error::other(Box::new(value))
    }
}

impl SysfsPath {
    fn parse_attr<T, E>(
        &self,
        attr: &str,
        parse: impl FnOnce(&str) -> Result<T, E>,
    ) -> Result<T, SysfsError> {
        let attr_path = self.0.join(attr);
        fs::read_to_string(&attr_path)
            .map_err(SysfsErrorKind::Io)
            .and_then(|v| parse(v.trim()).map_err(|_| SysfsErrorKind::Parse(v)))
            .map_err(|e| SysfsError(attr_path, e))
    }

    fn readlink_attr(&self, attr: &str) -> Result<PathBuf, SysfsError> {
        let attr_path = self.0.join(attr);
        fs::read_link(&attr_path).map_err(|e| SysfsError(attr_path, SysfsErrorKind::Io(e)))
    }

    pub(crate) fn read_attr<T: FromStr>(&self, attr: &str) -> Result<T, SysfsError> {
        self.parse_attr(attr, |s| s.parse())
    }

    fn read_attr_hex<T: FromHexStr>(&self, attr: &str) -> Result<T, SysfsError> {
        self.parse_attr(attr, |s| T::from_hex_str(s.strip_prefix("0x").unwrap_or(s)))
    }

    pub(crate) fn readlink_attr_filename(&self, attr: &str) -> Result<String, SysfsError> {
        self.readlink_attr(attr).map(|p| {
            p.file_name()
                .and_then(|s| s.to_str().to_owned())
                .map(str::to_owned)
                .ok_or_else(|| {
                    SysfsError(
                        p,
                        SysfsErrorKind::Parse(format!(
                            "Failed to read filename for readlink attribute {}",
                            attr
                        )),
                    )
                })
        })?
    }

    fn children(&self) -> impl Iterator<Item = SysfsPath> {
        fs::read_dir(&self.0)
            .ok()
            .into_iter()
            .flat_map(|x| x)
            .filter_map(|f| f.ok())
            .filter(|f| f.file_type().ok().is_some_and(|t| t.is_dir()))
            .map(|f| SysfsPath(f.path()))
    }
}

trait FromHexStr: Sized {
    fn from_hex_str(s: &str) -> Result<Self, ParseIntError>;
}

impl FromHexStr for u8 {
    fn from_hex_str(s: &str) -> Result<Self, ParseIntError> {
        u8::from_str_radix(s, 16)
    }
}

impl FromHexStr for u16 {
    fn from_hex_str(s: &str) -> Result<Self, ParseIntError> {
        u16::from_str_radix(s, 16)
    }
}

const SYSFS_USB_PREFIX: &'static str = "/sys/bus/usb/devices/";

pub async fn list_devices() -> Result<impl Iterator<Item = DeviceInfo>, Error> {
    Ok(fs::read_dir(SYSFS_USB_PREFIX)?.flat_map(|entry| {
        let path = entry.ok()?.path();
        let name = path.file_name()?;

        // Device names look like `1-6` or `1-6.4.2`
        // We'll ignore:
        //  * root hubs (`usb1`) -- they're not useful to talk to and are not exposed on other platforms
        //  * interfaces (`1-6:1.0`)
        if !name
            .as_encoded_bytes()
            .iter()
            .all(|c| matches!(c, b'0'..=b'9' | b'-' | b'.'))
        {
            return None;
        }

        probe_device(SysfsPath(path))
            .inspect_err(|e| warn!("{e}; ignoring device"))
            .ok()
    }))
}

pub fn list_root_hubs() -> Result<impl Iterator<Item = DeviceInfo>, Error> {
    Ok(fs::read_dir(SYSFS_USB_PREFIX)?.filter_map(|entry| {
        let path = entry.ok()?.path();
        let name = path.file_name()?;

        // root hubs are named `usbX` where X is the bus number
        if !name.to_string_lossy().starts_with("usb") {
            return None;
        }

        probe_device(SysfsPath(path))
            .inspect_err(|e| warn!("{e}; ignoring root hub"))
            .ok()
    }))
}

pub fn list_buses() -> Result<impl Iterator<Item = BusInfo>, Error> {
    Ok(list_root_hubs()?.filter_map(|rh| {
        // get the parent by following the absolute symlink; root hub in /bus/usb is a symlink to a dir in parent bus
        let parent_path = rh
            .path
            .0
            .canonicalize()
            .ok()
            .and_then(|p| p.parent().map(|p| SysfsPath(p.to_owned())))?;

        debug!("Probing parent device {:?}", parent_path.0);
        let driver = parent_path.readlink_attr_filename("driver").ok();

        Some(BusInfo {
            bus_id: rh.bus_id.to_owned(),
            path: rh.path.to_owned(),
            parent_path: parent_path.to_owned(),
            busnum: rh.busnum,
            controller_type: driver.as_ref().and_then(|p| UsbControllerType::from_str(p)),
            driver,
            root_hub: rh,
        })
    }))
}

pub fn probe_device(path: SysfsPath) -> Result<DeviceInfo, SysfsError> {
    debug!("Probing device {:?}", path.0);

    let busnum = path.read_attr("busnum")?;
    let device_address = path.read_attr("devnum")?;

    let port_chain = path
        .read_attr::<String>("devpath")
        .ok()
        .filter(|p| p != "0") // root hub should be empty but devpath is 0
        .and_then(|p| {
            p.split('.')
                .map(|v| v.parse::<u8>().ok())
                .collect::<Option<Vec<u8>>>()
        })
        .unwrap_or_default();

    Ok(DeviceInfo {
        busnum,
        bus_id: format!("{busnum:03}"),
        device_address,
        port_chain,
        vendor_id: path.read_attr_hex("idVendor")?,
        product_id: path.read_attr_hex("idProduct")?,
        device_version: path.read_attr_hex("bcdDevice")?,
        class: path.read_attr_hex("bDeviceClass")?,
        subclass: path.read_attr_hex("bDeviceSubClass")?,
        protocol: path.read_attr_hex("bDeviceProtocol")?,
        max_packet_size_0: path.read_attr("bMaxPacketSize0")?,
        speed: path
            .read_attr::<String>("speed")
            .ok()
            .as_deref()
            .and_then(Speed::from_str),
        manufacturer_string: path.read_attr("manufacturer").ok(),
        product_string: path.read_attr("product").ok(),
        serial_number: path.read_attr("serial").ok(),
        interfaces: {
            let mut interfaces: Vec<_> = path
                .children()
                .filter(|i| {
                    // Skip subdirectories like `power` that aren't interfaces
                    // (they would be skipped when missing required properties,
                    // but might as well not open them)
                    i.0.file_name()
                        .unwrap_or_default()
                        .as_encoded_bytes()
                        .contains(&b':')
                })
                .flat_map(|i| {
                    Some(InterfaceInfo {
                        interface_number: i.read_attr_hex("bInterfaceNumber").ok()?,
                        class: i.read_attr_hex("bInterfaceClass").ok()?,
                        subclass: i.read_attr_hex("bInterfaceSubClass").ok()?,
                        protocol: i.read_attr_hex("bInterfaceProtocol").ok()?,
                        interface_string: i.read_attr("interface").ok(),
                    })
                })
                .collect();
            interfaces.sort_unstable_by_key(|i| i.interface_number);
            interfaces
        },
        path,
    })
}
