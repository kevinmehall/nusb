use std::{
    alloc::{alloc_zeroed, dealloc, handle_alloc_error, Layout},
    mem::{self, ManuallyDrop},
    ptr::{addr_of_mut, null_mut, NonNull},
    slice,
    time::Instant,
};

use rustix::io::Errno;

use crate::{
    descriptors::TransferType,
    transfer::{
        internal::Pending, Allocator, Buffer, Completion, ControlIn, ControlOut, Direction,
        IsoCompletion, IsoLayoutError, IsoPacketResult, IsoPacketStatus, IsoTransfer,
        TransferError, SETUP_PACKET_SIZE,
    },
};

use super::{
    errno_to_transfer_error,
    usbfs::{
        IsoPacketDesc, Urb, USBDEVFS_URB_ISO_ASAP, USBDEVFS_URB_TYPE_BULK,
        USBDEVFS_URB_TYPE_CONTROL, USBDEVFS_URB_TYPE_INTERRUPT, USBDEVFS_URB_TYPE_ISO,
    },
};

/// Value placed in output-only packet fields before submission. If usbfs does
/// not replace it, the public result is `NotReported`, never a false success.
const ISO_PACKET_NOT_REPORTED: u32 = u32::MAX;

/// Owns either a normal boxed URB or an ABI-aligned URB followed by its
/// variable-length isochronous packet descriptor array.
struct UrbAllocation {
    ptr: NonNull<Urb>,
    layout: Option<Layout>,
    packet_offset: usize,
    packet_count: u32,
}

unsafe impl Send for UrbAllocation {}
unsafe impl Sync for UrbAllocation {}

impl UrbAllocation {
    fn plain(urb: Urb) -> Self {
        Self {
            ptr: NonNull::from(Box::leak(Box::new(urb))),
            layout: None,
            packet_offset: 0,
            packet_count: 0,
        }
    }

    fn iso(endpoint: u8, packet_count: u32) -> Result<Self, IsoLayoutError> {
        let packet_layout = Layout::array::<IsoPacketDesc>(packet_count as usize)
            .map_err(|_| IsoLayoutError::PacketCountOverflow)?;
        let (layout, packet_offset) = Layout::new::<Urb>()
            .extend(packet_layout)
            .map_err(|_| IsoLayoutError::PacketCountOverflow)?;
        let layout = layout.pad_to_align();
        let allocation = unsafe { alloc_zeroed(layout) };
        let Some(allocation) = NonNull::new(allocation) else {
            handle_alloc_error(layout)
        };
        let ptr = allocation.cast::<Urb>();
        unsafe {
            ptr.as_ptr().write(make_urb(
                endpoint,
                USBDEVFS_URB_TYPE_ISO,
                USBDEVFS_URB_ISO_ASAP,
                packet_count,
            ));
        }
        Ok(Self {
            ptr,
            layout: Some(layout),
            packet_offset,
            packet_count,
        })
    }

    fn urb(&self) -> &Urb {
        unsafe { self.ptr.as_ref() }
    }

    fn urb_mut(&mut self) -> &mut Urb {
        unsafe { self.ptr.as_mut() }
    }

    fn packet(&self, index: usize) -> &IsoPacketDesc {
        assert!(index < self.packet_count as usize);
        unsafe {
            &*self
                .ptr
                .cast::<u8>()
                .as_ptr()
                .add(self.packet_offset)
                .cast::<IsoPacketDesc>()
                .add(index)
        }
    }

    fn packet_mut(&mut self, index: usize) -> &mut IsoPacketDesc {
        assert!(index < self.packet_count as usize);
        unsafe {
            &mut *self
                .ptr
                .cast::<u8>()
                .as_ptr()
                .add(self.packet_offset)
                .cast::<IsoPacketDesc>()
                .add(index)
        }
    }
}

