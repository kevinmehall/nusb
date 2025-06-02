use std::{
    alloc::{self, Layout},
    ffi::OsStr,
    mem,
    ptr::{null, null_mut},
};

use windows_sys::{
    core::GUID,
    Win32::{
        Foundation::{GetLastError, ERROR_SUCCESS, S_OK},
        System::{
            Com::IIDFromString,
            Registry::{RegCloseKey, RegQueryValueExW, HKEY, REG_MULTI_SZ, REG_SZ},
        },
    },
};

use crate::Error;

use super::util::WCString;

pub struct RegKey(HKEY);

impl RegKey {
    pub unsafe fn new(k: HKEY) -> RegKey {
        RegKey(k)
    }

    pub fn query_value_guid(&self, value_name: &str) -> Result<GUID, Error> {
        unsafe {
            let value_name: WCString = OsStr::new(value_name).into();
            let mut ty = 0;
            let mut size = 0;

            // get size
            let r = RegQueryValueExW(
                self.0,
                value_name.as_ptr(),
                null_mut(),
                &mut ty,
                null_mut(),
                &mut size,
            );

            if r != ERROR_SUCCESS {
                return Err(Error::new_os(
                    crate::ErrorKind::Other,
                    "failed to read registry value",
                    GetLastError(),
                ));
            }

            if ty != REG_MULTI_SZ && ty != REG_SZ {
                return Err(Error::new_os(
                    crate::ErrorKind::Other,
                    "failed to read registry value: expected string",
                    GetLastError(),
                ));
            }

            let layout = Layout::from_size_align(size as usize, mem::align_of::<u16>()).unwrap();

            let buf = alloc::alloc(layout);

            let r = RegQueryValueExW(self.0, value_name.as_ptr(), null(), &mut ty, buf, &mut size);

            if r != ERROR_SUCCESS {
                alloc::dealloc(buf, layout);
                return Err(Error::new_os(
                    crate::ErrorKind::Other,
                    "failed to read registry value data",
                    GetLastError(),
                ));
            }

            let mut guid = GUID::from_u128(0);
            let r = IIDFromString(buf as *mut u16, &mut guid);

            alloc::dealloc(buf, layout);

            if r == S_OK {
                Ok(guid)
            } else {
                Err(Error::new(
                    crate::ErrorKind::Other,
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
