use libc::{sockaddr, sockaddr_nl, socklen_t, AF_NETLINK, MSG_DONTWAIT};
use log::{error, trace, warn};
use rustix::{
    fd::{AsFd, AsRawFd, OwnedFd},
    net::{netlink, socket_with, AddressFamily, SocketFlags, SocketType},
};
use std::{
    io::ErrorKind,
    mem,
    os::{raw::c_void, unix::prelude::BorrowedFd},
    path::Path,
    task::Poll,
};

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

        unsafe {
            // rustix doesn't support netlink yet (pending https://github.com/bytecodealliance/rustix/pull/1004)
            // so use libc for now.
            let mut addr: sockaddr_nl = mem::zeroed();
            addr.nl_family = AF_NETLINK as u16;
            addr.nl_groups = UDEV_MULTICAST_GROUP;
            let r = libc::bind(
                fd.as_raw_fd(),
                &addr as *const sockaddr_nl as *const sockaddr,
                mem::size_of_val(&addr) as socklen_t,
            );
            if r != 0 {
                return Err(Error::last_os_error());
            }
        }

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

    let received = unsafe {
        let mut addr: sockaddr_nl = mem::zeroed();
        let mut addrlen: socklen_t = mem::size_of_val(&addr) as socklen_t;
        let r = libc::recvfrom(
            fd.as_raw_fd(),
            buf.as_mut_ptr() as *mut c_void,
            buf.len(),
            MSG_DONTWAIT,
            &mut addr as *mut sockaddr_nl as *mut sockaddr,
            &mut addrlen,
        );
        if r >= 0 {
            Ok((r as usize, addr.nl_groups))
        } else {
            Err(Error::last_os_error())
        }
    };

    match received {
        // udev messages will normally be sent to a multicast group, which only
        // root can send to. Reject unicast messages that may be from anywhere.
        Ok((size, groups)) if groups == UDEV_MULTICAST_GROUP => parse_packet(&buf[..size]),
        Ok((_, src)) => {
            warn!("udev netlink socket received message from {src:?}");
            None
        }
        Err(e) if e.kind() == ErrorKind::WouldBlock => None,
        Err(e) => {
            error!("udev netlink socket recvfrom failed with {e}");
            None
        }
    }
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
