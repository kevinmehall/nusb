use std::{
    ffi::c_void,
    mem::{self, ManuallyDrop},
    ptr::null_mut,
};

use crate::transfer::{
    Completion, ControlIn, ControlOut, EndpointType, PlatformSubmit, PlatformTransfer,
    RequestBuffer, ResponseBuffer, TransferStatus, SETUP_PACKET_SIZE,
};

#[repr(C)]
pub struct TransferData {}

unsafe impl Send for TransferData {}

impl TransferData {
    pub(crate) fn new(
        clone: std::sync::Arc<super::Interface>,
        endpoint: u8,
        ep_type: EndpointType,
    ) -> TransferData {
        todo!()
    }

    fn fill(&mut self, v: Vec<u8>, len: usize, user_data: *mut c_void) {
        todo!()
    }

    /// SAFETY: requires that the transfer has completed and `length` bytes are initialized
    unsafe fn take_buf(&mut self, length: usize) -> Vec<u8> {
        todo!()
    }
}

impl Drop for TransferData {
    fn drop(&mut self) {
        todo!()
    }
}

impl PlatformTransfer for TransferData {
    fn cancel(&self) {
        todo!()
    }
}

impl PlatformSubmit<Vec<u8>> for TransferData {
    unsafe fn submit(&mut self, data: Vec<u8>, user_data: *mut c_void) {
        todo!()
    }

    unsafe fn take_completed(&mut self) -> Completion<ResponseBuffer> {
        todo!()
    }
}

impl PlatformSubmit<RequestBuffer> for TransferData {
    unsafe fn submit(&mut self, data: RequestBuffer, user_data: *mut c_void) {
        todo!()
    }

    unsafe fn take_completed(&mut self) -> Completion<Vec<u8>> {
        todo!()
    }
}

impl PlatformSubmit<ControlIn> for TransferData {
    unsafe fn submit(&mut self, data: ControlIn, user_data: *mut c_void) {
        todo!()
    }

    unsafe fn take_completed(&mut self) -> Completion<Vec<u8>> {
        todo!()
    }
}

impl PlatformSubmit<ControlOut<'_>> for TransferData {
    unsafe fn submit(&mut self, data: ControlOut, user_data: *mut c_void) {
        todo!()
    }

    unsafe fn take_completed(&mut self) -> Completion<ResponseBuffer> {
        todo!()
    }
}
