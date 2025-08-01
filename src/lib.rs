#![warn(missing_docs)]
//! A new library for cross-platform low-level access to USB devices.
//!
//! `nusb` supports Windows, macOS, and Linux, and provides both async and
//!  blocking APIs for listing and watching USB devices, reading descriptor
//!  details, opening and managing devices and interfaces, and performing
//!  transfers on control, bulk, and interrupt endpoints.
//!
//! `nusb` is comparable to the C library [libusb] and its Rust bindings [rusb],
//! but written in pure Rust.
//!
//! [libusb]: https://libusb.info
//! [rusb]: https://docs.rs/rusb/
//!
//! Use `nusb` to write user-space drivers in Rust for non-standard USB devices
//! or those without kernel support. For devices implementing a standard USB
//! class such as Mass Storage, CDC (Serial), HID, Audio, or Video, this is
//! probably not the library you're looking for -- use something built on the
//! existing kernel driver instead. (On some platforms you could detach or
//! replace the kernel driver and program the device from user-space using this
//! library, but you'd have to re-implement the class functionality yourself.)
//!
//! ## Example usage
//!
//! ```no_run
//! use nusb::{list_devices, MaybeFuture};
//! use nusb::transfer::{Bulk, In, Out, ControlOut, ControlType, Recipient};
//! use std::io::{Read, Write, Error, ErrorKind};
//! use std::time::Duration;
//!
//! # fn main() -> Result<(), std::io::Error> {
//! let device = list_devices().wait()?
//!     .find(|dev| dev.vendor_id() == 0xAAAA && dev.product_id() == 0xBBBB)
//!     .ok_or(Error::new(ErrorKind::NotFound, "device not found"))?;
//!
//! let device = device.open().wait()?;
//! let interface = device.claim_interface(0).wait()?;
//!
//! interface.control_out(ControlOut {
//!     control_type: ControlType::Vendor,
//!     recipient: Recipient::Device,
//!     request: 0x10,
//!     value: 0x0,
//!     index: 0x0,
//!     data: &[0x01, 0x02, 0x03, 0x04],
//! }, Duration::from_millis(100)).wait()?;
//!
//! let mut writer = interface.endpoint::<Bulk, Out>(0x01)?.writer(4096);
//! writer.write_all(&[0x00, 0xff])?;
//! writer.flush()?;
//!
//! let mut reader = interface.endpoint::<Bulk, In>(0x81)?.reader(4096);
//! let mut buf = [0; 64];
//! reader.read_exact(&mut buf)?;
//! # Ok(()) }
//! ```
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
//! USB devices consist of one or more interfaces. A device with multiple
//! interfaces is known as a composite device. To open an interface, call
//! [`Device::claim_interface`]. Only one program (or kernel driver) may claim
//! an interface at a time.
//!
//! Use the resulting [`Interface`] to perform control transfers or open
//! an [`Endpoint`] to perform bulk or interrupt transfers. Submitting a
//! transfer is a non-blocking operation that adds the transfer to an
//! internal queue for the endpoint. Completed transfers can be popped
//! from the queue synchronously or asynchronously.
//!
//! The [`EndpointRead`][io::EndpointRead] and
//! [`EndpointWrite`][io::EndpointWrite] types wrap the endpoint and
//! manage transfers and buffers to implement the standard `Read` and
//! `Write` traits and their async equivalents.
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
//! Users have access to USB devices by default, with no permission
//! configuration needed. Devices with a kernel driver are not accessible.
//!
//! ### Android
//!
//! `nusb` uses the Android SDK API for device listing and runtime permission
//! request; the rest operations are the same as Linux.
//!
//! The Android application must have the `android.hardware.usb.host` feature;
//! for permission issues, see [DeviceInfo::open] and `check_startup_intent`.
//!
//! Please make sure the [ndk-context] is configured correctly, unless you have a
//! native activity application based on [android-activity] or a similar glue crate.
//!
//! Note that a few fields are not available in [DeviceInfo].
//!
//! [android-activity]: https://docs.rs/android-activity
//! [ndk-context]: https://docs.rs/ndk-context
//!
//! ## Async support
//!
//! Many methods in `nusb` return a [`MaybeFuture`] type, which can either be
//! `.await`ed (via `IntoFuture`) or `.wait()`ed (blocking the current thread).
//! This allows for async usage in an async context, or blocking usage in a
//! non-async context.
//!
//! Operations such as [`list_devices`], [`list_buses`], [`DeviceInfo::open`],
//! [`Device::set_configuration`], [`Device::reset`],
//! [`Device::claim_interface`], [`Interface::set_alt_setting`], and
//! [`Endpoint::clear_halt`] require blocking system calls. To use these in an
//! asynchronous context, `nusb` requires an async runtime to run these
//! operations on an IO thread to avoid blocking in async code. Enable the cargo
//! feature `tokio` or `smol` to use the corresponding runtime for blocking IO.
//! If neither feature is enabled, `.await` on these methods will panic.
//!
//! For blocking usage, `.wait()` always runs the blocking operation directly
//! without the overhead of handing off to an IO thread.
//!
//! These features do not affect and are not required for transfers, which are
//! implemented on top of natively-async OS APIs.

