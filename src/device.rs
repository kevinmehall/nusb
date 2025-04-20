use crate::{
    descriptors::{
        decode_string_descriptor, validate_string_descriptor, ActiveConfigurationError,
        ConfigurationDescriptor, DeviceDescriptor, InterfaceDescriptor, DESCRIPTOR_TYPE_STRING,
    },
    platform,
    transfer::{
        Buffer, BulkOrInterrupt, ControlIn, ControlOut, Direction, EndpointDirection, EndpointType,
        TransferError,
    },
    DeviceInfo, Error, MaybeFuture, Speed,
};
use log::error;
use std::{
    future::{poll_fn, Future},
    io::ErrorKind,
    marker::PhantomData,
    num::NonZeroU8,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

/// An opened USB device.
///
/// Obtain a `Device` by calling [`DeviceInfo::open`]:
///
/// ```no_run
/// use nusb::{self, MaybeFuture};
/// let device_info = nusb::list_devices().wait().unwrap()
///     .find(|dev| dev.vendor_id() == 0xAAAA && dev.product_id() == 0xBBBB)
///     .expect("device not connected");
///
/// let device = device_info.open().wait().expect("failed to open device");
/// let interface = device.claim_interface(0);
/// ```
///
/// This type is reference-counted with an [`Arc`] internally, and can be cloned cheaply for
/// use in multiple places in your program. The device is closed when all clones and all
/// associated [`Interface`]s are dropped.
///
/// Use [`.claim_interface(i)`][`Device::claim_interface`] to open an interface to submit
/// transfers.
#[derive(Clone)]
pub struct Device {
    backend: Arc<crate::platform::Device>,
}

impl Device {
    pub(crate) fn wrap(backend: Arc<platform::Device>) -> Device {
        Device { backend }
    }

    pub(crate) fn open(
        d: &DeviceInfo,
    ) -> impl MaybeFuture<Output = Result<Device, std::io::Error>> {
        platform::Device::from_device_info(d).map(|d| d.map(Device::wrap))
    }

    /// Wraps a device that is already open.
    #[cfg(any(target_os = "android", target_os = "linux"))]
    pub fn from_fd(fd: std::os::fd::OwnedFd) -> impl MaybeFuture<Output = Result<Device, Error>> {
        platform::Device::from_fd(fd).map(|d| d.map(Device::wrap))
    }

    /// Open an interface of the device and claim it for exclusive use.
    pub fn claim_interface(
        &self,
        interface: u8,
    ) -> impl MaybeFuture<Output = Result<Interface, Error>> {
        self.backend
            .clone()
            .claim_interface(interface)
            .map(|i| i.map(Interface::wrap))
    }

    /// Detach kernel drivers and open an interface of the device and claim it for exclusive use.
    ///
    /// ### Platform notes
    /// This function can only detach kernel drivers on Linux. Calling on other platforms has
    /// the same effect as [`claim_interface`][`Device::claim_interface`].
    pub fn detach_and_claim_interface(
        &self,
        interface: u8,
    ) -> impl MaybeFuture<Output = Result<Interface, Error>> {
        self.backend
            .clone()
            .detach_and_claim_interface(interface)
            .map(|i| i.map(Interface::wrap))
    }

    /// Detach kernel drivers for the specified interface.
    ///
    /// ### Platform notes
    /// This function can only detach kernel drivers on Linux. Calling on other platforms has
    /// no effect.
    pub fn detach_kernel_driver(&self, interface: u8) -> Result<(), Error> {
        #[cfg(target_os = "linux")]
        self.backend.detach_kernel_driver(interface)?;
        let _ = interface;

        Ok(())
    }

    /// Attach kernel drivers for the specified interface.
    ///
    /// ### Platform notes
    /// This function can only attach kernel drivers on Linux. Calling on other platforms has
    /// no effect.
    pub fn attach_kernel_driver(&self, interface: u8) -> Result<(), Error> {
        #[cfg(target_os = "linux")]
        self.backend.attach_kernel_driver(interface)?;
        let _ = interface;

        Ok(())
    }

    /// Get the device descriptor.
    ///
    /// This returns cached data and does not perform IO.
    pub fn device_descriptor(&self) -> DeviceDescriptor {
        self.backend.device_descriptor()
    }

    /// Get device speed.
    pub fn speed(&self) -> Option<Speed> {
        self.backend.speed()
    }

    /// Get information about the active configuration.
    ///
    /// This returns cached data and does not perform IO. However, it can fail if the
    /// device is unconfigured, or if it can't find a configuration descriptor for
    /// the configuration reported as active by the OS.
    pub fn active_configuration(
        &self,
    ) -> Result<ConfigurationDescriptor, ActiveConfigurationError> {
        let active = self.backend.active_configuration_value();

        self.configurations()
            .find(|c| c.configuration_value() == active)
            .ok_or_else(|| ActiveConfigurationError {
                configuration_value: active,
            })
    }

    /// Get an iterator returning information about each configuration of the device.
    ///
    /// This returns cached data and does not perform IO.
    pub fn configurations(&self) -> impl Iterator<Item = ConfigurationDescriptor> {
        self.backend.configuration_descriptors()
    }

    /// Set the device configuration.
    ///
    /// The argument is the desired configuration's `bConfigurationValue`
    /// descriptor field from [`Configuration::configuration_value`] or `0` to
    /// unconfigure the device.
    ///
    /// ### Platform-specific notes
    /// * Not supported on Windows
    pub fn set_configuration(
        &self,
        configuration: u8,
    ) -> impl MaybeFuture<Output = Result<(), Error>> {
        self.backend.clone().set_configuration(configuration)
    }

    /// Request a descriptor from the device.
    ///
    /// The `language_id` should be `0` unless you are requesting a string descriptor.
    ///
    /// ### Platform-specific details
    ///
    /// * On Windows, the timeout argument is ignored, and an OS-defined timeout is used.
    /// * On Windows, this does not wake suspended devices. Reading their
    ///   descriptors will return an error.
    pub fn get_descriptor(
        &self,
        desc_type: u8,
        desc_index: u8,
        language_id: u16,
        timeout: Duration,
    ) -> impl MaybeFuture<Output = Result<Vec<u8>, Error>> {
        #[cfg(target_os = "windows")]
        {
            let _ = timeout;
            self.backend
                .clone()
                .get_descriptor(desc_type, desc_index, language_id)
        }

        #[cfg(not(target_os = "windows"))]
        {
            const STANDARD_REQUEST_GET_DESCRIPTOR: u8 = 0x06;
            use crate::transfer::{ControlType, Recipient};

            self.control_in(
                ControlIn {
                    control_type: ControlType::Standard,
                    recipient: Recipient::Device,
                    request: STANDARD_REQUEST_GET_DESCRIPTOR,
                    value: ((desc_type as u16) << 8) | desc_index as u16,
                    index: language_id,
                    length: 4096,
                },
                timeout,
            )
            .map(|r| Ok(r?))
        }
    }

    /// Request the list of supported languages for string descriptors.
    ///
    /// ### Platform-specific details
    ///
    /// See notes on [`get_descriptor`][`Self::get_descriptor`].
    pub fn get_string_descriptor_supported_languages(
        &self,
        timeout: Duration,
    ) -> impl MaybeFuture<Output = Result<impl Iterator<Item = u16>, Error>> {
        self.get_descriptor(DESCRIPTOR_TYPE_STRING, 0, 0, timeout)
            .map(move |r| {
                let data = r?;
                if !validate_string_descriptor(&data) {
                    error!("String descriptor language list read {data:?}, not a valid string descriptor");
                    return Err(Error::new(
                        ErrorKind::InvalidData,
                        "string descriptor data was invalid",
                    ));
                }

                //TODO: Use array_chunks once stable
                let mut iter = data.into_iter().skip(2);
                Ok(std::iter::from_fn(move || {
                    Some(u16::from_le_bytes([iter.next()?, iter.next()?]))
                }))
            })
    }

    /// Request a string descriptor from the device.
    ///
    /// Almost all devices support only the language ID [`US_ENGLISH`][`crate::descriptors::language_id::US_ENGLISH`].
    ///
    /// Unpaired UTF-16 surrogates will be replaced with `�`, like [`String::from_utf16_lossy`].
    ///
    /// ### Platform-specific details
    ///
    /// See notes on [`get_descriptor`][`Self::get_descriptor`].
    pub fn get_string_descriptor(
        &self,
        desc_index: NonZeroU8,
        language_id: u16,
        timeout: Duration,
    ) -> impl MaybeFuture<Output = Result<String, Error>> {
        self.get_descriptor(
            DESCRIPTOR_TYPE_STRING,
            desc_index.get(),
            language_id,
            timeout,
        )
        .map(|r| {
            let data = r?;
            decode_string_descriptor(&data).map_err(|_| {
                Error::new(ErrorKind::InvalidData, "string descriptor data was invalid")
            })
        })
    }

    /// Reset the device, forcing it to re-enumerate.
    ///
    /// This `Device` will no longer be usable, and you should drop it and call
    /// [`super::list_devices`] to find and re-open it again.
    ///
    /// ### Platform-specific notes
    /// * Not supported on Windows
    pub fn reset(&self) -> impl MaybeFuture<Output = Result<(), Error>> {
        self.backend.clone().reset()
    }

    /// Asynchronously submit a single **IN (device-to-host)** transfer on the default **control** endpoint.
    ///
    /// ### Example
    ///
    /// ```no_run
    /// use std::time::Duration;
    /// use futures_lite::future::block_on;
    /// use nusb::transfer::{ ControlIn, ControlType, Recipient };
    /// # use nusb::MaybeFuture;
    /// # fn main() -> Result<(), std::io::Error> {
    /// # let di = nusb::list_devices().wait().unwrap().next().unwrap();
    /// # let device = di.open().wait().unwrap();
    ///
    /// let data: Vec<u8> = device.control_in(ControlIn {
    ///     control_type: ControlType::Vendor,
    ///     recipient: Recipient::Device,
    ///     request: 0x30,
    ///     value: 0x0,
    ///     index: 0x0,
    ///     length: 64,
    /// }, Duration::from_millis(100)).wait()?;
    /// # Ok(()) }
    /// ```
    ///
    /// ### Platform-specific notes
    ///
    /// * Not supported on Windows. You must [claim an interface][`Device::claim_interface`]
    ///   and use the interface handle to submit transfers.
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "android"))]
    pub fn control_in(
        &self,
        data: ControlIn,
        timeout: Duration,
    ) -> impl MaybeFuture<Output = Result<Vec<u8>, TransferError>> {
        self.backend.clone().control_in(data, timeout)
    }

    /// Submit a single **OUT (host-to-device)** transfer on the default **control** endpoint.
    ///
    /// ### Example
    ///
    /// ```no_run
    /// use std::time::Duration;
    /// use futures_lite::future::block_on;
    /// use nusb::transfer::{ ControlOut, ControlType, Recipient };
    /// # use nusb::MaybeFuture;
    /// # fn main() -> Result<(), std::io::Error> {
    /// # let di = nusb::list_devices().wait().unwrap().next().unwrap();
    /// # let device = di.open().wait().unwrap();
    ///
    /// device.control_out(ControlOut {
    ///     control_type: ControlType::Vendor,
    ///     recipient: Recipient::Device,
    ///     request: 0x32,
    ///     value: 0x0,
    ///     index: 0x0,
    ///     data: &[0x01, 0x02, 0x03, 0x04],
    /// }, Duration::from_millis(100)).wait()?;
    /// # Ok(()) }
    /// ```
    ///
    /// ### Platform-specific notes
    ///
    /// * Not supported on Windows. You must [claim an interface][`Device::claim_interface`]
    ///   and use the interface handle to submit transfers.
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "android"))]
    pub fn control_out(
        &self,
        data: ControlOut,
        timeout: Duration,
    ) -> impl MaybeFuture<Output = Result<(), TransferError>> {
        self.backend.clone().control_out(data, timeout)
    }
}

