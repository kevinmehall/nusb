use std::{
    ffi::{c_char, c_void},
    io::ErrorKind,
    mem::ManuallyDrop,
    task::{Context, Poll},
};

use atomic_waker::AtomicWaker;
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

use crate::{
    hotplug::HotplugEvent,
    DeviceId, Error,
};

use super::{
    enumeration::{get_registry_id, probe_device},
    events::{add_event_source, EventRegistration},
    iokit::IoServiceIterator,
};

struct Inner {
    waker: AtomicWaker,
}

pub(crate) struct MacHotplugWatch {
    inner: *mut Inner,
    terminated_iter: IoServiceIterator,
    matched_iter: IoServiceIterator,
    registration: EventRegistration,
    notification_port: *mut IONotificationPort,
}

unsafe impl Send for MacHotplugWatch {}

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

impl MacHotplugWatch {
    pub(crate) fn new() -> Result<Self, Error> {
        let dictionary = unsafe {
            let d = IOServiceMatching(kIOUSBDeviceClassName);
            if d.is_null() {
                return Err(Error::new(ErrorKind::Other, "IOServiceMatching failed"));
            }
            CFDictionary::wrap_under_create_rule(d)
        };

        unsafe {
            let notification_port = IONotificationPortCreate(kIOMasterPortDefault);

            let inner = Box::into_raw(Box::new(Inner {
                waker: AtomicWaker::new(),
            }));

            let mut terminated_iter = 0;
            let r1 = IOServiceAddMatchingNotification(
                notification_port,
                kIOTerminatedNotification as *mut c_char,
                ManuallyDrop::new(dictionary.clone()).as_concrete_TypeRef(),
                callback,
                inner as *mut c_void,
                &mut terminated_iter,
            );

            let mut matched_iter = 0;
            let r2 = IOServiceAddMatchingNotification(
                notification_port,
                kIOFirstMatchNotification as *mut c_char,
                ManuallyDrop::new(dictionary.clone()).as_concrete_TypeRef(),
                callback,
                inner as *mut c_void,
                &mut matched_iter,
            );

            if r1 != kIOReturnSuccess || r2 != kIOReturnSuccess {
                IONotificationPortDestroy(notification_port);
                return Err(Error::new(
                    ErrorKind::Other,
                    "Failed to register notification",
                ));
            }

            let terminated_iter = IoServiceIterator::new(terminated_iter);
            let mut matched_iter = IoServiceIterator::new(matched_iter);

            // Drain events for already-connected devices
            while let Some(_) = matched_iter.next() {}

            let source = CFRunLoopSource::wrap_under_create_rule(
                IONotificationPortGetRunLoopSource(notification_port),
            );
            let registration = add_event_source(source);

            Ok(MacHotplugWatch {
                inner,
                terminated_iter,
                matched_iter,
                registration,
                notification_port,
            })
        }
    }

    fn inner(&self) -> &Inner {
        unsafe { &*self.inner }
    }

    pub fn poll_next(&mut self, cx: &mut Context) -> Poll<HotplugEvent> {
        self.inner().waker.register(cx.waker());

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

impl Drop for MacHotplugWatch {
    fn drop(&mut self) {
        unsafe { IONotificationPortDestroy(self.notification_port) }
        // TODO: this leaks `inner`, because it's not safe to drop it here,
        // since the callback could be currently executing and accessing it. One
        // way to fix this would be to send it to the callback thread and drop
        // it there.
    }
}

unsafe extern "C" fn callback(refcon: *mut c_void, _iterator: io_iterator_t) {
    debug!("hotplug event callback");
    let inner = unsafe { &*(refcon as *const Inner) };
    inner.waker.wake()
}
