//! WinUSB isochronous transfer support (Windows 8.1 and later).

use std::{ffi::c_void, mem, ptr, sync::Arc};

use windows_sys::Win32::{
    Devices::Usb::{
        WinUsb_QueryPipeEx, WinUsb_ReadIsochPipeAsap, WinUsb_RegisterIsochBuffer,
        WinUsb_UnregisterIsochBuffer, WinUsb_WriteIsochPipeAsap, USBD_ISO_PACKET_DESCRIPTOR,
        WINUSB_PIPE_INFORMATION_EX,
    },
    Foundation::{
        GetLastError, ERROR_BAD_COMMAND, ERROR_DEVICE_NOT_CONNECTED, ERROR_FILE_NOT_FOUND,
        ERROR_GEN_FAILURE, ERROR_IO_PENDING, ERROR_NO_MORE_ITEMS, ERROR_NO_SUCH_DEVICE,
        ERROR_OPERATION_ABORTED, ERROR_REQUEST_ABORTED, ERROR_SEM_TIMEOUT, ERROR_SUCCESS,
        ERROR_TIMEOUT, FALSE, TRUE,
    },
    System::{
        Threading::{CancelThreadpoolIo, StartThreadpoolIo},
        IO::OVERLAPPED,
    },
};

use crate::{
    transfer::{
        internal::{notify_completion, Idle, Pending},
        Direction, IsoCompletion, IsoLayout, IsoPacketResult, IsoPacketStatus, IsoTransfer,
        TransferError,
    },
    Error, ErrorKind, Speed,
};

use super::{
    transfer::{TransferHeader, TransferKind},
    Interface as WindowsInterface,
};

#[derive(Debug, Copy, Clone)]
pub(super) struct IsoPipeInfo {
    pub(super) maximum_bytes_per_interval: u32,
    interval: u8,
    speed: Speed,
}

pub(super) fn query_pipe(
    interface: &WindowsInterface,
    alternate_setting: u8,
    endpoint: u8,
) -> Result<IsoPipeInfo, Error> {
    for pipe_index in 0..=u8::MAX {
        let mut info: WINUSB_PIPE_INFORMATION_EX = unsafe { mem::zeroed() };
        let result = unsafe {
            WinUsb_QueryPipeEx(
                interface.winusb_handle,
                alternate_setting,
                pipe_index,
                &mut info,
            )
        };
        if result != TRUE {
            let error = unsafe { GetLastError() };
            if error == ERROR_NO_MORE_ITEMS {
                break;
            }
            return Err(Error::new_os(
                ErrorKind::Other,
                "failed to query WinUSB pipe capabilities",
                error,
            ));
        }
        if info.PipeId == endpoint {
            if info.PipeType != windows_sys::Win32::Devices::Usb::UsbdPipeTypeIsochronous {
                return Err(Error::new(
                    ErrorKind::Other,
                    "WinUSB pipe is not isochronous",
                ));
            }
            if info.MaximumBytesPerInterval == 0 {
                return Err(Error::new(
                    ErrorKind::Other,
                    "WinUSB reported a zero isochronous service-interval payload",
                ));
            }
            let Some(speed) = interface.device.speed() else {
                return Err(Error::new(
                    ErrorKind::Other,
                    "WinUSB did not report the USB connection speed",
                ));
            };
            return Ok(IsoPipeInfo {
                maximum_bytes_per_interval: info.MaximumBytesPerInterval,
                interval: info.Interval,
                speed,
            });
        }
    }

    Err(Error::new(
        ErrorKind::NotFound,
        "WinUSB pipe was not found in the active alternate setting",
    ))
}

/// WinUSB registration retained by the public transfer across completions.
///
/// `IsoTransfer` drops this platform state before dropping its backing buffer,
/// and this interface reference keeps the native WinUSB handle alive until
/// unregistration finishes.
struct IsoRegistration {
    handle: *mut c_void,
    _interface: Arc<WindowsInterface>,
    packet_descriptors: Vec<USBD_ISO_PACKET_DESCRIPTOR>,
}

// Access is serialized by the endpoint transfer state machine. WinUSB may
// write descriptors while the request is pending on its completion thread.
unsafe impl Send for IsoRegistration {}
unsafe impl Sync for IsoRegistration {}