/// An opened interface of a USB device.
///
/// Obtain an `Interface` with the [`Device::claim_interface`] method.
///
/// This type is reference-counted with an [`Arc`] internally, and can be cloned cheaply for
/// use in multiple places in your program. The interface is released when all clones, and all
/// associated [`TransferFuture`]s and [`Queue`]s are dropped.
#[derive(Clone)]
pub struct Interface {
    backend: Arc<platform::Interface>,
}

impl Interface {
    pub(crate) fn wrap(backend: Arc<platform::Interface>) -> Self {
        Interface { backend }
    }

    /// Select the alternate setting of this interface.
    ///
    /// An alternate setting is a mode of the interface that makes particular endpoints available
    /// and may enable or disable functionality of the device. The OS resets the device to the default
    /// alternate setting when the interface is released or the program exits.
    pub fn set_alt_setting(&self, alt_setting: u8) -> impl MaybeFuture<Output = Result<(), Error>> {
        self.backend.clone().set_alt_setting(alt_setting)
    }

    /// Get the current alternate setting of this interface.
    pub fn get_alt_setting(&self) -> u8 {
        self.backend.get_alt_setting()
    }

    /// Submit a single **IN (device-to-host)** transfer on the default **control** endpoint.
    ///
    /// ### Example
    ///
    /// ```no_run
    /// use std::time::Duration;
    /// use futures_lite::future::block_on;
    /// use nusb::transfer::{ ControlIn, ControlType, Recipient };
    /// # use nusb::MaybeFuture;
    /// # fn main() -> Result<(), std::io::Error> {
    /// # let di = nusb::list_devices().wait().unwrap().next().unwrap();
    /// # let device = di.open().wait().unwrap();
    /// # let interface = device.claim_interface(0).wait().unwrap();
    ///
    /// let data: Vec<u8> = interface.control_in(ControlIn {
    ///     control_type: ControlType::Vendor,
    ///     recipient: Recipient::Device,
    ///     request: 0x30,
    ///     value: 0x0,
    ///     index: 0x0,
    ///     length: 64,
    /// }, Duration::from_millis(100)).wait()?;
    /// # Ok(()) }
    /// ```
    ///
    /// ### Platform-specific notes
    /// * On Windows, if the `recipient` is `Interface`, the WinUSB driver sends
    ///   the interface number in the least significant byte of `index`,
    ///   overriding any value passed. A warning is logged if the passed `index`
    ///   least significant byte differs from the interface number, and this may
    ///   become an error in the future.
    pub fn control_in(
        &self,
        data: ControlIn,
        timeout: Duration,
    ) -> impl MaybeFuture<Output = Result<Vec<u8>, TransferError>> {
        self.backend.clone().control_in(data, timeout)
    }

