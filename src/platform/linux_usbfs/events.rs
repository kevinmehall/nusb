use once_cell::sync::OnceCell;
use rustix::{
    event::epoll::{self, EventData},
    fd::OwnedFd,
};
use slab::Slab;
use std::{
    sync::{Mutex, Weak},
    thread,
};

use crate::Error;

use super::Device;

static EPOLL_FD: OnceCell<OwnedFd> = OnceCell::new();
static DEVICES: Mutex<Slab<Weak<Device>>> = Mutex::new(Slab::new());

pub(super) fn register(usb_fd: &OwnedFd, weak_device: Weak<Device>) -> Result<usize, Error> {
    let mut start_thread = false;
    let epoll_fd = EPOLL_FD.get_or_try_init(|| {
        start_thread = true;
        epoll::create(epoll::CreateFlags::CLOEXEC)
    })?;

    let id = {
        let mut devices = DEVICES.lock().unwrap();
        devices.insert(weak_device)
    };

    if start_thread {
        thread::spawn(event_loop);
    }

    let data = EventData::new_u64(id as u64);
    epoll::add(epoll_fd, usb_fd, data, epoll::EventFlags::OUT)?;
    Ok(id)
}

pub(super) fn unregister_fd(fd: &OwnedFd) {
    let epoll_fd = EPOLL_FD.get().unwrap();
    epoll::delete(epoll_fd, fd).ok();
}

pub(super) fn unregister(fd: &OwnedFd, events_id: usize) {
    let epoll_fd = EPOLL_FD.get().unwrap();
    epoll::delete(epoll_fd, fd).ok();
    DEVICES.lock().unwrap().remove(events_id);
}

fn event_loop() {
    let epoll_fd = EPOLL_FD.get().unwrap();
    let mut event_list = epoll::EventVec::with_capacity(4);
    loop {
        epoll::wait(epoll_fd, &mut event_list, -1).unwrap();
        for event in &event_list {
            let key = event.data.u64() as usize;
            let device = DEVICES.lock().unwrap().get(key).and_then(|w| w.upgrade());

            if let Some(device) = device {
                device.handle_events();
                // `device` gets dropped here. if it was the last reference, the LinuxDevice will be dropped.
                // That will unregister its fd, so it's important that DEVICES is unlocked here, or we'd deadlock.
            }
        }
    }
}