impl Drop for UrbAllocation {
    fn drop(&mut self) {
        unsafe {
            if let Some(layout) = self.layout {
                dealloc(self.ptr.cast::<u8>().as_ptr(), layout);
            } else {
                drop(Box::from_raw(self.ptr.as_ptr()));
            }
        }
    }
}

fn make_urb(endpoint: u8, ep_type: u8, flags: u32, packet_count: u32) -> Urb {
    let mut empty = ManuallyDrop::new(Vec::new());
    Urb {
        ep_type,
        endpoint,
        status: 0,
        flags,
        buffer: empty.as_mut_ptr(),
        buffer_length: 0,
        actual_length: 0,
        start_frame: 0,
        number_of_packets_or_stream_id: packet_count,
        error_count: 0,
        signr: 0,
        usercontext: null_mut(),
    }
}

/// Linux-specific transfer state shared by control, bulk, interrupt, and
/// isochronous endpoints.
pub struct TransferData {
    urb: UrbAllocation,
    capacity: u32,
    allocator: Allocator,
    iso_transfer: Option<IsoTransfer>,
    iso_submitted: bool,
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

        TransferData {
            urb: UrbAllocation::plain(make_urb(endpoint, ep_type, 0, 0)),
            capacity: 0,
            allocator: Allocator::Default,
            iso_transfer: None,
            iso_submitted: false,
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
        debug_assert!(self.iso_transfer.is_none());
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

    pub(crate) fn set_iso_transfer(
        &mut self,
        transfer: IsoTransfer,
    ) -> Result<(), (IsoLayoutError, IsoTransfer)> {
        debug_assert!(self.capacity == 0);
        debug_assert!(self.iso_transfer.is_none());
        let layout = transfer.layout();
        if self.urb.packet_count != layout.packet_count() as u32 {
            let allocation =
                match UrbAllocation::iso(self.urb().endpoint, layout.packet_count() as u32) {
                    Ok(allocation) => allocation,
                    Err(error) => return Err((error, transfer)),
                };
            self.urb = allocation;
        }

        let urb = self.urb_mut();
        urb.ep_type = USBDEVFS_URB_TYPE_ISO;
        urb.flags = USBDEVFS_URB_ISO_ASAP;
        urb.buffer = transfer.buffer.ptr;
        urb.buffer_length = layout.total_len() as i32;
        urb.actual_length = 0;
        urb.start_frame = 0;
        urb.number_of_packets_or_stream_id = layout.packet_count() as u32;
        urb.error_count = 0;
        urb.status = 0;
        urb.usercontext = null_mut();

        for index in 0..layout.packet_count() {
            *self.urb.packet_mut(index) = IsoPacketDesc {
                length: layout.packet_size() as u32,
                actual_length: 0,
                status: ISO_PACKET_NOT_REPORTED,
            };
        }

        self.iso_submitted = false;
        self.iso_transfer = Some(transfer);
        Ok(())
    }

    pub(crate) fn mark_iso_submitting(&mut self) {
        if self.iso_transfer.is_some() {
            self.iso_submitted = true;
        }
    }

    pub(crate) fn mark_iso_submit_failed(&mut self) {
        if self.iso_transfer.is_some() {
            self.iso_submitted = false;
        }
    }

    pub fn take_completion(&mut self) -> Completion {
        debug_assert!(self.iso_transfer.is_none());
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

    pub(crate) fn take_iso_completion(&mut self) -> IsoCompletion {
        let mut status = self.status();
        let mut transfer = self
            .iso_transfer
            .take()
            .expect("isochronous completion without an isochronous transfer");
        let layout = transfer.layout();
        let submitted = self.iso_submitted;

        for (index, result) in transfer.packet_results_mut().iter_mut().enumerate() {
            let desc = self.urb.packet(index);
            let native_status = desc.status;
            let invalid_lengths = desc.length as usize != layout.packet_size()
                || desc.actual_length as usize > layout.packet_size();
            let (actual_len, packet_status, native_status) = if !submitted {
                (0, IsoPacketStatus::NotExecuted, None)
            } else if native_status == ISO_PACKET_NOT_REPORTED {
                (0, IsoPacketStatus::NotReported, None)
            } else if invalid_lengths {
                status = Err(TransferError::Fault);
                (
                    0,
                    IsoPacketStatus::Error(TransferError::Fault),
                    Some(native_status),
                )
            } else if native_status == (-Errno::XDEV.raw_os_error()) as u32 {
                (0, IsoPacketStatus::NotExecuted, Some(native_status))
            } else if native_status == 0 {
                (
                    desc.actual_length as usize,
                    IsoPacketStatus::Success,
                    Some(0),
                )
            } else {
                let signed = native_status as i32;
                let errno = signed.checked_abs().unwrap_or(i32::MAX);
                (
                    desc.actual_length as usize,
                    IsoPacketStatus::Error(errno_to_transfer_error(Errno::from_raw_os_error(
                        errno,
                    ))),
                    Some(native_status),
                )
            };
            *result = IsoPacketResult::new(
                layout.packet_size(),
                actual_len,
                packet_status,
                native_status,
            );
        }

        let mut empty = ManuallyDrop::new(Vec::new());
        self.urb_mut().buffer = empty.as_mut_ptr();
        self.urb_mut().buffer_length = 0;
        self.urb_mut().actual_length = 0;
        self.urb_mut().error_count = 0;
        self.iso_submitted = false;

        IsoCompletion::from_completed_transfer(transfer, status)
    }

    #[inline]
    pub(super) fn urb(&self) -> &Urb {
        self.urb.urb()
    }

    #[inline]
    pub(super) fn urb_mut(&mut self) -> &mut Urb {
        self.urb.urb_mut()
    }

    #[inline]
    pub(super) fn urb_ptr(&self) -> *mut Urb {
        self.urb.ptr.as_ptr()
    }

    #[inline]
    pub fn status(&self) -> Result<(), TransferError> {
        if self.urb().status == 0 {
            return Ok(());
        }

        let raw = self.urb().status;
        let errno = raw.checked_abs().unwrap_or(i32::MAX);
        Err(errno_to_transfer_error(Errno::from_raw_os_error(errno)))
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
        unsafe { (*addr_of_mut!((*self.as_ptr()).urb)).ptr.as_ptr() }
    }
}

impl Drop for TransferData {
    fn drop(&mut self) {
        if self.iso_transfer.take().is_some() || self.capacity == 0 {
            return;
        }

        let mut empty = ManuallyDrop::new(Vec::new());
        let ptr = mem::replace(&mut self.urb_mut().buffer, empty.as_mut_ptr());
        let capacity = mem::replace(&mut self.capacity, 0);
        let allocator = mem::replace(&mut self.allocator, Allocator::Default);
        drop(Buffer {
            ptr,
            len: 0,
            requested_len: 0,
            capacity,
            allocator,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transfer::{IsoLayout, IsoPacketStatus};

    fn transfer(layout: IsoLayout) -> IsoTransfer {
        IsoTransfer::new(7, layout, Buffer::new(layout.total_len()))
    }

    #[test]
    fn plain_urb_has_no_iso_metadata_allocation() {
        let transfer = TransferData::new(0x81, TransferType::Bulk);
        assert_eq!(transfer.urb.packet_count, 0);
        assert!(transfer.urb.layout.is_none());
    }

    #[test]
    fn sparse_completion_uses_slot_offsets_and_resets_for_reuse() {
        let layout = IsoLayout::new(3, 1024).unwrap();
        let mut data = TransferData::new(0x81, TransferType::Isochronous);
        data.set_iso_transfer(transfer(layout)).unwrap();
        data.mark_iso_submitting();

        data.iso_transfer.as_mut().unwrap().buffer[0..100].fill(1);
        data.iso_transfer.as_mut().unwrap().buffer[2048..2248].fill(3);
        *data.urb.packet_mut(0) = IsoPacketDesc {
            length: 1024,
            actual_length: 100,
            status: 0,
        };
        *data.urb.packet_mut(1) = IsoPacketDesc {
            length: 1024,
            actual_length: 0,
            status: (-71_i32) as u32,
        };
        *data.urb.packet_mut(2) = IsoPacketDesc {
            length: 1024,
            actual_length: 200,
            status: 0,
        };

        let completion = data.take_iso_completion();
        let packets = completion.packets().collect::<Vec<_>>();
        assert_eq!(packets[0].data(), &[1; 100]);
        assert_eq!(packets[1].offset(), 1024);
        assert_eq!(packets[2].offset(), 2048);
        assert_eq!(packets[2].data(), &[3; 200]);

        data.set_iso_transfer(completion.into_transfer()).unwrap();
        for index in 0..layout.packet_count() {
            let packet = data.urb.packet(index);
            assert_eq!(packet.actual_length, 0);
            assert_eq!(packet.status, ISO_PACKET_NOT_REPORTED);
        }
    }

    #[test]
    fn immediate_submit_failure_marks_every_packet_not_executed() {
        let layout = IsoLayout::new(3, 64).unwrap();
        let mut data = TransferData::new(0x81, TransferType::Isochronous);
        data.set_iso_transfer(transfer(layout)).unwrap();
        data.mark_iso_submitting();
        data.mark_iso_submit_failed();
        data.urb_mut().status = -22;

        let completion = data.take_iso_completion();
        assert_eq!(completion.status(), Err(TransferError::InvalidArgument));
        assert!(completion
            .packets()
            .all(|packet| packet.status() == IsoPacketStatus::NotExecuted));
    }

    #[test]
    fn expired_iso_packet_is_reported_as_not_executed() {
        let layout = IsoLayout::new(1, 8).unwrap();
        let mut data = TransferData::new(0x81, TransferType::Isochronous);
        data.set_iso_transfer(transfer(layout)).unwrap();
        data.mark_iso_submitting();
        data.urb.packet_mut(0).status = (-18_i32) as u32;

        let completion = data.take_iso_completion();
        let packet = completion.packets().next().unwrap();
        assert_eq!(packet.status(), IsoPacketStatus::NotExecuted);
        assert_eq!(packet.native_status(), Some((-18_i32) as u32));
        assert!(packet.data().is_empty());
    }

    #[test]
    fn out_of_bounds_native_actual_length_is_never_exposed() {
        let layout = IsoLayout::new(1, 8).unwrap();
        let mut data = TransferData::new(0x81, TransferType::Isochronous);
        data.set_iso_transfer(transfer(layout)).unwrap();
        data.mark_iso_submitting();
        data.urb.packet_mut(0).status = 0;
        data.urb.packet_mut(0).actual_length = 9;

        let completion = data.take_iso_completion();
        assert_eq!(completion.status(), Err(TransferError::Fault));
        let packet = completion.packets().next().unwrap();
        assert_eq!(packet.actual_len(), 0);
        assert_eq!(
            packet.status(),
            IsoPacketStatus::Error(TransferError::Fault)
        );
        assert!(packet.data().is_empty());
    }

    #[test]
    fn mismatched_native_packet_length_faults_completion() {
        let layout = IsoLayout::new(1, 8).unwrap();
        let mut data = TransferData::new(0x81, TransferType::Isochronous);
        data.set_iso_transfer(transfer(layout)).unwrap();
        data.mark_iso_submitting();
        *data.urb.packet_mut(0) = IsoPacketDesc {
            length: 7,
            actual_length: 7,
            status: 0,
        };

        let completion = data.take_iso_completion();
        assert_eq!(completion.status(), Err(TransferError::Fault));
        let packet = completion.packets().next().unwrap();
        assert_eq!(packet.actual_len(), 0);
        assert_eq!(
            packet.status(),
            IsoPacketStatus::Error(TransferError::Fault)
        );
        assert!(packet.data().is_empty());
    }
}