    /// Submit a single **OUT (host-to-device)** transfer on the default **control** endpoint.
    ///
    /// ### Example
    ///
    /// ```no_run
    /// use std::time::Duration;
    /// use futures_lite::future::block_on;
    /// use nusb::transfer::{ ControlOut, ControlType, Recipient };
    /// # use nusb::MaybeFuture;
    /// # fn main() -> Result<(), std::io::Error> {
    /// # let di = nusb::list_devices().wait().unwrap().next().unwrap();
    /// # let device = di.open().wait().unwrap();
    /// # let interface = device.claim_interface(0).wait().unwrap();
    ///
    /// interface.control_out(ControlOut {
    ///     control_type: ControlType::Vendor,
    ///     recipient: Recipient::Device,
    ///     request: 0x32,
    ///     value: 0x0,
    ///     index: 0x0,
    ///     data: &[0x01, 0x02, 0x03, 0x04],
    /// }, Duration::from_millis(100)).wait()?;
    /// # Ok(()) }
    /// ```
    ///
    /// ### Platform-specific notes
    /// * On Windows, if the `recipient` is `Interface`, the WinUSB driver sends
    ///   the interface number in the least significant byte of `index`,
    ///   overriding any value passed. A warning is logged if the passed `index`
    ///   least significant byte differs from the interface number, and this may
    ///   become an error in the future.
    pub fn control_out(
        &self,
        data: ControlOut,
        timeout: Duration,
    ) -> impl MaybeFuture<Output = Result<(), TransferError>> {
        self.backend.clone().control_out(data, timeout)
    }

