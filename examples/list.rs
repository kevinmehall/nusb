fn main() {
    env_logger::init();
    for dev in nusb::list_devices().unwrap() {
        println!("{:#?}", dev);
        match dev.configurations() {
            Err(e) => println!("  failed to read configurations: {:?}", e),
            Ok(configs) => {
                for c in configs {
                    println!("  configuration {}", c.number());
                    for i in c.interfaces() {
                        println!("    interface {}", i.number());
                        for a in i.alternate_settings() {
                            println!(
                                "      altsetting {}: class={} subclass={} protocol={}",
                                a.alternate_setting_number(),
                                a.class_code(),
                                a.sub_class_code(),
                                a.protocol_code()
                            );
                            for e in a.endpoints() {
                                println!(
                                    "        endpoint {:?} {}: type={:?} max_packet_size={}",
                                    e.direction(),
                                    e.number(),
                                    e.transfer_type(),
                                    e.max_packet_size()
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}
