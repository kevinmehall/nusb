//! Epoll based event loop for Linux.
//!
//! Launches a thread when opening the first device that polls
//! for events on usbfs devices and arbitrary file descriptors
//! (used for udev hotplug).
//!
//! ### Why not share an event loop with `tokio` or `async-io`?
//!
//! This event loop will call USBFS_REAP_URB on the event thread and
//! dispatch to the transfer's waker directly. Since all USB transfers
//! on a device use the same file descriptor, putting USB-specific
//! dispatch in the event loop avoids additonal synchronization.

use crate::Error;
use once_cell::sync::OnceCell;
use rustix::{
    event::epoll::{self, EventData, EventFlags},
    fd::{AsFd, BorrowedFd, OwnedFd},
    io::Errno,
};
use slab::Slab;
use std::{
    io,
    mem::MaybeUninit,
    sync::{Arc, Mutex},
    task::Waker,
    thread,
};

use atomic_waker::AtomicWaker;

use super::Device;

static EPOLL_FD: OnceCell<OwnedFd> = OnceCell::new();

pub(crate) enum Tag {
    Device(usize),
    DeviceTimer(usize),
    Waker(usize),
}

impl Tag {
    const DEVICE: u64 = 1;
    const DEVICE_TIMER: u64 = 2;
    const WAKER: u64 = 3;

    fn as_event_data(&self) -> EventData {
        let (tag, id) = match *self {
            Tag::Device(id) => (Self::DEVICE, id),
            Tag::DeviceTimer(id) => (Self::DEVICE_TIMER, id),
            Tag::Waker(id) => (Self::WAKER, id),
        };
        EventData::new_u64((id as u64) << 3 | tag)
    }

    fn from_event_data(data: EventData) -> Self {
        let id = (data.u64() >> 3) as usize;
        let tag = data.u64() & 0b111;
        match (tag, id) {
            (Self::DEVICE, id) => Tag::Device(id),
            (Self::DEVICE_TIMER, id) => Tag::DeviceTimer(id),
            (Self::WAKER, id) => Tag::Waker(id),
            _ => panic!("Invalid event data"),
        }
    }
}

pub(super) fn register_fd(fd: BorrowedFd, tag: Tag, flags: EventFlags) -> Result<(), Error> {
    let mut start_thread = false;
    let epoll_fd = EPOLL_FD.get_or_try_init(|| {
        start_thread = true;
        epoll::create(epoll::CreateFlags::CLOEXEC).inspect_err(|e| {
            log::error!("Failed to initialize epoll: {e}");
        })
    })?;

    if start_thread {
        thread::spawn(event_loop);
    }

    epoll::add(epoll_fd, fd, tag.as_event_data(), flags).inspect_err(|e| {
        log::error!("Failed to add epoll watch: {e}");
    })?;

    Ok(())
}

pub(super) fn unregister_fd(fd: BorrowedFd) {
    let epoll_fd = EPOLL_FD.get().unwrap();
    epoll::delete(epoll_fd, fd).ok();
}

fn event_loop() {
    let epoll_fd = EPOLL_FD.get().unwrap();
    let mut event_buf = [MaybeUninit::<epoll::Event>::uninit(); 4];
    loop {
        let events = match epoll::wait(epoll_fd, &mut event_buf, None) {
            Ok((events, _)) => events,
            Err(Errno::INTR) => &mut [],
            Err(e) => panic!("epoll::wait failed: {e}"),
        };
        for event in events {
            match Tag::from_event_data(event.data) {
                Tag::Device(id) => Device::handle_usb_epoll(id),
                Tag::DeviceTimer(id) => Device::handle_timer_epoll(id),
                Tag::Waker(id) => {
                    if let Some(waker) = WAKERS.lock().unwrap().get(id) {
                        waker.wake();
                    }
                }
            }
        }
    }
}

static WAKERS: Mutex<Slab<Arc<AtomicWaker>>> = Mutex::new(Slab::new());

pub(crate) struct Async<T: AsFd> {
    pub(crate) inner: T,
    waker: Arc<AtomicWaker>,
    id: usize,
}

impl<T: AsFd> Async<T> {
    pub fn new(inner: T) -> Result<Self, io::Error> {
        let waker = Arc::new(AtomicWaker::new());
        let id = WAKERS.lock().unwrap().insert(waker.clone());
        register_fd(inner.as_fd(), Tag::Waker(id), EventFlags::empty())?;
        Ok(Async { inner, id, waker })
    }

    pub fn register(&self, waker: &Waker) -> Result<(), io::Error> {
        self.waker.register(waker);
        let epoll_fd = EPOLL_FD.get().unwrap();
        epoll::modify(
            epoll_fd,
            self.inner.as_fd(),
            Tag::Waker(self.id).as_event_data(),
            EventFlags::ONESHOT | EventFlags::IN,
        )?;
        Ok(())
    }
}

impl<T: AsFd> Drop for Async<T> {
    fn drop(&mut self) {
        unregister_fd(self.inner.as_fd());
        WAKERS.lock().unwrap().remove(self.id);
    }
}
