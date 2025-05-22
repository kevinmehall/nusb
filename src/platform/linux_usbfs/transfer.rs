use std::{
    mem::{self, ManuallyDrop},
    ptr::{addr_of_mut, null_mut},
    slice,
    time::Instant,
};

use rustix::io::Errno;

use crate::{
    descriptors::TransferType,
    transfer::{
        internal::Pending, Allocator, Buffer, Completion, ControlIn, ControlOut, Direction,
        TransferError, SETUP_PACKET_SIZE,
    },
};

use super::{
    errno_to_transfer_error,
    usbfs::{
        Urb, USBDEVFS_URB_TYPE_BULK, USBDEVFS_URB_TYPE_CONTROL, USBDEVFS_URB_TYPE_INTERRUPT,
        USBDEVFS_URB_TYPE_ISO,
    },
};

/// Linux-specific transfer state.
///
/// This logically contains a `Vec` with urb.buffer and capacity.
/// It also owns the `urb` allocation itself, which is stored out-of-line
/// to enable isochronous transfers to allocate the variable-length
/// `iso_packet_desc` array.
pub struct TransferData {
    urb: *mut Urb,
    capacity: u32,
    allocator: Allocator,
    pub(crate) deadline: Option<Instant>,
}

unsafe impl Send for TransferData {}
unsafe impl Sync for TransferData {}

impl TransferData {
    pub(super) fn new(endpoint: u8, ep_type: TransferType) -> TransferData {
        let ep_type = match ep_type {
            TransferType::Control => USBDEVFS_URB_TYPE_CONTROL,
            TransferType::Interrupt => USBDEVFS_URB_TYPE_INTERRUPT,
            TransferType::Bulk => USBDEVFS_URB_TYPE_BULK,
            TransferType::Isochronous => USBDEVFS_URB_TYPE_ISO,
        };

        let mut empty = ManuallyDrop::new(Vec::new());

        TransferData {
            urb: Box::into_raw(Box::new(Urb {
                ep_type,
                endpoint,
                status: 0,
                flags: 0,
                buffer: empty.as_mut_ptr(),
                buffer_length: 0,
                actual_length: 0,
                start_frame: 0,
                number_of_packets_or_stream_id: 0,
                error_count: 0,
                signr: 0,
                usercontext: null_mut(),
            })),
            capacity: 0,
            allocator: Allocator::Default,
            deadline: None,
        }
    }

    pub(super) fn new_control_out(data: ControlOut) -> TransferData {
        let mut t = TransferData::new(0x00, TransferType::Control);
        let mut buffer = Buffer::new(SETUP_PACKET_SIZE.checked_add(data.data.len()).unwrap());
        buffer.extend_from_slice(&data.setup_packet());
        buffer.extend_from_slice(data.data);
        t.set_buffer(buffer);
        t
    }

    pub(super) fn new_control_in(data: ControlIn) -> TransferData {
        let mut t = TransferData::new(0x80, TransferType::Control);
        let mut buffer = Buffer::new(SETUP_PACKET_SIZE.checked_add(data.length as usize).unwrap());
        buffer.extend_from_slice(&data.setup_packet());
        t.set_buffer(buffer);
        t
    }

    pub fn set_buffer(&mut self, buf: Buffer) {
        debug_assert!(self.capacity == 0);
        let buf = ManuallyDrop::new(buf);
        self.capacity = buf.capacity;
        self.urb_mut().buffer = buf.ptr;
        self.urb_mut().actual_length = 0;
        self.urb_mut().buffer_length = match Direction::from_address(self.urb().endpoint) {
            Direction::Out => buf.len as i32,
            Direction::In => buf.requested_len as i32,
        };
        self.allocator = buf.allocator;
    }

    pub fn take_completion(&mut self) -> Completion {
        let status = self.status();
        let requested_len = self.urb().buffer_length as u32;
        let actual_len = self.urb().actual_length as usize;
        let len = match Direction::from_address(self.urb().endpoint) {
            Direction::Out => self.urb().buffer_length as u32,
            Direction::In => self.urb().actual_length as u32,
        };

        let mut empty = ManuallyDrop::new(Vec::new());
        let ptr = mem::replace(&mut self.urb_mut().buffer, empty.as_mut_ptr());
        let capacity = mem::replace(&mut self.capacity, 0);
        self.urb_mut().buffer_length = 0;
        self.urb_mut().actual_length = 0;
        let allocator = mem::replace(&mut self.allocator, Allocator::Default);

        Completion {
            status,
            actual_len,
            buffer: Buffer {
                ptr,
                len,
                requested_len,
                capacity,
                allocator,
            },
        }
    }

    #[inline]
    pub(super) fn urb(&self) -> &Urb {
        unsafe { &*self.urb }
    }

    #[inline]
    pub(super) fn urb_mut(&mut self) -> &mut Urb {
        unsafe { &mut *self.urb }
    }

    #[inline]
    pub(super) fn urb_ptr(&self) -> *mut Urb {
        self.urb
    }

    #[inline]
    pub fn status(&self) -> Result<(), TransferError> {
        if self.urb().status == 0 {
            return Ok(());
        }

        // It's sometimes positive, sometimes negative, but rustix panics if negative.
        Err(errno_to_transfer_error(Errno::from_raw_os_error(
            self.urb().status.abs(),
        )))
    }

    #[inline]
    pub fn control_in_data(&self) -> &[u8] {
        debug_assert!(self.urb().endpoint == 0x80);
        let urb = self.urb();
        unsafe {
            slice::from_raw_parts(
                urb.buffer.add(SETUP_PACKET_SIZE),
                urb.actual_length as usize,
            )
        }
    }
}

impl Pending<TransferData> {
    pub fn urb_ptr(&self) -> *mut Urb {
        // Get urb pointer without dereferencing as `TransferData`, because
        // it may be mutably aliased.
        unsafe { *addr_of_mut!((*self.as_ptr()).urb) }
    }
}

impl Drop for TransferData {
    fn drop(&mut self) {
        unsafe {
            drop(self.take_completion());
            drop(Box::from_raw(self.urb));
        }
    }
}
