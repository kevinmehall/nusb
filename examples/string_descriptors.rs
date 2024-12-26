use std::time::Duration;

use nusb::{descriptors::language_id::US_ENGLISH, DeviceInfo};

#[pollster::main]
async fn main() {
    env_logger::init();
    for dev in nusb::list_devices().await.unwrap() {
        inspect_device(dev).await;
    }
}

async fn inspect_device(dev: DeviceInfo) {
    println!(
        "Device {:03}.{:03} ({:04x}:{:04x}) {} {}",
        dev.bus_id(),
        dev.device_address(),
        dev.vendor_id(),
        dev.product_id(),
        dev.manufacturer_string().unwrap_or(""),
        dev.product_string().unwrap_or("")
    );
    let dev = match dev.open().await {
        Ok(dev) => dev,
        Err(e) => {
            println!("Failed to open device: {}", e);
            return;
        }
    };

    let timeout = Duration::from_millis(100);

    let dev_descriptor = dev.get_descriptor(0x01, 0, 0, timeout).await.unwrap();
    if dev_descriptor.len() < 18
        || dev_descriptor[0] as usize > dev_descriptor.len()
        || dev_descriptor[1] != 0x01
    {
        println!("  Invalid device descriptor: {dev_descriptor:?}");
        return;
    }

    let languages: Vec<u16> = dev
        .get_string_descriptor_supported_languages(timeout)
        .await
        .map(|i| i.collect())
        .unwrap_or_default();
    println!("  Languages: {languages:02x?}");

    let language = languages.first().copied().unwrap_or(US_ENGLISH);

    let i_manufacturer = dev_descriptor[14];
    if i_manufacturer != 0 {
        let s = dev
            .get_string_descriptor(i_manufacturer, language, timeout)
            .await;
        println!("  Manufacturer({i_manufacturer}): {s:?}");
    }

    let i_product = dev_descriptor[15];
    if i_product != 0 {
        let s = dev
            .get_string_descriptor(i_product, language, timeout)
            .await;
        println!("  Product({i_product}): {s:?}");
    }

    let i_serial = dev_descriptor[16];
    if i_serial != 0 {
        let s = dev.get_string_descriptor(i_serial, language, timeout).await;
        println!("  Serial({i_serial}): {s:?}");
    }

    println!();
}
