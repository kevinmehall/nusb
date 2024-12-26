#![warn(missing_docs)]
//! A new library for cross-platform low-level access to USB devices.
//!
//! `nusb` is comparable to the C library [libusb] and its Rust bindings [rusb],
//! but written in pure Rust. It's built on and exposes async APIs by default,
//! but can be made blocking using [`futures_lite::future::block_on`][block_on]
//! or similar.
//!
//! [libusb]: https://libusb.info
//! [rusb]: https://docs.rs/rusb/
//! [block_on]: https://docs.rs/futures-lite/latest/futures_lite/future/fn.block_on.html
//!
//! Use `nusb` to write user-space drivers in Rust for non-standard USB devices
//! or those without kernel support. For devices implementing a standard USB
//! class such as Mass Storage, CDC (Serial), HID, Audio, or Video, this is
//! probably not the library you're looking for -- use something built on the
//! existing kernel driver instead. (On some platforms you could detach or
//! replace the kernel driver and program the device from user-space using this
//! library, but you'd have to re-implement the class functionality yourself.)
//!
//! ## USB and usage overview
//!
//! When a USB device connects, the OS queries the device descriptor containing
//! basic device information such as the vendor and product ID (VID / PID) and
//! string descriptors like the manufacturer, product, and serial number
//! strings. [`list_devices`] returns an iterator listing connected USB devices,
//! which can be filtered by these fields to identify and select the desired
//! device.
//!
//! Call [`device_info.open()`](`DeviceInfo::open`) to open a selected device.
//! Additional information about the device can be queried with
//! [`device.active_configuration()`](`Device::active_configuration`).
//!
//! USB devices consist of one or more interfaces exposing a group of
//! functionality. A device with multiple interfaces is known as a composite
//! device. To open an interface, call [`Device::claim_interface`]. Only one
//! program (or kernel driver) may claim an interface at a time.
//!
//! Use the resulting [`Interface`] to transfer data on the device's control,
//! bulk or interrupt endpoints. Transfers are async by default, and can be
//! awaited as individual [`Future`][`transfer::TransferFuture`]s, or use a
//! [`Queue`][`transfer::Queue`] to manage streams of data.
//!
//! *For more details on how USB works, [USB in a
//! Nutshell](https://beyondlogic.org/usbnutshell/usb1.shtml) is a good
//! overview.*
//!
//! ## Logging
//!
//! `nusb` uses the [`log`](https://docs.rs/log) crate to log debug and error
//! information.
//!
//! When [submitting a bug report][gh-issues], please include the logs: use a
//! `log` backend like [`env_logger`](https://docs.rs/env_logger) and configure
//! it to enable log output for this crate (for `env_logger` set environment
//! variable `RUST_LOG=nusb=debug`.)
//!
//! [gh-issues]: https://github.com/kevinmehall/nusb/issues
//!
//! ## Platform support
//!
//! ### Linux
//!
//! `nusb` is built on the kernel's [usbfs] API.
//!
//! A user must have write access on the `/dev/bus/usb/XXX/YYY` nodes to
//! successfully open a device. Use [udev rules] to configure these permissions.
//!
//! For a single-user system used for development, it may be desirable to give
//! your user account access to all USB devices by placing the following in
//! `/etc/udev/rules.d/70-plugdev-usb.rules`:
//!
//! ```not_rust
//! SUBSYSTEM=="usb", MODE="0660", GROUP="plugdev"
//! ```
//!
//! This grants access on all USB devices to the `plugdev` group, which your
//! user may be a member of by default on Debian/Ubuntu-based distros. If you
//! are developing an app for others to install, you should scope the
//! permissions more narrowly using the `ATTRS{idVendor}=="ZZZZ",
//! ATTRS{idProduct}=="ZZZZ"` filters to only apply to your device.
//!
//! [usbfs]:
//!     https://www.kernel.org/doc/html/latest/driver-api/usb/usb.html#the-usb-character-device-nodes
//! [udev rules]: https://www.reactivated.net/writing_udev_rules.html
//!
//! ### Windows
//!
//! `nusb` uses [WinUSB] on Windows.
//!
//! On Windows, devices are associated with a particular driver, which persists
//! across connections and reboots. Composite devices appear as multiple devices
//! in the Windows device model, and each interface can be associated with a
//! separate driver.
//!
//! To use `nusb`, your device or interface must be associated with the `WinUSB`
//! driver. If you control the device firmware, the recommended way is to use a
//! [WCID] descriptor to tell Windows to install the WinUSB driver automatically
//! when the device is first connected. Alternatively [Zadig] (GUI) or [libwdi]
//! (CLI / C library) can be used to manually install the WinUSB driver for a
//! device.
//!
//! [SetupAPI]:
//!     https://learn.microsoft.com/en-us/windows-hardware/drivers/install/setupapi
//! [WinUSB]: https://learn.microsoft.com/en-us/windows/win32/api/winusb/
//! [WCID]: https://github.com/pbatard/libwdi/wiki/WCID-Devices
//! [Zadig]:https://zadig.akeo.ie/
//! [libwdi]: https://github.com/pbatard/libwdi
//!
//! ### macOS
//!
//! `nusb` uses IOKit on macOS.
//!
//! Users have access to USB devices by default, with no permission configuration needed.
//! Devices with a kernel driver are not accessible.

