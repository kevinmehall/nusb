use std::{fmt::Display, ops::Range};

use crate::descriptors::TransferType;

use super::{private, Buffer, EndpointType, TransferError};

/// Type-level endpoint type: Isochronous.
///
/// Isochronous endpoints use [`IsoTransfer`] and [`IsoCompletion`] rather than
/// the contiguous [`Buffer`] / [`Completion`][super::Completion] API used by
/// bulk and interrupt endpoints. This preserves the packet boundaries reported
/// by the operating system.
pub enum Isochronous {}
impl private::Sealed for Isochronous {}
impl EndpointType for Isochronous {
    const TYPE: TransferType = TransferType::Isochronous;
}

/// Errors constructing or validating an isochronous transfer layout.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum IsoLayoutError {
    /// A transfer must contain at least one packet slot.
    ZeroPackets,
    /// Every packet slot must have a non-zero capacity.
    ZeroPacketSize,
    /// The packet count cannot be represented by a platform USB API.
    PacketCountOverflow,
    /// The packet size cannot be represented by a platform USB API.
    PacketSizeOverflow,
    /// The total transfer length cannot be represented by a [`Buffer`].
    TotalLengthOverflow,
}

impl Display for IsoLayoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::ZeroPackets => "isochronous layout has no packet slots",
            Self::ZeroPacketSize => "isochronous packet size is zero",
            Self::PacketCountOverflow => "isochronous packet count exceeds platform limits",
            Self::PacketSizeOverflow => "isochronous packet size exceeds platform limits",
            Self::TotalLengthOverflow => "isochronous transfer length exceeds platform limits",
        })
    }
}

impl std::error::Error for IsoLayoutError {}

/// An equal-sized packet-slot layout for one isochronous transfer.
///
/// Packet count belongs to each transfer, not to the endpoint. Version 0 of
/// this API deliberately supports only equal-sized slots because that shape is
/// implementable by the native APIs without silently changing packet lengths.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct IsoLayout {
    packet_count: u32,
    packet_size: u32,
    total_len: u32,
}

impl IsoLayout {
    /// Construct a layout containing `packet_count` slots of `packet_size`
    /// bytes each.
    pub fn new(packet_count: usize, packet_size: usize) -> Result<Self, IsoLayoutError> {
        if packet_count == 0 {
            return Err(IsoLayoutError::ZeroPackets);
        }
        if packet_size == 0 {
            return Err(IsoLayoutError::ZeroPacketSize);
        }

        let packet_count =
            u32::try_from(packet_count).map_err(|_| IsoLayoutError::PacketCountOverflow)?;
        let packet_size =
            u32::try_from(packet_size).map_err(|_| IsoLayoutError::PacketSizeOverflow)?;
        let total_len = packet_count
            .checked_mul(packet_size)
            .ok_or(IsoLayoutError::TotalLengthOverflow)?;
        i32::try_from(total_len).map_err(|_| IsoLayoutError::TotalLengthOverflow)?;

        Ok(Self {
            packet_count,
            packet_size,
            total_len,
        })
    }

    /// Number of packet slots in the transfer.
    pub fn packet_count(self) -> usize {
        self.packet_count as usize
    }

    /// Capacity of every packet slot.
    pub fn packet_size(self) -> usize {
        self.packet_size as usize
    }

    /// Total number of bytes in the backing allocation.
    pub fn total_len(self) -> usize {
        self.total_len as usize
    }

    pub(crate) fn packet_range(self, index: usize) -> Option<Range<usize>> {
        if index >= self.packet_count() {
            return None;
        }
        let start = index * self.packet_size();
        Some(start..start + self.packet_size())
    }
}

