//! Detach the kernel driver for an FTDI device and then reattach it.

#[cfg(not(target_os = "linux"))]
fn main() {
    println!("This example is only supported on Linux.");
}

#[cfg(target_os = "linux")]
use std::{thread::sleep, time::Duration};

#[cfg(target_os = "linux")]
use nusb::MaybeFuture;

#[cfg(target_os = "linux")]
fn main() {
    env_logger::init();
    let di = nusb::list_devices()
        .wait()
        .unwrap()
        .find(|d| d.vendor_id() == 0x0403 && d.product_id() == 0x6001)
        .expect("device should be connected");

    let device = di.open().wait().unwrap();
    device.detach_kernel_driver(0).unwrap();
    sleep(Duration::from_secs(10));
    device.attach_kernel_driver(0).unwrap();
}
