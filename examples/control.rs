use std::time::Duration;

use nusb::{
    transfer::{ControlIn, ControlOut, ControlType, Recipient},
    MaybeFuture,
};
fn main() {
    env_logger::init();
    let di = nusb::list_devices()
        .wait()
        .unwrap()
        .find(|d| d.vendor_id() == 0x59e3 && d.product_id() == 0x0a23)
        .expect("device should be connected");

    println!("Device info: {di:?}");

    let device = di.open().wait().unwrap();

    // Linux can make control transfers without claiming an interface
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let result = device
            .control_out(
                ControlOut {
                    control_type: ControlType::Vendor,
                    recipient: Recipient::Device,
                    request: 0x81,
                    value: 0x9999,
                    index: 0x9999,
                    data: &[1, 2, 3, 4],
                },
                Duration::from_millis(100),
            )
            .wait();
        println!("{result:?}");

        let result = device
            .control_in(
                ControlIn {
                    control_type: ControlType::Vendor,
                    recipient: Recipient::Device,
                    request: 0x81,
                    value: 0x9999,
                    index: 0x9999,
                    length: 256,
                },
                Duration::from_millis(100),
            )
            .wait();
        println!("{result:?}");
    }

    // but we also provide an API on the `Interface` to support Windows
    let interface = device.claim_interface(0).wait().unwrap();

    let result = interface
        .control_out(
            ControlOut {
                control_type: ControlType::Vendor,
                recipient: Recipient::Device,
                request: 0x81,
                value: 0x9999,
                index: 0x9999,
                data: &[1, 2, 3, 4],
            },
            Duration::from_millis(100),
        )
        .wait();
    println!("{result:?}");

    let result = interface
        .control_in(
            ControlIn {
                control_type: ControlType::Vendor,
                recipient: Recipient::Device,
                request: 0x81,
                value: 0x9999,
                index: 0x9999,
                length: 256,
            },
            Duration::from_millis(100),
        )
        .wait();
    println!("{result:?}");
}
