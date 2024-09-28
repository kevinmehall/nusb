fn main() {
    env_logger::init();
    for dev in nusb::list_buses().unwrap() {
        println!("{:#?}", dev);
    }
}
