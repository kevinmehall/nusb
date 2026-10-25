#[cfg(not(target_os = "android"))]
fn main() {
    use nusb::MaybeFuture;
    env_logger::init();
    for dev in nusb::list_buses().wait().unwrap() {
        println!("{:#?}", dev);
    }
}

#[cfg(target_os = "android")]
fn main() {
    println!("`nusb::list_buses()` is currently unsupported on Android.");
}
