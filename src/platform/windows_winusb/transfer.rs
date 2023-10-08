use std::{
    ffi::c_void,
    io,
    mem::{self, ManuallyDrop},
    ptr::{addr_of_mut, null_mut},
    sync::Arc,
};

use log::{debug, error};
use windows_sys::Win32::{
    Devices::Usb::{
        WinUsb_ControlTransfer, WinUsb_GetOverlappedResult, WinUsb_ReadPipe, WinUsb_WritePipe,
        WINUSB_SETUP_PACKET,
    },
    Foundation::{
        GetLastError, ERROR_DEVICE_NOT_CONNECTED, ERROR_FILE_NOT_FOUND, ERROR_GEN_FAILURE,
        ERROR_IO_PENDING, ERROR_NOT_FOUND, ERROR_NO_SUCH_DEVICE, ERROR_REQUEST_ABORTED, FALSE,
        TRUE, WIN32_ERROR,
    },
    System::IO::{CancelIoEx, OVERLAPPED},
};

use crate::transfer::{
    notify_completion, Completion, ControlIn, ControlOut, EndpointType, PlatformSubmit,
    PlatformTransfer, RequestBuffer, ResponseBuffer, TransferError,
};

use super::util::raw_handle;

#[repr(C)]
pub(crate) struct EventNotify {
    // first member of repr(C) struct; can cast pointer between types
    overlapped: OVERLAPPED,
    ptr: *mut c_void,
}

pub struct TransferData {
    interface: Arc<super::Interface>,
    event: *mut EventNotify,
    buf: *mut u8,
    capacity: usize,
    endpoint: u8,
    ep_type: EndpointType,
    submit_error: Option<WIN32_ERROR>,
}

unsafe impl Send for TransferData {}

impl TransferData {
    pub(crate) fn new(
        interface: std::sync::Arc<super::Interface>,
        endpoint: u8,
        ep_type: EndpointType,
    ) -> TransferData {
        TransferData {
            interface,
            event: Box::into_raw(Box::new(unsafe { mem::zeroed() })),
            buf: null_mut(),
            capacity: 0,
            endpoint,
            ep_type,
            submit_error: None,
        }
    }

    /// SAFETY: requires that the transfer has completed and `length` bytes are initialized
    unsafe fn take_buf(&mut self, length: usize) -> Vec<u8> {
        let v = Vec::from_raw_parts(self.buf, length, self.capacity);
        self.buf = null_mut();
        self.capacity = 0;
        v
    }

    /// SAFETY: user_data must be the callback pointer passed to `submit`
    unsafe fn post_submit(&mut self, r: i32, func: &str, user_data: *mut c_void) {
        if r == TRUE {
            error!("{func} completed synchronously")
        }

        let err = GetLastError();

        if err != ERROR_IO_PENDING {
            self.submit_error = Some(err);
            error!("{func} failed: {}", io::Error::from_raw_os_error(err as _));

            // Safety: Transfer was not submitted, so we still own it
            // and must complete it in place of the event thread.
            notify_completion::<TransferData>(user_data);
        } else {
            self.submit_error = None;
        }
    }

    /// SAFETY: transfer must be completed
    unsafe fn get_status(&mut self) -> (usize, Result<(), TransferError>) {
        if let Some(err) = self.submit_error {
            debug!(
                "Transfer {:?} on endpoint {:02x} failed on submit: {}",
                self.event, self.endpoint, err
            );
            return (0, Err(map_error(err)));
        }

        let mut actual_len = 0;
        let r = WinUsb_GetOverlappedResult(
            self.interface.winusb_handle,
            self.event as *mut OVERLAPPED,
            &mut actual_len,
            FALSE,
        );

        let status = if r != 0 {
            debug!(
                "Transfer {:?} on endpoint {:02x} complete: {} bytes transferred",
                self.event, self.endpoint, actual_len
            );
            Ok(())
        } else {
            let err = GetLastError();
            debug!(
                "Transfer {:?} on endpoint {:02x} failed: {}, {} bytes transferred",
                self.event, self.endpoint, err, actual_len
            );
            Err(map_error(err))
        };

        (actual_len as usize, status)
    }
}

impl Drop for TransferData {
    fn drop(&mut self) {
        if !self.buf.is_null() {
            unsafe { drop(Vec::from_raw_parts(self.buf, 0, self.capacity)) }
        }
        unsafe { drop(Box::from_raw(self.event)) }
    }
}

impl PlatformTransfer for TransferData {
    fn cancel(&self) {
        debug!("Cancelling transfer {:?}", self.event);
        unsafe {
            let r = CancelIoEx(
                raw_handle(&self.interface.handle),
                self.event as *mut OVERLAPPED,
            );
            if r == 0 {
                let err = GetLastError();
                if err != ERROR_NOT_FOUND {
                    error!(
                        "CancelIoEx failed: {}",
                        io::Error::from_raw_os_error(err as i32)
                    );
                }
            }
        }
    }
}

impl PlatformSubmit<Vec<u8>> for TransferData {
    unsafe fn submit(&mut self, data: Vec<u8>, user_data: *mut c_void) {
        addr_of_mut!((*self.event).ptr).write(user_data);

        let mut data = ManuallyDrop::new(data);
        self.buf = data.as_mut_ptr();
        self.capacity = data.capacity();
        let len = data.len();

        debug!(
            "Submit transfer {:?} on endpoint {:02X} for {} bytes OUT",
            self.event, self.endpoint, len
        );

        let r = WinUsb_WritePipe(
            self.interface.winusb_handle,
            self.endpoint,
            self.buf,
            len.try_into().expect("transfer size should fit in u32"),
            null_mut(),
            self.event as *mut OVERLAPPED,
        );
        self.post_submit(r, "WinUsb_WritePipe", user_data);
    }

