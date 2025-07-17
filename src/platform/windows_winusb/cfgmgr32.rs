use std::{ffi::OsString, iter, mem, ptr};

use log::debug;
use windows_sys::{
    core::GUID,
    Win32::{
        Devices::{
            DeviceAndDriverInstallation::{
                CM_Get_Child, CM_Get_DevNode_PropertyW, CM_Get_Device_Interface_ListW,
                CM_Get_Device_Interface_List_SizeW, CM_Get_Device_Interface_PropertyW,
                CM_Get_Parent, CM_Get_Sibling, CM_Locate_DevNodeW, CM_Open_DevNode_Key,
                RegDisposition_OpenExisting, CM_GET_DEVICE_INTERFACE_LIST_PRESENT,
                CM_LOCATE_DEVNODE_PHANTOM, CM_REGISTRY_HARDWARE, CR_BUFFER_SMALL, CR_SUCCESS,
            },
            Properties::{
                DEVPKEY_Device_InstanceId, DEVPROPTYPE, DEVPROP_TYPE_STRING,
                DEVPROP_TYPE_STRING_LIST, DEVPROP_TYPE_UINT32,
            },
        },
        Foundation::{DEVPROPKEY, INVALID_HANDLE_VALUE},
        System::Registry::KEY_READ,
    },
};

use super::{
    registry::RegKey,
    util::{NulSepList, NulSepListIter, WCStr, WCString},
};

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct DevInst(u32);

impl DevInst {
    pub fn from_instance_id(id: &WCStr) -> Option<DevInst> {
        let mut devinst = 0;
        let c = unsafe { CM_Locate_DevNodeW(&mut devinst, id.as_ptr(), CM_LOCATE_DEVNODE_PHANTOM) };
        if c == CR_SUCCESS {
            Some(DevInst(devinst))
        } else {
            None
        }
    }

    pub fn get_property<T: PropertyType>(&self, pkey: DEVPROPKEY) -> Option<T> {
        let mut property_type: DEVPROPTYPE = 0;
        let mut buffer: T::Buffer = T::empty_buffer();
        let mut size: u32 = mem::size_of_val(&buffer) as u32;

        let r = unsafe {
            CM_Get_DevNode_PropertyW(
                self.0,
                &pkey,
                &mut property_type,
                &mut buffer as *mut _ as *mut u8,
                &mut size,
                0,
            )
        };

        if r == CR_SUCCESS && property_type == T::PROPTYPE {
            Some(T::from_buffer(&buffer))
        } else {
            None
        }
    }

    pub fn instance_id(&self) -> WCString {
        self.get_property(DEVPKEY_Device_InstanceId)
            .expect("device should always have instance ID")
    }

    pub fn parent(&self) -> Option<DevInst> {
        let mut out = 0;
        let cr = unsafe { CM_Get_Parent(&mut out, self.0, 0) };
        if cr == CR_SUCCESS {
            Some(DevInst(out))
        } else {
            None
        }
    }

    pub fn first_child(&self) -> Option<DevInst> {
        let mut out = 0;
        let cr = unsafe { CM_Get_Child(&mut out, self.0, 0) };
        if cr == CR_SUCCESS {
            Some(DevInst(out))
        } else {
            None
        }
    }

    pub fn next_sibling(&self) -> Option<DevInst> {
        let mut out = 0;
        let cr = unsafe { CM_Get_Sibling(&mut out, self.0, 0) };
        if cr == CR_SUCCESS {
            Some(DevInst(out))
        } else {
            None
        }
    }

    pub fn children(&self) -> impl Iterator<Item = DevInst> {
        let mut node = self.first_child();
        iter::from_fn(move || {
            if let Some(n) = node {
                node = n.next_sibling();
                Some(n)
            } else {
                None
            }
        })
    }

    pub fn registry_key(&self) -> Option<RegKey> {
        let mut hkey = INVALID_HANDLE_VALUE;
        let cr = unsafe {
            CM_Open_DevNode_Key(
                self.0,
                KEY_READ,
                0,
                RegDisposition_OpenExisting,
                &mut hkey,
                CM_REGISTRY_HARDWARE,
            )
        };

        if cr == CR_SUCCESS && hkey != INVALID_HANDLE_VALUE {
            Some(unsafe { RegKey::new(hkey) })
        } else {
            None
        }
    }

