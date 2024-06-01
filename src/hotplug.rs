//! Types for receiving notifications when USB devices are connected or
//! disconnected from the system.
//!
//! See [`super::watch_devices`] for a usage example.

use futures_core::Stream;

use crate::{DeviceId, DeviceInfo};

/// Stream of device connection / disconnection events.
///
/// Call [`super::watch_devices`] to begin watching device
/// events and create a `HotplugWatch`.
pub struct HotplugWatch(pub(crate) crate::platform::HotplugWatch);

impl Stream for HotplugWatch {
    type Item = HotplugEvent;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.0.poll_next(cx).map(Some)
    }
}

/// Event returned from the [`HotplugWatch`] stream.
#[derive(Debug)]
pub enum HotplugEvent {
    /// A device has been connected.
    Connected(DeviceInfo),

    /// A device has been disconnected.
    Disconnected(DeviceId),
}