    /// Get the interface number.
    pub fn interface_number(&self) -> u8 {
        self.backend.interface_number
    }

    /// Get the interface descriptors for the alternate settings of this interface.
    ///
    /// This returns cached data and does not perform IO.
    pub fn descriptors(&self) -> impl Iterator<Item = InterfaceDescriptor> {
        let active = self.backend.device.active_configuration_value();

        let configuration = self
            .backend
            .device
            .configuration_descriptors()
            .find(|c| c.configuration_value() == active);

        configuration
            .into_iter()
            .flat_map(|i| i.interface_alt_settings())
            .filter(|g| g.interface_number() == self.backend.interface_number)
    }

    /// Get the interface descriptor for the current alternate setting.
    pub fn descriptor(&self) -> Option<InterfaceDescriptor> {
        self.descriptors()
            .find(|i| i.alternate_setting() == self.get_alt_setting())
    }

    /// Open an endpoint.
    pub fn endpoint<EpType: EndpointType, Dir: EndpointDirection>(
        &self,
        address: u8,
    ) -> Result<Endpoint<EpType, Dir>, ClaimEndpointError> {
        let intf_desc = self.descriptor();
        let ep_desc =
            intf_desc.and_then(|desc| desc.endpoints().find(|ep| ep.address() == address));
        let Some(ep_desc) = ep_desc else {
            return Err(ClaimEndpointError::InvalidAddress);
        };

        if ep_desc.transfer_type() != EpType::TYPE || address & Direction::MASK != Dir::DIR as u8 {
            return Err(ClaimEndpointError::InvalidType);
        }

        let backend = self.backend.endpoint(ep_desc)?;
        Ok(Endpoint {
            backend,
            ep_type: PhantomData,
            ep_dir: PhantomData,
        })
    }
}