/// A reusable, fully initialized isochronous transfer allocation.
///
/// Create this with [`Endpoint::allocate_iso`][crate::Endpoint::allocate_iso].
/// It is bound to the endpoint instance that allocated it and is returned by
/// [`IsoCompletion::into_transfer`] for reuse.
pub struct IsoTransfer {
    // Platform state is dropped before the backing buffer. WinUSB relies on
    // this order because unregistering an isochronous buffer may still inspect
    // the registered allocation.
    #[cfg(any(
        target_os = "macos",
        all(target_os = "windows", not(target_vendor = "win7"))
    ))]
    platform_state: crate::platform::IsoTransferState,
    pub(crate) buffer: Buffer,
    layout: IsoLayout,
    pub(crate) endpoint_id: u64,
    packet_results: Vec<IsoPacketResult>,
}

impl IsoTransfer {
    pub(crate) fn new(endpoint_id: u64, layout: IsoLayout, mut buffer: Buffer) -> Self {
        debug_assert!(buffer.capacity() >= layout.total_len());
        buffer.clear();
        buffer.extend_fill(layout.total_len(), 0);
        buffer.set_requested_len(layout.total_len());
        Self {
            #[cfg(any(
                target_os = "macos",
                all(target_os = "windows", not(target_vendor = "win7"))
            ))]
            platform_state: crate::platform::IsoTransferState::default(),
            buffer,
            layout,
            endpoint_id,
            packet_results: Self::empty_packet_results(layout),
        }
    }

    fn empty_packet_results(layout: IsoLayout) -> Vec<IsoPacketResult> {
        vec![
            IsoPacketResult::new(layout.packet_size(), 0, IsoPacketStatus::NotReported, None,);
            layout.packet_count()
        ]
    }

    #[cfg(test)]
    fn set_packet_results(&mut self, results: Vec<IsoPacketResult>) {
        self.packet_results = results;
    }

    pub(crate) fn packet_results_mut(&mut self) -> &mut [IsoPacketResult] {
        &mut self.packet_results
    }

    #[cfg(any(
        target_os = "macos",
        all(target_os = "windows", not(target_vendor = "win7"))
    ))]
    pub(crate) fn platform_state_mut(&mut self) -> &mut crate::platform::IsoTransferState {
        &mut self.platform_state
    }

    /// Layout of this transfer.
    pub fn layout(&self) -> IsoLayout {
        self.layout
    }

    /// Mutable access to one initialized packet slot.
    ///
    /// For OUT transfers, fill each slot before submission. IN transfers ignore
    /// existing contents. Returning slots instead of a flattened buffer keeps
    /// packet boundaries explicit.
    pub fn packet_mut(&mut self, index: usize) -> Option<&mut [u8]> {
        let range = self.layout.packet_range(index)?;
        Some(&mut self.buffer[range])
    }

    #[cfg(test)]
    fn from_parts_for_test(layout: IsoLayout, buffer: Buffer) -> Self {
        Self {
            #[cfg(any(
                target_os = "macos",
                all(target_os = "windows", not(target_vendor = "win7"))
            ))]
            platform_state: crate::platform::IsoTransferState::default(),
            buffer,
            layout,
            endpoint_id: 0,
            packet_results: Self::empty_packet_results(layout),
        }
    }
}

impl std::fmt::Debug for IsoTransfer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IsoTransfer")
            .field("buffer", &self.buffer)
            .field("layout", &self.layout)
            .field("endpoint_id", &self.endpoint_id)
            .field("packet_results", &self.packet_results)
            .finish()
    }
}

/// A synchronous isochronous submission error that returns ownership of the
/// reusable transfer allocation.
pub struct IsoSubmitError {
    error: TransferError,
    transfer: IsoTransfer,
}

impl std::fmt::Debug for IsoSubmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IsoSubmitError")
            .field("error", &self.error)
            .field("transfer_layout", &self.transfer.layout)
            .field("endpoint_id", &self.transfer.endpoint_id)
            .finish()
    }
}

impl IsoSubmitError {
    pub(crate) fn new(error: TransferError, transfer: IsoTransfer) -> Self {
        Self { error, transfer }
    }

