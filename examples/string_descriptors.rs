use std::time::Duration;

use nusb::{descriptors::language_id::US_ENGLISH, DeviceInfo, MaybeFuture};

fn main() {
    env_logger::init();
    for dev in nusb::list_devices().wait().unwrap() {
        inspect_device(dev);
    }
}

fn inspect_device(dev: DeviceInfo) {
    println!(
        "Device {}.{:03} ({:04x}:{:04x}) {} {}",
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

    let timeout = Duration::from_millis(100);

    let dev_descriptor = dev.device_descriptor();

    let languages: Vec<u16> = dev
        .get_string_descriptor_supported_languages(timeout)
        .wait()
        .map(|i| i.collect())
        .unwrap_or_default();
    println!("  Languages: {languages:02x?}");

    let language = languages.first().copied().unwrap_or(US_ENGLISH);

    if let Some(i_manufacturer) = dev_descriptor.manufacturer_string_index() {
        let s = dev
            .get_string_descriptor(i_manufacturer, language, timeout)
            .wait();
        println!("  Manufacturer({i_manufacturer}): {s:?}");
    }

    if let Some(i_product) = dev_descriptor.product_string_index() {
        let s = dev
            .get_string_descriptor(i_product, language, timeout)
            .wait();
        println!("  Product({i_product}): {s:?}");
    }

    if let Some(i_serial) = dev_descriptor.serial_number_string_index() {
        let s = dev
            .get_string_descriptor(i_serial, language, timeout)
            .wait();
        println!("  Serial({i_serial}): {s:?}");
    }

    println!();
}