impl Drop for IsoRegistration {
    fn drop(&mut self) {
        if self.handle.is_null() {
            return;
        }
        let result = unsafe { WinUsb_UnregisterIsochBuffer(self.handle) };
        if result != TRUE {
            log::warn!(
                "WinUsb_UnregisterIsochBuffer failed with error {}",
                unsafe { GetLastError() }
            );
        }
        self.handle = ptr::null_mut();
    }
}

#[derive(Default)]
pub(crate) struct IsoTransferState {
    registration: Option<IsoRegistration>,
}

#[repr(C)]
pub(super) struct IsoTransferData {
    // Shared prefix used by the interface's single threadpool-I/O callback.
    header: TransferHeader,
    transfer: Option<IsoTransfer>,
    interface: Arc<WindowsInterface>,
    endpoint: u8,
    completion_status: Result<(), TransferError>,
}

unsafe impl Send for IsoTransferData {}
unsafe impl Sync for IsoTransferData {}

impl IsoTransferData {
    pub(super) fn new(
        interface: Arc<WindowsInterface>,
        endpoint: u8,
        mut transfer: IsoTransfer,
        pipe: IsoPipeInfo,
    ) -> Result<Self, (IsoTransfer, TransferError)> {
        if !layout_is_supported(
            pipe.speed,
            pipe.interval,
            pipe.maximum_bytes_per_interval,
            transfer.layout(),
        ) {
            return Err((transfer, TransferError::InvalidArgument));
        }

        if transfer.platform_state_mut().registration.is_none() {
            let mut handle = ptr::null_mut();
            let result = unsafe {
                WinUsb_RegisterIsochBuffer(
                    interface.winusb_handle,
                    endpoint,
                    transfer.buffer.ptr,
                    transfer.layout().total_len() as u32,
                    &mut handle,
                )
            };
            if result != TRUE {
                let error = unsafe { GetLastError() };
                return Err((
                    transfer,
                    map_win32_transfer_error(error)
                        .err()
                        .unwrap_or(TransferError::Unknown(error)),
                ));
            }

            let registration = IsoRegistration {
                handle,
                _interface: interface.clone(),
                packet_descriptors: vec![
                    unsafe { mem::zeroed() };
                    transfer.layout().packet_count()
                ],
            };
            transfer.platform_state_mut().registration = Some(registration);
        }

        Ok(Self {
            header: TransferHeader {
                overlapped: unsafe { mem::zeroed() },
                kind: TransferKind::Isochronous,
            },
            transfer: Some(transfer),
            interface,
            endpoint,
            completion_status: Ok(()),
        })
    }

    fn prepare(&mut self) {
        self.header.overlapped = unsafe { mem::zeroed() };
        self.completion_status = Ok(());
        self.registration_mut()
            .packet_descriptors
            .fill(unsafe { mem::zeroed() });
    }

    fn registration_mut(&mut self) -> &mut IsoRegistration {
        self.transfer
            .as_mut()
            .unwrap()
            .platform_state_mut()
            .registration
            .as_mut()
            .expect("WinUSB ISO transfer is missing its registration")
    }

    unsafe fn submit_asap(&mut self, continue_stream: bool, overlapped: *const OVERLAPPED) -> i32 {
        let direction = Direction::from_address(self.endpoint);
        let transfer = self.transfer.as_ref().unwrap();
        let length = transfer.layout().total_len() as u32;
        let packet_count = transfer.layout().packet_count() as u32;
        let registration = self.registration_mut();
        match direction {
            Direction::In => WinUsb_ReadIsochPipeAsap(
                registration.handle,
                0,
                length,
                if continue_stream { TRUE } else { FALSE },
                packet_count,
                registration.packet_descriptors.as_mut_ptr(),
                overlapped,
            ),
            Direction::Out => WinUsb_WriteIsochPipeAsap(
                registration.handle,
                0,
                length,
                if continue_stream { TRUE } else { FALSE },
                overlapped,
            ),
        }
    }

