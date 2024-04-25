//! Wrappers for IOKit USB device and interface
//!
//! Based on Kate Temkin's [usrs](https://github.com/ktemkin/usrs)
//! licensed under MIT OR Apache-2.0.

use std::{collections::BTreeMap, io::ErrorKind, ptr, slice, time::Duration};

use core_foundation::{base::TCFType, runloop::CFRunLoopSource};
use core_foundation_sys::runloop::CFRunLoopSourceRef;
use io_kit_sys::{
    ret::{kIOReturnNoResources, kIOReturnSuccess},
    types::io_iterator_t,
};
use log::error;

use crate::{
    platform::macos_iokit::{
        iokit::usb_interface_type_id, iokit_c::kIOUsbInterfaceUserClientTypeID,
    },
    Error,
};

use super::{
    iokit::{
        self, call_iokit_function, check_iokit_return, usb_device_type_id, IoService,
        IoServiceIterator, PluginInterface,
    },
    iokit_c::{
        kIOCFPlugInInterfaceID, kIOUSBFindInterfaceDontCare, kIOUsbDeviceUserClientTypeID,
        IOCFPlugInInterface, IOCreatePlugInInterfaceForService, IOUSBConfigurationDescriptor,
        IOUSBFindInterfaceRequest,
    },
};

/// Wrapper around an IOKit UsbDevice
pub(crate) struct IoKitDevice {
    pub(crate) raw: *mut *mut iokit::UsbDevice,
}

impl IoKitDevice {
    /// Get the raw USB device associated with the service.
    pub(crate) fn new(service: IoService) -> Result<IoKitDevice, Error> {
        unsafe {
            // According to the libusb maintainers, this will sometimes spuriously
            // return `kIOReturnNoResources` for reasons Apple won't explain, usually
            // when a device is freshly plugged in. We'll allow this a few retries,
            // accordingly.
            //
            // [This behavior actually makes sense to me -- when the device is first plugged
            // in, it exists to IOKit, but hasn't been enumerated, yet. Accordingly, the device
            // interface doesn't actually yet exist for us to grab, and/or doesn't yet have the
            // right permissions for us to grab it. MacOS needs to see if a kernel driver binds
            // to it; as its security model won't allow the userland to grab a device that the
            // kernel owns.]
            //
            // If the kIOReturnNoResources persists, it's typically an indication that
            // macOS is preventing us from touching the relevant device due to its security
            // model. This happens when the device has a kernel-mode driver bound to the
            // whole device -- the kernel owns it, and it's unwilling to give it to us.
            for _ in 0..5 {
                let mut _score: i32 = 0;
                let mut raw_device_plugin: *mut *mut IOCFPlugInInterface = std::ptr::null_mut();

                let rc = IOCreatePlugInInterfaceForService(
                    service.get(),
                    kIOUsbDeviceUserClientTypeID(),
                    kIOCFPlugInInterfaceID(),
                    &mut raw_device_plugin,
                    &mut _score,
                );

                if rc == kIOReturnNoResources {
                    std::thread::sleep(Duration::from_millis(1));
                    continue;
                }

                if rc != kIOReturnSuccess {
                    return Err(Error::from_raw_os_error(rc));
                }

                if raw_device_plugin.is_null() {
                    error!("IOKit indicated it successfully created a PlugInInterface, but the pointer was NULL");
                    return Err(Error::new(
                        ErrorKind::Other,
                        "Could not create PlugInInterface",
                    ));
                }

                let device_plugin = PluginInterface::new(raw_device_plugin);

                let mut raw_device: *mut *mut iokit::UsbDevice = std::ptr::null_mut();

                call_iokit_function!(
                    device_plugin.get(),
                    QueryInterface(
                        usb_device_type_id(),
                        &mut raw_device as *mut *mut *mut _ as *mut *mut c_void
                    )
                );

                // macOS claims that call will never fail, and will always produce a valid pointer.
                // We don't trust it, so we're going to panic if it's lied to us.
                if raw_device.is_null() {
                    panic!(
                        "query_interface returned a null pointer, which Apple says is impossible"
                    );
                }

                return Ok(IoKitDevice { raw: raw_device });
            }

            return Err(Error::new(
                ErrorKind::NotFound,
                "Couldn't create device after retries",
            ));
        }
    }