    /// Portable reason the request was rejected before it became pending.
    pub fn error(&self) -> TransferError {
        self.error
    }

    /// Recover the transfer allocation after a rejected submission.
    pub fn into_transfer(self) -> IsoTransfer {
        self.transfer
    }

    /// Split the error into its reason and returned transfer allocation.
    pub fn into_parts(self) -> (TransferError, IsoTransfer) {
        (self.error, self.transfer)
    }
}

impl Display for IsoSubmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "isochronous transfer was not submitted: {}", self.error)
    }
}

impl std::error::Error for IsoSubmitError {}

/// Per-packet completion state.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum IsoPacketStatus {
    /// The backend reported successful completion for this packet.
    Success,
    /// The backend reported an error for this packet.
    Error(TransferError),
    /// The backend confirmed that this packet was not executed.
    NotExecuted,
    /// The native API did not report a result for this packet.
    NotReported,
}

/// Metadata reported for one isochronous packet slot.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct IsoPacketResult {
    requested_len: usize,
    actual_len: usize,
    status: IsoPacketStatus,
    native_status: Option<u32>,
}

impl IsoPacketResult {
    pub(crate) fn new(
        requested_len: usize,
        actual_len: usize,
        status: IsoPacketStatus,
        native_status: Option<u32>,
    ) -> Self {
        Self {
            requested_len,
            actual_len,
            status,
            native_status,
        }
    }

    /// Requested capacity of this packet slot.
    #[cfg(test)]
    pub(crate) fn requested_len(self) -> usize {
        self.requested_len
    }

    /// Bytes the backend reported as valid for this packet.
    #[cfg(test)]
    pub(crate) fn actual_len(self) -> usize {
        self.actual_len
    }

    /// Cross-platform completion state.
    #[cfg(test)]
    pub(crate) fn status(self) -> IsoPacketStatus {
        self.status
    }

    /// Optional raw status supplied by the native backend.
    ///
    /// This is diagnostic information only. Use [`status`][Self::status] for
    /// portable behavior.
    #[cfg(test)]
    pub(crate) fn native_status(self) -> Option<u32> {
        self.native_status
    }
}

/// Read-only view of one packet and its valid data range.
#[derive(Debug, Copy, Clone)]
pub struct IsoPacketView<'a> {
    index: usize,
    result: &'a IsoPacketResult,
    data: &'a [u8],
}

impl<'a> IsoPacketView<'a> {
    /// Zero-based packet slot index.
    pub fn index(self) -> usize {
        self.index
    }

    /// Byte offset of the packet slot in the backing allocation.
    pub fn offset(self) -> usize {
        self.index * self.result.requested_len
    }

    /// Requested capacity of the packet slot.
    pub fn requested_len(self) -> usize {
        self.result.requested_len
    }

    /// Number of bytes valid in [`data`][Self::data].
    pub fn actual_len(self) -> usize {
        self.result.actual_len
    }

    /// Portable per-packet status.
    pub fn status(self) -> IsoPacketStatus {
        self.result.status
    }

    /// Optional raw native status for diagnostics.
    pub fn native_status(self) -> Option<u32> {
        self.result.native_status
    }

    /// Bytes confirmed valid for this packet.
    pub fn data(self) -> &'a [u8] {
        self.data
    }
}

/// Completion of one isochronous transfer.
///
/// Overall and per-packet status are independent. Packet data stays at the
/// original slot offset; it is never flattened or compacted.
#[derive(Debug)]
pub struct IsoCompletion {
    transfer: IsoTransfer,
    status: Result<(), TransferError>,
}

#[cfg(test)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum IsoCompletionValidationError {
    ActualLengthOverflow,
    LayoutMismatch,
}

