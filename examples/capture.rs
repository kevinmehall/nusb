//! Capture an FTDI device on macOS and release it after its interface is dropped.
//! Run as root or with the com.apple.vm.device-access entitlement.

#[cfg(target_os = "macos")]
use nusb::MaybeFuture;

#[cfg(target_os = "macos")]
fn main() {
    env_logger::init();
    let info = nusb::list_devices()
        .wait()
        .unwrap()
        .find(|d| d.vendor_id() == 0x0403 && d.product_id() == 0x6001)
        .expect("device should be connected");
    let device = info.open_captured().wait().unwrap();
    let interface = device.claim_interface(0).wait().unwrap();
    drop(interface);
    device.release_capture().wait().unwrap();
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("Whole-device capture is supported on macOS only");
}
