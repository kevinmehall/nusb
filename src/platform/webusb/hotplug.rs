use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    task::{Poll, Waker},
};

use wasm_bindgen_futures::spawn_local;
use web_sys::{
    wasm_bindgen::{prelude::Closure, JsCast},
    UsbConnectionEvent,
};

use crate::{hotplug::HotplugEvent, Error};

use super::{enumeration::device_to_info, DeviceId, UniqueUsbDevice};

struct Inner {
    waker: Option<Waker>,
    events: VecDeque<HotplugEvent>,
}

pub(crate) struct WebusbHotplugWatch {
    inner: Arc<Mutex<Inner>>,
}

// WebUSB callbacks dispatch on the JS event loop, which is single-threaded.
// We expose Send/Sync to satisfy the cross-platform HotplugWatch bound.
unsafe impl Send for WebusbHotplugWatch {}
unsafe impl Sync for WebusbHotplugWatch {}

fn push(inner: &Mutex<Inner>, event: HotplugEvent) {
    let waker = {
        let mut guard = inner.lock().unwrap();
        guard.events.push_back(event);
        guard.waker.take()
    };
    if let Some(w) = waker {
        w.wake();
    }
}

impl WebusbHotplugWatch {
    pub fn new() -> Result<Self, Error> {
        let usb = super::usb()?;
        let inner = Arc::new(Mutex::new(Inner {
            waker: None,
            events: VecDeque::new(),
        }));

        {
            let inner = inner.clone();
            let onconnect = Closure::wrap(Box::new(move |event: UsbConnectionEvent| {
                let inner = inner.clone();
                spawn_local(async move {
                    let device = Arc::new(UniqueUsbDevice::new(event.device()));
                    match device_to_info(device).await {
                        Ok(info) => push(&inner, HotplugEvent::Connected(info)),
                        Err(e) => log::warn!("hotplug connect descriptor read: {e:?}"),
                    }
                });
            }) as Box<dyn FnMut(UsbConnectionEvent)>);
            usb.set_onconnect(Some(onconnect.as_ref().unchecked_ref()));
            onconnect.forget();
        }

        {
            let inner = inner.clone();
            let ondisconnect = Closure::wrap(Box::new(move |event: UsbConnectionEvent| {
                let id = crate::DeviceId(DeviceId::from_device(&event.device()));
                push(&inner, HotplugEvent::Disconnected(id));
            }) as Box<dyn FnMut(UsbConnectionEvent)>);
            usb.set_ondisconnect(Some(ondisconnect.as_ref().unchecked_ref()));
            ondisconnect.forget();
        }

        Ok(Self { inner })
    }

    pub(crate) fn poll_next(&mut self, cx: &mut std::task::Context<'_>) -> Poll<HotplugEvent> {
        let mut guard = self.inner.lock().unwrap();
        if let Some(event) = guard.events.pop_front() {
            Poll::Ready(event)
        } else {
            guard.waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}
