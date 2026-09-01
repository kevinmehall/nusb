//! Extension point for USB devices whose raw transfers are supplied by another process.
//!
//! Implementations provide the same device, interface, and endpoint operations as the native
//! platform backend. Scanner policy belongs above this layer; implementations only move USB
//! requests and completions.

use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use crate::descriptors::{ConfigurationDescriptor, DeviceDescriptor, EndpointDescriptor};
use crate::transfer::{
    Buffer, Completion, ControlIn, ControlOut, ControlType, Recipient, TransferError,
};
use crate::{Error, ErrorKind, MaybeFuture as _, Speed};

/// Raw operations for one open USB device.
pub trait Device: Send + Sync {
    /// Cached device descriptor.
    fn device_descriptor(&self) -> DeviceDescriptor;

    /// Cached configuration descriptor byte strings.
    fn configuration_descriptors(&self) -> Vec<Vec<u8>>;

    /// Current connection speed, when known.
    fn speed(&self) -> Option<Speed>;

    /// Active `bConfigurationValue`, or zero while unconfigured.
    fn active_configuration_value(&self) -> u8;

    /// Select the active configuration.
    fn set_configuration(&self, configuration: u8) -> Result<(), Error>;

    /// Reset the device.
    fn reset(&self) -> Result<(), Error>;

    /// Claim an interface.
    fn claim_interface(&self, interface: u8) -> Result<Arc<dyn Interface>, Error>;

    /// Detach a kernel driver, where supported, then claim an interface.
    fn detach_and_claim_interface(&self, interface: u8) -> Result<Arc<dyn Interface>, Error>;

    /// Detach the kernel driver for an interface, where supported.
    fn detach_kernel_driver(&self, interface: u8) -> Result<(), Error>;

    /// Reattach the kernel driver for an interface, where supported.
    fn attach_kernel_driver(&self, interface: u8) -> Result<(), Error>;

    /// Perform a control-IN transfer on endpoint zero.
    fn control_in(&self, data: ControlIn, timeout: Duration) -> Result<Vec<u8>, TransferError>;

    /// Perform a control-OUT transfer on endpoint zero.
    fn control_out(&self, data: ControlOut<'_>, timeout: Duration) -> Result<(), TransferError>;

    /// Read one USB descriptor.
    fn get_descriptor(
        &self,
        desc_type: u8,
        desc_index: u8,
        language_id: u16,
        timeout: Duration,
    ) -> Result<Vec<u8>, TransferError> {
        self.control_in(
            ControlIn {
                control_type: ControlType::Standard,
                recipient: Recipient::Device,
                request: 0x06,
                value: (u16::from(desc_type) << 8) | u16::from(desc_index),
                index: language_id,
                length: 4096,
            },
            timeout,
        )
    }
}

/// Raw operations for one claimed USB interface.
pub trait Interface: Send + Sync {
    /// Interface number.
    fn interface_number(&self) -> u8;

    /// Select an alternate setting.
    fn set_alt_setting(&self, alt_setting: u8) -> Result<(), Error>;

    /// Current alternate setting.
    fn get_alt_setting(&self) -> u8;

    /// Perform a control-IN transfer.
    fn control_in(&self, data: ControlIn, timeout: Duration) -> Result<Vec<u8>, TransferError>;

