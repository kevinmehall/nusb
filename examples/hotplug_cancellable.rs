use futures_lite::stream;
use std::thread;

fn main() {
    env_logger::init();

    let (stream_it, cancellation) = nusb::watch_devices_cancellable().unwrap();

    ctrlc::set_handler(move || {
        println!("ctrl-c, cancel hotplug");
        cancellation.cancel();
    })
    .expect("Error setting Ctrl-C handler");

    let join_handle = thread::spawn(move || {
        for event in stream::block_on(stream_it) {
            println!("{:#?}", event);
        }
        println!("exit thread");
    });

    join_handle.join().unwrap();

    println!("main exit");
}
