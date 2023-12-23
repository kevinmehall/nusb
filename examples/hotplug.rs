use futures_lite::stream;

fn main() {
    env_logger::init();
    for event in stream::block_on(nusb::watch_devices().unwrap()) {
        println!("{:#?}", event);
    }
}
