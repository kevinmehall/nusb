use nusb::DeviceInfo;

fn main() {
    env_logger::init();
    for dev in nusb::list_devices().unwrap() {
        inspect_device(dev);
    }
}

fn inspect_device(dev: DeviceInfo) {
    println!(
        "Device {:03}.{:03} ({:04x}:{:04x}) {} {}",
        dev.bus_number(),
        dev.device_address(),
        dev.vendor_id(),
        dev.product_id(),
        dev.manufacturer_string().unwrap_or(""),
        dev.product_string().unwrap_or("")
    );
    let dev = match dev.open() {
        Ok(dev) => dev,
        Err(e) => {
            println!("\tFailed to open device: {}", e);
            return;
        }
    };

    for config in dev.configurations() {
        println!("  Configuration {}", config.configuration_value());
        for intf in config.interfaces() {
            for alt in intf.alt_settings() {
                println!(
                    "    Interface {}, alt setting {}",
                    alt.interface_number(),
                    alt.alternate_setting()
                );
                for ep in alt.endpoints() {
                    println!(
                        "      Endpoint {:02x} {:?} {:?}",
                        ep.address(),
                        ep.transfer_type(),
                        ep.direction()
                    );
                }
            }
        }
    }
    println!("");
}
