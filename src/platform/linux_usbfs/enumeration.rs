use std::fs;
use std::io;
use std::num::ParseIntError;
use std::path::PathBuf;
use std::str::FromStr;

use log::debug;

use crate::enumeration::InterfaceInfo;
use crate::DeviceInfo;
use crate::Error;
use crate::Speed;

#[derive(Debug, Clone)]
pub struct SysfsPath(pub(crate) PathBuf);

impl SysfsPath {
    pub(crate) fn read_attr<T: FromStr>(&self, attr: &str) -> Result<T, io::Error>
    where
        T: FromStr,
        T::Err: std::error::Error + Send + Sync + 'static,
    {
        let attr_path = self.0.join(attr);
        let read_res = fs::read_to_string(&attr_path);
        debug!("sysfs read {attr_path:?}: {read_res:?}");

        read_res?
            .trim()
            .parse()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    fn read_attr_hex<T: FromHexStr>(&self, attr: &str) -> Result<T, io::Error> {
        let s = self.read_attr::<String>(attr)?;
        T::from_hex_str(&s)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid hex str"))
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

const SYSFS_PREFIX: &'static str = "/sys/bus/usb/devices/";

pub fn list_devices() -> Result<impl Iterator<Item = DeviceInfo>, Error> {
    Ok(fs::read_dir(SYSFS_PREFIX)?.flat_map(|entry| {
        let res = probe_device(SysfsPath(entry.ok()?.path()));
        if let Err(x) = &res {
            debug!("failed to probe, skipping: {x}")
        }
        res.ok()
    }))
}

pub fn probe_device(path: SysfsPath) -> Result<DeviceInfo, Error> {
    debug!("probe device {path:?}");
    let port_chain: Vec<u32> = path
        .read_attr::<String>("devpath")?
        .split('.')
        .flat_map(|v| v.parse::<u32>())
        .collect();
    Ok(DeviceInfo {
        bus_number: path.read_attr("busnum")?,
        port_number: *port_chain.last().unwrap_or(&0),
        port_chain,
        device_address: path.read_attr("devnum")?,
        vendor_id: path.read_attr_hex("idVendor")?,
        product_id: path.read_attr_hex("idProduct")?,
        device_version: path.read_attr_hex("bcdDevice")?,
        class: path.read_attr_hex("bDeviceClass")?,
        subclass: path.read_attr_hex("bDeviceSubClass")?,
        protocol: path.read_attr_hex("bDeviceProtocol")?,
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
