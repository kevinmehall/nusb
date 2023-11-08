use std::sync::Arc;

use log::debug;

use crate::{
    transfer::{EndpointType, TransferHandle},
    DeviceInfo, Error,
};

pub(crate) struct MacDevice {}

impl MacDevice {
    pub(crate) fn from_device_info(d: &DeviceInfo) -> Result<Arc<MacDevice>, Error> {
        todo!()
    }

    pub(crate) fn set_configuration(&self, configuration: u8) -> Result<(), Error> {
        todo!()
    }

    pub(crate) fn reset(&self) -> Result<(), Error> {
        todo!()
    }

    pub(crate) fn make_control_transfer(self: &Arc<Self>) -> TransferHandle<super::TransferData> {
        todo!()
    }

    pub(crate) fn claim_interface(
        self: &Arc<Self>,
        interface: u8,
    ) -> Result<Arc<MacInterface>, Error> {
        todo!()
    }
}

pub(crate) struct MacInterface {
    pub(crate) interface: u8,
    pub(crate) device: Arc<MacDevice>,
}

impl MacInterface {
    pub(crate) fn make_transfer(
        self: &Arc<Self>,
        endpoint: u8,
        ep_type: EndpointType,
    ) -> TransferHandle<super::TransferData> {
        todo!()
    }

    pub fn set_alt_setting(&self, alt_setting: u8) -> Result<(), Error> {
        debug!(
            "Set interface {} alt setting to {alt_setting}",
            self.interface
        );

        todo!()
    }
}