impl IsoCompletion {
    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_arch = "wasm32",
        all(target_os = "windows", not(target_vendor = "win7"))
    ))]
    pub(crate) fn from_completed_transfer(
        transfer: IsoTransfer,
        status: Result<(), TransferError>,
    ) -> Self {
        debug_assert_eq!(
            transfer.packet_results.len(),
            transfer.layout.packet_count()
        );
        debug_assert!(transfer
            .packet_results
            .iter()
            .all(
                |packet| packet.requested_len == transfer.layout.packet_size()
                    && packet.actual_len <= packet.requested_len
            ));
        Self { transfer, status }
    }

    #[cfg(test)]
    fn from_parts(
        transfer: IsoTransfer,
        status: Result<(), TransferError>,
        packets: Vec<IsoPacketResult>,
    ) -> Result<Self, IsoCompletionValidationError> {
        let layout = transfer.layout;
        if packets.len() != layout.packet_count()
            || packets
                .iter()
                .any(|packet| packet.requested_len != layout.packet_size())
        {
            return Err(IsoCompletionValidationError::LayoutMismatch);
        }
        if packets
            .iter()
            .any(|packet| packet.actual_len > packet.requested_len)
        {
            return Err(IsoCompletionValidationError::ActualLengthOverflow);
        }
        let mut transfer = transfer;
        transfer.set_packet_results(packets);
        Ok(Self { transfer, status })
    }

    #[cfg(test)]
    fn from_parts_for_test(
        transfer: IsoTransfer,
        status: Result<(), TransferError>,
        packets: Vec<IsoPacketResult>,
    ) -> Result<Self, IsoCompletionValidationError> {
        Self::from_parts(transfer, status, packets)
    }

    /// Overall transfer status.
    pub fn status(&self) -> Result<(), TransferError> {
        self.status
    }

    /// Iterate packet results and their valid, non-compacted data slices.
    pub fn packets(&self) -> impl ExactSizeIterator<Item = IsoPacketView<'_>> {
        self.transfer
            .packet_results
            .iter()
            .enumerate()
            .map(|(index, result)| {
                let start = index * result.requested_len;
                let data = &self.transfer.buffer[start..start + result.actual_len];
                IsoPacketView {
                    index,
                    result,
                    data,
                }
            })
    }

    /// Iterate only packets explicitly reported as successful.
    pub fn successful_packets(&self) -> impl Iterator<Item = IsoPacketView<'_>> {
        self.packets()
            .filter(|packet| packet.status() == IsoPacketStatus::Success)
    }

    /// Sum of valid bytes across all packet slots.
    pub fn total_actual_len(&self) -> usize {
        self.transfer
            .packet_results
            .iter()
            .map(|packet| packet.actual_len)
            .sum()
    }

    /// Recover the initialized transfer allocation for reuse.
    pub fn into_transfer(mut self) -> IsoTransfer {
        for result in self.transfer.packet_results_mut() {
            *result =
                IsoPacketResult::new(result.requested_len, 0, IsoPacketStatus::NotReported, None);
        }
        self.transfer
    }
}

#[cfg(test)]
mod iso_tests {
    use super::{
        Buffer, IsoCompletion, IsoCompletionValidationError, IsoLayout, IsoLayoutError,
        IsoPacketResult, IsoPacketStatus, IsoSubmitError, IsoTransfer, TransferError,
    };

    #[test]
    fn iso_layout_rejects_zero_and_overflow() {
        assert_eq!(IsoLayout::new(0, 1024), Err(IsoLayoutError::ZeroPackets));
        assert_eq!(IsoLayout::new(3, 0), Err(IsoLayoutError::ZeroPacketSize));
        assert_eq!(
            IsoLayout::new(usize::MAX, 1),
            Err(IsoLayoutError::PacketCountOverflow)
        );
        assert_eq!(
            IsoLayout::new(1, usize::MAX),
            Err(IsoLayoutError::PacketSizeOverflow)
        );
        assert_eq!(
            IsoLayout::new(u32::MAX as usize, 2),
            Err(IsoLayoutError::TotalLengthOverflow)
        );
    }

