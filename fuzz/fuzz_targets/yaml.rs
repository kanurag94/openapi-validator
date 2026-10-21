#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = openapi_validator::yaml::parse(data, Default::default());
});
