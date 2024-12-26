use nusb::transfer::RequestBuffer;

#[pollster::main]
async fn main() {
    env_logger::init();
    let di = nusb::list_devices()
        .await
        .unwrap()
        .find(|d| d.vendor_id() == 0x59e3 && d.product_id() == 0x0a23)
        .expect("device should be connected");

    println!("Device info: {di:?}");

    let device = di.open().await.unwrap();
    let interface = device.claim_interface(0).await.unwrap();

    interface
        .bulk_out(0x02, Vec::from([1, 2, 3, 4, 5]))
        .await
        .into_result()
        .unwrap();

    let mut queue = interface.bulk_in_queue(0x81);

    loop {
        while queue.pending() < 8 {
            queue.submit(RequestBuffer::new(256));
        }
        let result = queue.next_complete().await;
        println!("{result:?}");
        if result.status.is_err() {
            break;
        }
    }
}
