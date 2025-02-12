//! Detach the kernel driver for an FTDI device, claim the USB interface, and
//! then reattach it.
use std::{thread::sleep, time::Duration};

use nusb::MaybeFuture;
fn main() {
    env_logger::init();
    let di = nusb::list_devices()
        .wait()
        .unwrap()
        .find(|d| d.vendor_id() == 0x0403 && d.product_id() == 0x6010)
        .expect("device should be connected");

    let device = di.open().wait().unwrap();
    let interface = device.detach_and_claim_interface(0).wait().unwrap();
    sleep(Duration::from_secs(1));
    drop(interface);
}
