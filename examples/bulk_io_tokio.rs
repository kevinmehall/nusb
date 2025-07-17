use std::time::Duration;

use nusb::transfer::{Bulk, In, Out};

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

#[tokio::main]
async fn main() {
    env_logger::init();
    let di = nusb::list_devices()
        .await
        .unwrap()
        .find(|d| d.vendor_id() == 0x59e3 && d.product_id() == 0x00aa)
        .expect("device should be connected");

    println!("Device info: {di:?}");

    let device = di.open().await.unwrap();

    let main_interface = device.claim_interface(0).await.unwrap();

    let mut writer = main_interface
        .endpoint::<Bulk, Out>(0x03)
        .unwrap()
        .writer(128)
        .with_num_transfers(8);

    let mut reader = main_interface
        .endpoint::<Bulk, In>(0x83)
        .unwrap()
        .reader(128)
        .with_num_transfers(8);

    writer.write_all(&[1; 16]).await.unwrap();
    writer.write_all(&[2; 256]).await.unwrap();
    writer.flush().await.unwrap();
    writer.write_all(&[3; 64]).await.unwrap();
    writer.flush_end_async().await.unwrap();

    let mut buf = [0; 16];
    reader.read_exact(&mut buf).await.unwrap();

    let mut buf = [0; 64];
    reader.read_exact(&mut buf).await.unwrap();

    dbg!(reader.fill_buf().await.unwrap().len());

    let mut buf = [0; 1000];
    for len in 0..1000 {
        reader.read_exact(&mut buf[..len]).await.unwrap();
        writer.write_all(&buf[..len]).await.unwrap();
    }

    reader.cancel_all();
    loop {
        let n = reader.read(&mut buf).await.unwrap();
        dbg!(n);
        if n == 0 {
            break;
        }
    }

    let echo_interface = device.claim_interface(1).await.unwrap();
    echo_interface.set_alt_setting(1).await.unwrap();

    let mut writer = echo_interface
        .endpoint::<Bulk, Out>(0x01)
        .unwrap()
        .writer(64)
        .with_num_transfers(1);
    let mut reader = echo_interface
        .endpoint::<Bulk, In>(0x81)
        .unwrap()
        .reader(64)
        .with_num_transfers(8)
        .with_read_timeout(Duration::from_millis(100));

    let mut pkt_reader = reader.until_short_packet();

    writer.write_all(&[1; 16]).await.unwrap();
    writer.flush_end_async().await.unwrap();

    writer.write_all(&[2; 128]).await.unwrap();
    writer.flush_end_async().await.unwrap();

    let mut v = Vec::new();
    pkt_reader.read_to_end(&mut v).await.unwrap();
    assert_eq!(&v[..], &[1; 16]);
    pkt_reader.consume_end().unwrap();

    let mut v = Vec::new();
    pkt_reader.read_to_end(&mut v).await.unwrap();
    assert_eq!(&v[..], &[2; 128]);
    pkt_reader.consume_end().unwrap();
}