    #[test]
    fn iso_layout_and_transfer_expose_checked_packet_slots() {
        let layout = IsoLayout::new(3, 8).unwrap();
        assert_eq!(layout.packet_count(), 3);
        assert_eq!(layout.packet_size(), 8);
        assert_eq!(layout.total_len(), 24);
        assert_eq!(layout.packet_range(2), Some(16..24));
        assert_eq!(layout.packet_range(3), None);

        let mut transfer = IsoTransfer::new(9, layout, Buffer::new(layout.total_len()));
        assert_eq!(transfer.layout(), layout);
        assert_eq!(transfer.packet_mut(2).unwrap(), &[0; 8]);
        assert!(transfer.packet_mut(3).is_none());
        transfer.packet_mut(1).unwrap().fill(7);
        assert_eq!(&transfer.buffer[8..16], &[7; 8]);
    }

    #[test]
    fn sparse_iso_completion_preserves_slot_offsets() {
        let layout = IsoLayout::new(3, 1024).unwrap();
        let mut buffer = Buffer::new(layout.total_len());
        buffer.extend_fill(layout.total_len(), 0);
        buffer[0..100].fill(1);
        buffer[2048..2248].fill(3);

        let transfer = IsoTransfer::from_parts_for_test(layout, buffer);
        let completion = IsoCompletion::from_parts_for_test(
            transfer,
            Ok(()),
            vec![
                IsoPacketResult::new(1024, 100, IsoPacketStatus::Success, Some(0)),
                IsoPacketResult::new(
                    1024,
                    0,
                    IsoPacketStatus::Error(TransferError::Fault),
                    Some(71),
                ),
                IsoPacketResult::new(1024, 200, IsoPacketStatus::Success, Some(0)),
            ],
        )
        .unwrap();

        let packets = completion.packets().collect::<Vec<_>>();
        assert_eq!(packets[0].offset(), 0);
        assert_eq!(packets[0].data(), &[1; 100]);
        assert_eq!(packets[1].offset(), 1024);
        assert!(packets[1].data().is_empty());
        assert_eq!(packets[2].offset(), 2048);
        assert_eq!(packets[2].data(), &[3; 200]);
        assert_eq!(completion.total_actual_len(), 300);
    }

    #[test]
    fn overall_failure_keeps_valid_packet_data() {
        let layout = IsoLayout::new(2, 16).unwrap();
        let mut buffer = Buffer::new(layout.total_len());
        buffer.extend_fill(layout.total_len(), 0);
        buffer[0..4].copy_from_slice(&[1, 2, 3, 4]);

        let completion = IsoCompletion::from_parts_for_test(
            IsoTransfer::from_parts_for_test(layout, buffer),
            Err(TransferError::Cancelled),
            vec![
                IsoPacketResult::new(16, 4, IsoPacketStatus::Success, Some(0)),
                IsoPacketResult::new(16, 0, IsoPacketStatus::NotExecuted, None),
            ],
        )
        .unwrap();

        assert_eq!(completion.status(), Err(TransferError::Cancelled));
        assert_eq!(completion.packets().next().unwrap().data(), &[1, 2, 3, 4]);
        assert_eq!(completion.successful_packets().count(), 1);
    }

    #[test]
    fn unreported_packet_is_not_successful() {
        let layout = IsoLayout::new(1, 8).unwrap();
        let mut buffer = Buffer::new(layout.total_len());
        buffer.extend_fill(layout.total_len(), 0);
        let completion = IsoCompletion::from_parts_for_test(
            IsoTransfer::from_parts_for_test(layout, buffer),
            Ok(()),
            vec![IsoPacketResult::new(
                8,
                0,
                IsoPacketStatus::NotReported,
                None,
            )],
        )
        .unwrap();

        assert_eq!(completion.successful_packets().count(), 0);
    }