use std::io;

mod platform;

pub mod descriptors;
mod enumeration;
pub use enumeration::{BusInfo, DeviceId, DeviceInfo, InterfaceInfo, Speed, UsbControllerType};

mod device;
pub use device::{Device, Interface};

pub mod transfer;

pub mod hotplug;

mod maybe;

/// OS error returned from operations other than transfers.
pub type Error = io::Error;

/// Get an iterator listing the connected devices.
///
/// ### Example
///
/// ```no_run
/// # #[pollster::main]
/// # async fn main() {
/// use nusb;
/// let device = nusb::list_devices().await.unwrap()
///     .find(|dev| dev.vendor_id() == 0xAAAA && dev.product_id() == 0xBBBB)
///     .expect("device not connected");
/// # }
/// ```
pub async fn list_devices() -> Result<impl Iterator<Item = DeviceInfo>, Error> {
    platform::list_devices().await
}

/// Get an iterator listing the system USB buses.
///
/// ### Example
///
/// Group devices by bus:
///
/// ```no_run
/// # #[pollster::main]
/// # async fn main() {
/// use std::collections::HashMap;
///
/// let devices = nusb::list_devices().await.unwrap().collect::<Vec<_>>();
/// let buses: HashMap<String, (nusb::BusInfo, Vec::<nusb::DeviceInfo>)> = nusb::list_buses().unwrap()
///     .map(|bus| {
///         let bus_id = bus.bus_id().to_owned();
///         (bus.bus_id().to_owned(), (bus, devices.clone().into_iter().filter(|dev| dev.bus_id() == bus_id).collect()))
///     })
///     .collect();
/// # }
/// ```
///
/// ### Platform-specific notes
/// * On Linux, the abstraction of the "bus" is a phony device known as the root hub. This device is available at bus.root_hub()
/// * On Android, this will only work on rooted devices due to sysfs path usage
pub fn list_buses() -> Result<impl Iterator<Item = BusInfo>, Error> {
    platform::list_buses()
}

/// Get a [`Stream`][`futures_core::Stream`] that yields an
/// [event][`hotplug::HotplugEvent`] when a USB device is connected or
/// disconnected from the system.
///
/// Events will be returned for devices connected or disconnected beginning at
/// the time this function is called. To maintain a list of connected devices,
/// call [`list_devices`] after creating the watch with this function to avoid
/// potentially missing a newly-attached device:
///
/// ## Example
///
/// ```no_run
/// # #[pollster::main]
/// # async fn main() {
/// use std::collections::HashMap;
/// use nusb::{DeviceInfo, DeviceId, hotplug::HotplugEvent};
/// let watch = nusb::watch_devices().unwrap();
/// let mut devices: HashMap<DeviceId, DeviceInfo> = nusb::list_devices().await.unwrap()
///     .map(|d| (d.id(), d)).collect();
/// for event in futures_lite::stream::block_on(watch) {
///     match event {
///         HotplugEvent::Connected(d) => {
///             devices.insert(d.id(), d);
///         }
///         HotplugEvent::Disconnected(id) => {
///             devices.remove(&id);
///         }
///     }
/// }
/// # }
/// ```
///
/// ### Platform-specific notes:
///
///   * On Windows, the interfaces of a composite device might not be ready
///     when the `Connected` event is emitted. If you are immediately opening the device
///     and claiming an interface when receiving a `Connected` event,
///     you should retry after a short delay if opening or claiming fails.
pub fn watch_devices() -> Result<hotplug::HotplugWatch, Error> {
    Ok(hotplug::HotplugWatch(platform::HotplugWatch::new()?))
}
