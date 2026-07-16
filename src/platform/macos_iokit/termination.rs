//! Complete pending transfers when a device's IOKit service is terminated.
//!
//! When a device is physically unplugged (or its hub re-enumerates), the
//! device's user client dies with it. In-flight async transfers submitted
//! through that user client receive no completion callback — and the
//! request's own `completionTimeout`/`noDataTimeout` are enforced by the
//! dead user client, so they never fire either. Anything blocked in
//! `MaybeFuture::wait()` then parks forever on the transfer's `Notify`.
//!
//! Fix: each `MacDevice` registers a `kIOGeneralInterest` notification on
//! its service and keeps a registry of in-flight transfer pointers. On
//! `kIOMessageServiceIsTerminated`, every still-pending transfer is
//! completed with `kIOReturnAborted` exactly the way the submit-failure
//! path already synthesizes completions.
//!
//! Safety model: the interest notification's run-loop source is added to
//! the same shared `nusb-events` run loop as the transfer completion
//! sources ([`add_event_source`]), so the termination sweep and genuine
//! completion callbacks execute on one thread and cannot race; and once
//! the service is terminated its user client can deliver no further
//! callbacks, so a swept transfer cannot also complete later. Bookkeeping
//! closes the remaining edges: a transfer is registered *before* it is
//! submitted and removed *before* `notify_completion` on every completion
//! path, and after a sweep the registry is closed so late submits fail
//! fast instead of orphaning a transfer.

use std::{ffi::c_void, sync::Mutex};

use core_foundation::base::TCFType;

use io_kit_sys::{
    kIOMasterPortDefault,
    keys::kIOGeneralInterest,
    ret::{kIOReturnAborted, kIOReturnSuccess},
    types::io_object_t,
    IONotificationPort, IONotificationPortCreate, IONotificationPortDestroy,
    IONotificationPortGetRunLoopSource, IOServiceAddInterestNotification,
};
use log::{debug, warn};
use slab::Slab;

use crate::transfer::internal::notify_completion;

use super::{
    events::{add_event_source, EventRegistration},
    iokit::{IoObject, IoService},
    TransferData,
};

/// `kIOMessageServiceIsTerminated` from `IOKit/IOMessage.h`
/// (`iokit_common_msg(0x010)`); not exported by `io-kit-sys`.
const K_IO_MESSAGE_SERVICE_IS_TERMINATED: u32 = 0xE000_0010;

struct DeviceEntry {
    /// Set when the service terminated: late submits must fail fast rather
    /// than register a transfer whose completion can never arrive.
    closed: bool,
}

#[derive(Default)]
struct Registry {
    devices: Slab<DeviceEntry>,
    /// In-flight `(transfer pointer, owning device's slab key)` pairs. A
    /// device rarely has more than a few transfers in flight, so a flat
    /// vector beats a map (and `Vec::new` is const for the static).
    pending: Vec<(usize, usize)>,
}

/// Owned by a global so the interest callback can never dangle: the
/// callback looks its device up by slab key and no-ops if it was already
/// unregistered (mirrors the hotplug `WAKERS` slab).
static REGISTRY: Mutex<Registry> = Mutex::new(Registry {
    devices: Slab::new(),
    pending: Vec::new(),
});

/// Register a device; returns its slab key (the interest callback refcon).
fn register_device() -> usize {
    REGISTRY
        .lock()
        .unwrap()
        .devices
        .insert(DeviceEntry { closed: false })
}

/// Forget a device and any transfers still charged to it (they are either
/// abandoned — the kernel callback will free them — or already swept).
fn unregister_device(key: usize) {
    let mut r = REGISTRY.lock().unwrap();
    if r.devices.try_remove(key).is_some() {
        r.pending.retain(|&(_, dev)| dev != key);
    }
}

/// Record an about-to-be-submitted transfer. `false` = the device already
/// terminated; the caller must NOT submit and should synthesize an aborted
/// completion instead. Must be called BEFORE the submit syscall so the
/// completion callback's [`remove_pending`] always observes the entry.
pub(super) fn try_add_pending(device_key: Option<usize>, transfer: *mut TransferData) -> bool {
    let Some(key) = device_key else {
        // Interest registration failed at open; behave as before the fix.
        return true;
    };
    let mut r = REGISTRY.lock().unwrap();
    match r.devices.get(key) {
        Some(entry) if entry.closed => false,
        Some(_) => {
            r.pending.push((transfer as usize, key));
            true
        }
        None => false,
    }
}

/// Drop a transfer from the registry on any completion path (kernel
/// callback, submit-failure synthesis, termination sweep). No-op for
/// transfers that were never registered (endpoint transfers, or a sweep
/// that already drained the entry).
pub(super) fn remove_pending(transfer: *mut TransferData) {
    let ptr = transfer as usize;
    REGISTRY.lock().unwrap().pending.retain(|&(p, _)| p != ptr);
}

/// The termination sweep: mark the device closed, drain its in-flight
/// transfers, and complete each with `kIOReturnAborted` — the same
/// synthesis the submit-failure path performs. `notify_completion` runs
/// outside the registry lock (it wakes waiters, which may immediately
/// re-enter to submit or drop transfers).
fn sweep_terminated(key: usize) {
    let swept: Vec<(usize, usize)> = {
        let mut r = REGISTRY.lock().unwrap();
        let Some(entry) = r.devices.get_mut(key) else {
            return;
        };
        entry.closed = true;
        let (swept, kept) = r.pending.drain(..).partition(|&(_, dev)| dev == key);
        r.pending = kept;
        swept
    };
    for (ptr, _) in swept {
        let transfer = ptr as *mut TransferData;
        debug!("Completing transfer {transfer:?} as aborted: device terminated");
        unsafe {
            // Complete the transfer in the place of the callback that the
            // dead user client can no longer deliver.
            (*transfer).status = kIOReturnAborted;
            notify_completion::<TransferData>(transfer);
        }
    }
}