    #[test]
    fn completion_rejects_out_of_bounds_actual_length() {
        let layout = IsoLayout::new(1, 8).unwrap();
        let mut buffer = Buffer::new(layout.total_len());
        buffer.extend_fill(layout.total_len(), 0);
        let result = IsoCompletion::from_parts_for_test(
            IsoTransfer::from_parts_for_test(layout, buffer),
            Ok(()),
            vec![IsoPacketResult::new(
                8,
                9,
                IsoPacketStatus::Success,
                Some(0),
            )],
        );

        assert_eq!(
            result.unwrap_err(),
            IsoCompletionValidationError::ActualLengthOverflow
        );
    }

    #[test]
    fn completion_rejects_mismatched_packet_metadata() {
        let layout = IsoLayout::new(2, 8).unwrap();
        let transfer = IsoTransfer::new(1, layout, Buffer::new(layout.total_len()));
        let wrong_count = IsoCompletion::from_parts_for_test(
            transfer,
            Ok(()),
            vec![IsoPacketResult::new(
                8,
                0,
                IsoPacketStatus::NotReported,
                None,
            )],
        );
        assert_eq!(
            wrong_count.unwrap_err(),
            IsoCompletionValidationError::LayoutMismatch
        );

        let transfer = IsoTransfer::new(1, layout, Buffer::new(layout.total_len()));
        let wrong_size = IsoCompletion::from_parts_for_test(
            transfer,
            Ok(()),
            vec![
                IsoPacketResult::new(8, 0, IsoPacketStatus::Success, Some(0)),
                IsoPacketResult::new(4, 0, IsoPacketStatus::Success, Some(0)),
            ],
        );
        assert_eq!(
            wrong_size.unwrap_err(),
            IsoCompletionValidationError::LayoutMismatch
        );
    }

    #[test]
    fn completion_reuse_clears_results_without_losing_allocation() {
        let layout = IsoLayout::new(1, 8).unwrap();
        let transfer = IsoTransfer::new(1, layout, Buffer::new(layout.total_len()));
        let original_ptr = transfer.buffer.ptr;
        let completion = IsoCompletion::from_parts_for_test(
            transfer,
            Ok(()),
            vec![IsoPacketResult::new(
                8,
                4,
                IsoPacketStatus::Success,
                Some(123),
            )],
        )
        .unwrap();

        let packet = completion.packets().next().unwrap();
        assert_eq!(packet.index(), 0);
        assert_eq!(packet.requested_len(), 8);
        assert_eq!(packet.actual_len(), 4);
        assert_eq!(packet.status(), IsoPacketStatus::Success);
        assert_eq!(packet.native_status(), Some(123));

        let transfer = completion.into_transfer();
        assert_eq!(transfer.buffer.ptr, original_ptr);
        assert_eq!(transfer.packet_results[0].requested_len(), 8);
        assert_eq!(transfer.packet_results[0].actual_len(), 0);
        assert_eq!(
            transfer.packet_results[0].status(),
            IsoPacketStatus::NotReported
        );
        assert_eq!(transfer.packet_results[0].native_status(), None);
    }

    #[test]
    fn submission_error_returns_the_reusable_transfer() {
        let layout = IsoLayout::new(1, 8).unwrap();
        let mut transfer = IsoTransfer::new(42, layout, Buffer::new(layout.total_len()));
        transfer.packet_mut(0).unwrap().fill(0xab);
        let error = IsoSubmitError::new(TransferError::InvalidArgument, transfer);
        assert_eq!(error.error(), TransferError::InvalidArgument);
        assert!(error
            .to_string()
            .contains("invalid or unsupported argument"));
        let debug = format!("{error:?}");
        assert!(debug.contains("InvalidArgument"));
        assert!(debug.contains("IsoLayout"));
        assert!(!debug.contains("ab, ab"));
        let (reason, transfer) = error.into_parts();
        assert_eq!(reason, TransferError::InvalidArgument);
        assert_eq!(transfer.endpoint_id, 42);

        let error = IsoSubmitError::new(TransferError::Disconnected, transfer);
        assert_eq!(error.into_transfer().layout(), layout);
    }
}
