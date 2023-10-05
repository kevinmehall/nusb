//! Safe wrappers for SetupAPI for device enumeration

use std::{
    alloc,
    alloc::Layout,
    ffi::{OsStr, OsString},
    mem::{self, size_of},
    os::windows::prelude::OsStrExt,
    ptr::{addr_of_mut, null, null_mut},
    slice,
};

use log::{debug, error};
use windows_sys::{
    core::GUID,
    Win32::{
        Devices::{
            DeviceAndDriverInstallation::{
                SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiEnumDeviceInterfaces,
                SetupDiGetClassDevsW, SetupDiGetDeviceInterfaceDetailW, SetupDiGetDevicePropertyW,
                DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, SP_DEVICE_INTERFACE_DATA,
                SP_DEVICE_INTERFACE_DETAIL_DATA_W, SP_DEVINFO_DATA,
            },
            Properties::{DEVPROPKEY, DEVPROPTYPE, DEVPROP_TYPE_STRING, DEVPROP_TYPE_UINT32},
        },
        Foundation::{GetLastError, FALSE, INVALID_HANDLE_VALUE, TRUE},
    },
};

use super::util::from_wide_with_nul;

/// Wrapper for a device info set as returned by [`SetupDiGetClassDevs`]
pub struct DeviceInfoSet {
    handle: isize,
}

impl DeviceInfoSet {
    pub fn get_by_setup_class(guid: GUID, enumerator: Option<&OsStr>) -> Result<DeviceInfoSet, ()> {
        let enumerator: Option<Vec<u16>> =
            enumerator.map(|e| e.encode_wide().chain(Some(0)).collect());
        let handle = unsafe {
            SetupDiGetClassDevsW(
                &guid,
                enumerator.as_ref().map_or(null(), |s| s.as_ptr()),
                0,
                DIGCF_DEVICEINTERFACE | DIGCF_PRESENT,
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            error!("SetupDiGetClassDevsW failed: {}", unsafe { GetLastError() });
            Err(())
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

        if r == 1 && property_type == DEVPROP_TYPE_UINT32 {
            Some(buffer)
        } else {
            None
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
