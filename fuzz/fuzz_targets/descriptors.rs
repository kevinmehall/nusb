#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    nusb::descriptor::fuzz_parse_configurations(data);
});
