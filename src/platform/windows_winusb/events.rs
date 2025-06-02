use once_cell::sync::OnceCell;
use std::{
    os::windows::{
        io::HandleOrNull,
        prelude::{OwnedHandle, RawHandle},
    },
    ptr, thread,
};
use windows_sys::Win32::{
    Foundation::{GetLastError, FALSE, INVALID_HANDLE_VALUE},
    System::IO::{CreateIoCompletionPort, GetQueuedCompletionStatusEx, OVERLAPPED_ENTRY},
};

use crate::Error;

use super::util::raw_handle;

struct IoCompletionPort(OwnedHandle);

impl IoCompletionPort {
    fn new() -> Result<IoCompletionPort, Error> {
        unsafe {
            let port = CreateIoCompletionPort(INVALID_HANDLE_VALUE, ptr::null_mut(), 0, 0);
            match HandleOrNull::from_raw_handle(port as RawHandle).try_into() {
                Ok(handle) => Ok(IoCompletionPort(handle)),
                Err(_) => Err(Error::new_os(
                    crate::ErrorKind::Other,
                    "failed to create IO completion port",
                    GetLastError(),
                )
                .log_error()),
            }
        }
    }

    fn register(&self, handle: &OwnedHandle) -> Result<(), Error> {
        unsafe {
            let r = CreateIoCompletionPort(raw_handle(handle), raw_handle(&self.0), 0, 0);
            if r.is_null() {
                Err(Error::new_os(
                    crate::ErrorKind::Other,
                    "failed to register IO completion port",
                    GetLastError(),
                )
                .log_error())
            } else {
                Ok(())
            }
        }
    }

    fn wait(&self, events: &mut Vec<OVERLAPPED_ENTRY>) -> Result<(), Error> {
        unsafe {
            let mut event_count = 0;
            let r = GetQueuedCompletionStatusEx(
                raw_handle(&self.0),
                events.as_mut_ptr(),
                events
                    .capacity()
                    .try_into()
                    .expect("events capacity should fit in u32"),
                &mut event_count,
                u32::MAX,
                0,
            );

            if r == FALSE {
                Err(Error::new_os(
                    crate::ErrorKind::Other,
                    "failed to get events from IO completion port",
                    GetLastError(),
                )
                .log_error())
            } else {
                events.set_len(event_count as usize);
                Ok(())
            }
        }
    }
}

static IOCP_HANDLE: OnceCell<IoCompletionPort> = OnceCell::new();

pub(super) fn register(usb_fd: &OwnedHandle) -> Result<(), Error> {
    let mut start_thread = false;
    let iocp = IOCP_HANDLE.get_or_try_init(|| {
        start_thread = true;
        IoCompletionPort::new()
    })?;

    if start_thread {
        thread::spawn(event_loop);
    }

    iocp.register(usb_fd)
}

fn event_loop() {
    let iocp = IOCP_HANDLE.get().unwrap();
    let mut event_list = Vec::with_capacity(8);
    loop {
        event_list.clear();
        iocp.wait(&mut event_list).unwrap();

        for event in &event_list {
            super::transfer::handle_event(event.lpOverlapped);
        }
    }
}
