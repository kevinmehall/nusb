use std::sync::Arc;

use crate::{
    platform,
    transfer::{ControlIn, ControlOut, EndpointType, Queue, RequestBuffer, TransferFuture},
    DeviceInfo, Error,
};

/// An opened USB device.
///
/// Obtain a `Device` by calling [`DeviceInfo::open`]:
///
/// ```no_run
/// use nusb;
/// let device_info = nusb::list_devices().unwrap()
///     .find(|dev| dev.vendor_id() == 0xAAAA && dev.product_id() == 0xBBBB)
///     .expect("device not connected");
///
/// let device = device_info.open().expect("failed to open device");
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
    pub(crate) fn open(d: &DeviceInfo) -> Result<Device, std::io::Error> {
        let backend = platform::Device::from_device_info(d)?;
        Ok(Device { backend })
    }

    /// Open an interface of the device and claim it for exclusive use.
    pub fn claim_interface(&self, interface: u8) -> Result<Interface, Error> {
        let backend = self.backend.claim_interface(interface)?;
        Ok(Interface { backend })
    }

    /// Set the device configuration.
    ///
    /// ### Platform-specific notes
    /// * Not supported on Windows
    pub fn set_configuration(&self, configuration: u8) -> Result<(), Error> {
        self.backend.set_configuration(configuration)
    }

    /// Reset the device, forcing it to re-enumerate.
    ///
    /// This `Device` will no longer be usable, and you should drop it and call
    /// [`super::list_devices`] to find and re-open it again.
    ///
    /// ### Platform-specific notes
    /// * Not supported on Windows
    pub fn reset(&self) -> Result<(), Error> {
        self.backend.reset()
    }
}

/// An opened interface of a USB device.
///
/// This type is reference-counted with an [`Arc`] internally, and can be cloned cheaply for
/// use in multiple places in your program. The interface is released when all clones, and all
/// associated transfers ([`TransferFuture`]s and [`Queue`]s) are dropped.
#[derive(Clone)]
pub struct Interface {
    backend: Arc<platform::Interface>,
}

impl Interface {
    /// Select the alternate setting of this interface.
    ///
    /// An alternate setting is a mode of the interface that makes particular endpoints available
    /// and may enable or disable functionality of the device. The OS resets the device to the default
    /// alternate setting when the interface is released or the program exits.
    pub fn set_alt_setting(&self, alt_setting: u8) -> Result<(), Error> {
        self.backend.set_alt_setting(alt_setting)
    }

    /// Submit a single **IN (device-to-host)** transfer on the default **control** endpoint.
    ///
    /// ### Example
    ///
    /// ```no_run
    /// use futures_lite::future::block_on;
    /// use nusb::transfer::{ ControlIn, ControlType, Recipient };
    /// # fn main() -> Result<(), std::io::Error> {
    /// # let di = nusb::list_devices().unwrap().next().unwrap();
    /// # let device = di.open().unwrap();
    /// # let interface = device.claim_interface(0).unwrap();
    ///
    /// let data: Vec<u8> = block_on(interface.control_in(ControlIn {
    ///     control_type: ControlType::Vendor,
    ///     recipient: Recipient::Device,
    ///     request: 0x30,
    ///     value: 0x0,
    ///     index: 0x0,
    ///     length: 64,
    /// })).into_result()?;
    /// # Ok(()) }
    /// ```
    pub fn control_in(&self, data: ControlIn) -> TransferFuture<ControlIn> {
        let mut t = self.backend.make_transfer(0, EndpointType::Control);
        t.submit::<ControlIn>(data);
        TransferFuture::new(t)
    }

    /// Submit a single **OUT (host-to-device)** transfer on the default **control** endpoint.
    ///
    /// ### Example
    ///
    /// ```no_run
    /// use futures_lite::future::block_on;
    /// use nusb::transfer::{ ControlOut, ControlType, Recipient };
    /// # fn main() -> Result<(), std::io::Error> {
    /// # let di = nusb::list_devices().unwrap().next().unwrap();
    /// # let device = di.open().unwrap();
    /// # let interface = device.claim_interface(0).unwrap();
    ///
    /// block_on(interface.control_out(ControlOut {
    ///     control_type: ControlType::Vendor,
    ///     recipient: Recipient::Device,
    ///     request: 0x32,
    ///     value: 0x0,
    ///     index: 0x0,
    ///     data: &[0x01, 0x02, 0x03, 0x04],
    /// })).into_result()?;
    /// # Ok(()) }
    /// ```
    pub fn control_out(&self, data: ControlOut) -> TransferFuture<ControlOut> {
        let mut t = self.backend.make_transfer(0, EndpointType::Control);
        t.submit::<ControlOut>(data);
        TransferFuture::new(t)
    }

