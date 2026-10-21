//! Pattern translation must never panic, and an accepted pattern must
//! compile and match in bounded time.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: (&str, &str)| {
    let (pattern, text) = data;
    if let Ok(re) = openapi_validator::pattern::compile(pattern) {
        let _ = re.is_match(text);
    }
});
