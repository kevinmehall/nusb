fn main() {
    env_logger::init();
    for dev in nusb::list_devices().unwrap() {
        println!("{:#?}", dev);
    }
}
