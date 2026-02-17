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
        IsoCompletion, IsoPacketResult, TransferError, SETUP_PACKET_SIZE,
    },
};

use super::{
    errno_to_transfer_error,
    usbfs::{
        IsoPacketDesc, Urb, USBDEVFS_URB_ISO_ASAP, USBDEVFS_URB_TYPE_BULK,
        USBDEVFS_URB_TYPE_CONTROL, USBDEVFS_URB_TYPE_INTERRUPT, USBDEVFS_URB_TYPE_ISO,
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

/// Linux-specific isochronous transfer state.
///
/// Similar to TransferData but handles the variable-length iso_packet_desc array
/// required for isochronous transfers.
pub struct IsoTransferData {
    /// Pointer to the URB allocation (includes iso_packet_desc array).
    urb: *mut u8,
    /// Total allocated size for the URB + iso_packet_desc array.
    urb_alloc_size: usize,
    /// Number of isochronous packets.
    num_packets: usize,
    /// Buffer capacity.
    capacity: u32,
    /// Buffer allocator.
    allocator: Allocator,
    /// Transfer deadline for timeout (reserved for future use).
    #[allow(dead_code)]
    pub(crate) deadline: Option<Instant>,
}

unsafe impl Send for IsoTransferData {}
unsafe impl Sync for IsoTransferData {}

impl IsoTransferData {
    /// Create a new isochronous transfer with the specified number of packets.
    pub(super) fn new(endpoint: u8, num_packets: usize) -> IsoTransferData {
        // Calculate allocation size: Urb + num_packets * IsoPacketDesc
        let urb_size = mem::size_of::<Urb>();
        let iso_desc_size = mem::size_of::<IsoPacketDesc>() * num_packets;
        let total_size = urb_size + iso_desc_size;

        // Allocate with proper alignment
        let layout = std::alloc::Layout::from_size_align(total_size, mem::align_of::<Urb>())
            .expect("Invalid layout for IsoUrb");

        let urb_ptr = unsafe { std::alloc::alloc_zeroed(layout) };
        if urb_ptr.is_null() {
            std::alloc::handle_alloc_error(layout);
        }

        let mut empty = ManuallyDrop::new(Vec::new());

        // Initialize the URB
        unsafe {
            let urb = urb_ptr as *mut Urb;
            (*urb).ep_type = USBDEVFS_URB_TYPE_ISO;
            (*urb).endpoint = endpoint;
            (*urb).status = 0;
            (*urb).flags = USBDEVFS_URB_ISO_ASAP;
            (*urb).buffer = empty.as_mut_ptr();
            (*urb).buffer_length = 0;
            (*urb).actual_length = 0;
            (*urb).start_frame = 0;
            (*urb).number_of_packets_or_stream_id = num_packets as u32;
            (*urb).error_count = 0;
            (*urb).signr = 0;
            (*urb).usercontext = null_mut();

            // Initialize iso_packet_desc array to zeros (already done by alloc_zeroed)
        }

        IsoTransferData {
            urb: urb_ptr,
            urb_alloc_size: total_size,
            num_packets,
            capacity: 0,
            allocator: Allocator::Default,
            deadline: None,
        }
    }

    /// Set the buffer and initialize packet descriptors.
    ///
    /// `packet_size` is the size of each isochronous packet.
    pub fn set_buffer(&mut self, buf: Buffer, packet_size: usize) {
        debug_assert!(self.capacity == 0);
        let buf = ManuallyDrop::new(buf);
        self.capacity = buf.capacity;

        let urb = self.urb_mut();
        urb.buffer = buf.ptr;
        urb.actual_length = 0;

        let total_len = match Direction::from_address(urb.endpoint) {
            Direction::Out => buf.len as i32,
            Direction::In => buf.requested_len as i32,
        };
        urb.buffer_length = total_len;

        self.allocator = buf.allocator;

        // Initialize packet descriptors
        let mut offset = 0usize;
        for i in 0..self.num_packets {
            let remaining = (total_len as usize).saturating_sub(offset);
            let len = remaining.min(packet_size);

            let desc = self.iso_packet_desc_mut(i);
            desc.length = len as u32;
            desc.actual_length = 0;
            desc.status = 0;

            offset += len;
        }
    }

    /// Take the completion result, consuming the buffer.
    pub fn take_completion(&mut self) -> IsoCompletion {
        let status = self.status();
        let urb = self.urb();

        // Collect packet results
        let mut packets = Vec::with_capacity(self.num_packets);
        let mut offset = 0usize;
        for i in 0..self.num_packets {
            let desc = self.iso_packet_desc(i);
            packets.push(IsoPacketResult {
                offset,
                length: desc.length as usize,
                actual_length: desc.actual_length as usize,
                status: desc.status as i32,
            });
            offset += desc.length as usize;
        }

        let error_count = urb.error_count as usize;
        let requested_len = urb.buffer_length as u32;

        // For isochronous transfers, the buffer length is always the requested length
        // because each packet's data is placed at its expected offset (based on desc.length),
        // not packed together. This is different from bulk/interrupt where actual_length
        // represents contiguous data.
        let len = urb.buffer_length as u32;

        let mut empty = ManuallyDrop::new(Vec::new());
        let ptr = mem::replace(&mut self.urb_mut().buffer, empty.as_mut_ptr());
        let capacity = mem::replace(&mut self.capacity, 0);
        self.urb_mut().buffer_length = 0;
        self.urb_mut().actual_length = 0;
        let allocator = mem::replace(&mut self.allocator, Allocator::Default);

        IsoCompletion {
            buffer: Buffer {
                ptr,
                len,
                requested_len,
                capacity,
                allocator,
            },
            packets,
            error_count,
            status,
        }
    }

    #[inline]
    pub(super) fn urb(&self) -> &Urb {
        unsafe { &*(self.urb as *const Urb) }
    }

    #[inline]
    pub(super) fn urb_mut(&mut self) -> &mut Urb {
        unsafe { &mut *(self.urb as *mut Urb) }
    }

    #[inline]
    #[allow(dead_code)]
    pub(super) fn urb_ptr(&self) -> *mut Urb {
        self.urb as *mut Urb
    }

    /// Get a reference to the i-th iso packet descriptor.
    #[inline]
    fn iso_packet_desc(&self, index: usize) -> &IsoPacketDesc {
        debug_assert!(index < self.num_packets);
        unsafe {
            let base = self.urb.add(mem::size_of::<Urb>()) as *const IsoPacketDesc;
            &*base.add(index)
        }
    }

    /// Get a mutable reference to the i-th iso packet descriptor.
    #[inline]
    fn iso_packet_desc_mut(&mut self, index: usize) -> &mut IsoPacketDesc {
        debug_assert!(index < self.num_packets);
        unsafe {
            let base = self.urb.add(mem::size_of::<Urb>()) as *mut IsoPacketDesc;
            &mut *base.add(index)
        }
    }

    /// Get the number of packets in this transfer.
    pub fn num_packets(&self) -> usize {
        self.num_packets
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
}

impl Pending<IsoTransferData> {
    pub fn urb_ptr(&self) -> *mut Urb {
        // Get urb pointer without dereferencing as `IsoTransferData`, because
        // it may be mutably aliased.
        unsafe { *addr_of_mut!((*self.as_ptr()).urb) as *mut Urb }
    }
}

impl Drop for IsoTransferData {
    fn drop(&mut self) {
        unsafe {
            // Take and drop the completion to free the buffer
            drop(self.take_completion());

            // Free the URB allocation
            let layout =
                std::alloc::Layout::from_size_align(self.urb_alloc_size, mem::align_of::<Urb>())
                    .expect("Invalid layout for IsoUrb");
            std::alloc::dealloc(self.urb, layout);
        }
    }
}