    /// Perform a control-OUT transfer.
    fn control_out(&self, data: ControlOut<'_>, timeout: Duration) -> Result<(), TransferError>;

    /// Open an endpoint described by the active alternate setting.
    fn endpoint(&self, descriptor: EndpointDescriptor<'_>) -> Result<Box<dyn Endpoint>, Error>;

    /// Release this interface after every public handle has been dropped.
    fn release(self: Arc<Self>) -> Result<(), Error>;
}

/// Raw operations and completion queue for one open endpoint.
pub trait Endpoint: Send + Sync {
    /// Endpoint address.
    fn endpoint_address(&self) -> u8;

    /// Maximum packet size.
    fn max_packet_size(&self) -> usize;

    /// Number of submitted transfers not yet returned to the caller.
    fn pending(&self) -> usize;

    /// Allocate a transfer buffer, using backend-specific memory when available.
    fn allocate(&self, len: usize) -> Result<Buffer, Error> {
        Ok(Buffer::new(len))
    }

    /// Request cancellation of all submitted transfers.
    fn cancel_all(&mut self);

    /// Submit one transfer buffer.
    fn submit(&mut self, buffer: Buffer);

    /// Queue a transfer that completes with a validation error.
    fn submit_err(&mut self, buffer: Buffer, error: TransferError);

    /// Poll the next completion.
    fn poll_next_complete(&mut self, cx: &mut Context<'_>) -> Poll<Completion>;

    /// Wait up to `timeout` for the next completion.
    fn wait_next_complete(&mut self, timeout: Duration) -> Option<Completion>;

    /// Clear the endpoint halt condition.
    fn clear_halt(&mut self) -> Result<(), Error>;
}

pub(crate) struct NativeDevice(pub(crate) Arc<crate::platform::Device>);

impl Device for NativeDevice {
    fn device_descriptor(&self) -> DeviceDescriptor {
        self.0.device_descriptor()
    }

    fn configuration_descriptors(&self) -> Vec<Vec<u8>> {
        self.0
            .configuration_descriptors()
            .map(|descriptor| descriptor.as_bytes().to_vec())
            .collect()
    }

    fn speed(&self) -> Option<Speed> {
        self.0.speed()
    }

    fn active_configuration_value(&self) -> u8 {
        self.0.active_configuration_value()
    }

    fn set_configuration(&self, configuration: u8) -> Result<(), Error> {
        self.0.clone().set_configuration(configuration).wait()
    }

    fn reset(&self) -> Result<(), Error> {
        self.0.clone().reset().wait()
    }

    fn claim_interface(&self, interface: u8) -> Result<Arc<dyn Interface>, Error> {
        self.0
            .clone()
            .claim_interface(interface)
            .wait()
            .map(|interface| Arc::new(NativeInterface(interface)) as Arc<dyn Interface>)
    }

    fn detach_and_claim_interface(&self, interface: u8) -> Result<Arc<dyn Interface>, Error> {
        self.0
            .clone()
            .detach_and_claim_interface(interface)
            .wait()
            .map(|interface| Arc::new(NativeInterface(interface)) as Arc<dyn Interface>)
    }

    fn detach_kernel_driver(&self, interface: u8) -> Result<(), Error> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        self.0.detach_kernel_driver(interface)?;
        let _ = interface;
        Ok(())
    }

    fn attach_kernel_driver(&self, interface: u8) -> Result<(), Error> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        self.0.attach_kernel_driver(interface)?;
        let _ = interface;
        Ok(())
    }

    fn control_in(&self, data: ControlIn, timeout: Duration) -> Result<Vec<u8>, TransferError> {
        #[cfg(not(target_os = "windows"))]
        {
            self.0.clone().control_in(data, timeout).wait()
        }
        #[cfg(target_os = "windows")]
        {
            let _ = (data, timeout);
            Err(TransferError::InvalidArgument)
        }
    }

    fn control_out(&self, data: ControlOut<'_>, timeout: Duration) -> Result<(), TransferError> {
        #[cfg(not(target_os = "windows"))]
        {
            self.0.clone().control_out(data, timeout).wait()
        }
        #[cfg(target_os = "windows")]
        {
            let _ = (data, timeout);
            Err(TransferError::InvalidArgument)
        }
    }

    fn get_descriptor(
        &self,
        desc_type: u8,
        desc_index: u8,
        language_id: u16,
        timeout: Duration,
    ) -> Result<Vec<u8>, TransferError> {
        #[cfg(target_os = "windows")]
        {
            let _ = timeout;
            self.0
                .clone()
                .get_descriptor(desc_type, desc_index, language_id)
                .wait()
        }
        #[cfg(not(target_os = "windows"))]
        {
            self.control_in(
                ControlIn {
                    control_type: ControlType::Standard,
                    recipient: Recipient::Device,
                    request: 0x06,
                    value: (u16::from(desc_type) << 8) | u16::from(desc_index),
                    index: language_id,
                    length: 4096,
                },
                timeout,
            )
        }
    }
}

struct NativeInterface(Arc<crate::platform::Interface>);

impl Interface for NativeInterface {
    fn interface_number(&self) -> u8 {
        self.0.interface_number
    }

    fn set_alt_setting(&self, alt_setting: u8) -> Result<(), Error> {
        self.0.clone().set_alt_setting(alt_setting).wait()
    }

    fn get_alt_setting(&self) -> u8 {
        self.0.get_alt_setting()
    }

    fn control_in(&self, data: ControlIn, timeout: Duration) -> Result<Vec<u8>, TransferError> {
        self.0.control_in(data, timeout).wait()
    }

    fn control_out(&self, data: ControlOut<'_>, timeout: Duration) -> Result<(), TransferError> {
        self.0.control_out(data, timeout).wait()
    }

    fn endpoint(&self, descriptor: EndpointDescriptor<'_>) -> Result<Box<dyn Endpoint>, Error> {
        self.0
            .endpoint(descriptor)
            .map(|endpoint| Box::new(NativeEndpoint(endpoint)) as Box<dyn Endpoint>)
    }