    pub(super) fn take_completion(&mut self) -> IsoCompletion {
        let direction = Direction::from_address(self.endpoint);
        let mut transfer = self.transfer.take().unwrap();
        let mut status = self.completion_status;

        match direction {
            Direction::In => {
                let descriptors = mem::take(
                    &mut transfer
                        .platform_state_mut()
                        .registration
                        .as_mut()
                        .expect("WinUSB ISO transfer is missing its registration")
                        .packet_descriptors,
                );
                apply_in_packet_results(&mut transfer, &descriptors, &mut status);
                transfer
                    .platform_state_mut()
                    .registration
                    .as_mut()
                    .unwrap()
                    .packet_descriptors = descriptors;
            }
            Direction::Out => {
                let packet_size = transfer.layout().packet_size();
                for result in transfer.packet_results_mut() {
                    *result =
                        IsoPacketResult::new(packet_size, 0, IsoPacketStatus::NotReported, None);
                }
            }
        }

        IsoCompletion::from_completed_transfer(transfer, status)
    }
}

impl WindowsInterface {
    pub(super) fn submit_iso(
        &self,
        mut transfer: Idle<IsoTransferData>,
        continue_stream: bool,
    ) -> Pending<IsoTransferData> {
        transfer.prepare();
        let transfer = transfer.pre_submit();
        let ptr = transfer.as_ptr();

        log::debug!(
            "Submit WinUSB ISO transfer {ptr:?} on endpoint {:02X}, continue_stream={continue_stream}",
            unsafe { (*ptr).endpoint }
        );

        unsafe { StartThreadpoolIo(self.threadpool_io) };
        let result = unsafe {
            (*ptr).submit_asap(
                continue_stream,
                ptr.cast::<TransferHeader>().cast::<OVERLAPPED>(),
            )
        };

        if result != TRUE {
            let error = unsafe { GetLastError() };
            if error != ERROR_IO_PENDING {
                log::error!(
                    "WinUSB ISO submit failed with error {}",
                    std::io::Error::from_raw_os_error(error as i32)
                );
                unsafe {
                    CancelThreadpoolIo(self.threadpool_io);
                    (*ptr).completion_status = map_win32_transfer_error(error);
                    notify_completion::<IsoTransferData>(ptr);
                }
            }
        }

        transfer
    }
}

pub(super) unsafe fn complete_callback(
    overlapped: *mut c_void,
    result: u32,
    bytes_transferred: usize,
) {
    let transfer = overlapped as *mut IsoTransferData;
    log::debug!(
        "WinUSB ISO transfer {transfer:?} on endpoint {:02X} complete: status {result}, {bytes_transferred} bytes",
        (*transfer).endpoint
    );
    (*transfer).completion_status = map_win32_transfer_error(result);
    notify_completion::<IsoTransferData>(transfer);
}

pub(super) fn continue_stream_for_tail(_tail_complete: Option<bool>) -> bool {
    // The portable v0 contract is ASAP scheduling, not a gapless stream.
    // Passing FALSE still lets WinUSB schedule a request after transfers that
    // are already pending, but it does not fail the request merely because the
    // host could not place it in the immediately following frame.
    false
}

pub(super) fn can_submit_with_pending(pending: usize) -> bool {
    // The reusable-transfer API currently registers each allocation as a
    // separate WinUSB isochronous buffer. Long-running hardware tests found
    // that multiple such registrations in flight can stop completing without
    // a device disconnect. Keep v0 safe and explicit until the Windows backend
    // has an endpoint-owned registered ring with non-overlapping offsets.
    pending == 0
}

fn layout_is_supported(
    speed: Speed,
    interval: u8,
    maximum_bytes_per_interval: u32,
    layout: IsoLayout,
) -> bool {
    if !(1..=16).contains(&interval) || layout.packet_size() != maximum_bytes_per_interval as usize
    {
        return false;
    }

    let packet_count = layout.packet_count();
    match speed {
        Speed::Low => false,
        Speed::Full => packet_count <= 255,
        Speed::High | Speed::Super | Speed::SuperPlus => {
            if packet_count > 1024 {
                return false;
            }
            // WinUSB requires each request to end on a one-millisecond frame
            // boundary. For sub-frame service intervals, that makes the
            // packet count a multiple of the opportunities in one frame.
            let opportunities_per_frame = if interval <= 4 {
                8 >> (interval - 1)
            } else {
                1
            };
            packet_count % opportunities_per_frame as usize == 0
        }
    }
}

