use std::fs;
use std::io;
use std::num::ParseIntError;
use std::path::PathBuf;
use std::str::FromStr;

use log::debug;

use crate::DeviceInfo;
use crate::Error;

#[derive(Debug, Clone)]
pub struct SysfsPath(PathBuf);

impl SysfsPath {
    fn read_attr<T: FromStr>(&self, attr: &str) -> Result<T, io::Error>
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
    Ok(DeviceInfo {
        bus_number: path.read_attr("busnum")?,
        device_address: path.read_attr("devnum")?,
        vendor_id: path.read_attr_hex("idVendor")?,
        product_id: path.read_attr_hex("idProduct")?,
        device_version: path.read_attr_hex("bcdDevice")?,
        class: path.read_attr_hex("bDeviceClass")?,
        subclass: path.read_attr_hex("bDeviceSubClass")?,
        protocol: path.read_attr_hex("bDeviceProtocol")?,
        speed: path.read_attr("speed")?,
        manufacturer_string: path.read_attr("manufacturer").ok(),
        product_string: path.read_attr("product").ok(),
        serial_number: path.read_attr("serial").ok(),
        path: path,
    })
}
/// Returns the path of a device in usbfs
fn usb_devfs_path(busnum: u8, devnum: u8) -> PathBuf {
    PathBuf::from(format!("/dev/bus/usb/{busnum:03}/{devnum:03}"))
}
