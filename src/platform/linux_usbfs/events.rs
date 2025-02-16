use atomic_waker::AtomicWaker;
/// Epoll based event loop for Linux.
///
/// Launches a thread when opening the first device that polls
/// for events on usbfs devices and arbitrary file descriptors
/// (used for udev hotplug).
///
/// ### Why not share an event loop with `tokio` or `async-io`?
///
/// This event loop will call USBFS_REAP_URB on the event thread and
/// dispatch to the transfer's waker directly. Since all USB transfers
/// on a device use the same file descriptor, putting USB-specific
/// dispatch in the event loop avoids additonal synchronization.
use once_cell::sync::OnceCell;
use rustix::{
    event::epoll::{self, EventData, EventFlags},
    fd::{AsFd, BorrowedFd, OwnedFd},
    io::retry_on_intr,
};
use slab::Slab;
use std::{
    io,
    sync::{Arc, Mutex, Weak},
    task::Waker,
    thread,
};

use crate::Error;

use super::Device;

static EPOLL_FD: OnceCell<OwnedFd> = OnceCell::new();
static WATCHES: Mutex<Slab<Watch>> = Mutex::new(Slab::new());

pub(super) enum Watch {
    Device(Weak<Device>),
    Fd(Arc<AtomicWaker>),
}

pub(super) fn register(fd: BorrowedFd, watch: Watch, flags: EventFlags) -> Result<usize, Error> {
    let mut start_thread = false;
    let epoll_fd = EPOLL_FD.get_or_try_init(|| {
        start_thread = true;
        epoll::create(epoll::CreateFlags::CLOEXEC)
    })?;

    let id = {
        let mut watches = WATCHES.lock().unwrap();
        watches.insert(watch)
    };

    if start_thread {
        thread::spawn(event_loop);
    }

    let data = EventData::new_u64(id as u64);
    epoll::add(epoll_fd, fd, data, flags)?;
    Ok(id)
}

pub(super) fn unregister_fd(fd: BorrowedFd) {
    let epoll_fd = EPOLL_FD.get().unwrap();
    epoll::delete(epoll_fd, fd).ok();
}

pub(super) fn unregister(fd: BorrowedFd, events_id: usize) {
    let epoll_fd = EPOLL_FD.get().unwrap();
    epoll::delete(epoll_fd, fd).ok();
    WATCHES.lock().unwrap().remove(events_id);
}

fn event_loop() {
    let epoll_fd = EPOLL_FD.get().unwrap();
    let mut event_list = Vec::with_capacity(4);
    loop {
        retry_on_intr(|| epoll::wait(epoll_fd, &mut event_list, None)).unwrap();
        for event in &event_list {
            let key = event.data.u64() as usize;
            log::trace!("event on {key}");
            let lock = WATCHES.lock().unwrap();
            let Some(watch) = lock.get(key) else { continue };

            match watch {
                Watch::Device(w) => {
                    if let Some(device) = w.upgrade() {
                        drop(lock);
                        device.handle_events();
                        // `device` gets dropped here. if it was the last reference, the LinuxDevice will be dropped.
                        // That will unregister its fd, so it's important that WATCHES is unlocked here, or we'd deadlock.
                    }
                }
                Watch::Fd(waker) => waker.wake(),
            }
        }
    }
}

pub(crate) struct Async<T> {
    pub(crate) inner: T,
    waker: Arc<AtomicWaker>,
    id: usize,
}

impl<T: AsFd> Async<T> {
    pub fn new(inner: T) -> Result<Self, io::Error> {
        let waker = Arc::new(AtomicWaker::new());
        let id = register(inner.as_fd(), Watch::Fd(waker.clone()), EventFlags::empty())?;
        Ok(Async { inner, id, waker })
    }

    pub fn register(&self, waker: &Waker) -> Result<(), io::Error> {
        self.waker.register(waker);
        let epoll_fd = EPOLL_FD.get().unwrap();
        epoll::modify(
            epoll_fd,
            self.inner.as_fd(),
            EventData::new_u64(self.id as u64),
            EventFlags::ONESHOT | EventFlags::IN,
        )?;
        Ok(())
    }
}
