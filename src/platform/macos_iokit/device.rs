use std::{collections::BTreeMap, io::ErrorKind, sync::Arc};

use log::{debug, error};

use crate::{
    platform::macos_iokit::events::add_event_source,
    transfer::{EndpointType, TransferHandle},
    DeviceInfo, Error,
};

use super::{
    enumeration::service_by_registry_id,
    events::EventRegistration,
    iokit::{call_iokit_function, check_iokit_return},
    iokit_usb::{EndpointInfo, IoKitDevice, IoKitInterface},
};

pub(crate) struct MacDevice {
    _event_registration: EventRegistration,
    pub(super) device: IoKitDevice,
}

impl MacDevice {
    pub(crate) fn from_device_info(d: &DeviceInfo) -> Result<Arc<MacDevice>, Error> {
        log::info!("Opening device from registry id {}", d.registry_id);
        let service = service_by_registry_id(d.registry_id)?;
        let device = IoKitDevice::new(service)?;
        let _event_registration = add_event_source(device.create_async_event_source()?);

        Ok(Arc::new(MacDevice {
            _event_registration,
            device,
        }))
    }

    pub(crate) fn set_configuration(&self, configuration: u8) -> Result<(), Error> {
        unsafe {
            check_iokit_return(call_iokit_function!(
                self.device.raw,
                SetConfiguration(configuration)
            ))
        }
    }

    pub(crate) fn reset(&self) -> Result<(), Error> {
        unsafe {
            check_iokit_return(call_iokit_function!(
                self.device.raw,
                USBDeviceReEnumerate(0)
            ))
        }
    }

    pub(crate) fn make_control_transfer(self: &Arc<Self>) -> TransferHandle<super::TransferData> {
        TransferHandle::new(super::TransferData::new_control(self.clone()))
    }

    pub(crate) fn claim_interface(
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

        Ok(Arc::new(MacInterface {
            device: self.clone(),
            interface_number,
            interface,
            endpoints,
            _event_registration,
        }))
    }
}

pub(crate) struct MacInterface {
    pub(crate) interface_number: u8,
    _event_registration: EventRegistration,
    pub(crate) interface: IoKitInterface,
    pub(crate) device: Arc<MacDevice>,

    /// Map from address to a structure that contains the `pipe_ref` used by iokit
    pub(crate) endpoints: BTreeMap<u8, EndpointInfo>,
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
            let endpoint = self.endpoints.get(&endpoint).expect("Endpoint not found");
            TransferHandle::new(super::TransferData::new(
                self.device.clone(),
                self.clone(),
                endpoint,
            ))
        }
    }

    pub fn set_alt_setting(&self, alt_setting: u8) -> Result<(), Error> {
        debug!(
            "Set interface {} alt setting to {alt_setting}",
            self.interface_number
        );

        unsafe {
            check_iokit_return(call_iokit_function!(
                self.interface.raw,
                SetAlternateInterface(alt_setting)
            ))
        }
    }
}

impl Drop for MacInterface {
    fn drop(&mut self) {
        if let Err(err) = self.interface.close() {
            error!("Failed to close interface: {err}")
        }
    }
}
