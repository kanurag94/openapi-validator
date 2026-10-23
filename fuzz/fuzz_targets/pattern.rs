//! Pattern translation must never panic, and an accepted pattern must
//! compile and match in bounded time.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: (&str, &str)| {
    let (pattern, text) = data;
    if let Ok(re) = openapi_validator::pattern::compile(pattern) {
        let _ = re.is_match(text);
    }
    let _ = openapi_validator::pattern::is_strict_unicode(pattern);
    // OpenAPI 3.0 reads patterns, and the strings they match, as UTF-16.
    use openapi_validator::pattern::{build, translate_in, utf16_view, Dialect};
    if let Ok(t) = translate_in(pattern, Dialect::Legacy) {
        if let Ok(re) = build(&t) {
            let _ = re.is_match(&utf16_view(text));
        }
    }
});
