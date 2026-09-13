use std::mem;

use io_kit_sys::ret::{kIOReturnIsoTooOld, kIOReturnSuccess, IOReturn};

use crate::{
    transfer::{IsoCompletion, IsoPacketResult, IsoPacketStatus, IsoTransfer, TransferError},
    Speed,
};

use super::iokit_c::IOUSBIsocFrame;

const FRAME_LEAD: u64 = 4;
const FRAME_NOT_REPORTED: IOReturn = i32::MIN;

pub(super) fn scheduled_frame_span(speed: Speed, interval: u8, packet_count: u32) -> Option<u64> {
    if packet_count == 0 || !(1..=16).contains(&interval) {
        return None;
    }
    let service_interval = 1u64.checked_shl((interval - 1) as u32)?;
    let opportunities = u64::from(packet_count).checked_mul(service_interval)?;
    match speed {
        Speed::Full => Some(opportunities),
        Speed::High | Speed::Super | Speed::SuperPlus => {
            opportunities.checked_add(7).map(|value| value / 8)
        }
        _ => None,
    }
}

pub(super) fn choose_start_frame(current: u64, next: Option<u64>) -> Option<u64> {
    let earliest = current.checked_add(FRAME_LEAD)?;
    Some(next.unwrap_or(earliest).max(earliest))
}

#[derive(Default)]
pub(crate) struct IsoTransferState {
    frames: Vec<IOUSBIsocFrame>,
}

pub(super) struct IsoTransferData {
    transfer: Option<IsoTransfer>,
    pub(super) status: IOReturn,
}

unsafe impl Send for IsoTransferData {}
unsafe impl Sync for IsoTransferData {}

impl IsoTransferData {
    pub(super) fn new(mut transfer: IsoTransfer) -> Result<Self, (IsoTransfer, TransferError)> {
        let Ok(packet_size) = u16::try_from(transfer.layout().packet_size()) else {
            return Err((transfer, TransferError::InvalidArgument));
        };

        let packet_count = transfer.layout().packet_count();
        let state = transfer.platform_state_mut();
        if state.frames.is_empty() {
            state.frames = vec![
                IOUSBIsocFrame {
                    frStatus: FRAME_NOT_REPORTED,
                    frReqCount: packet_size,
                    frActCount: 0,
                };
                packet_count
            ];
        } else if state.frames.len() != packet_count {
            return Err((transfer, TransferError::InvalidArgument));
        }

        Ok(Self {
            transfer: Some(transfer),
            status: kIOReturnSuccess,
        })
    }

    pub(super) fn prepare(&mut self) {
        self.status = kIOReturnSuccess;
        let transfer = self.transfer.as_mut().unwrap();
        let packet_size = transfer.layout().packet_size() as u16;
        transfer.platform_state_mut().frames.fill(IOUSBIsocFrame {
            frStatus: FRAME_NOT_REPORTED,
            frReqCount: packet_size,
            frActCount: 0,
        });
    }

    pub(super) fn submission_parts(&mut self) -> (*mut u8, u32, *mut IOUSBIsocFrame) {
        let transfer = self.transfer.as_mut().unwrap();
        let buffer = transfer.buffer.ptr;
        let packet_count = transfer.layout().packet_count() as u32;
        let frames = transfer.platform_state_mut().frames.as_mut_ptr();
        (buffer, packet_count, frames)
    }

    pub(super) fn take_completion(&mut self) -> IsoCompletion {
        let mut transfer = self.transfer.take().unwrap();
        let mut overall_status = super::status_to_transfer_result(self.status);
        let frames = mem::take(&mut transfer.platform_state_mut().frames);
        apply_packet_results(&mut transfer, &frames, &mut overall_status);
        transfer.platform_state_mut().frames = frames;
        IsoCompletion::from_completed_transfer(transfer, overall_status)
    }
}

fn apply_packet_results(
    transfer: &mut IsoTransfer,
    frames: &[IOUSBIsocFrame],
    overall_status: &mut Result<(), TransferError>,
) {
    let packet_size = transfer.layout().packet_size();
    debug_assert_eq!(transfer.packet_results_mut().len(), frames.len());
    for (result, frame) in transfer.packet_results_mut().iter_mut().zip(frames) {
        let requested_matches = frame.frReqCount as usize == packet_size;
        let length_fits = frame.frActCount <= frame.frReqCount;
        if (!requested_matches || !length_fits) && overall_status.is_ok() {
            *overall_status = Err(TransferError::Fault);
        }

        let actual_len = if requested_matches && length_fits {
            frame.frActCount as usize
        } else {
            0
        };
        let (status, native_status) = if !requested_matches || !length_fits {
            (
                IsoPacketStatus::Error(TransferError::Fault),
                Some(frame.frStatus as u32),
            )
        } else if frame.frStatus == FRAME_NOT_REPORTED {
            (IsoPacketStatus::NotReported, None)
        } else if frame.frStatus == kIOReturnIsoTooOld {
            (IsoPacketStatus::NotExecuted, Some(frame.frStatus as u32))
        } else {
            match super::status_to_transfer_result(frame.frStatus) {
                Ok(()) => (IsoPacketStatus::Success, Some(frame.frStatus as u32)),
                Err(error) => (IsoPacketStatus::Error(error), Some(frame.frStatus as u32)),
            }
        };
        *result = IsoPacketResult::new(packet_size, actual_len, status, native_status);
    }
}

