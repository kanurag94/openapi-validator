//! `Spec::load` on arbitrary bytes must never panic, overflow or hang; when
//! it accepts a document, validating requests against it must not either.
#![no_main]

use libfuzzer_sys::fuzz_target;
use openapi_validator::{Options, Request, Spec};

fuzz_target!(|data: &[u8]| {
    let Ok(spec) = Spec::load(data, Options::default()) else {
        return;
    };
    for (method, path) in [("GET", "/"), ("POST", "/a/b"), ("GET", "/pets/1"), ("HEAD", "/x")] {
        let r = spec.validate_request(&Request {
            method,
            path,
            query: "a=1&b=%20",
            headers: &[("Content-Type", "application/json"), ("Authorization", "Bearer x")],
            body: b"{\"a\": [1, 2.5, \"x\", null, {\"b\": true}]}",
        });
        std::hint::black_box(r);
    }
});
