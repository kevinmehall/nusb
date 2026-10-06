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

#[cfg(target_arch = "wasm32")]
mod webusb;

#[cfg(target_arch = "wasm32")]
pub use webusb::*;

/// String descriptors are always cached on these platforms
#[cfg(not(target_os = "windows"))]
#[derive(Debug, Clone)]
pub struct StringDescriptorRef(std::convert::Infallible);

#[cfg(not(target_os = "windows"))]
impl StringDescriptorRef {
    pub fn fetch(
        &self,
    ) -> impl crate::MaybeFuture<Output = Result<Option<String>, crate::Error>> + Unpin {
        #[expect(unreachable_code)]
        crate::maybe_future::Ready(match self.0 {})
    }
}