fn map_win32_transfer_error(error: u32) -> Result<(), TransferError> {
    match error {
        ERROR_SUCCESS => Ok(()),
        ERROR_GEN_FAILURE => Err(TransferError::Stall),
        ERROR_REQUEST_ABORTED | ERROR_TIMEOUT | ERROR_SEM_TIMEOUT | ERROR_OPERATION_ABORTED => {
            Err(TransferError::Cancelled)
        }
        ERROR_BAD_COMMAND
        | ERROR_FILE_NOT_FOUND
        | ERROR_DEVICE_NOT_CONNECTED
        | ERROR_NO_SUCH_DEVICE => Err(TransferError::Disconnected),
        other => Err(TransferError::Unknown(other)),
    }
}

fn apply_in_packet_results(
    transfer: &mut IsoTransfer,
    descriptors: &[USBD_ISO_PACKET_DESCRIPTOR],
    overall_status: &mut Result<(), TransferError>,
) {
    const USBD_STATUS_ISO_NOT_ACCESSED_BY_HW: u32 = 0xc002_0000;

    let packet_size = transfer.layout().packet_size();
    for (index, (result, native)) in transfer
        .packet_results_mut()
        .iter_mut()
        .zip(descriptors)
        .enumerate()
    {
        let native_status = native.Status as u32;
        let offset_matches = native.Offset as usize == index * packet_size;
        let length_fits = native.Length as usize <= packet_size;
        if (!offset_matches || !length_fits) && overall_status.is_ok() {
            *overall_status = Err(TransferError::Fault);
        }
        let actual_len = if offset_matches && length_fits {
            native.Length as usize
        } else {
            0
        };
        let packet_status = if !offset_matches || !length_fits {
            IsoPacketStatus::Error(TransferError::Fault)
        } else if native_status == 0 {
            IsoPacketStatus::Success
        } else if native_status == USBD_STATUS_ISO_NOT_ACCESSED_BY_HW {
            IsoPacketStatus::NotExecuted
        } else {
            IsoPacketStatus::Error(TransferError::Fault)
        };
        *result = IsoPacketResult::new(packet_size, actual_len, packet_status, Some(native_status));
    }
}

#[cfg(test)]
mod tests {
    use windows_sys::Win32::{
        Devices::Usb::USBD_ISO_PACKET_DESCRIPTOR,
        Foundation::{ERROR_DEVICE_NOT_CONNECTED, ERROR_GEN_FAILURE, ERROR_OPERATION_ABORTED},
    };

    use super::{
        apply_in_packet_results, can_submit_with_pending, continue_stream_for_tail,
        layout_is_supported, map_win32_transfer_error,
    };
    use crate::{
        transfer::{Buffer, IsoLayout, IsoPacketStatus, IsoTransfer, TransferError},
        Speed,
    };

    #[test]
    fn asap_policy_never_requires_continuity() {
        assert!(!continue_stream_for_tail(None));
        assert!(!continue_stream_for_tail(Some(false)));
        assert!(!continue_stream_for_tail(Some(true)));
    }

    #[test]
    fn windows_v0_serializes_iso_requests() {
        assert!(can_submit_with_pending(0));
        assert!(!can_submit_with_pending(1));
        assert!(!can_submit_with_pending(4));
    }

    #[test]
    fn validates_winusb_packet_count_and_frame_boundaries() {
        let layout = |count, size| IsoLayout::new(count, size).unwrap();

        assert!(layout_is_supported(Speed::Full, 1, 1023, layout(255, 1023)));
        assert!(!layout_is_supported(
            Speed::Full,
            1,
            1023,
            layout(256, 1023)
        ));

        assert!(layout_is_supported(Speed::High, 1, 3072, layout(8, 3072)));
        assert!(!layout_is_supported(Speed::High, 1, 3072, layout(7, 3072)));
        assert!(layout_is_supported(Speed::High, 2, 1024, layout(4, 1024)));
        assert!(!layout_is_supported(Speed::High, 2, 1024, layout(2, 1024)));
        assert!(layout_is_supported(Speed::High, 4, 1024, layout(1, 1024)));
        assert!(layout_is_supported(Speed::Super, 5, 1024, layout(1, 1024)));
        assert!(!layout_is_supported(
            Speed::Super,
            1,
            1024,
            layout(1025, 1024)
        ));
    }

