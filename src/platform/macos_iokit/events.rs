use std::{
    sync::{mpsc, Mutex},
    thread,
};

use core_foundation::runloop::{CFRunLoop, CFRunLoopSource};
use core_foundation_sys::runloop::kCFRunLoopCommonModes;
use log::info;

// Pending https://github.com/servo/core-foundation-rs/pull/649
struct SendCFRunLoopSource(CFRunLoopSource);
unsafe impl Send for SendCFRunLoopSource {}
unsafe impl Sync for SendCFRunLoopSource {}

struct EventLoop {
    runloop: Option<CFRunLoop>,
    count: usize,
}

static EVENT_LOOP: Mutex<EventLoop> = Mutex::new(EventLoop {
    runloop: None,
    count: 0,
});

pub(crate) fn add_event_source(source: CFRunLoopSource) -> EventRegistration {
    let mut event_loop = EVENT_LOOP.lock().unwrap();
    if let Some(runloop) = event_loop.runloop.as_ref() {
        if runloop.contains_source(&source, unsafe { kCFRunLoopCommonModes }) {
            panic!("source already registered");
        }
        runloop.add_source(&source, unsafe { kCFRunLoopCommonModes });
        event_loop.count += 1;
    } else {
        let (tx, rx) = mpsc::channel();
        let source = SendCFRunLoopSource(source.clone());
        info!("starting event loop thread");
        thread::spawn(move || {
            let runloop = CFRunLoop::get_current();
            let source = source;
            runloop.add_source(&source.0, unsafe { kCFRunLoopCommonModes });
            tx.send(runloop).unwrap();
            CFRunLoop::run_current();
            info!("event loop thread exited");
        });
        event_loop.runloop = Some(rx.recv().expect("failed to start run loop thread"));
        event_loop.count = 1;
    }
    EventRegistration(SendCFRunLoopSource(source))
}
pub(crate) struct EventRegistration(SendCFRunLoopSource);

impl Drop for EventRegistration {
    fn drop(&mut self) {
        let mut event_loop = EVENT_LOOP.lock().unwrap();
        event_loop.count -= 1;

        let runloop = event_loop
            .runloop
            .as_ref()
            .expect("runloop should exist while events are registered");
        runloop.remove_source(&self.0 .0, unsafe { kCFRunLoopCommonModes });

        if event_loop.count == 0 {
            runloop.stop();
            event_loop.runloop.take();
        }
    }
}