/// Error from [`Interface::endpoint`].
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClaimEndpointError {
    /// The specified address does not exist on this interface and alternate setting
    InvalidAddress,

    /// The type or direction does not match the endpoint descriptor for this address
    InvalidType,

    /// The endpoint is already claimed
    Busy,
}

impl std::fmt::Display for ClaimEndpointError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClaimEndpointError::InvalidAddress => write!(f, "invalid endpoint address"),
            ClaimEndpointError::InvalidType => write!(f, "incorrect endpoint type or direction"),
            ClaimEndpointError::Busy => write!(f, "endpoint is already claimed"),
        }
    }
}

impl std::error::Error for ClaimEndpointError {}

/// Exclusive access to an endpoint of a USB device.
///
/// Obtain an `Endpoint` with the [`Interface::endpoint`] method.
pub struct Endpoint<EpType, Dir> {
    backend: platform::Endpoint,
    ep_type: PhantomData<EpType>,
    ep_dir: PhantomData<Dir>,
}

/// Methods for all endpoints.
impl<EpType: EndpointType, Dir: EndpointDirection> Endpoint<EpType, Dir> {
    /// Get the endpoint address.
    pub fn endpoint_address(&self) -> u8 {
        self.backend.endpoint_address()
    }

    /// Get the maximum packet size for this endpoint.
    ///
    /// Transfers can consist of multiple packets, but are split into packets
    /// of this size on the bus.
    pub fn max_packet_size(&self) -> usize {
        self.backend.max_packet_size
    }

    /// Get the number of transfers that have been submitted with `submit` that
    /// have not yet been returned from `next_complete`.
    pub fn pending(&self) -> usize {
        self.backend.pending()
    }

    /// Request cancellation of all pending transfers.
    ///
    /// The transfers are cancelled asynchronously. Once cancelled, they will be
    /// returned from calls to `next_complete` so you can tell which were
    /// completed, partially-completed, or cancelled.
    pub fn cancel_all(&mut self) {
        self.backend.cancel_all()
    }
}

