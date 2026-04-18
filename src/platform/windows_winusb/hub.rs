use std::{
    alloc::{self, Layout},
    ffi::c_void,
    mem,
    os::windows::prelude::OwnedHandle,
    ptr::{addr_of, null_mut},
    slice,
};

use log::debug;
use windows_sys::Win32::{
    Devices::{
        Properties::DEVPKEY_Device_Address,
        Usb::{
            UsbFullSpeed, UsbHighSpeed, UsbLowSpeed, GUID_DEVINTERFACE_USB_HUB,
            IOCTL_USB_GET_DESCRIPTOR_FROM_NODE_CONNECTION,
            IOCTL_USB_GET_NODE_CONNECTION_INFORMATION_EX,
            IOCTL_USB_GET_NODE_CONNECTION_INFORMATION_EX_V2, USB_DESCRIPTOR_REQUEST,
            USB_DESCRIPTOR_REQUEST_0, USB_DEVICE_DESCRIPTOR, USB_DEVICE_SPEED,
            USB_NODE_CONNECTION_INFORMATION_EX, USB_NODE_CONNECTION_INFORMATION_EX_V2,
        },
    },
    Foundation::{
        CloseHandle, GetLastError, ERROR_GEN_FAILURE, ERROR_IO_PENDING, FALSE, TRUE, WAIT_OBJECT_0,
        WAIT_TIMEOUT,
    },
    System::Threading::*,
    System::IO::{CancelIo, DeviceIoControl, GetOverlappedResult, OVERLAPPED},
};

use crate::{descriptors::DESCRIPTOR_TYPE_DEVICE, Error, ErrorKind, Speed};

use super::{
    cfgmgr32::DevInst,
    util::{create_file, raw_handle},
};

// flags for USB_NODE_CONNECTION_INFORMATION_EX_V2.SupportedUsbProtocols
const USB110: u32 = 0x01;
const USB200: u32 = 0x02;
const USB300: u32 = 0x04;

// USB_NODE_CONNECTION_INFORMATION_EX_V2_FLAGS
const DEVICE_IS_OPERATING_AT_SUPER_SPEED_OR_HIGHER: u32 = 0x01;
const DEVICE_IS_SUPER_SPEED_CAPABLE_OR_HIGHER: u32 = 0x02;
const DEVICE_IS_OPERATING_AT_SUPER_SPEED_PLUS_OR_HIGHER: u32 = 0x04;
const DEVICE_IS_SUPER_SPEED_PLUS_CAPABLE_OR_HIGHER: u32 = 0x08;

struct Overlapped(OVERLAPPED);

impl Overlapped {
    #[inline]
    fn new() -> Self {
        let event =
            unsafe { CreateEventW(std::ptr::null_mut(), TRUE, FALSE, std::ptr::null_mut()) };
        assert!(!event.is_null());
        Self(OVERLAPPED {
            Internal: 0,
            InternalHigh: 0,
            Anonymous: unsafe { std::mem::zeroed() },
            hEvent: event,
        })
    }
}

impl Drop for Overlapped {
    #[inline(always)]
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0.hEvent);
        }
    }
}

/// Safe wrapper around hub ioctls used to get descriptors for child devices.
pub struct HubHandle(OwnedHandle);

impl HubHandle {
    pub fn by_devinst(devinst: DevInst) -> Option<HubHandle> {
        let paths = devinst.interfaces(GUID_DEVINTERFACE_USB_HUB);
        let Some(path) = paths.iter().next() else {
            debug!("Failed to find hub interface");
            return None;
        };

        match create_file(path) {
            Ok(f) => Some(HubHandle(f)),
            Err(e) => {
                debug!("Failed to open hub: {e}");
                None
            }
        }
    }

