mod transfer;
use crate::ErrorKind;
use rustix::io::Errno;
use std::num::NonZeroU32;
pub(crate) use transfer::TransferData;
mod enumeration;
use crate::error::Error;
pub use enumeration::{list_buses, list_devices, DevfsPath};

mod device;
pub(crate) use device::IllumosDevice as Device;
pub(crate) use device::IllumosEndpoint as Endpoint;
pub(crate) use device::IllumosInterface as Interface;

use crate::transfer::TransferError;

#[derive(Copy, Clone, Debug, Hash, PartialEq, Eq)]
pub struct DeviceId {
    pub(crate) bus: u8,
    pub(crate) addr: u8,
}

// from usr/src/uts/common/sys/usb/clients/ugen/usb_ugen.h
fn ugen_to_transfer_error(e: u32) -> TransferError {
    match e {
        // define USB_LC_STAT_NOERROR             0x00    /* No error               */
        // We really should not get here but `panic` seems harsh....
        0 => TransferError::Unknown(0),
        // #define USB_LC_STAT_CRC                 0x01    /* CRC timeout detected   */
        1 => TransferError::Cancelled,
        // #define USB_LC_STAT_BITSTUFFING         0x02    /* Bit-stuffing violation */
        2 => TransferError::Fault,
        // #define USB_LC_STAT_DATA_TOGGLE_MM      0x03    /* Data toggle mismatch   */
        3 => TransferError::Fault,
        // #define USB_LC_STAT_STALL               0x04    /* Endpoint stalled       */
        4 => TransferError::Stall,
        // #define USB_LC_STAT_DEV_NOT_RESP        0x05    /* Device not responding  */
        5 => TransferError::Disconnected,
        // #define USB_LC_STAT_PID_CHECKFAILURE    0x06    /* PID Check failure      */
        6 => TransferError::Fault,
        // #define USB_LC_STAT_UNEXP_PID           0x07    /* Unexpected PID         */
        7 => TransferError::Fault,
        // #define USB_LC_STAT_DATA_OVERRUN        0x08    /* Data size exceeded     */
        8 => TransferError::InvalidArgument,
        //  #define USB_LC_STAT_DATA_UNDERRUN       0x09    /* Less data received     */
        9 => TransferError::InvalidArgument,
        // #define USB_LC_STAT_BUFFER_OVERRUN      0x0a    /* Buffer size exceeded   */
        0xa => TransferError::InvalidArgument,
        // #define USB_LC_STAT_BUFFER_UNDERRUN     0x0b    /* Buffer under run       */
        0xb => TransferError::InvalidArgument,
        // #define USB_LC_STAT_TIMEOUT             0x0c    /* Command timed out      */
        0xc => TransferError::Cancelled,
        // #define USB_LC_STAT_NOT_ACCESSED        0x0d    /* Not accessed by h/w    */
        0xd => TransferError::InvalidArgument,
        // #define USB_LC_STAT_UNSPECIFIED_ERR     0x0e    /* Unspecified error      */
        0xe => TransferError::Fault,
        // #define USB_LC_STAT_NO_BANDWIDTH        0x41    /* No bandwidth           */
        0x41 => TransferError::Fault,
        // #define USB_LC_STAT_HW_ERR              0x42    /* Hardware error         */
        0x42 => TransferError::Fault,
        // #define USB_LC_STAT_SUSPENDED           0x43    /* Device suspended/resumed */
        0x43 => TransferError::Disconnected,
        // #define USB_LC_STAT_DISCONNECTED        0x44    /* Device disconnected    */
        0x44 => TransferError::Disconnected,
        // #define USB_LC_STAT_INTR_BUF_FULL       0x45    /* Interrupt buf was full */
        0x45 => TransferError::Fault,
        // #define USB_LC_STAT_INVALID_REQ         0x46    /* request was invalid    */
        0x46 => TransferError::InvalidArgument,
        // #define USB_LC_STAT_INTERRUPTED         0x47    /* request was interrupted  */
        0x47 => TransferError::Cancelled,
        // #define USB_LC_STAT_NO_RESOURCES        0x48    /* no resources for req   */
        0x48 => TransferError::Fault,
        // #define USB_LC_STAT_INTR_POLLING_FAILED 0x49    /* failed to restart poll  */
        0x49 => TransferError::Fault,
        // #define USB_LC_STAT_ISOC_POLLING_FAILED 0x50    /* failed to restart iso poll */
        0x50 => TransferError::Fault,
        // #define USB_LC_STAT_ISOC_UNINITIALIZED  0x51    /* isoc_info not inited yet */
        0x51 => TransferError::Fault,
        // #define USB_LC_STAT_ISOC_PKT_ERROR      0x52    /* All pkts in last req fail */
        0x52 => TransferError::Fault,
        _ => TransferError::Unknown(e),
    }
}

fn errno_to_transfer_error(e: Errno) -> TransferError {
    match e {
        Errno::NODEV | Errno::SHUTDOWN => TransferError::Disconnected,
        Errno::PIPE => TransferError::Stall,
        Errno::NOENT | Errno::CONNRESET | Errno::TIMEDOUT => TransferError::Cancelled,
        Errno::PROTO | Errno::ILSEQ | Errno::OVERFLOW | Errno::COMM | Errno::TIME => {
            TransferError::Fault
        }
        Errno::BADF => {
            println!("we have closed it somehow?");

            TransferError::Unknown(e.raw_os_error() as u32)
        }
        _ => TransferError::Unknown(e.raw_os_error() as u32),
    }
}

pub fn format_os_error_code(f: &mut std::fmt::Formatter<'_>, code: u32) -> std::fmt::Result {
    write!(f, "errno {}", code)
}

impl Error {
    pub(crate) fn new_os(kind: ErrorKind, message: &'static str, code: Errno) -> Self {
        Self {
            kind,
            code: NonZeroU32::new(code.raw_os_error() as u32),
            message,
        }
    }
}

impl From<Errno> for Error {
    fn from(e: Errno) -> Self {
        match e {
            Errno::NOENT => Error::new_os(ErrorKind::Disconnected, "device not found", e),
            Errno::PERM => Error::new_os(ErrorKind::PermissionDenied, "permission denied", e),
            e => Error::new_os(ErrorKind::Other, "failed to open device", e),
        }
        .log_debug()
    }
}
