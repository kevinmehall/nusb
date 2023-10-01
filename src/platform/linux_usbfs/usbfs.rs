//! Wrappers for the [usbfs] character device ioctls, translated from the
//! [C structures and ioctl definitions][uapi].
//! [usbfs]: https://www.kernel.org/doc/html/latest/driver-api/usb/usb.html#the-usb-character-device-nodes
//! [uapi]: https://github.com/torvalds/linux/blob/master/tools/include/uapi/linux/usbdevice_fs.h

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

pub fn claim_interface<Fd: AsFd>(fd: Fd, interface: u8) -> io::Result<()> {
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
