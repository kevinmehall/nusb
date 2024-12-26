//! Wrappers for the [usbfs] character device ioctls, translated from the
//! [C structures and ioctl definitions][uapi].
//!
//! [usbfs]: https://www.kernel.org/doc/html/latest/driver-api/usb/usb.html#the-usb-character-device-nodes
//! [uapi]: https://github.com/torvalds/linux/blob/master/tools/include/uapi/linux/usbdevice_fs.h
#![allow(dead_code)]
use std::{
    ffi::{c_int, c_uchar, c_uint, c_void},
    marker::PhantomData,
};

use rustix::{
    fd::AsFd,
    io,
    ioctl::{self, CompileTimeOpcode, Ioctl, IoctlOutput},
};

pub fn set_configuration<Fd: AsFd>(fd: Fd, configuration: u8) -> io::Result<()> {
    unsafe {
        let ctl =
            ioctl::Setter::<ioctl::ReadOpcode<b'U', 5, c_uint>, c_uint>::new(configuration.into());
        ioctl::ioctl(fd, ctl)
    }
}

pub async fn claim_interface<Fd: AsFd>(fd: Fd, interface: u8) -> io::Result<()> {
    unsafe {
        let ctl =
            ioctl::Setter::<ioctl::ReadOpcode<b'U', 15, c_uint>, c_uint>::new(interface.into());
        ioctl::ioctl(fd, ctl)
    }
}

pub fn release_interface<Fd: AsFd>(fd: Fd, interface: u8) -> io::Result<()> {
    unsafe {
        let ctl =
            ioctl::Setter::<ioctl::ReadOpcode<b'U', 16, c_uint>, c_uint>::new(interface.into());
        ioctl::ioctl(fd, ctl)
    }
}

#[repr(C)]
struct DetachAndClaim {
    interface: c_uint,
    flags: c_uint,
    driver: [c_uchar; 255 + 1],
}

pub async fn detach_and_claim_interface<Fd: AsFd>(fd: Fd, interface: u8) -> io::Result<()> {
    const USBDEVFS_DISCONNECT_CLAIM_EXCEPT_DRIVER: c_uint = 0x02;
    unsafe {
        let mut dc = DetachAndClaim {
            interface: interface.into(),
            flags: USBDEVFS_DISCONNECT_CLAIM_EXCEPT_DRIVER,
            driver: [0; 256],
        };

        dc.driver[0..6].copy_from_slice(b"usbfs\0");

        let ctl = ioctl::Setter::<opcodes::USBDEVFS_DISCONNECT_CLAIM, DetachAndClaim>::new(dc);

        ioctl::ioctl(&fd, ctl)
    }
}

#[repr(C)]
struct UsbFsIoctl {
    interface: c_uint,
    ioctl_code: c_uint,
    data: *mut c_void,
}

/// Opcodes used in ioctl with the usb device fs.
///
/// Taken from https://github.com/torvalds/linux/blob/e9680017b2dc8686a908ea1b51941a91b6da9f1d/include/uapi/linux/usbdevice_fs.h#L187
// We repeat the USBDEVFS_ prefix to help keep the same names as what linux uses.
// This makes the code more searchable.
// TODO: Move the rest of the opcodes into here.
#[allow(non_camel_case_types)]
mod opcodes {
    use super::*;

    pub type USBDEVFS_IOCTL = ioctl::ReadWriteOpcode<b'U', 18, UsbFsIoctl>;
    pub type USBDEVFS_DISCONNECT_CLAIM = ioctl::ReadOpcode<b'U', 27, DetachAndClaim>;

    /// These opcodes are nested inside a [`USBDEVFS_IOCTL`] operation.
    pub mod nested {
        use super::*;

        pub type USBDEVFS_DISCONNECT = ioctl::NoneOpcode<b'U', 22, ()>;
        pub type USBDEVFS_CONNECT = ioctl::NoneOpcode<b'U', 23, ()>;
    }
}

pub fn detach_kernel_driver<Fd: AsFd>(fd: Fd, interface: u8) -> io::Result<()> {
    let command = UsbFsIoctl {
        interface: interface.into(),
        // NOTE: Cast needed since on android this type is i32 vs u32 on linux
        ioctl_code: opcodes::nested::USBDEVFS_DISCONNECT::OPCODE.raw() as _,
        data: std::ptr::null_mut(),
    };
    unsafe {
        let ctl = ioctl::Setter::<opcodes::USBDEVFS_IOCTL, UsbFsIoctl>::new(command);
        ioctl::ioctl(fd, ctl)
    }
}

pub fn attach_kernel_driver<Fd: AsFd>(fd: Fd, interface: u8) -> io::Result<()> {
    let command = UsbFsIoctl {
        interface: interface.into(),
        ioctl_code: opcodes::nested::USBDEVFS_CONNECT::OPCODE.raw() as _,
        data: std::ptr::null_mut(),
    };
    unsafe {
        let ctl = ioctl::Setter::<opcodes::USBDEVFS_IOCTL, UsbFsIoctl>::new(command);
        ioctl::ioctl(fd, ctl)
    }
}

#[repr(C)]
struct SetAltSetting {
    interface: c_int,
    alt_setting: c_int,
}

pub fn set_interface<Fd: AsFd>(fd: Fd, interface: u8, alt_setting: u8) -> io::Result<()> {
    unsafe {
        let ctl = ioctl::Setter::<ioctl::ReadOpcode<b'U', 4, SetAltSetting>, SetAltSetting>::new(
            SetAltSetting {
                interface: interface.into(),
                alt_setting: alt_setting.into(),
            },
        );
        ioctl::ioctl(fd, ctl)
    }
}

