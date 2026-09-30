use std::{
    ffi::OsStr,
    ptr::{null, null_mut},
};

use windows_sys::{
    core::GUID,
    Win32::{
        Foundation::{ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_SUCCESS, S_OK},
        System::{
            Com::IIDFromString,
            Registry::{RegCloseKey, RegGetValueW, HKEY, RRF_RT_REG_MULTI_SZ, RRF_RT_REG_SZ},
        },
    },
};

use crate::{Error, ErrorKind};

use super::util::WCString;

pub struct RegKey(HKEY);

impl RegKey {
    pub unsafe fn new(k: HKEY) -> RegKey {
        RegKey(k)
    }

    pub fn query_value_guid(&self, value_name: &str) -> Result<GUID, Error> {
        unsafe {
            let value_name = WCString::from(OsStr::new(value_name));
            let mut r = 0;

            // Start with expected size for one GUID string
            let mut buf: Vec<u16> = Vec::with_capacity(40);
            for can_resize in [true, false] {
                let mut size = (buf.capacity() * 2) as u32;
                r = RegGetValueW(
                    self.0,
                    null(),
                    value_name.as_ptr(),
                    RRF_RT_REG_SZ | RRF_RT_REG_MULTI_SZ,
                    null_mut(),
                    buf.as_mut_ptr().cast(),
                    &mut size,
                );

                if r == ERROR_MORE_DATA && can_resize {
                    log::debug!("Resizing GUID buffer to {size} bytes");
                    buf = Vec::with_capacity(size.div_ceil(2) as usize + 1);
                    continue;
                } else {
                    break;
                }
            }

            if r != ERROR_SUCCESS {
                return Err(Error::new_os(
                    ErrorKind::Other,
                    match r {
                        ERROR_FILE_NOT_FOUND => "registry value not found",
                        _ => "failed to read registry value",
                    },
                    r,
                ));
            }

            let mut guid = GUID::from_u128(0);
            if IIDFromString(buf.as_mut_ptr(), &mut guid) == S_OK {
                Ok(guid)
            } else {
                Err(Error::new(
                    ErrorKind::Other,
                    "failed to parse GUID from registry value",
                ))
            }
        }
    }
}

impl Drop for RegKey {
    fn drop(&mut self) {
        unsafe {
            RegCloseKey(self.0);
        }
    }
}
