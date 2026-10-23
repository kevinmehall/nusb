#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    for config in nusb::descriptors::fuzz_parse_concatenated_config_descriptors(data) {
        let s = format!("{config:?}");
        std::hint::black_box(s);
    }
});
