use crate::{DeviceInfo, Error};

pub fn list_devices() -> Result<impl Iterator<Item = DeviceInfo>, Error> {
    Ok([].into_iter())
}