    fn release(self: Arc<Self>) -> Result<(), Error> {
        let this = Arc::into_inner(self)
            .ok_or_else(|| Error::new(ErrorKind::Busy, "interface is still in use"))?;
        this.0.release().wait()
    }
}

struct NativeEndpoint(crate::platform::Endpoint);

impl Endpoint for NativeEndpoint {
    fn endpoint_address(&self) -> u8 {
        self.0.endpoint_address()
    }

    fn max_packet_size(&self) -> usize {
        self.0.max_packet_size
    }

    fn pending(&self) -> usize {
        self.0.pending()
    }

    fn allocate(&self, len: usize) -> Result<Buffer, Error> {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            self.0.allocate(len).map_err(|error| {
                Error::new_os(
                    ErrorKind::Other,
                    "failed to allocate transfer buffer",
                    error,
                )
            })
        }
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        {
            Ok(Buffer::new(len))
        }
    }

    fn cancel_all(&mut self) {
        self.0.cancel_all();
    }

    fn submit(&mut self, buffer: Buffer) {
        self.0.submit(buffer);
    }

    fn submit_err(&mut self, buffer: Buffer, error: TransferError) {
        self.0.submit_err(buffer, error);
    }

    fn poll_next_complete(&mut self, cx: &mut Context<'_>) -> Poll<Completion> {
        self.0.poll_next_complete(cx)
    }

    fn wait_next_complete(&mut self, timeout: Duration) -> Option<Completion> {
        self.0.wait_next_complete(timeout)
    }

    fn clear_halt(&mut self) -> Result<(), Error> {
        self.0.clear_halt().wait()
    }
}

pub(crate) fn validate_configurations(configurations: &[Vec<u8>]) -> Result<(), Error> {
    if configurations
        .iter()
        .any(|bytes| ConfigurationDescriptor::new(bytes).is_none())
    {
        return Err(Error::new(
            ErrorKind::Other,
            "backend supplied an invalid configuration descriptor",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIGURATION: [u8; 9] = [9, 2, 9, 0, 0, 1, 0, 0x80, 50];

    struct DescriptorBackend {
        configurations: Vec<Vec<u8>>,
    }

    impl Device for DescriptorBackend {
        fn device_descriptor(&self) -> DeviceDescriptor {
            DeviceDescriptor::from_fields(0x0300, 0, 0, 0, 64, 0x2bc5, 0x080c, 0x0100, 0, 0, 0, 1)
        }

        fn configuration_descriptors(&self) -> Vec<Vec<u8>> {
            self.configurations.clone()
        }

        fn speed(&self) -> Option<Speed> {
            Some(Speed::Super)
        }

        fn active_configuration_value(&self) -> u8 {
            1
        }

        fn set_configuration(&self, _configuration: u8) -> Result<(), Error> {
            unreachable!("descriptor test does not perform USB operations")
        }

        fn reset(&self) -> Result<(), Error> {
            unreachable!("descriptor test does not perform USB operations")
        }

        fn claim_interface(&self, _interface: u8) -> Result<Arc<dyn Interface>, Error> {
            unreachable!("descriptor test does not perform USB operations")
        }

        fn detach_and_claim_interface(&self, _interface: u8) -> Result<Arc<dyn Interface>, Error> {
            unreachable!("descriptor test does not perform USB operations")
        }

        fn detach_kernel_driver(&self, _interface: u8) -> Result<(), Error> {
            unreachable!("descriptor test does not perform USB operations")
        }

        fn attach_kernel_driver(&self, _interface: u8) -> Result<(), Error> {
            unreachable!("descriptor test does not perform USB operations")
        }

        fn control_in(
            &self,
            _data: ControlIn,
            _timeout: Duration,
        ) -> Result<Vec<u8>, TransferError> {
            unreachable!("descriptor test does not perform USB operations")
        }

        fn control_out(
            &self,
            _data: ControlOut<'_>,
            _timeout: Duration,
        ) -> Result<(), TransferError> {
            unreachable!("descriptor test does not perform USB operations")
        }
    }

    #[test]
    fn external_device_uses_validated_cached_descriptors() {
        let backend = Arc::new(DescriptorBackend {
            configurations: vec![CONFIGURATION.to_vec()],
        });
        let device = crate::Device::from_backend(backend).unwrap();
        assert_eq!(device.device_descriptor().vendor_id(), 0x2bc5);
        assert_eq!(device.speed(), Some(Speed::Super));
        assert_eq!(
            device.active_configuration().unwrap().configuration_value(),
            1
        );
    }

    #[test]
    fn external_device_rejects_invalid_configuration_bytes() {
        let backend = Arc::new(DescriptorBackend {
            configurations: vec![vec![1, 2, 3]],
        });
        assert_eq!(
            crate::Device::from_backend(backend).unwrap_err().kind(),
            ErrorKind::Other
        );
    }
}
