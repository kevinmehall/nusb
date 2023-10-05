use std::sync::Arc;

use crate::{
    transfer::{EndpointType, TransferHandle},
    DeviceInfo, Error,
};

pub(crate) struct WindowsDevice {}

impl WindowsDevice {
    pub(crate) fn from_device_info(d: &DeviceInfo) -> Result<Arc<WindowsDevice>, Error> {
        todo!()
    }

    pub(crate) fn set_configuration(&self, configuration: u8) -> Result<(), Error> {
        todo!()
    }

    pub(crate) fn reset(&self) -> Result<(), Error> {
        todo!()
    }

    pub(crate) fn claim_interface(
        self: &Arc<Self>,
        interface: u8,
    ) -> Result<Arc<WindowsInterface>, Error> {
        todo!()
    }
}

impl Drop for WindowsDevice {
    fn drop(&mut self) {
        todo!()
    }
}

pub(crate) struct WindowsInterface {
    pub(crate) interface: u8,
    pub(crate) device: Arc<WindowsDevice>,
}

impl WindowsInterface {
    pub(crate) fn make_transfer(
        self: &Arc<Self>,
        endpoint: u8,
        ep_type: EndpointType,
    ) -> TransferHandle<super::TransferData> {
        TransferHandle::new(super::TransferData::new(self.clone(), endpoint, ep_type))
    }

    pub(crate) unsafe fn submit_urb(&self, urb: *mut ()) {
        todo!()
    }

    pub(crate) unsafe fn cancel_urb(&self, urb: *mut ()) {
        todo!()
    }
}

impl Drop for WindowsInterface {
    fn drop(&mut self) {
        todo!()
    }
}
