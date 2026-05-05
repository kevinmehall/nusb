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
    _onconnect: Closure<dyn FnMut(UsbConnectionEvent)>,
    _ondisconnect: Closure<dyn FnMut(UsbConnectionEvent)>,
}

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

        let onconnect = {
            let inner = inner.clone();
            Closure::wrap(Box::new(move |event: UsbConnectionEvent| {
                let inner = inner.clone();
                spawn_local(async move {
                    let device = Arc::new(UniqueUsbDevice::new(event.device()));
                    match device_to_info(device).await {
                        Ok(info) => push(&inner, HotplugEvent::Connected(info)),
                        Err(e) => log::warn!("hotplug connect descriptor read: {e:?}"),
                    }
                });
            }) as Box<dyn FnMut(UsbConnectionEvent)>)
        };
        usb.set_onconnect(Some(onconnect.as_ref().unchecked_ref()));

        let ondisconnect = {
            let inner = inner.clone();
            Closure::wrap(Box::new(move |event: UsbConnectionEvent| {
                let id = crate::DeviceId(DeviceId::from_device(&event.device()));
                push(&inner, HotplugEvent::Disconnected(id));
            }) as Box<dyn FnMut(UsbConnectionEvent)>)
        };
        usb.set_ondisconnect(Some(ondisconnect.as_ref().unchecked_ref()));

        Ok(Self {
            inner,
            _onconnect: onconnect,
            _ondisconnect: ondisconnect,
        })
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

impl Drop for WebusbHotplugWatch {
    fn drop(&mut self) {
        if let Ok(usb) = super::usb() {
            usb.set_onconnect(None);
            usb.set_ondisconnect(None);
        }
    }
}
