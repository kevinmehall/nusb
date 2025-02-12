use nusb::{DeviceInfo, MaybeFuture};

fn main() {
    env_logger::init();
    for dev in nusb::list_devices().wait().unwrap() {
        inspect_device(dev);
    }
}

fn inspect_device(dev: DeviceInfo) {
    println!(
        "Device {:03}.{:03} ({:04x}:{:04x}) {} {}",
        dev.bus_id(),
        dev.device_address(),
        dev.vendor_id(),
        dev.product_id(),
        dev.manufacturer_string().unwrap_or(""),
        dev.product_string().unwrap_or("")
    );
    let dev = match dev.open().wait() {
        Ok(dev) => dev,
        Err(e) => {
            println!("Failed to open device: {}", e);
            return;
        }
    };

    println!("{:#?}", dev.device_descriptor());

    println!("Speed: {:?}", dev.speed());

    match dev.active_configuration() {
        Ok(config) => println!("Active configuration is {}", config.configuration_value()),
        Err(e) => println!("Unknown active configuration: {e}"),
    }

    for config in dev.configurations() {
        println!("{config:#?}");
    }
    println!("");
    println!("");
}