    #[test]
    fn rejects_winusb_ambiguous_or_mismatched_layouts() {
        let layout = IsoLayout::new(8, 1024).unwrap();
        assert!(!layout_is_supported(Speed::High, 0, 1024, layout));
        assert!(!layout_is_supported(Speed::High, 17, 1024, layout));
        assert!(!layout_is_supported(Speed::High, 1, 512, layout));
        assert!(!layout_is_supported(Speed::Low, 1, 1024, layout));
    }

    #[test]
    fn maps_win32_completion_errors() {
        assert_eq!(map_win32_transfer_error(0), Ok(()));
        assert_eq!(
            map_win32_transfer_error(ERROR_OPERATION_ABORTED),
            Err(TransferError::Cancelled)
        );
        assert_eq!(
            map_win32_transfer_error(ERROR_GEN_FAILURE),
            Err(TransferError::Stall)
        );
        assert_eq!(
            map_win32_transfer_error(ERROR_DEVICE_NOT_CONNECTED),
            Err(TransferError::Disconnected)
        );
        assert_eq!(
            map_win32_transfer_error(0xdead_beef),
            Err(TransferError::Unknown(0xdead_beef))
        );
    }

    #[test]
    fn maps_sparse_in_packet_results_without_compaction() {
        let layout = IsoLayout::new(2, 8).unwrap();
        let mut transfer = IsoTransfer::new(1, layout, Buffer::new(layout.total_len()));
        let descriptors = [
            USBD_ISO_PACKET_DESCRIPTOR {
                Offset: 0,
                Length: 3,
                Status: 0,
            },
            USBD_ISO_PACKET_DESCRIPTOR {
                Offset: 8,
                Length: 7,
                Status: 0,
            },
        ];
        let mut status = Ok(());

        apply_in_packet_results(&mut transfer, &descriptors, &mut status);

        assert_eq!(status, Ok(()));
        let results = transfer.packet_results_mut();
        assert_eq!(results[0].actual_len(), 3);
        assert_eq!(results[1].actual_len(), 7);
        assert_eq!(results[0].status(), IsoPacketStatus::Success);
        assert_eq!(results[1].status(), IsoPacketStatus::Success);
    }

    #[test]
    fn malformed_in_packet_metadata_faults_the_completion() {
        let layout = IsoLayout::new(2, 8).unwrap();
        let mut transfer = IsoTransfer::new(1, layout, Buffer::new(layout.total_len()));
        let descriptors = [
            USBD_ISO_PACKET_DESCRIPTOR {
                Offset: 4,
                Length: 3,
                Status: 0,
            },
            USBD_ISO_PACKET_DESCRIPTOR {
                Offset: 8,
                Length: 9,
                Status: 0,
            },
        ];
        let mut status = Ok(());

        apply_in_packet_results(&mut transfer, &descriptors, &mut status);

        assert_eq!(status, Err(TransferError::Fault));
        for result in transfer.packet_results_mut() {
            assert_eq!(result.actual_len(), 0);
            assert_eq!(
                result.status(),
                IsoPacketStatus::Error(TransferError::Fault)
            );
        }
    }

    #[test]
    fn reports_packets_not_accessed_by_hardware() {
        let layout = IsoLayout::new(1, 8).unwrap();
        let mut transfer = IsoTransfer::new(1, layout, Buffer::new(layout.total_len()));
        let descriptors = [USBD_ISO_PACKET_DESCRIPTOR {
            Offset: 0,
            Length: 0,
            Status: 0xc002_0000u32 as i32,
        }];
        let mut status = Ok(());

        apply_in_packet_results(&mut transfer, &descriptors, &mut status);

        assert_eq!(status, Ok(()));
        let result = transfer.packet_results_mut()[0];
        assert_eq!(result.status(), IsoPacketStatus::NotExecuted);
        assert_eq!(result.native_status(), Some(0xc002_0000));
    }
}