    pub fn get_node_connection_info(
        &self,
        port_number: u32,
    ) -> Result<USB_NODE_CONNECTION_INFORMATION_EX, Error> {
        unsafe {
            let mut info: USB_NODE_CONNECTION_INFORMATION_EX = mem::zeroed();
            info.ConnectionIndex = port_number;
            let mut bytes_returned: u32 = 0;
            let r = DeviceIoControl(
                raw_handle(&self.0),
                IOCTL_USB_GET_NODE_CONNECTION_INFORMATION_EX,
                &info as *const _ as *const c_void,
                mem::size_of_val(&info) as u32,
                &mut info as *mut _ as *mut c_void,
                mem::size_of_val(&info) as u32,
                &mut bytes_returned,
                null_mut(),
            );

            if r == TRUE {
                if info.ConnectionStatus != windows_sys::Win32::Devices::Usb::DeviceConnected {
                    return Err(Error::new(ErrorKind::Disconnected, "device disconnected"));
                }
                if info.DeviceDescriptor.bDescriptorType != DESCRIPTOR_TYPE_DEVICE {
                    // When the device is disconnected during this call, Windows is observed to
                    // sometimes return an all-zero device descriptor.
                    return Err(Error::new(ErrorKind::Other,
                       "IOCTL_USB_GET_NODE_CONNECTION_INFORMATION_EX returned an invalid device descriptor",
                    ));
                }

                Ok(info)
            } else {
                Err(Error::new_os(
                    ErrorKind::Other,
                    "hub DeviceIoControl failed",
                    GetLastError(),
                )
                .log_debug())
            }
        }
    }

    pub fn get_node_connection_info_v2(
        &self,
        port_number: u32,
    ) -> Result<USB_NODE_CONNECTION_INFORMATION_EX_V2, Error> {
        unsafe {
            let mut info: USB_NODE_CONNECTION_INFORMATION_EX_V2 = mem::zeroed();
            info.ConnectionIndex = port_number;
            info.Length = mem::size_of_val(&info) as u32;
            info.SupportedUsbProtocols.ul = USB110 | USB200 | USB300;
            let mut bytes_returned: u32 = 0;
            let r = DeviceIoControl(
                raw_handle(&self.0),
                IOCTL_USB_GET_NODE_CONNECTION_INFORMATION_EX_V2,
                &info as *const _ as *const c_void,
                mem::size_of_val(&info) as u32,
                &mut info as *mut _ as *mut c_void,
                mem::size_of_val(&info) as u32,
                &mut bytes_returned,
                null_mut(),
            );

            if r == TRUE {
                Ok(info)
            } else {
                Err(Error::new_os(
                    ErrorKind::Other,
                    "hub DeviceIoControl failed",
                    GetLastError(),
                )
                .log_debug())
            }
        }
    }

    pub fn get_descriptor(
        &self,
        port_number: u32,
        descriptor_type: u8,
        descriptor_index: u8,
        language_id: u16,
        timeout: std::time::Duration,
    ) -> Result<Vec<u8>, Error> {
        // Experimentally determined on Windows 10 19045.3803 that this fails
        // with ERROR_INVALID_PARAMETER for non-cached descriptors when
        // requesting length greater than 4095.
        let length = 4095;

        unsafe {
            let layout = Layout::from_size_align(
                mem::size_of::<USB_DESCRIPTOR_REQUEST>() + length,
                mem::align_of::<USB_DESCRIPTOR_REQUEST>(),
            )
            .unwrap();

            let req = alloc::alloc(layout).cast::<USB_DESCRIPTOR_REQUEST>();

            let _guard = scopeguard::guard((), |_| {
                alloc::dealloc(req as *mut _, layout);
            });

            req.write(USB_DESCRIPTOR_REQUEST {
                ConnectionIndex: port_number,
                SetupPacket: USB_DESCRIPTOR_REQUEST_0 {
                    bmRequest: 0x80,
                    bRequest: 0x06,
                    wValue: ((descriptor_type as u16) << 8) | descriptor_index as u16,
                    wIndex: language_id,
                    wLength: length as u16,
                },
                Data: [0],
            });

            let mut bytes_returned: u32 = 0;
            let mut overlapped = Overlapped::new();
            let r = DeviceIoControl(
                raw_handle(&self.0),
                IOCTL_USB_GET_DESCRIPTOR_FROM_NODE_CONNECTION,
                req as *const c_void,
                layout.size() as u32,
                req as *mut c_void,
                layout.size() as u32,
                &mut bytes_returned,
                &mut overlapped.0,
            );

            assert_eq!(r, FALSE);

            if GetLastError() == ERROR_IO_PENDING {
                let wait_result =
                    WaitForSingleObject(overlapped.0.hEvent, timeout.as_millis() as u32) as u32;
                match wait_result {
                    WAIT_OBJECT_0 => {
                        if GetOverlappedResult(
                            raw_handle(&self.0),
                            &mut overlapped.0,
                            &mut bytes_returned,
                            TRUE,
                        ) == TRUE
                        {
                            assert!(bytes_returned != 0);
                            let start = addr_of!((*req).Data[0]);
                            let end = (req as *mut u8).offset(bytes_returned as isize);
                            let len = end.offset_from(start) as usize;
                            let vec = slice::from_raw_parts(start, len).to_owned();
                            return Ok(vec);
                        }
                    }
                    WAIT_TIMEOUT => {
                        assert_eq!(CancelIo(raw_handle(&self.0)), TRUE);
                        // still need to wait for the event to be signaled
                        assert_eq!(
                            GetOverlappedResult(
                                raw_handle(&self.0),
                                &mut overlapped.0,
                                &mut bytes_returned,
                                TRUE,
                            ),
                            FALSE
                        );
                        assert_eq!(bytes_returned, 0);
                    }
                    _ => {
                        panic!("Unexpected result from WaitForSingleObject {wait_result}");
                    }
                }
            }

            let err = GetLastError();
            debug!("IOCTL_USB_GET_DESCRIPTOR_FROM_NODE_CONNECTION failed: type={descriptor_type} index={descriptor_index} error={err:?}");
            Err(match err {
                ERROR_GEN_FAILURE => Error::new_os(
                    ErrorKind::Other,
                    "descriptor request failed: device might be suspended.",
                    err,
                ),
                _ => Error::new_os(ErrorKind::Other, "descriptor request failed", err),
            })
        }
    }
}