#[cfg(test)]
mod tests {
    use io_kit_sys::ret::{kIOReturnBadArgument, kIOReturnIsoTooOld, kIOReturnSuccess};

    use super::{apply_packet_results, choose_start_frame, scheduled_frame_span, IsoTransferData};
    use crate::{
        platform::macos_iokit::iokit_c::IOUSBIsocFrame,
        transfer::{Buffer, IsoLayout, IsoPacketStatus, IsoTransfer, TransferError},
        Speed,
    };

    #[test]
    fn frame_span_respects_speed_and_interval() {
        assert_eq!(scheduled_frame_span(Speed::Full, 1, 8), Some(8));
        assert_eq!(scheduled_frame_span(Speed::High, 1, 8), Some(1));
        assert_eq!(scheduled_frame_span(Speed::High, 4, 8), Some(8));
        assert_eq!(scheduled_frame_span(Speed::Super, 1, 1), Some(1));
        assert_eq!(scheduled_frame_span(Speed::High, 0, 8), None);
        assert_eq!(scheduled_frame_span(Speed::High, 17, 8), None);
    }

    #[test]
    fn start_frame_uses_a_lead_and_recovers_an_expired_cursor() {
        assert_eq!(choose_start_frame(100, None), Some(104));
        assert_eq!(choose_start_frame(100, Some(120)), Some(120));
        assert_eq!(choose_start_frame(100, Some(90)), Some(104));
        assert_eq!(choose_start_frame(u64::MAX, None), None);
    }

    #[test]
    fn maps_sparse_frame_results_without_compaction() {
        let layout = IsoLayout::new(3, 8).unwrap();
        let mut transfer = IsoTransfer::new(1, layout, Buffer::new(layout.total_len()));
        transfer.packet_mut(0).unwrap()[..3].fill(1);
        transfer.packet_mut(2).unwrap()[..7].fill(3);
        let frames = [
            IOUSBIsocFrame {
                frStatus: kIOReturnSuccess,
                frReqCount: 8,
                frActCount: 3,
            },
            IOUSBIsocFrame {
                frStatus: kIOReturnIsoTooOld,
                frReqCount: 8,
                frActCount: 0,
            },
            IOUSBIsocFrame {
                frStatus: kIOReturnSuccess,
                frReqCount: 8,
                frActCount: 7,
            },
        ];
        let mut status = Ok(());

        apply_packet_results(&mut transfer, &frames, &mut status);

        assert_eq!(status, Ok(()));
        assert_eq!(transfer.packet_results_mut()[0].actual_len(), 3);
        assert_eq!(
            transfer.packet_results_mut()[1].status(),
            IsoPacketStatus::NotExecuted
        );
        assert_eq!(transfer.packet_results_mut()[2].actual_len(), 7);
    }

    #[test]
    fn malformed_frame_metadata_faults_the_completion() {
        let layout = IsoLayout::new(1, 8).unwrap();
        let mut transfer = IsoTransfer::new(1, layout, Buffer::new(layout.total_len()));
        let frames = [IOUSBIsocFrame {
            frStatus: kIOReturnSuccess,
            frReqCount: 8,
            frActCount: 9,
        }];
        let mut status = Ok(());

        apply_packet_results(&mut transfer, &frames, &mut status);

        assert_eq!(status, Err(TransferError::Fault));
        assert_eq!(transfer.packet_results_mut()[0].actual_len(), 0);
        assert_eq!(
            transfer.packet_results_mut()[0].status(),
            IsoPacketStatus::Error(TransferError::Fault)
        );
    }

    #[test]
    fn frame_storage_survives_transfer_reuse() {
        let layout = IsoLayout::new(2, 8).unwrap();
        let transfer = IsoTransfer::new(1, layout, Buffer::new(layout.total_len()));
        let mut data = IsoTransferData::new(transfer).unwrap();
        data.prepare();
        let (_, _, first_frames) = data.submission_parts();
        let transfer = data.take_completion().into_transfer();
        let mut data = IsoTransferData::new(transfer).unwrap();
        data.prepare();
        let (_, _, second_frames) = data.submission_parts();
        assert_eq!(first_frames, second_frames);
    }

    #[test]
    fn synchronous_failure_does_not_fabricate_packet_success() {
        let layout = IsoLayout::new(2, 8).unwrap();
        let transfer = IsoTransfer::new(1, layout, Buffer::new(layout.total_len()));
        let mut data = IsoTransferData::new(transfer).unwrap();
        data.prepare();
        data.status = kIOReturnBadArgument;

        let completion = data.take_completion();

        assert_eq!(completion.status(), Err(TransferError::InvalidArgument));
        assert!(completion
            .packets()
            .all(|packet| packet.status() == IsoPacketStatus::NotReported));
    }
}
