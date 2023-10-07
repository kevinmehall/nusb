//! Safe wrappers for SetupAPI for device enumeration

use std::{
    alloc,
    alloc::Layout,
    ffi::{OsStr, OsString},
    io::{self, ErrorKind},
    mem::{self, size_of},
    os::windows::prelude::{OsStrExt, OsStringExt},
    ptr::{addr_of_mut, null, null_mut},
    slice,
};

use log::error;
use windows_sys::{
    core::GUID,
    Win32::{
        Devices::{
            DeviceAndDriverInstallation::{
                SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiEnumDeviceInterfaces,
                SetupDiGetClassDevsW, SetupDiGetDeviceInterfaceDetailW, SetupDiGetDevicePropertyW,
                SetupDiOpenDevRegKey, DICS_FLAG_GLOBAL, DIGCF_ALLCLASSES, DIGCF_DEVICEINTERFACE,
                DIGCF_PRESENT, DIREG_DEV, SP_DEVICE_INTERFACE_DATA,
                SP_DEVICE_INTERFACE_DETAIL_DATA_W, SP_DEVINFO_DATA,
            },
            Properties::{
                DEVPROPKEY, DEVPROPTYPE, DEVPROP_TYPE_STRING, DEVPROP_TYPE_STRING_LIST,
                DEVPROP_TYPE_UINT32,
            },
        },
        Foundation::{GetLastError, ERROR_SUCCESS, FALSE, INVALID_HANDLE_VALUE, S_OK, TRUE},
        System::{
            Com::IIDFromString,
            Registry::{RegCloseKey, RegQueryValueExW, HKEY, KEY_READ, REG_MULTI_SZ, REG_SZ},
        },
    },
};

use super::util::from_wide_with_nul;

/// Wrapper for a device info set as returned by [`SetupDiGetClassDevs`]
pub struct DeviceInfoSet {
    handle: isize,
}

impl DeviceInfoSet {
    pub fn get(
        class: Option<GUID>,
        enumerator: Option<&OsStr>,
    ) -> Result<DeviceInfoSet, io::Error> {
        let enumerator: Option<Vec<u16>> =
            enumerator.map(|e| e.encode_wide().chain(Some(0)).collect());
        let handle = unsafe {
            SetupDiGetClassDevsW(
                class.as_ref().map_or(null(), |g| g as *const _),
                enumerator.as_ref().map_or(null(), |s| s.as_ptr()),
                0,
                if class.is_some() { 0 } else { DIGCF_ALLCLASSES }
                    | DIGCF_DEVICEINTERFACE
                    | DIGCF_PRESENT,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            let err = io::Error::last_os_error();
            error!("SetupDiGetClassDevsW failed: {}", err);
            Err(err)
        } else {
            Ok(DeviceInfoSet { handle })
        }
    }

    pub fn iter_devices(&self) -> DeviceInfoSetIter {
        DeviceInfoSetIter {
            set: self,
            index: 0,
        }
    }

    pub fn iter_interfaces(&self, interface_class_guid: GUID) -> DeviceInfoSetInterfaceIter {
        DeviceInfoSetInterfaceIter {
            set: self,
            device: None,
            interface_class_guid,
            index: 0,
        }
    }
}

impl Drop for DeviceInfoSet {
    fn drop(&mut self) {
        unsafe {
            SetupDiDestroyDeviceInfoList(self.handle);
        }
    }
}

/// Iterator for devices in [`DeviceInfoSet`]
pub struct DeviceInfoSetIter<'a> {
    set: &'a DeviceInfoSet,
    index: u32,
}

impl<'a> Iterator for DeviceInfoSetIter<'a> {
    type Item = DeviceInfo<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut device_info: SP_DEVINFO_DATA = unsafe { mem::zeroed() };
        device_info.cbSize = size_of::<SP_DEVINFO_DATA>() as u32;

        if unsafe { SetupDiEnumDeviceInfo(self.set.handle, self.index, &mut device_info) } == 0 {
            None
        } else {
            self.index += 1;
            Some(DeviceInfo {
                set: self.set,
                device_info,
            })
        }
    }
}

pub struct DeviceInfo<'a> {
    set: &'a DeviceInfoSet,
    device_info: SP_DEVINFO_DATA,
}

impl<'a> DeviceInfo<'a> {
    pub fn get_string_property(&self, pkey: DEVPROPKEY) -> Option<OsString> {
        let mut property_type: DEVPROPTYPE = unsafe { mem::zeroed() };
        let mut buffer = [0u16; 1024];
        let mut size: u32 = 0; // in bytes

        let r = unsafe {
            SetupDiGetDevicePropertyW(
                self.set.handle,
                &self.device_info,
                &pkey,
                &mut property_type,
                buffer.as_mut_ptr() as *mut u8,
                (buffer.len() * mem::size_of::<u16>()) as u32,
                &mut size,
                0,
            )
        };

        if r == 1 && property_type == DEVPROP_TYPE_STRING {
            Some(from_wide_with_nul(
                &buffer[..(size as usize / mem::size_of::<u16>())],
            ))
        } else {
            None
        }
    }

    pub fn get_string_list_property(&self, pkey: DEVPROPKEY) -> Option<Vec<OsString>> {
        let mut property_type: DEVPROPTYPE = unsafe { mem::zeroed() };
        let mut buffer = [0u16; 4096];
        let mut size: u32 = 0; // in bytes

        let r = unsafe {
            SetupDiGetDevicePropertyW(
                self.set.handle,
                &self.device_info,
                &pkey,
                &mut property_type,
                buffer.as_mut_ptr() as *mut u8,
                (buffer.len() * mem::size_of::<u16>()) as u32,
                &mut size,
                0,
            )
        };

        if r == TRUE && property_type == DEVPROP_TYPE_STRING_LIST {
            let buffer = &buffer[..(size as usize / mem::size_of::<u16>())];
            Some(
                buffer
                    .split(|&c| c == 0)
                    .filter(|e| e.len() > 0)
                    .map(|s| OsString::from_wide(s))
                    .collect(),
            )
        } else {
            None
        }
    }

