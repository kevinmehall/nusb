use std::sync::Arc;

use crate::transfer::{ControlIn, ControlOut, PlatformSubmit, PlatformTransfer, RequestBuffer};

pub struct TransferData {
    capacity: usize,
    device: Arc<super::Device>,

    /// Not directly used, exists just to keep the interface from being released
    /// while active.
    _interface: Option<Arc<super::Interface>>,
}

unsafe impl Send for TransferData {}

impl PlatformTransfer for TransferData {
    fn cancel(&self) {
        todo!()
    }
}

impl PlatformSubmit<Vec<u8>> for TransferData {
    unsafe fn submit(&mut self, data: Vec<u8>, transfer: *mut std::ffi::c_void) {
        todo!()
    }

    unsafe fn take_completed(
        &mut self,
    ) -> crate::transfer::Completion<<Vec<u8> as crate::transfer::TransferRequest>::Response> {
        todo!()
    }
}

impl PlatformSubmit<RequestBuffer> for TransferData {
    unsafe fn submit(&mut self, data: RequestBuffer, transfer: *mut std::ffi::c_void) {
        todo!()
    }

    unsafe fn take_completed(
        &mut self,
    ) -> crate::transfer::Completion<<RequestBuffer as crate::transfer::TransferRequest>::Response>
    {
        todo!()
    }
}

impl PlatformSubmit<ControlIn> for TransferData {
    unsafe fn submit(&mut self, data: ControlIn, transfer: *mut std::ffi::c_void) {
        todo!()
    }

    unsafe fn take_completed(
        &mut self,
    ) -> crate::transfer::Completion<<ControlIn as crate::transfer::TransferRequest>::Response>
    {
        todo!()
    }
}

impl PlatformSubmit<ControlOut<'_>> for TransferData {
    unsafe fn submit(&mut self, data: ControlOut<'_>, transfer: *mut std::ffi::c_void) {
        todo!()
    }

    unsafe fn take_completed(
        &mut self,
    ) -> crate::transfer::Completion<<ControlOut<'_> as crate::transfer::TransferRequest>::Response>
    {
        todo!()
    }
}