pub struct PassPtr<Opcode, Input> {
    input: *mut Input,
    _opcode: PhantomData<Opcode>,
}

impl<Opcode: CompileTimeOpcode, Input> PassPtr<Opcode, Input> {
    /// Create a new pointer setter-style `ioctl` object.
    ///
    /// # Safety
    ///
    /// - `Opcode` must provide a valid opcode.
    /// - For this opcode, `Input` must be the type that the kernel expects to
    ///   get.
    #[inline]
    pub unsafe fn new(input: *mut Input) -> Self {
        Self {
            input,
            _opcode: PhantomData,
        }
    }
}

unsafe impl<Opcode: CompileTimeOpcode, Input> Ioctl for PassPtr<Opcode, Input> {
    type Output = ();

    const IS_MUTATING: bool = false;
    const OPCODE: rustix::ioctl::Opcode = Opcode::OPCODE;

    fn as_ptr(&mut self) -> *mut c_void {
        self.input as *mut c_void
    }

    unsafe fn output_from_ptr(_: IoctlOutput, _: *mut c_void) -> rustix::io::Result<Self::Output> {
        Ok(())
    }
}

pub unsafe fn submit_urb<Fd: AsFd>(fd: Fd, urb: *mut Urb) -> io::Result<()> {
    unsafe {
        let ctl = PassPtr::<ioctl::ReadOpcode<b'U', 10, Urb>, Urb>::new(urb);
        ioctl::ioctl(fd, ctl)
    }
}

pub fn reap_urb_ndelay<Fd: AsFd>(fd: Fd) -> io::Result<*mut Urb> {
    unsafe {
        let ctl = ioctl::Getter::<ioctl::WriteOpcode<b'U', 13, *mut Urb>, *mut Urb>::new();
        ioctl::ioctl(fd, ctl)
    }
}

pub unsafe fn discard_urb<Fd: AsFd>(fd: Fd, urb: *mut Urb) -> io::Result<()> {
    unsafe {
        let ctl = PassPtr::<ioctl::NoneOpcode<b'U', 11, ()>, Urb>::new(urb);
        ioctl::ioctl(fd, ctl)
    }
}

pub fn reset<Fd: AsFd>(fd: Fd) -> io::Result<()> {
    unsafe {
        let ctl = ioctl::NoArg::<ioctl::NoneOpcode<b'U', 20, ()>>::new();
        ioctl::ioctl(fd, ctl)
    }
}

const USBDEVFS_URB_SHORT_NOT_OK: c_uint = 0x01;
const USBDEVFS_URB_ISO_ASAP: c_uint = 0x02;
const USBDEVFS_URB_BULK_CONTINUATION: c_uint = 0x04;
const USBDEVFS_URB_ZERO_PACKET: c_uint = 0x40;
const USBDEVFS_URB_NO_INTERRUPT: c_uint = 0x80;

pub const USBDEVFS_URB_TYPE_ISO: c_uchar = 0;
pub const USBDEVFS_URB_TYPE_INTERRUPT: c_uchar = 1;
pub const USBDEVFS_URB_TYPE_CONTROL: c_uchar = 2;
pub const USBDEVFS_URB_TYPE_BULK: c_uchar = 3;

#[repr(C)]
#[derive(Debug)]
pub struct Urb {
    pub ep_type: c_uchar,
    pub endpoint: c_uchar,
    pub status: c_int,
    pub flags: c_uint,
    pub buffer: *mut u8,
    pub buffer_length: c_int,
    pub actual_length: c_int,
    pub start_frame: c_int,
    pub number_of_packets_or_stream_id: c_uint, // a union in C
    pub error_count: c_int,
    pub signr: c_uint,
    pub usercontext: *mut c_void,
    // + variable size array of iso_packet_desc
}

pub struct Transfer<Opcode, Input> {
    input: Input,
    _opcode: PhantomData<Opcode>,
}

impl<Opcode: CompileTimeOpcode, Input> Transfer<Opcode, Input> {
    #[inline]
    pub unsafe fn new(input: Input) -> Self {
        Self {
            input,
            _opcode: PhantomData,
        }
    }
}

unsafe impl<Opcode: CompileTimeOpcode, Input> Ioctl for Transfer<Opcode, Input> {
    type Output = usize;

    const IS_MUTATING: bool = true;
    const OPCODE: rustix::ioctl::Opcode = Opcode::OPCODE;

    fn as_ptr(&mut self) -> *mut c_void {
        &mut self.input as *mut Input as *mut c_void
    }

    unsafe fn output_from_ptr(r: IoctlOutput, _: *mut c_void) -> io::Result<usize> {
        Ok(r as usize)
    }
}

#[repr(C)]
#[allow(non_snake_case)]
pub struct CtrlTransfer {
    pub bRequestType: u8,
    pub bRequest: u8,
    pub wValue: u16,
    pub wIndex: u16,
    pub wLength: u16,
    pub timeout: u32, /* in milliseconds */
    pub data: *mut c_void,
}

pub fn control<Fd: AsFd>(fd: Fd, transfer: CtrlTransfer) -> io::Result<usize> {
    unsafe {
        let ctl =
            Transfer::<ioctl::ReadWriteOpcode<b'U', 0, CtrlTransfer>, CtrlTransfer>::new(transfer);
        ioctl::ioctl(fd, ctl)
    }
}

pub async fn clear_halt<Fd: AsFd>(fd: Fd, endpoint: u8) -> io::Result<()> {
    unsafe {
        let ctl =
            ioctl::Setter::<ioctl::ReadOpcode<b'U', 21, c_uint>, c_uint>::new(endpoint.into());
        ioctl::ioctl(fd, ctl)
    }
}
