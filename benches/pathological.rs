//! The pathological inputs other validators stall or crash on.

use criterion::{criterion_group, criterion_main, Criterion};
use openapi_validator::{Options, Request, Spec};

const DOC: &str = r##"{"openapi": "3.1.0", "info": {"title": "p", "version": "1"},
"paths": {
  "/any": {"post": {"requestBody": {"content": {"application/json": {"schema": {"$ref": "#/components/schemas/N"}}}}, "responses": {"200": {"description": "ok"}}}},
  "/pat": {"post": {"requestBody": {"content": {"application/json": {"schema": {"type": "string", "pattern": "^(a+)+$"}}}}, "responses": {"200": {"description": "ok"}}}},
  "/uniq": {"post": {"requestBody": {"content": {"application/json": {"schema": {"type": "array", "uniqueItems": true}}}}, "responses": {"200": {"description": "ok"}}}},
  "/deep": {"post": {"requestBody": {"content": {"application/json": {}}}, "responses": {"200": {"description": "ok"}}}},
  "/q": {"get": {"parameters": [{"name": "k", "in": "query", "schema": {"type": "array", "items": {"type": "integer"}}}], "responses": {"200": {"description": "ok"}}}}
},
"components": {"schemas": {"N": {"anyOf": [
  {"type": "object", "properties": {"c": {"$ref": "#/components/schemas/N"}, "v": {"type": "integer"}}},
  {"type": "object", "properties": {"c": {"$ref": "#/components/schemas/N"}, "v": {"type": "number"}}}
]}}}}"##;

fn nested(depth: usize, leaf: &str) -> String {
    let mut s = leaf.to_string();
    for _ in 0..depth {
        s = format!(r#"{{"v": 1, "c": {s}}}"#);
    }
    s
}

fn post<'a>(path: &'a str, body: &'a [u8]) -> Request<'a> {
    Request {
        method: "POST",
        path,
        headers: &[("Content-Type", "application/json")],
        body,
        ..Request::default()
    }
}

fn pathological(c: &mut Criterion) {
    let spec = Spec::load(DOC.as_bytes(), Options::default()).unwrap();
    let valid = nested(16, r#"{"v": 1}"#);
    let invalid = nested(16, r#"{"v": "x"}"#);
    let pattern = format!("\"{}!\"", "a".repeat(50_000));
    let uniq = format!(
        "[{}]",
        (0..10_000)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",")
    );
    let deep = format!("{}{}", "[".repeat(127), "]".repeat(127));
    let query: String = (0..5000).map(|i| format!("k={i}&")).collect();
    for b in [&valid, &invalid] {
        spec.validate_request(&post("/any", b.as_bytes()));
    }

    let mut g = c.benchmark_group("pathological");
    g.bench_function("recursive_anyOf_16_valid", |b| {
        b.iter(|| spec.validate_request(&post("/any", valid.as_bytes())))
    });
    g.bench_function("recursive_anyOf_16_invalid", |b| {
        b.iter(|| spec.validate_request(&post("/any", invalid.as_bytes())))
    });
    g.bench_function("backtracking_pattern_50k", |b| {
        b.iter(|| spec.validate_request(&post("/pat", pattern.as_bytes())))
    });
    g.bench_function("unique_items_10k", |b| {
        b.iter(|| spec.validate_request(&post("/uniq", uniq.as_bytes())))
    });
    g.bench_function("json_depth_127", |b| {
        b.iter(|| spec.validate_request(&post("/deep", deep.as_bytes())))
    });
    let q = Request {
        method: "GET",
        path: "/q",
        query: &query,
        ..Request::default()
    };
    g.bench_function("query_5000_pairs", |b| b.iter(|| spec.validate_request(&q)));
    g.finish();
}

criterion_group!(benches, pathological);
criterion_main!(benches);
