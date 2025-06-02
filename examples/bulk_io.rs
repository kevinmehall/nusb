use std::io::{BufRead, Read, Write};

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
    let ep_out = interface.endpoint::<Bulk, Out>(0x02).unwrap();
    let ep_in = interface.endpoint::<Bulk, In>(0x81).unwrap();

    let mut writer = nusb::io::EndpointWrite::new(ep_out, 128).with_num_transfers(8);

    writer.write_all(&[1; 16]).unwrap();
    writer.write_all(&[2; 256]).unwrap();
    writer.flush().unwrap();
    writer.write_all(&[3; 64]).unwrap();
    writer.flush_end().unwrap();

    let mut reader = nusb::io::EndpointRead::new(ep_in, 64);
    let mut buf = [0; 16];
    reader.read_exact(&mut buf).unwrap();

    let mut buf = [0; 64];
    reader.read_exact(&mut buf).unwrap();

    dbg!(reader.fill_buf().unwrap().len());
}
