use log::{error, trace, warn};
use rustix::{
    fd::{AsFd, OwnedFd},
    net::{
        bind,
        netlink::{self, SocketAddrNetlink},
        recvfrom, socket_with, AddressFamily, RecvFlags, SocketFlags, SocketType,
    },
};
use std::{io::ErrorKind, os::unix::prelude::BorrowedFd, path::Path, task::Poll};

use crate::{hotplug::HotplugEvent, Error};

use super::{enumeration::probe_device, events::Async, SysfsPath};

const UDEV_MAGIC: &[u8; 12] = b"libudev\0\xfe\xed\xca\xfe";
const UDEV_MULTICAST_GROUP: u32 = 1 << 1;

pub(crate) struct LinuxHotplugWatch {
    fd: Async<OwnedFd>,
}

impl LinuxHotplugWatch {
    pub(crate) fn new() -> Result<Self, Error> {
        let fd = socket_with(
            AddressFamily::NETLINK,
            SocketType::RAW,
            SocketFlags::CLOEXEC,
            Some(netlink::KOBJECT_UEVENT),
        )?;
        bind(&fd, &SocketAddrNetlink::new(0, UDEV_MULTICAST_GROUP))?;
        Ok(LinuxHotplugWatch {
            fd: Async::new(fd)?,
        })
    }

    pub(crate) fn poll_next(&mut self, cx: &mut std::task::Context<'_>) -> Poll<HotplugEvent> {
        if let Some(event) = try_receive_event(self.fd.inner.as_fd()) {
            return Poll::Ready(event);
        }

        if let Err(e) = self.fd.register(cx.waker()) {
            log::error!("failed to register udev socket with epoll: {e}");
        }

        Poll::Pending
    }
}

fn try_receive_event(fd: BorrowedFd) -> Option<HotplugEvent> {
    let mut buf = [0; 8192];

    let (data, src) = match recvfrom(fd, &mut buf, RecvFlags::DONTWAIT) {
        Ok((size, src)) => (&buf[..size], src),
        Err(e) if e.kind() == ErrorKind::WouldBlock => return None,
        Err(e) => {
            error!("udev netlink socket recvfrom failed with {e}");
            return None;
        }
    };

    // udev messages will normally be sent to a multicast group, which only
    // root can send to. Reject unicast messages that may be from anywhere.
    match src.map(SocketAddrNetlink::try_from).transpose() {
        Ok(Some(nl)) if nl.groups() == UDEV_MULTICAST_GROUP => {}
        src => {
            warn!("udev netlink socket received message from {src:?}");
            return None;
        }
    }

    parse_packet(data)
}

fn parse_packet(buf: &[u8]) -> Option<HotplugEvent> {
    if buf.len() < 24 {
        error!("packet too short: {buf:x?}");
        return None;
    }

    if !buf.starts_with(UDEV_MAGIC) {
        error!("packet does not start with expected header: {buf:x?}");
        return None;
    }

    let properties_off = u32::from_ne_bytes(buf[16..20].try_into().unwrap()) as usize;
    let properties_len = u32::from_ne_bytes(buf[20..24].try_into().unwrap()) as usize;
    let Some(properties_buf) = buf.get(properties_off..properties_off + properties_len) else {
        error!("properties offset={properties_off} length={properties_len} exceeds buffer length {len}", len = buf.len());
        return None;
    };

    let mut is_add = None;
    let mut busnum = None;
    let mut devnum = None;
    let mut devpath = None;

    for (k, v) in parse_properties(properties_buf) {
        trace!("uevent property {k} = {v}");
        match k {
            "SUBSYSTEM" if v != "usb" => return None,
            "DEVTYPE" if v != "usb_device" => return None,
            "ACTION" => {
                is_add = Some(match v {
                    "add" => true,
                    "remove" => false,
                    _ => return None,
                });
            }
            "BUSNUM" => {
                busnum = v.parse::<u8>().ok();
            }
            "DEVNUM" => {
                devnum = v.parse::<u8>().ok();
            }
            "DEVPATH" => {
                devpath = Some(v);
            }
            _ => {}
        }
    }

    let is_add = is_add?;
    let busnum = busnum?;
    let devnum = devnum?;
    let devpath = devpath?;

    if is_add {
        let path = Path::new("/sys/").join(devpath.trim_start_matches('/'));
        match probe_device(SysfsPath(path.clone())) {
            Ok(d) => Some(HotplugEvent::Connected(d)),
            Err(e) => {
                warn!("Failed to probe device {path:?}: {e}");
                None
            }
        }
    } else {
        Some(HotplugEvent::Disconnected(crate::DeviceId(
            super::DeviceId {
                bus: busnum,
                addr: devnum,
            },
        )))
    }
}

/// Split nul-separated key=value pairs
fn parse_properties(buf: &[u8]) -> impl Iterator<Item = (&str, &str)> + '_ {
    buf.split(|b| b == &0)
        .filter_map(|entry| std::str::from_utf8(entry).ok()?.split_once('='))
}
