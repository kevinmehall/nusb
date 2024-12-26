#[pollster::main]
async fn main() {
    env_logger::init();
    for dev in nusb::list_devices().await.unwrap() {
        println!("{:#?}", dev);
    }
}
