use nusb::MaybeFuture;

fn main() {
    env_logger::init();
    for dev in nusb::list_buses().wait().unwrap() {
        println!("{:#?}", dev);
    }
}
