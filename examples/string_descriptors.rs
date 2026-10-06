use std::time::Duration;

use nusb::{descriptors::language_id::US_ENGLISH, DeviceInfo, MaybeFuture};

fn main() {
    env_logger::init();
    for dev in nusb::list_devices().wait().unwrap() {
        inspect_device(dev);
    }
}

#[rustfmt::skip]
fn inspect_device(di: DeviceInfo) {
    println!(
        "Device {}.{:03} ({:04x}:{:04x})",
        di.bus_id(),
        di.device_address(),
        di.vendor_id(),
        di.product_id(),
    );
    let dev = match di.open().wait() {
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

    println!("  Manufacturer:");
    println!("    cached:                     {:?}", di.manufacturer_string());
    println!("    get_manufacturer_string():  {:?}", di.get_manufacturer_string().wait());
    if let Some(i_manufacturer) = dev_descriptor.manufacturer_string_index() {
        let s = dev
            .get_string_descriptor(i_manufacturer, language, timeout)
            .wait();
        println!("    get_string_descriptor({i_manufacturer}):   {s:?}");
    }

    println!("  Product:");
    println!("    cached:                     {:?}", di.product_string());
    println!("    get_product_string():       {:?}", di.get_product_string().wait());
    if let Some(i_product) = dev_descriptor.product_string_index() {
        let s = dev
            .get_string_descriptor(i_product, language, timeout)
            .wait();
        println!("    get_string_descriptor({i_product}):   {s:?}");
    }

    println!("  Serial:");
    println!("    cached:                     {:?}", di.serial_number());
    println!("    get_serial_number_string(): {:?}", di.get_serial_number_string().wait());
    if let Some(i_serial) = dev_descriptor.serial_number_string_index() {
        let s = dev
            .get_string_descriptor(i_serial, language, timeout)
            .wait();
        println!("    get_string_descriptor({i_serial}):   {s:?}");
    }

    if let Ok(config) = dev.active_configuration() {
        for intf in config.interfaces() {
            println!("  Interface {}:", intf.interface_number());

            if let Some(interface_info) = di.interfaces().find(|i| i.interface_number() == intf.interface_number()) {
                println!("    cached:                     {:?}", interface_info.interface_string());
                println!("    get_interface_string():     {:?}", interface_info.get_interface_string().wait());
            }

            if let Some(i_interface) = intf.first_alt_setting().string_index() {
                let s = dev
                    .get_string_descriptor(i_interface, language, timeout)
                    .wait();
                println!("    get_string_descriptor({i_interface}):   {s:?}");
            }
        }
    }

    println!();
}