    pub(crate) fn create_async_event_source(&self) -> Result<CFRunLoopSource, Error> {
        unsafe {
            let mut raw_source: CFRunLoopSourceRef = std::ptr::null_mut();
            check_iokit_return(call_iokit_function!(
                self.raw,
                CreateDeviceAsyncEventSource(&mut raw_source)
            ))?;
            Ok(CFRunLoopSource::wrap_under_create_rule(raw_source))
        }
    }

    /// Returns an IOKit iterator that can be used to iterate over all interfaces on this device.
    pub(crate) fn create_interface_iterator(&self) -> Result<IoServiceIterator, Error> {
        unsafe {
            let mut iterator: io_iterator_t = 0;

            let mut dont_care = IOUSBFindInterfaceRequest {
                bInterfaceClass: kIOUSBFindInterfaceDontCare,
                bInterfaceSubClass: kIOUSBFindInterfaceDontCare,
                bInterfaceProtocol: kIOUSBFindInterfaceDontCare,
                bAlternateSetting: kIOUSBFindInterfaceDontCare,
            };

            check_iokit_return(call_iokit_function!(
                self.raw,
                CreateInterfaceIterator(&mut dont_care, &mut iterator)
            ))?;

            Ok(IoServiceIterator::new(iterator))
        }
    }

    pub(crate) fn get_number_of_configurations(&self) -> Result<u8, Error> {
        unsafe {
            let mut num = 0;
            check_iokit_return(call_iokit_function!(
                self.raw,
                GetNumberOfConfigurations(&mut num)
            ))?;
            Ok(num)
        }
    }

    pub(crate) fn get_configuration_descriptor(&self, index: u8) -> Result<&[u8], Error> {
        unsafe {
            let mut ptr: *mut IOUSBConfigurationDescriptor = ptr::null_mut();
            check_iokit_return(call_iokit_function!(
                self.raw,
                GetConfigurationDescriptorPtr(index, &mut ptr)
            ))?;
            let len = u16::to_le((*ptr).wTotalLength) as usize;
            Ok(slice::from_raw_parts(ptr as *const u8, len))
        }
    }

    pub(crate) fn get_configuration(&self) -> Result<u8, Error> {
        unsafe {
            let mut val = 0;
            check_iokit_return(call_iokit_function!(self.raw, GetConfiguration(&mut val)))?;
            Ok(val)
        }
    }
}

impl Drop for IoKitDevice {
    fn drop(&mut self) {
        unsafe {
            call_iokit_function!(self.raw, Release());
        }
    }
}

unsafe impl Send for IoKitDevice {}
unsafe impl Sync for IoKitDevice {}

#[derive(Debug)]
#[allow(dead_code)]
pub(crate) struct EndpointInfo {
    pub(crate) pipe_ref: u8,
    pub(crate) direction: u8,
    pub(crate) number: u8,
    pub(crate) transfer_type: u8,
    pub(crate) max_packet_size: u16,
    pub(crate) interval: u8,
    pub(crate) max_burst: u8,
    pub(crate) mult: u8,
    pub(crate) bytes_per_interval: u16,
}

impl EndpointInfo {
    pub(crate) fn address(&self) -> u8 {
        if self.direction == 0 {
            self.number
        } else {
            self.number | 0x80
        }
    }
}

/// Wrapper around an IOKit UsbInterface
pub(crate) struct IoKitInterface {
    pub(crate) raw: *mut *mut iokit::UsbInterface,
}

