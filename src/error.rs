use std::{fmt::Display, io, num::NonZeroU32};

use crate::{platform::format_os_error_code, transfer::TransferError};

/// Error returned from `nusb` operations other than transfers.
#[derive(Debug, Clone)]
pub struct Error {
    pub(crate) kind: ErrorKind,
    pub(crate) code: Option<NonZeroU32>,
    pub(crate) message: &'static str,
}

impl Error {
    pub(crate) fn new(kind: ErrorKind, message: &'static str) -> Self {
        Self {
            kind,
            code: None,
            message,
        }
    }

    #[track_caller]
    pub(crate) fn log_error(self) -> Self {
        log::error!("{}", self);
        self
    }

    #[track_caller]
    pub(crate) fn log_debug(self) -> Self {
        log::debug!("{}", self);
        self
    }

    /// Get the error kind.
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// Get the error code from the OS, if applicable.
    ///
    /// * On Linux this is the `errno` value.
    /// * On Windows this is the `WIN32_ERROR` value.
    /// * On macOS this is the `IOReturn` value.
    pub fn os_error(&self) -> Option<u32> {
        self.code.map(|c| c.get())
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)?;
        if let Some(code) = self.code {
            write!(f, " (")?;
            format_os_error_code(f, code.get())?;
            write!(f, ")")?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {}

impl From<Error> for io::Error {
    fn from(err: Error) -> Self {
        let kind = match err.kind {
            ErrorKind::Disconnected => io::ErrorKind::NotConnected,
            ErrorKind::Busy => io::ErrorKind::Other, // TODO: ResourceBusy
            ErrorKind::PermissionDenied => io::ErrorKind::PermissionDenied,
            ErrorKind::NotFound => io::ErrorKind::NotFound,
            ErrorKind::Unsupported => io::ErrorKind::Unsupported,
            ErrorKind::Other => io::ErrorKind::Other,
        };
        io::Error::new(kind, err)
    }
}

/// General category of error as part of an [`Error`].
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    /// Device is disconnected.
    Disconnected,

    /// Device, interface, or endpoint is in use by another application, kernel driver, or handle.
    Busy,

    /// This user or application does not have permission to perform the requested operation.
    PermissionDenied,

    /// Requested configuration, interface, or alternate setting not found
    NotFound,

    /// The requested operation is not supported by the platform or its currently-configured driver.
    Unsupported,

    /// Uncategorized error.
    Other,
}

/// Error from [`crate::Device::active_configuration`]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ActiveConfigurationError {
    pub(crate) configuration_value: u8,
}

impl Display for ActiveConfigurationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.configuration_value == 0 {
            write!(f, "device is not configured")
        } else {
            write!(
                f,
                "no descriptor found for active configuration {}",
                self.configuration_value
            )
        }
    }
}

impl std::error::Error for ActiveConfigurationError {}

impl From<ActiveConfigurationError> for Error {
    fn from(value: ActiveConfigurationError) -> Self {
        let message = if value.configuration_value == 0 {
            "device is not configured"
        } else {
            "no descriptor found for active configuration"
        };
        Error::new(ErrorKind::Other, message)
    }
}

impl From<ActiveConfigurationError> for std::io::Error {
    fn from(value: ActiveConfigurationError) -> Self {
        std::io::Error::other(value)
    }
}

/// Error for descriptor reads.
#[derive(Debug, Copy, Clone)]
pub enum GetDescriptorError {
    /// Transfer error when getting the descriptor.
    Transfer(TransferError),

    /// Invalid descriptor data
    InvalidDescriptor,
}

impl Display for GetDescriptorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GetDescriptorError::Transfer(e) => write!(f, "{}", e),
            GetDescriptorError::InvalidDescriptor => write!(f, "invalid descriptor"),
        }
    }
}

impl std::error::Error for GetDescriptorError {}

impl From<GetDescriptorError> for std::io::Error {
    fn from(value: GetDescriptorError) -> Self {
        match value {
            GetDescriptorError::Transfer(e) => e.into(),
            GetDescriptorError::InvalidDescriptor => {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid descriptor")
            }
        }
    }
}