    /// Submit a single **IN (device-to-host)** transfer on the specified **bulk** endpoint.
    ///
    /// * The requested length must be a multiple of the endpoint's maximum packet size
    /// * An IN endpoint address must have the top (`0x80`) bit set.
    pub fn bulk_in(&self, endpoint: u8, buf: RequestBuffer) -> TransferFuture<RequestBuffer> {
        let mut t = self.backend.make_transfer(endpoint, EndpointType::Bulk);
        t.submit(buf);
        TransferFuture::new(t)
    }

    /// Submit a single **OUT (host-to-device)** transfer on the specified **bulk** endpoint.
    ///
    /// * An OUT endpoint address must have the top (`0x80`) bit clear.
    pub fn bulk_out(&self, endpoint: u8, buf: Vec<u8>) -> TransferFuture<Vec<u8>> {
        let mut t = self.backend.make_transfer(endpoint, EndpointType::Bulk);
        t.submit(buf);
        TransferFuture::new(t)
    }

    /// Create a queue for managing multiple **IN (device-to-host)** transfers on a **bulk** endpoint.
    ///
    /// * An IN endpoint address must have the top (`0x80`) bit set.
    pub fn bulk_in_queue(&self, endpoint: u8) -> Queue<RequestBuffer> {
        Queue::new(self.backend.clone(), endpoint, EndpointType::Bulk)
    }

    /// Create a queue for managing multiple **OUT (device-to-host)** transfers on a **bulk** endpoint.
    ///
    /// An OUT endpoint address must have the top (`0x80`) bit clear.
    pub fn bulk_out_queue(&self, endpoint: u8) -> Queue<Vec<u8>> {
        Queue::new(self.backend.clone(), endpoint, EndpointType::Bulk)
    }

    /// Submit a single **IN (device-to-host)** transfer on the specified **interrupt** endpoint.
    ///
    /// * The requested length must be a multiple of the endpoint's maximum packet size
    /// * An IN endpoint address must have the top (`0x80`) bit set.
    pub fn interrupt_in(&self, endpoint: u8, buf: RequestBuffer) -> TransferFuture<RequestBuffer> {
        let mut t = self
            .backend
            .make_transfer(endpoint, EndpointType::Interrupt);
        t.submit(buf);
        TransferFuture::new(t)
    }

    /// Submit a single **OUT (host-to-device)** transfer on the specified **interrupt** endpoint.
    ///
    /// * An OUT endpoint address must have the top (`0x80`) bit clear.
    pub fn interrupt_out(&self, endpoint: u8, buf: Vec<u8>) -> TransferFuture<Vec<u8>> {
        let mut t = self
            .backend
            .make_transfer(endpoint, EndpointType::Interrupt);
        t.submit(buf);
        TransferFuture::new(t)
    }

    /// Create a queue for managing multiple **IN (device-to-host)** transfers on an **interrupt** endpoint.
    ///
    /// * An IN endpoint address must have the top (`0x80`) bit set.
    pub fn interrupt_in_queue(&self, endpoint: u8) -> Queue<RequestBuffer> {
        Queue::new(self.backend.clone(), endpoint, EndpointType::Interrupt)
    }

    /// Create a queue for managing multiple **OUT (device-to-host)** transfers on an **interrupt** endpoint.
    ///
    /// * An OUT endpoint address must have the top (`0x80`) bit clear.
    pub fn interrupt_out_queue(&self, endpoint: u8) -> Queue<Vec<u8>> {
        Queue::new(self.backend.clone(), endpoint, EndpointType::Interrupt)
    }
}

#[test]
fn assert_send_sync() {
    fn require_send_sync<T: Send + Sync>() {}
    require_send_sync::<Interface>();
    require_send_sync::<Device>();
}