unsafe extern "C" fn interest_callback(
    refcon: *mut c_void,
    _service: io_object_t,
    message_type: u32,
    _argument: *mut c_void,
) {
    if message_type == K_IO_MESSAGE_SERVICE_IS_TERMINATED {
        debug!("device service terminated; sweeping pending transfers");
        sweep_terminated(refcon as usize);
    }
}

struct NotificationPort(*mut IONotificationPort);
unsafe impl Send for NotificationPort {}
unsafe impl Sync for NotificationPort {}

impl Drop for NotificationPort {
    fn drop(&mut self) {
        unsafe { IONotificationPortDestroy(self.0) }
    }
}

/// A device's registration for termination sweeps. Dropping it detaches
/// the run-loop source, releases the notification, and forgets the device
/// (an interest callback already in flight no-ops on the stale slab key).
pub(super) struct TerminationRegistration {
    device_key: usize,
    _notification: IoObject,
    _event_registration: EventRegistration,
    _port: NotificationPort,
}

impl TerminationRegistration {
    /// The registry key transfers should be charged to.
    pub(super) fn device_key(&self) -> usize {
        self.device_key
    }
}

impl Drop for TerminationRegistration {
    fn drop(&mut self) {
        unregister_device(self.device_key);
    }
}

/// Register for termination sweeps on `service`. Best-effort: on failure
/// the device works exactly as before this fix (transfers can orphan on
/// termination), so callers treat `None` as "feature unavailable".
pub(super) fn register(service: &IoService) -> Option<TerminationRegistration> {
    let device_key = register_device();
    unsafe {
        let port = NotificationPort(IONotificationPortCreate(kIOMasterPortDefault));
        let mut notification: io_object_t = 0;
        let r = IOServiceAddInterestNotification(
            port.0,
            service.get(),
            kIOGeneralInterest,
            interest_callback,
            device_key as *mut c_void,
            &mut notification,
        );
        if r != kIOReturnSuccess {
            warn!("failed to register termination interest: {r:x}");
            unregister_device(device_key);
            return None;
        }
        let notification = IoObject::new(notification);
        let source = core_foundation::runloop::CFRunLoopSource::wrap_under_get_rule(
            IONotificationPortGetRunLoopSource(port.0),
        );
        let event_registration = add_event_source(source);
        Some(TerminationRegistration {
            device_key,
            _notification: notification,
            _event_registration: event_registration,
            _port: port,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transfer::internal::{take_completed_from_option, Idle, Notify, Pending};
    use std::sync::Arc;
    use std::time::Duration;

    fn pending_transfer() -> (Arc<Notify>, Pending<TransferData>, *mut TransferData) {
        let notify = Arc::new(Notify::new());
        let dyn_notify: Arc<dyn AsRef<Notify> + Send + Sync> = notify.clone();
        let idle = Idle::new(dyn_notify, TransferData::new());
        let pending = idle.pre_submit();
        let ptr = pending.as_ptr();
        (notify, pending, ptr)
    }

    /// Complete a transfer the way the kernel callback / submit-failure
    /// synthesis does (the test stands in for the kernel).
    unsafe fn complete(ptr: *mut TransferData, status: io_kit_sys::ret::IOReturn) {
        unsafe {
            (*ptr).status = status;
            notify_completion::<TransferData>(ptr);
        }
    }

    #[test]
    fn sweep_completes_pending_transfers_with_aborted_and_wakes_waiters() {
        let key = register_device();
        let (notify, pending, ptr) = pending_transfer();
        assert!(try_add_pending(Some(key), ptr));

        // A REAL blocked waiter — the same Notify + take_completed loop
        // `TransferFuture::wait` runs — with a timeout watchdog so a
        // regression fails the test instead of hanging the suite.
        let waiter = std::thread::spawn(move || {
            let mut transfer = Some(pending);
            notify
                .wait_timeout(Duration::from_secs(5), || {
                    take_completed_from_option(&mut transfer)
                })
                .map(|idle| idle.status)
        });

        sweep_terminated(key);
        let status = waiter.join().unwrap();
        assert_eq!(
            status,
            Some(kIOReturnAborted),
            "waiter woke with the synthesized aborted completion"
        );
        unregister_device(key);
    }

    #[test]
    fn closed_device_rejects_new_submissions() {
        let key = register_device();
        sweep_terminated(key);
        let (_notify, pending, ptr) = pending_transfer();
        assert!(
            !try_add_pending(Some(key), ptr),
            "post-termination submits must fail fast"
        );
        // Reclaim: complete it as the submit-failure path would after the
        // rejected registration, then drop the Idle it becomes.
        let mut transfer = Some(pending);
        unsafe { complete(ptr, kIOReturnAborted) };
        drop(take_completed_from_option(&mut transfer));
        unregister_device(key);
    }

    #[test]
    fn normally_completed_transfer_is_not_swept() {
        let key = register_device();
        let (_notify, pending, ptr) = pending_transfer();
        assert!(try_add_pending(Some(key), ptr));

        // The kernel-callback path: remove from the registry, then complete.
        remove_pending(ptr);
        unsafe { complete(ptr, kIOReturnSuccess) };
        let mut transfer = Some(pending);
        let idle = take_completed_from_option(&mut transfer).expect("completed");

        // The sweep must not double-complete the already-completed transfer.
        sweep_terminated(key);
        assert_eq!(
            idle.status, kIOReturnSuccess,
            "sweep left the completed transfer untouched"
        );
        unregister_device(key);
    }
}