mod platform;

pub mod descriptors;
mod enumeration;
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub use enumeration::BusInfo;
pub use enumeration::{DeviceId, DeviceInfo, InterfaceInfo, Speed, UsbControllerType};

mod device;
pub use device::{Device, Endpoint, Interface};

pub mod transfer;

#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "windows",
    target_os = "android"
))]
pub mod hotplug;

mod maybe_future;
pub use maybe_future::MaybeFuture;

mod bitset;

pub mod io;

mod error;
pub use error::{ActiveConfigurationError, Error, ErrorKind, GetDescriptorError};

#[cfg(target_os = "android")]
pub use platform::{check_startup_intent, PermissionRequest};

/// Get an iterator listing the connected devices.
///
/// ### Example
///
/// ```no_run
/// use nusb::{self, MaybeFuture};
/// let device = nusb::list_devices().wait().unwrap()
///     .find(|dev| dev.vendor_id() == 0xAAAA && dev.product_id() == 0xBBBB)
///     .expect("device not connected");
/// ```
#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "windows",
    target_os = "android"
))]
pub fn list_devices() -> impl MaybeFuture<Output = Result<impl Iterator<Item = DeviceInfo>, Error>>
{
    platform::list_devices()
}

/// Get an iterator listing the system USB buses.
///
/// ### Example
///
/// Group devices by bus:
///
/// ```no_run
/// use std::collections::HashMap;
/// use nusb::MaybeFuture;
///
/// let devices = nusb::list_devices().wait().unwrap().collect::<Vec<_>>();
/// let buses: HashMap<_, _> = nusb::list_buses().wait().unwrap()
///     .map(|bus| {
///         let bus_id = bus.bus_id().to_owned();
///         let devs: Vec<_> = devices.iter().filter(|dev| dev.bus_id() == bus_id).cloned().collect();
///         (bus_id, (bus, devs))
///     })
///     .collect();
/// ```
///
/// ### Platform-specific notes:
///
///   * On Android, this is currently unavailable.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub fn list_buses() -> impl MaybeFuture<Output = Result<impl Iterator<Item = BusInfo>, Error>> {
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
/// use std::collections::HashMap;
/// use nusb::{MaybeFuture, DeviceInfo, DeviceId, hotplug::HotplugEvent};
/// let watch = nusb::watch_devices().unwrap();
/// let mut devices: HashMap<DeviceId, DeviceInfo> = nusb::list_devices().wait().unwrap()
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
/// ```
///
/// ### Platform-specific notes:
///
///   * On Windows, the interfaces of a composite device might not be ready
///     when the `Connected` event is emitted. If you are immediately opening the device
///     and claiming an interface when receiving a `Connected` event,
///     you should retry after a short delay if opening or claiming fails.
#[cfg(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "windows",
    target_os = "android"
))]
pub fn watch_devices() -> Result<hotplug::HotplugWatch, Error> {
    Ok(hotplug::HotplugWatch(platform::HotplugWatch::new()?))
}