pub struct HubPort {
    hub_handle: HubHandle,
    port_number: u32,
}
pub struct HubDeviceInfo {
    pub device_desc: USB_DEVICE_DESCRIPTOR,
    pub speed: Option<Speed>,
    pub address: u8,
    pub active_config: u8,
}

impl HubPort {
    pub fn by_child_devinst(devinst: DevInst) -> Result<HubPort, Error> {
        let parent_hub = devinst
            .parent()
            .ok_or_else(|| Error::new(ErrorKind::Disconnected, "failed to find parent hub"))?;
        let hub_handle = HubHandle::by_devinst(parent_hub)
            .ok_or_else(|| Error::new(ErrorKind::Disconnected, "failed to open parent hub"))?;
        let Some(port_number) = devinst.get_property::<u32>(DEVPKEY_Device_Address) else {
            return Err(Error::new(
                ErrorKind::Disconnected,
                "Could not find hub port number",
            ));
        };

        Ok(HubPort {
            hub_handle,
            port_number,
        })
    }

    pub fn get_info(&self) -> Result<HubDeviceInfo, Error> {
        #![allow(non_upper_case_globals)]

        let info = self.hub_handle.get_node_connection_info(self.port_number)?;
        let info_v2 = self
            .hub_handle
            .get_node_connection_info_v2(self.port_number)?;

        const SUPER_PLUS: u32 = DEVICE_IS_OPERATING_AT_SUPER_SPEED_PLUS_OR_HIGHER
            | DEVICE_IS_SUPER_SPEED_PLUS_CAPABLE_OR_HIGHER;
        const SUPER: u32 =
            DEVICE_IS_OPERATING_AT_SUPER_SPEED_OR_HIGHER | DEVICE_IS_SUPER_SPEED_CAPABLE_OR_HIGHER;

        let v2_flags = unsafe { info_v2.Flags.ul };

        let speed = match info.Speed as USB_DEVICE_SPEED {
            _ if v2_flags & SUPER_PLUS == SUPER_PLUS => Some(Speed::SuperPlus),
            _ if v2_flags & SUPER == SUPER => Some(Speed::Super),
            UsbHighSpeed => Some(Speed::High),
            UsbFullSpeed => Some(Speed::Full),
            UsbLowSpeed => Some(Speed::Low),
            _ => None,
        };

        Ok(HubDeviceInfo {
            device_desc: info.DeviceDescriptor,
            address: info.DeviceAddress as u8,
            active_config: info.CurrentConfigurationValue,
            speed,
        })
    }

    pub fn get_descriptor(
        &self,
        descriptor_type: u8,
        descriptor_index: u8,
        language_id: u16,
        timeout: std::time::Duration,
    ) -> Result<Vec<u8>, Error> {
        self.hub_handle.get_descriptor(
            self.port_number,
            descriptor_type,
            descriptor_index,
            language_id,
            timeout,
        )
    }
}
