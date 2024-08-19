fn main() {
    env_logger::init();
    for dev in nusb::list_root_hubs().unwrap() {
        println!("{:#?}", dev);
    }
}
