use std::time::Duration;

use nusb::{
    transfer::{Bulk, In, Out},
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
    let interface = device.claim_interface(0).wait().unwrap();
    let mut ep_out = interface.endpoint::<Bulk, Out>(0x02).unwrap();
    let mut ep_in = interface.endpoint::<Bulk, In>(0x81).unwrap();
    ep_out.submit(vec![1, 2, 3, 4, 5].into());
    ep_out
        .wait_next_complete(Duration::from_millis(1000))
        .unwrap()
        .status
        .unwrap();

    loop {
        while ep_in.pending() < 8 {
            let buffer = ep_in.allocate(4096);
            ep_in.submit(buffer);
        }
        let result = ep_in
            .wait_next_complete(Duration::from_millis(1000))
            .unwrap();
        println!("{result:?}");
        if result.status.is_err() {
            break;
        }
        ep_in.submit(result.buffer);
    }
}