    pub fn get_u32_property(&self, pkey: DEVPROPKEY) -> Option<u32> {
        let mut property_type: DEVPROPTYPE = unsafe { mem::zeroed() };
        let mut buffer: u32 = 0;

        let r = unsafe {
            SetupDiGetDevicePropertyW(
                self.set.handle,
                &self.device_info,
                &pkey,
                &mut property_type,
                &mut buffer as *mut u32 as *mut u8,
                mem::size_of::<u32>() as u32,
                null_mut(),
                0,
            )
        };

        if r == TRUE && property_type == DEVPROP_TYPE_UINT32 {
            Some(buffer)
        } else {
            None
        }
    }

    pub fn registry_key(&self) -> Result<RegKey, io::Error> {
        unsafe {
            let key = SetupDiOpenDevRegKey(
                self.set.handle,
                &self.device_info,
                DICS_FLAG_GLOBAL,
                0,
                DIREG_DEV,
                KEY_READ,
            );

            if key == INVALID_HANDLE_VALUE {
                Err(io::Error::last_os_error())
            } else {
                Ok(RegKey(key))
            }
        }
    }

    pub fn interfaces(&self, interface_class_guid: GUID) -> DeviceInfoSetInterfaceIter {
        DeviceInfoSetInterfaceIter {
            set: self.set,
            device: Some(self.device_info),
            interface_class_guid,
            index: 0,
        }
    }
}

pub struct DeviceInfoSetInterfaceIter<'a> {
    set: &'a DeviceInfoSet,
    device: Option<SP_DEVINFO_DATA>,
    interface_class_guid: GUID,
    index: u32,
}

impl<'a> Iterator for DeviceInfoSetInterfaceIter<'a> {
    type Item = InterfaceInfo<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut interface_info: SP_DEVICE_INTERFACE_DATA = unsafe { mem::zeroed() };
        interface_info.cbSize = mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;

        let r = unsafe {
            SetupDiEnumDeviceInterfaces(
                self.set.handle,
                self.device.as_ref().map_or(null(), |x| x as *const _),
                &self.interface_class_guid,
                self.index,
                &mut interface_info,
            )
        };

        if r == FALSE {
            None
        } else {
            self.index += 1;
            Some(InterfaceInfo {
                set: self.set,
                interface_info,
            })
        }
    }
}

pub struct InterfaceInfo<'a> {
    set: &'a DeviceInfoSet,
    interface_info: SP_DEVICE_INTERFACE_DATA,
}

impl<'a> InterfaceInfo<'a> {
    pub fn get_path(&self) -> Option<OsString> {
        unsafe {
            // Initial call to get required size
            let mut required_size: u32 = 0;
            SetupDiGetDeviceInterfaceDetailW(
                self.set.handle,
                &self.interface_info,
                null_mut(),
                0,
                &mut required_size,
                null_mut(),
            );
            if (required_size as usize) < mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() {
                error!("SetupDiGetDeviceInterfaceDetailW unexpected required size {required_size}");
                return None;
            }

            let layout = Layout::from_size_align(
                required_size as usize,
                mem::align_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>(),
            )
            .unwrap();

            let buf = alloc::alloc(layout).cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();

            // The passed argument is a variable-sized struct, but the only
            // fixed struct field is cbSize, which is the size of the fixed part.
            addr_of_mut!((*buf).cbSize)
                .write(mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32);

            let r = SetupDiGetDeviceInterfaceDetailW(
                self.set.handle,
                &self.interface_info,
                buf,
                required_size,
                null_mut(),
                null_mut(),
            );

            let val = if r == TRUE {
                let ptr = addr_of_mut!((*buf).DevicePath).cast::<u16>();
                let header_size = (ptr as usize) - (buf as usize);
                let len = (required_size as usize - header_size) / size_of::<u16>();
                Some(from_wide_with_nul(slice::from_raw_parts(ptr, len)))
            } else {
                let err = GetLastError();
                error!("SetupDiGetDeviceInterfaceDetailW failed: {err}");
                None
            };

            alloc::dealloc(buf.cast(), layout);

            val
        }
    }
}

pub struct RegKey(HKEY);

impl RegKey {
    pub fn query_value_guid(&self, value_name: &str) -> Result<GUID, io::Error> {
        unsafe {
            let value_name: Vec<u16> = OsStr::new(value_name)
                .encode_wide()
                .chain(Some(0))
                .collect();
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
                return Err(io::Error::from_raw_os_error(r as i32));
            }

            if ty != REG_MULTI_SZ && ty != REG_SZ {
                return Err(io::Error::new(
                    ErrorKind::InvalidInput,
                    "registry value type not string",
                ));
            }

            let layout = Layout::from_size_align(size as usize, mem::align_of::<u16>()).unwrap();

            let buf = alloc::alloc(layout);

            let r = RegQueryValueExW(self.0, value_name.as_ptr(), null(), &mut ty, buf, &mut size);

            if r != ERROR_SUCCESS {
                alloc::dealloc(buf, layout);
                return Err(io::Error::from_raw_os_error(r as i32));
            }

            let mut guid = GUID::from_u128(0);
            let r = IIDFromString(buf as *mut u16, &mut guid);

            alloc::dealloc(buf, layout);

            if r == S_OK {
                Ok(guid)
            } else {
                Err(io::Error::new(ErrorKind::InvalidData, "invalid UUID"))
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
