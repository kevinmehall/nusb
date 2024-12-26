//! Detach the kernel driver for an FTDI device and then reattach it.
use std::{thread::sleep, time::Duration};

#[pollster::main]
async fn main() {
    env_logger::init();
    let di = nusb::list_devices()
        .await
        .unwrap()
        .find(|d| d.vendor_id() == 0x0403 && d.product_id() == 0x6001)
        .expect("device should be connected");

    let device = di.open().await.unwrap();
    device.detach_kernel_driver(0).unwrap();
    sleep(Duration::from_secs(10));
    device.attach_kernel_driver(0).unwrap();
}
