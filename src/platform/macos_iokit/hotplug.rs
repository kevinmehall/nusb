use std::{
    ffi::{c_char, c_void},
    mem::ManuallyDrop,
    sync::Mutex,
    task::{Context, Poll, Waker},
};

use core_foundation::{base::TCFType, dictionary::CFDictionary, runloop::CFRunLoopSource};
use io_kit_sys::{
    kIOMasterPortDefault,
    keys::{kIOFirstMatchNotification, kIOTerminatedNotification},
    ret::kIOReturnSuccess,
    types::io_iterator_t,
    usb::lib::kIOUSBDeviceClassName,
    IONotificationPort, IONotificationPortCreate, IONotificationPortDestroy,
    IONotificationPortGetRunLoopSource, IOServiceAddMatchingNotification, IOServiceMatching,
};
use log::debug;
use slab::Slab;

use crate::{hotplug::HotplugEvent, DeviceId, Error, ErrorKind};

use super::{
    enumeration::{get_registry_id, probe_device},
    events::{add_event_source, EventRegistration},
    iokit::IoServiceIterator,
};

// Wakers are owned by a global slab to avoid race conditions when freeing them
static WAKERS: Mutex<Slab<Option<Waker>>> = Mutex::new(Slab::new());

/// An AtomicWaker registered with `WAKERS`
struct SlabWaker(usize);

impl SlabWaker {
    fn new() -> SlabWaker {
        SlabWaker(WAKERS.lock().unwrap().insert(None))
    }

    fn register(&self, w: &Waker) {
        WAKERS.lock().unwrap()[self.0].replace(w.clone());
    }
}

impl Drop for SlabWaker {
    fn drop(&mut self) {
        WAKERS.lock().unwrap().remove(self.0);
    }
}

pub(crate) struct MacHotplugWatch {
    waker_id: SlabWaker,
    terminated_iter: IoServiceIterator,
    matched_iter: IoServiceIterator,
    _registration: EventRegistration,
    _notification_port: NotificationPort,
}

struct NotificationPort(*mut IONotificationPort);

impl NotificationPort {
    fn new() -> NotificationPort {
        unsafe { NotificationPort(IONotificationPortCreate(kIOMasterPortDefault)) }
    }
}

impl Drop for NotificationPort {
    fn drop(&mut self) {
        unsafe { IONotificationPortDestroy(self.0) }
    }
}

unsafe impl Send for NotificationPort {}

impl MacHotplugWatch {
    pub(crate) fn new() -> Result<Self, Error> {
        let waker_id = SlabWaker::new();

        let dictionary = unsafe {
            let d = IOServiceMatching(kIOUSBDeviceClassName);
            if d.is_null() {
                return Err(Error::new(ErrorKind::Other, "IOServiceMatching failed"));
            }
            CFDictionary::wrap_under_create_rule(d)
        };

        let notification_port = NotificationPort::new();
        let terminated_iter = register_notification(
            &notification_port,
            &dictionary,
            &waker_id,
            kIOTerminatedNotification,
        )?;
        let matched_iter = register_notification(
            &notification_port,
            &dictionary,
            &waker_id,
            kIOFirstMatchNotification,
        )?;

        let source = unsafe {
            CFRunLoopSource::wrap_under_get_rule(IONotificationPortGetRunLoopSource(
                notification_port.0,
            ))
        };
        let registration = add_event_source(source);

        Ok(MacHotplugWatch {
            waker_id,
            terminated_iter,
            matched_iter,
            _registration: registration,
            _notification_port: notification_port,
        })
    }

    pub fn poll_next(&mut self, cx: &mut Context) -> Poll<HotplugEvent> {
        self.waker_id.register(cx.waker());

        while let Some(s) = self.matched_iter.next() {
            if let Some(dev) = probe_device(s) {
                return Poll::Ready(HotplugEvent::Connected(dev));
            } else {
                debug!("failed to probe connected device");
            }
        }

        if let Some(s) = self.terminated_iter.next() {
            if let Some(registry_id) = get_registry_id(&s) {
                debug!("device {registry_id} disconnected");
                let id = DeviceId(registry_id);
                return Poll::Ready(HotplugEvent::Disconnected(id));
            } else {
                debug!("failed to get registry ID for disconnected device")
            }
        }

        Poll::Pending
    }
}

// Safety: Structurally Send and only method is &mut self, so Sync
// doesn't have any additional requirements.
unsafe impl Sync for MacHotplugWatch {}

fn register_notification(
    port: &NotificationPort,
    dictionary: &CFDictionary,
    waker: &SlabWaker,
    event: *const i8,
) -> Result<IoServiceIterator, Error> {
    assert!(event == kIOFirstMatchNotification || event == kIOTerminatedNotification);
    unsafe {
        let mut iter = 0;
        let r = IOServiceAddMatchingNotification(
            port.0,
            event as *const c_char,
            ManuallyDrop::new(dictionary.clone()).as_concrete_TypeRef(),
            callback,
            waker.0 as *mut c_void,
            &mut iter,
        );

        if r != kIOReturnSuccess {
            return Err(
                Error::new_os(ErrorKind::Other, "failed to register notification", r).log_error(),
            );
        }
        let mut iter = IoServiceIterator::new(iter);

        // Drain events for already-connected devices and to arm the notification for future events
        while let Some(_) = iter.next() {}

        Ok(iter)
    }
}

unsafe extern "C" fn callback(refcon: *mut c_void, _iterator: io_iterator_t) {
    debug!("hotplug event callback");
    let id = refcon as usize;
    if let Some(waker) = WAKERS.lock().unwrap().get_mut(id) {
        if let Some(w) = waker.take() {
            w.wake()
        }
    }
}
