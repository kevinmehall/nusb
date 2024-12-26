#[cfg(not(target_arch = "wasm32"))]
use nusb::transfer::{Control, ControlType, Recipient};
#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

#[pollster::main]
async fn main() {
    env_logger::init();
    let di = nusb::list_devices()
        .await
        .unwrap()
        .find(|d| d.vendor_id() == 0x59e3 && d.product_id() == 0x0a23)
        .expect("device should be connected");

    println!("Device info: {di:?}");

    #[cfg(not(target_arch = "wasm32"))]
    let device = di.open().await.unwrap();

    // Linux can make control transfers without claiming an interface
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let result = device.control_out_blocking(
            Control {
                control_type: ControlType::Vendor,
                recipient: Recipient::Device,
                request: 0x81,
                value: 0x9999,
                index: 0x9999,
            },
            &[1, 2, 3, 4],
            Duration::from_secs(1),
        );
        println!("{result:?}");

        let mut buf = [0; 64];

        let len = device
            .control_in_blocking(
                Control {
                    control_type: ControlType::Vendor,
                    recipient: Recipient::Device,
                    request: 0x81,
                    value: 0x9999,
                    index: 0x9999,
                },
                &mut buf,
                Duration::from_secs(1),
            )
            .unwrap();

        println!("{result:?}, {data:?}", data = &buf[..len]);
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        // but we also provide an API on the `Interface` to support Windows
        let interface = device.claim_interface(0).await.unwrap();

        let result = interface.control_out_blocking(
            Control {
                control_type: ControlType::Vendor,
                recipient: Recipient::Device,
                request: 0x81,
                value: 0x9999,
                index: 0x9999,
            },
            &[1, 2, 3, 4, 5],
            Duration::from_secs(1),
        );
        println!("{result:?}");

        let mut buf = [0; 64];

        let len = interface
            .control_in_blocking(
                Control {
                    control_type: ControlType::Vendor,
                    recipient: Recipient::Device,
                    request: 0x81,
                    value: 0x9999,
                    index: 0x9999,
                },
                &mut buf,
                Duration::from_secs(1),
            )
            .unwrap();
        println!("{data:?}", data = &buf[..len]);
    }
}
