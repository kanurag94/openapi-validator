//! Per-call latency on a warm validator.

use criterion::{criterion_group, criterion_main, Criterion};
use openapi_validator::{Options, Request, Spec};

#[path = "support/synthetic.rs"]
mod synthetic;

fn calls(c: &mut Criterion) {
    let doc = synthetic::document(2000, 4);
    let spec = Spec::load(doc.as_bytes(), Options::default()).unwrap();
    let body = synthetic::body(3);
    let bad = body
        .replace("\"price\": 9.99", "\"price\": -9.999")
        .replace("\"leaf\"", "1");
    let get = Request {
        method: "GET",
        path: "/v1/resource1500/item-42/items",
        query: "limit=10&tags=a&tags=b",
        headers: &[("X-Request-Id", "2eb8aa08-aa98-11ea-b4aa-73b441d16380")],
        ..Request::default()
    };
    let post = |b: &'static [u8]| Request {
        method: "POST",
        path: "/v1/resource1500/item-42/items",
        headers: &[("Content-Type", "application/json")],
        body: b,
        ..Request::default()
    };
    let body: &'static [u8] = Box::leak(body.into_bytes().into_boxed_slice());
    let bad: &'static [u8] = Box::leak(bad.into_bytes().into_boxed_slice());
    // Warm the lazily compiled schemas.
    spec.validate_request(&get);
    spec.validate_request(&post(body));
    spec.validate_request(&post(bad));

    let mut g = c.benchmark_group("call");
    g.bench_function("get_with_parameters", |b| {
        b.iter(|| spec.validate_request(std::hint::black_box(&get)))
    });
    let p = post(body);
    g.bench_function("post_json_body_valid", |b| {
        b.iter(|| spec.validate_request(std::hint::black_box(&p)))
    });
    let q = post(bad);
    g.bench_function("post_json_body_invalid", |b| {
        b.iter(|| spec.validate_request(std::hint::black_box(&q)))
    });
    g.finish();
}

criterion_group!(benches, calls);
criterion_main!(benches);
