#[cfg(any(target_os = "linux", target_os = "android"))]
mod linux_usbfs;

#[cfg(any(target_os = "linux", target_os = "android"))]
pub use linux_usbfs::*;

#[cfg(target_os = "windows")]
mod windows_winusb;

#[cfg(target_os = "windows")]
pub use windows_winusb::*;

#[cfg(target_os = "macos")]
mod macos_iokit;

#[cfg(target_os = "macos")]
pub use macos_iokit::*;

#[cfg(target_family = "wasm")]
mod webusb;

#[cfg(target_family = "wasm")]
pub use webusb::*;
