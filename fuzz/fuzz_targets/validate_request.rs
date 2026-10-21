//! Arbitrary requests and responses against a document that exercises
//! every parameter style, body media type and schema feature.
#![no_main]

use std::sync::OnceLock;

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use openapi_validator::{Options, Request, Response, Spec};

const DOC: &str = include_str!("../documents/kitchen_sink.yaml");

#[derive(Debug, Arbitrary)]
struct Input<'a> {
    method: u8,
    path: &'a str,
    query: &'a str,
    headers: Vec<(&'a str, &'a str)>,
    content_type: u8,
    body: &'a [u8],
    status: u16,
    response_body: &'a [u8],
}

const METHODS: [&str; 6] = ["GET", "POST", "PUT", "HEAD", "DELETE", "get"];
const TYPES: [&str; 7] = [
    "application/json",
    "application/x-www-form-urlencoded",
    "multipart/form-data; boundary=B",
    "text/plain; charset=utf-8",
    "application/xml",
    "application/vnd.x+json",
    "",
];

fn spec() -> &'static Spec {
    static SPEC: OnceLock<Spec> = OnceLock::new();
    SPEC.get_or_init(|| {
        Spec::load(
            DOC.as_bytes(),
            Options {
                allow_unknown_query: false,
                allow_unknown_headers: false,
                allow_unknown_cookies: false,
                ..Options::default()
            },
        )
        .expect("the fuzz document loads")
    })
}

fuzz_target!(|input: Input<'_>| {
    let s = spec();
    let mut headers = input.headers.clone();
    let ct = TYPES[usize::from(input.content_type) % TYPES.len()];
    if !ct.is_empty() {
        headers.push(("Content-Type", ct));
    }
    let req = Request {
        method: METHODS[usize::from(input.method) % METHODS.len()],
        path: input.path,
        query: input.query,
        headers: &headers,
        body: input.body,
    };
    std::hint::black_box(s.validate_request(&req));
    std::hint::black_box(s.validate_response(
        &req,
        &Response {
            status: input.status,
            headers: &headers,
            body: input.response_body,
        },
    ));
});