    unsafe fn take_completed(&mut self) -> Completion<ResponseBuffer> {
        let (actual_len, status) = self.get_status();
        let data = ResponseBuffer::from_vec(self.take_buf(0), actual_len);
        Completion { data, status }
    }
}

impl PlatformSubmit<RequestBuffer> for TransferData {
    unsafe fn submit(&mut self, data: RequestBuffer, user_data: *mut c_void) {
        addr_of_mut!((*self.event).ptr).write(user_data);

        let (buf, request_len) = data.into_vec();
        let mut buf = ManuallyDrop::new(buf);
        self.buf = buf.as_mut_ptr();
        self.capacity = buf.capacity();

        debug!(
            "Submit transfer {:?} on endpoint {:02X} for {} bytes IN",
            self.event, self.endpoint, request_len
        );

        let r = WinUsb_ReadPipe(
            self.interface.winusb_handle,
            self.endpoint,
            self.buf,
            request_len
                .try_into()
                .expect("transfer size should fit in u32"),
            null_mut(),
            self.event as *mut OVERLAPPED,
        );
        self.post_submit(r, "WinUsb_ReadPipe", user_data);
    }

    unsafe fn take_completed(&mut self) -> Completion<Vec<u8>> {
        let (actual_len, status) = self.get_status();
        let data = self.take_buf(actual_len);
        Completion { data, status }
    }
}

impl PlatformSubmit<ControlIn> for TransferData {
    unsafe fn submit(&mut self, data: ControlIn, user_data: *mut c_void) {
        assert_eq!(self.endpoint, 0);
        assert_eq!(self.ep_type, EndpointType::Control);
        addr_of_mut!((*self.event).ptr).write(user_data);

        let mut buf = ManuallyDrop::new(Vec::with_capacity(data.length as usize));
        self.buf = buf.as_mut_ptr();
        self.capacity = buf.capacity();

        debug!(
            "Submit transfer {:?} on endpoint {:02X} for {} bytes ControlIN",
            self.event, self.endpoint, data.length
        );

        let pkt = WINUSB_SETUP_PACKET {
            RequestType: data.request_type(),
            Request: data.request,
            Value: data.value,
            Index: data.index,
            Length: data.length,
        };

        let r = WinUsb_ControlTransfer(
            self.interface.winusb_handle,
            pkt,
            self.buf,
            data.length as u32,
            null_mut(),
            self.event as *mut OVERLAPPED,
        );

        self.post_submit(r, "WinUsb_ControlTransfer", user_data);
    }

    unsafe fn take_completed(&mut self) -> Completion<Vec<u8>> {
        let (actual_len, status) = self.get_status();
        let data = self.take_buf(actual_len);
        Completion { data, status }
    }
}

impl PlatformSubmit<ControlOut<'_>> for TransferData {
    unsafe fn submit(&mut self, data: ControlOut, user_data: *mut c_void) {
        assert_eq!(self.endpoint, 0);
        assert_eq!(self.ep_type, EndpointType::Control);
        addr_of_mut!((*self.event).ptr).write(user_data);

        let mut buf = ManuallyDrop::new(data.data.to_vec());
        self.buf = buf.as_mut_ptr();
        self.capacity = buf.capacity();
        let len: u16 = buf
            .len()
            .try_into()
            .expect("transfer size should fit in u16");

        debug!(
            "Submit transfer {:?} on endpoint {:02X} for {} bytes ControlOUT",
            self.event, self.endpoint, len
        );

        let pkt = WINUSB_SETUP_PACKET {
            RequestType: data.request_type(),
            Request: data.request,
            Value: data.value,
            Index: data.index,
            Length: len as u16,
        };

        let r = WinUsb_ControlTransfer(
            self.interface.winusb_handle,
            pkt,
            self.buf,
            len as u32,
            null_mut(),
            self.event as *mut OVERLAPPED,
        );

        self.post_submit(r, "WinUsb_ControlTransfer", user_data);
    }

    unsafe fn take_completed(&mut self) -> Completion<ResponseBuffer> {
        let (actual_len, status) = self.get_status();
        let data = ResponseBuffer::from_vec(self.take_buf(0), actual_len);
        Completion { data, status }
    }
}

pub(super) fn handle_event(completion: *mut OVERLAPPED) {
    let completion = completion as *mut EventNotify;
    debug!("Handling completion for transfer {completion:?}");
    unsafe {
        let p = addr_of_mut!((*completion).ptr).read();
        notify_completion::<TransferData>(p)
    }
}

fn map_error(err: WIN32_ERROR) -> TransferError {
    match err {
        ERROR_GEN_FAILURE => TransferError::Stall,
        ERROR_REQUEST_ABORTED => TransferError::Cancelled,
        ERROR_FILE_NOT_FOUND | ERROR_DEVICE_NOT_CONNECTED | ERROR_NO_SUCH_DEVICE => {
            TransferError::Disconnected
        }
        _ => TransferError::Unknown,
    }
}
