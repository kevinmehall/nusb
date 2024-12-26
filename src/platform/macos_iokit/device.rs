use std::{
    collections::BTreeMap,
    ffi::c_void,
    io::ErrorKind,
    sync::{
        atomic::{AtomicU8, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use log::{debug, error};

use crate::{
    platform::macos_iokit::events::add_event_source,
    transfer::{Control, Direction, EndpointType, TransferError, TransferHandle},
    DeviceInfo, Error,
};

use super::{
    enumeration::service_by_registry_id,
    events::EventRegistration,
    iokit::{call_iokit_function, check_iokit_return},
    iokit_c::IOUSBDevRequestTO,
    iokit_usb::{EndpointInfo, IoKitDevice, IoKitInterface},
    status_to_transfer_result,
};

pub(crate) struct MacDevice {
    _event_registration: EventRegistration,
    pub(super) device: IoKitDevice,
    active_config: AtomicU8,
    is_open_exclusive: Mutex<bool>,
    claimed_interfaces: AtomicUsize,
}

// `get_configuration` does IO, so avoid it in the common case that:
//    * the device has a single configuration
//    * the device has at least one interface, indicating that it is configured
fn guess_active_config(dev: &IoKitDevice) -> Option<u8> {
    if dev.get_number_of_configurations().unwrap_or(0) != 1 {
        return None;
    }
    let mut intf = dev.create_interface_iterator().ok()?;
    intf.next()?;
    let config_desc = dev.get_configuration_descriptor(0).ok()?;
    config_desc.get(5).copied() // get bConfigurationValue from descriptor
}

impl MacDevice {
    pub(crate) async fn from_device_info(d: &DeviceInfo) -> Result<Arc<MacDevice>, Error> {
        log::info!("Opening device from registry id {}", d.registry_id);
        let service = service_by_registry_id(d.registry_id)?;
        let device = IoKitDevice::new(service)?;
        let _event_registration = add_event_source(device.create_async_event_source()?);

        let opened = match unsafe { call_iokit_function!(device.raw, USBDeviceOpen()) } {
            io_kit_sys::ret::kIOReturnSuccess => true,
            err => {
                // Most methods don't require USBDeviceOpen() so this can be ignored
                // to allow different processes to open different interfaces.
                log::debug!("Could not open device for exclusive access: {err:x}");
                false
            }
        };

        let active_config = if let Some(active_config) = guess_active_config(&device) {
            log::debug!("Active config from single descriptor is {}", active_config);
            active_config
        } else {
            let res = device.get_configuration();
            log::debug!("Active config from request is {:?}", res);
            res.unwrap_or(0)
        };

        Ok(Arc::new(MacDevice {
            _event_registration,
            device,
            active_config: AtomicU8::new(active_config),
            is_open_exclusive: Mutex::new(opened),
            claimed_interfaces: AtomicUsize::new(0),
        }))
    }

    pub(crate) fn active_configuration_value(&self) -> u8 {
        self.active_config.load(Ordering::SeqCst)
    }

    pub(crate) fn configuration_descriptors(&self) -> impl Iterator<Item = &[u8]> {
        let num_configs = self.device.get_number_of_configurations().unwrap_or(0);
        (0..num_configs).flat_map(|i| self.device.get_configuration_descriptor(i).ok())
    }

    fn require_open_exclusive(&self) -> Result<(), Error> {
        let mut state = self.is_open_exclusive.lock().unwrap();
        if *state == false {
            unsafe { check_iokit_return(call_iokit_function!(self.device.raw, USBDeviceOpen()))? };
            *state = true;
        }

        if self.claimed_interfaces.load(Ordering::Relaxed) != 0 {
            return Err(Error::new(
                ErrorKind::Other,
                "cannot perform this operation while interfaces are claimed",
            ));
        }

        Ok(())
    }

    pub(crate) async fn set_configuration(&self, configuration: u8) -> Result<(), Error> {
        self.require_open_exclusive()?;
        unsafe {
            check_iokit_return(call_iokit_function!(
                self.device.raw,
                SetConfiguration(configuration)
            ))?
        }
        log::debug!("Set configuration {configuration}");
        self.active_config.store(configuration, Ordering::SeqCst);
        Ok(())
    }

    pub(crate) async fn reset(&self) -> Result<(), Error> {
        self.require_open_exclusive()?;
        unsafe {
            check_iokit_return(call_iokit_function!(
                self.device.raw,
                USBDeviceReEnumerate(0)
            ))
        }
    }

    /// SAFETY: `data` must be valid for `len` bytes to read or write, depending on `Direction`
    unsafe fn control_blocking(
        &self,
        direction: Direction,
        control: Control,
        data: *mut u8,
        len: usize,
        timeout: Duration,
    ) -> Result<usize, TransferError> {
        let timeout_ms = timeout.as_millis().min(u32::MAX as u128) as u32;
        let mut req = IOUSBDevRequestTO {
            bmRequestType: control.request_type(direction),
            bRequest: control.request,
            wValue: control.value,
            wIndex: control.index,
            wLength: len.try_into().expect("length must fit in u16"),
            pData: data.cast::<c_void>(),
            wLenDone: 0,
            noDataTimeout: timeout_ms,
            completionTimeout: timeout_ms,
        };

        let r = unsafe { call_iokit_function!(self.device.raw, DeviceRequestTO(&mut req)) };

        status_to_transfer_result(r).map(|()| req.wLenDone as usize)
    }

    pub fn control_in_blocking(
        &self,
        control: Control,
        data: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, TransferError> {
        unsafe {
            self.control_blocking(
                Direction::In,
                control,
                data.as_mut_ptr(),
                data.len(),
                timeout,
            )
        }
    }

    pub fn control_out_blocking(
        &self,
        control: Control,
        data: &[u8],
        timeout: Duration,
    ) -> Result<usize, TransferError> {
        unsafe {
            self.control_blocking(
                Direction::Out,
                control,
                data.as_ptr() as *mut u8,
                data.len(),
                timeout,
            )
        }
    }

    pub(crate) fn make_control_transfer(self: &Arc<Self>) -> TransferHandle<super::TransferData> {
        TransferHandle::new(super::TransferData::new_control(self.clone()))
    }

    pub(crate) async fn claim_interface(
        self: &Arc<Self>,
        interface_number: u8,
    ) -> Result<Arc<MacInterface>, Error> {
        let intf_service = self
            .device
            .create_interface_iterator()?
            .nth(interface_number as usize)
            .ok_or(Error::new(ErrorKind::NotFound, "interface not found"))?;

        let mut interface = IoKitInterface::new(intf_service)?;
        let _event_registration = add_event_source(interface.create_async_event_source()?);

        interface.open()?;

        let endpoints = interface.endpoints()?;
        debug!("Found endpoints: {endpoints:?}");

        self.claimed_interfaces.fetch_add(1, Ordering::Acquire);

        Ok(Arc::new(MacInterface {
            device: self.clone(),
            interface_number,
            interface,
            endpoints: Mutex::new(endpoints),
            _event_registration,
        }))
    }

    pub(crate) async fn detach_and_claim_interface(
        self: &Arc<Self>,
        interface: u8,
    ) -> Result<Arc<MacInterface>, Error> {
        self.claim_interface(interface).await
    }
}

impl Drop for MacDevice {
    fn drop(&mut self) {
        if *self.is_open_exclusive.get_mut().unwrap() {
            match unsafe { call_iokit_function!(self.device.raw, USBDeviceClose()) } {
                io_kit_sys::ret::kIOReturnSuccess => {}
                err => log::debug!("Failed to close device: {err:x}"),
            };
        }
    }
}

pub(crate) struct MacInterface {
    pub(crate) interface_number: u8,
    _event_registration: EventRegistration,
    pub(crate) interface: IoKitInterface,
    pub(crate) device: Arc<MacDevice>,

    /// Map from address to a structure that contains the `pipe_ref` used by iokit
    pub(crate) endpoints: Mutex<BTreeMap<u8, EndpointInfo>>,
}

impl MacInterface {
    pub(crate) fn make_transfer(
        self: &Arc<Self>,
        endpoint: u8,
        ep_type: EndpointType,
    ) -> TransferHandle<super::TransferData> {
        if ep_type == EndpointType::Control {
            assert!(endpoint == 0);
            TransferHandle::new(super::TransferData::new_control(self.device.clone()))
        } else {
            let endpoints = self.endpoints.lock().unwrap();

            // This function can't fail, so if the endpoint is not found, use an invalid
            // pipe_ref that will fail when submitting the transfer.
            let pipe_ref = endpoints.get(&endpoint).map(|e| e.pipe_ref).unwrap_or(0);

            TransferHandle::new(super::TransferData::new(
                self.device.clone(),
                self.clone(),
                endpoint,
                pipe_ref,
            ))
        }
    }

    pub fn control_in_blocking(
        &self,
        control: Control,
        data: &mut [u8],
        timeout: Duration,
    ) -> Result<usize, TransferError> {
        self.device.control_in_blocking(control, data, timeout)
    }

    pub fn control_out_blocking(
        &self,
        control: Control,
        data: &[u8],
        timeout: Duration,
    ) -> Result<usize, TransferError> {
        self.device.control_out_blocking(control, data, timeout)
    }

    pub async fn set_alt_setting(&self, alt_setting: u8) -> Result<(), Error> {
        debug!(
            "Set interface {} alt setting to {alt_setting}",
            self.interface_number
        );

        let mut endpoints = self.endpoints.lock().unwrap();

        unsafe {
            check_iokit_return(call_iokit_function!(
                self.interface.raw,
                SetAlternateInterface(alt_setting)
            ))?;
        }

        *endpoints = self.interface.endpoints()?;
        debug!("Found endpoints: {endpoints:?}");

        Ok(())
    }

    pub async fn clear_halt(&self, endpoint: u8) -> Result<(), Error> {
        debug!("Clear halt, endpoint {endpoint:02x}");

        let pipe_ref = {
            let endpoints = self.endpoints.lock().unwrap();
            let ep = endpoints
                .get(&endpoint)
                .ok_or_else(|| Error::new(ErrorKind::NotFound, "Endpoint not found"))?;
            ep.pipe_ref
        };

        unsafe {
            check_iokit_return(call_iokit_function!(
                self.interface.raw,
                ClearPipeStallBothEnds(pipe_ref)
            ))
        }
    }
}

impl Drop for MacInterface {
    fn drop(&mut self) {
        if let Err(err) = self.interface.close() {
            error!("Failed to close interface: {err}")
        }
        self.device
            .claimed_interfaces
            .fetch_sub(1, Ordering::Release);
    }
}
