use nusb::IoAction;

fn main() {
    env_logger::init();
    for dev in nusb::list_buses().wait().unwrap() {
        println!("{:#?}", dev);
    }
}
