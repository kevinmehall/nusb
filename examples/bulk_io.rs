use std::{
    io::{BufRead, Read, Write},
    time::Duration,
};

use nusb::{
    transfer::{Bulk, In, Out},
    MaybeFuture,
};

fn main() {
    env_logger::init();
    let di = nusb::list_devices()
        .wait()
        .unwrap()
        .find(|d| d.vendor_id() == 0x59e3 && d.product_id() == 0x00aa)
        .expect("device should be connected");

    println!("Device info: {di:?}");

    let device = di.open().wait().unwrap();

    let main_interface = device.claim_interface(0).wait().unwrap();

    let mut writer = main_interface
        .endpoint::<Bulk, Out>(0x03)
        .unwrap()
        .writer(128)
        .with_num_transfers(8);

    let mut reader = main_interface
        .endpoint::<Bulk, In>(0x83)
        .unwrap()
        .reader(128);

    writer.write_all(&[1; 16]).unwrap();
    writer.write_all(&[2; 256]).unwrap();
    writer.flush().unwrap();
    writer.write_all(&[3; 64]).unwrap();
    writer.flush_end().unwrap();

    let mut buf = [0; 16];
    reader.read_exact(&mut buf).unwrap();

    let mut buf = [0; 64];
    reader.read_exact(&mut buf).unwrap();

    dbg!(reader.fill_buf().unwrap().len());

    let echo_interface = device.claim_interface(1).wait().unwrap();
    echo_interface.set_alt_setting(1).wait().unwrap();

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

    assert_eq!(
        reader.fill_buf().unwrap_err().kind(),
        std::io::ErrorKind::TimedOut
    );

    let mut pkt_reader = reader.until_short_packet();

    writer.write_all(&[1; 16]).unwrap();
    writer.flush_end().unwrap();

    writer.write_all(&[2; 128]).unwrap();
    writer.flush_end().unwrap();

    let mut v = Vec::new();
    pkt_reader.read_to_end(&mut v).unwrap();
    assert_eq!(&v[..], &[1; 16]);
    pkt_reader.consume_end().unwrap();

    let mut v = Vec::new();
    pkt_reader.read_to_end(&mut v).unwrap();
    assert_eq!(&v[..], &[2; 128]);
    pkt_reader.consume_end().unwrap();
}