/// Methods for Bulk and Interrupt endpoints.
impl<EpType: BulkOrInterrupt, Dir: EndpointDirection> Endpoint<EpType, Dir> {
    /// Allocate a buffer for use on this endpoint, zero-copy if possible.
    ///
    /// A zero-copy buffer allows the kernel to DMA directly to/from this
    /// buffer for improved performance. However, because it is not allocated
    /// with the system allocator, it cannot be converted to a `Vec` without
    /// copying.
    ///
    /// This is currently only supported on Linux, falling back to [`Buffer::new`]
    /// on other platforms, or if the memory allocation fails.
    pub fn allocate(&self, len: usize) -> Buffer {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            if let Ok(b) = self.backend.allocate(len) {
                return b;
            }
        }

        Buffer::new(len)
    }

    /// Begin a transfer on the endpoint.
    ///
    /// Submitted transfers are queued and completed in order. Once the transfer
    /// completes, it will be returned from [`Self::next_complete`]. Any error
    /// in submitting or performing the transfer is deferred until
    /// [`next_complete`][`Self::next_complete`].
    ///
    /// For an OUT transfer, the buffer's `len` field is the number of bytes
    /// initialized, which will be sent to the device.
    ///
    /// For an IN transfer, the buffer's `transfer_len` field is the number of
    /// bytes requested. It must be a multiple of the endpoint's [maximum packet
    /// size][`Self::max_packet_size`].
    pub fn submit(&mut self, buf: Buffer) {
        self.backend.submit(buf)
    }

    /// Return a `Future` that waits for the next pending transfer to complete.
    ///
    /// This future is cancel-safe: it can be cancelled and re-created without
    /// side effects, enabling its use in `select!{}` or similar.
    ///
    /// ## Panics
    /// * if there are no transfers pending (that is, if [`Self::pending()`]
    ///   would return 0).
    pub fn next_complete(&mut self) -> impl Future<Output = Completion> + Send + Sync + '_ {
        poll_fn(|cx| self.poll_next_complete(cx))
    }

    /// Poll for a pending transfer completion.
    ///
    /// Returns a completed transfer if one is available, or arranges for the
    /// context's waker to be notified when a transfer completes.
    ///
    /// ## Panics
    ///  * if there are no transfers pending (that is, if [`Self::pending()`]
    /// would return 0).
    pub fn poll_next_complete(&mut self, cx: &mut Context<'_>) -> Poll<Completion> {
        self.backend.poll_next_complete(cx)
    }

    /// Clear the endpoint's halt / stall condition.
    ///
    /// Sends a `CLEAR_FEATURE` `ENDPOINT_HALT` control transfer to tell the
    /// device to reset the endpoint's data toggle and clear the halt / stall
    /// condition, and resets the host-side data toggle.
    ///
    /// Use this after receiving
    /// [`TransferError::Stall`][crate::transfer::TransferError::Stall] to clear
    /// the error and resume use of the endpoint.
    ///
    /// This should not be called when transfers are pending on the endpoint.
    pub fn clear_halt(&mut self) -> impl MaybeFuture<Output = Result<(), Error>> {
        self.backend.clear_halt()
    }
}

/// A completed transfer returned from [`Endpoint::next_complete`].
///
/// A transfer can partially complete even in the case of failure or
/// cancellation, thus the [`actual_len`][`Self::actual_len`] may be nonzero
/// even if the [`status`][`Self::status`] is an error.
#[derive(Debug)]
pub struct Completion {
    /// The transfer buffer.
    pub data: Buffer,

    /// Status of the transfer.
    pub status: Result<(), TransferError>,
}

impl Completion {
    /// Ignore any partial completion, turning `self` into a `Result` containing
    /// either the completed buffer for a successful transfer or a
    /// `TransferError`.
    pub fn into_result(self) -> Result<Buffer, TransferError> {
        self.status.map(|()| self.data)
    }
}

#[test]
fn assert_send_sync() {
    use crate::transfer::{Bulk, In, Interrupt, Out};

    fn require_send_sync<T: Send + Sync>() {}
    require_send_sync::<Interface>();
    require_send_sync::<Device>();
    require_send_sync::<Endpoint<Bulk, In>>();
    require_send_sync::<Endpoint<Bulk, Out>>();
    require_send_sync::<Endpoint<Interrupt, In>>();
    require_send_sync::<Endpoint<Interrupt, Out>>();
}
