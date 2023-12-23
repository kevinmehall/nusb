use std::{io::ErrorKind, task::Poll};

use crate::{hotplug::HotplugEvent, Error};

pub(crate) struct LinuxHotplugWatch {}

impl LinuxHotplugWatch {
    pub(crate) fn new() -> Result<Self, Error> {
        Err(Error::new(ErrorKind::Unsupported, "Not implemented."))
    }

    pub(crate) fn poll_next(&mut self, cx: &mut std::task::Context<'_>) -> Poll<HotplugEvent> {
        Poll::Pending
    }
}
