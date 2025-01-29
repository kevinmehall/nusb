//! Types for receiving notifications when USB devices are connected or
//! disconnected from the system.
//!
//! See [`super::watch_devices_cancellable`] for a usage example.

use atomic_waker::AtomicWaker;
use futures_core::Stream;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::task::Poll;

use crate::hotplug::HotplugEvent;

struct HotplugCancellationTokenInner {
    waker: AtomicWaker,
    cancelled: AtomicBool,
}

/// Cancellation token
///
/// Call cancel() once hotplug events needs to be stopped
#[derive(Clone)]
pub struct HotplugCancellationToken(Arc<HotplugCancellationTokenInner>);

/// Stream of device connection / disconnection events.
///
/// Call [`super::watch_devices`] to begin watching device
/// events and create a `HotplugWatch`.
pub struct CancellableHotplugWatch {
    platform: crate::platform::HotplugWatch,
    cancellation: HotplugCancellationToken,
}

impl Stream for CancellableHotplugWatch {
    type Item = HotplugEvent;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.cancellation.0.waker.register(cx.waker());
        if self
            .cancellation
            .0
            .cancelled
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return Poll::Ready(None);
        }

        self.platform.poll_next(cx).map(Some)
    }
}

impl HotplugCancellationToken {
    fn new() -> Self {
        Self(Arc::new(HotplugCancellationTokenInner {
            waker: AtomicWaker::new(),
            cancelled: AtomicBool::new(false),
        }))
    }

    /// Cancel lazily hotplug events
    pub fn cancel(&self) {
        self.0
            .cancelled
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.0.waker.wake();
    }
}

impl CancellableHotplugWatch {
    /// Create new CancellableHotplugWatch with HotplugCancellationToken
    pub fn new() -> Result<(Self, HotplugCancellationToken), crate::Error> {
        let token = HotplugCancellationToken::new();
        Ok((
            Self {
                platform: crate::platform::HotplugWatch::new()?,
                cancellation: token.clone(),
            },
            token,
        ))
    }
}
