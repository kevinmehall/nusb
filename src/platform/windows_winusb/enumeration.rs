use crate::{DeviceInfo, Error};

pub fn list_devices() -> Result<impl Iterator<Item = DeviceInfo>, Error> {
    Ok([].iter().cloned())
}