impl IoKitInterface {
    pub(crate) fn new(service: IoService) -> Result<IoKitInterface, Error> {
        unsafe {
            let mut _score: i32 = 0;
            let mut raw_interface_plugin: *mut *mut IOCFPlugInInterface = std::ptr::null_mut();

            let rc = IOCreatePlugInInterfaceForService(
                service.get(),
                kIOUsbInterfaceUserClientTypeID(),
                kIOCFPlugInInterfaceID(),
                &mut raw_interface_plugin,
                &mut _score,
            );

            if rc != kIOReturnSuccess {
                return Err(Error::from_raw_os_error(rc));
            }

            if raw_interface_plugin.is_null() {
                error!("IOKit indicated it successfully created a PlugInInterface, but the pointer was NULL");
                return Err(Error::new(
                    ErrorKind::Other,
                    "Could not create PlugInInterface",
                ));
            }

            let interface_plugin = PluginInterface::new(raw_interface_plugin);

            let mut raw: *mut *mut iokit::UsbInterface = std::ptr::null_mut();

            call_iokit_function!(
                interface_plugin.get(),
                QueryInterface(
                    usb_interface_type_id(),
                    &mut raw as *mut *mut *mut _ as *mut *mut c_void
                )
            );

            // macOS claims that call will never fail, and will always produce a valid pointer.
            // We don't trust it, so we're going to panic if it's lied to us.
            if raw.is_null() {
                panic!("query_interface returned a null pointer, which Apple says is impossible");
            }

            return Ok(IoKitInterface { raw });
        }
    }

    pub(crate) fn open(&mut self) -> Result<(), Error> {
        unsafe { check_iokit_return(call_iokit_function!(self.raw, USBInterfaceOpen())) }
    }

    pub(crate) fn close(&mut self) -> Result<(), Error> {
        unsafe { check_iokit_return(call_iokit_function!(self.raw, USBInterfaceClose())) }
    }

    pub(crate) fn create_async_event_source(&self) -> Result<CFRunLoopSource, Error> {
        unsafe {
            let mut raw_source: CFRunLoopSourceRef = std::ptr::null_mut();
            check_iokit_return(call_iokit_function!(
                self.raw,
                CreateInterfaceAsyncEventSource(&mut raw_source)
            ))?;
            Ok(CFRunLoopSource::wrap_under_create_rule(raw_source))
        }
    }

    pub(crate) fn endpoints(&self) -> Result<BTreeMap<u8, EndpointInfo>, Error> {
        unsafe {
            let mut endpoints = BTreeMap::new();
            let mut count = 0;
            check_iokit_return(call_iokit_function!(self.raw, GetNumEndpoints(&mut count)))?;

            // Pipe references are 1-indexed
            for pipe_ref in 1..=count {
                let mut direction: u8 = 0;
                let mut number: u8 = 0;
                let mut transfer_type: u8 = 0;
                let mut max_packet_size: u16 = 0;
                let mut interval: u8 = 0;
                let mut max_burst: u8 = 0;
                let mut mult: u8 = 0;
                let mut bytes_per_interval: u16 = 0;

                check_iokit_return(call_iokit_function!(
                    self.raw,
                    GetPipePropertiesV2(
                        pipe_ref,
                        &mut direction,
                        &mut number,
                        &mut transfer_type,
                        &mut max_packet_size,
                        &mut interval,
                        &mut max_burst,
                        &mut mult,
                        &mut bytes_per_interval
                    )
                ))?;

                let endpoint = EndpointInfo {
                    pipe_ref,
                    direction,
                    number,
                    transfer_type,
                    max_packet_size,
                    interval,
                    max_burst,
                    mult,
                    bytes_per_interval,
                };

                endpoints.insert(endpoint.address(), endpoint);
            }
            Ok(endpoints)
        }
    }
}

impl Drop for IoKitInterface {
    fn drop(&mut self) {
        unsafe {
            call_iokit_function!(self.raw, Release());
        }
    }
}

unsafe impl Send for IoKitInterface {}
unsafe impl Sync for IoKitInterface {}
