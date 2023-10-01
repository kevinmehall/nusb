use futures_lite::future::block_on;
use nusb::TransferStatus;

fn main() {
    env_logger::init();
    let di = nusb::list_devices()
        .unwrap()
        .find(|d| d.vendor_id() == 0x59e3 && d.product_id() == 0x0a23)
        .expect("device should be connected");

    println!("Device info: {di:?}");

    let device = di.open().unwrap();
    let interface = device.claim_interface(0).unwrap();

    let mut queue = interface.bulk_queue(0x81);

    loop {
        while queue.pending() < 8 {
            queue.submit(Vec::with_capacity(256));
        }
        let result = block_on(queue.next_complete());
        println!("{result:?}");
        if result.status != TransferStatus::Complete {
            break;
        }
    }
}