    /// Get interfaces of this device with the specified interface class.
    ///
    /// Note: these are Windows device interfaces (paths to open a device
    /// handle), not to be confused with USB interfaces.
    pub fn interfaces(&self, interface: GUID) -> NulSepList {
        let id = self.instance_id();
        list_interfaces(interface, Some(&id))
    }
}

pub trait PropertyType {
    const PROPTYPE: DEVPROPTYPE;
    type Buffer;

    fn empty_buffer() -> Self::Buffer;
    fn from_buffer(b: &Self::Buffer) -> Self;
}

impl PropertyType for u32 {
    const PROPTYPE: DEVPROPTYPE = DEVPROP_TYPE_UINT32;
    type Buffer = u32;
    fn empty_buffer() -> u32 {
        0
    }
    fn from_buffer(b: &Self::Buffer) -> Self {
        *b
    }
}

impl PropertyType for WCString {
    const PROPTYPE: DEVPROPTYPE = DEVPROP_TYPE_STRING;
    type Buffer = [u16; 1024];
    fn empty_buffer() -> Self::Buffer {
        [0; 1024]
    }
    fn from_buffer(b: &Self::Buffer) -> Self {
        WCStr::from_slice_until_nul(b).to_owned()
    }
}

impl PropertyType for Vec<WCString> {
    const PROPTYPE: DEVPROPTYPE = DEVPROP_TYPE_STRING_LIST;
    type Buffer = [u16; 1024];
    fn empty_buffer() -> Self::Buffer {
        [0; 1024]
    }
    fn from_buffer(b: &Self::Buffer) -> Self {
        NulSepListIter(b).map(|s| s.to_owned()).collect()
    }
}

impl PropertyType for OsString {
    const PROPTYPE: DEVPROPTYPE = DEVPROP_TYPE_STRING;
    type Buffer = [u16; 1024];
    fn empty_buffer() -> Self::Buffer {
        [0; 1024]
    }
    fn from_buffer(b: &Self::Buffer) -> Self {
        WCStr::from_slice_until_nul(b).into()
    }
}

impl PropertyType for Vec<OsString> {
    const PROPTYPE: DEVPROPTYPE = DEVPROP_TYPE_STRING_LIST;
    type Buffer = [u16; 1024];
    fn empty_buffer() -> Self::Buffer {
        [0; 1024]
    }
    fn from_buffer(b: &Self::Buffer) -> Self {
        NulSepListIter(b).map(|s| s.into()).collect()
    }
}

pub fn list_interfaces(interface: GUID, instance_id: Option<&WCStr>) -> NulSepList {
    let flags = CM_GET_DEVICE_INTERFACE_LIST_PRESENT;
    let mut buf: Vec<u16> = Vec::new();
    loop {
        let mut len = 0;
        let cr = unsafe {
            CM_Get_Device_Interface_List_SizeW(
                &mut len,
                &interface,
                instance_id.map_or(ptr::null(), |x| x.as_ptr()),
                flags,
            )
        };

        if cr != CR_SUCCESS {
            buf.clear();
            debug!("CM_Get_Device_Interface_List_SizeW failed, status {cr}");
            break;
        }

        buf.resize(len as usize, 0);

        let cr = unsafe {
            CM_Get_Device_Interface_ListW(
                &interface,
                instance_id.map_or(ptr::null(), |x| x.as_ptr()),
                buf.as_mut_ptr(),
                buf.len() as u32,
                flags,
            )
        };

        if cr == CR_SUCCESS {
            break;
        } else if cr == CR_BUFFER_SMALL {
            continue;
        } else {
            buf.clear();
            debug!("CM_Get_Device_Interface_ListW failed, status {cr}");
            break;
        }
    }

    NulSepList(buf)
}

pub fn get_device_interface_property<T: PropertyType>(
    interface: &WCStr,
    pkey: DEVPROPKEY,
) -> Option<T> {
    let mut property_type: DEVPROPTYPE = 0;
    let mut buffer: T::Buffer = T::empty_buffer();
    let mut size: u32 = mem::size_of_val(&buffer) as u32;

    let r = unsafe {
        CM_Get_Device_Interface_PropertyW(
            interface.as_ptr(),
            &pkey,
            &mut property_type,
            &mut buffer as *mut _ as *mut u8,
            &mut size,
            0,
        )
    };

    if r == CR_SUCCESS && property_type == T::PROPTYPE {
        Some(T::from_buffer(&buffer))
    } else {
        None
    }
}
