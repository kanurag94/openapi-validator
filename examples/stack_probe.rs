//! Finds the smallest thread stack on which pathological validations still
//! complete. Run with `cargo run --release --example stack_probe`.

use openapi_validator::{Options, Request, Spec};

fn doc() -> String {
    r##"{"openapi": "3.1.0", "info": {"title": "t", "version": "1"},
"paths": {"/n": {"post": {"requestBody": {"content": {"application/json": {"schema": {"$ref": "#/components/schemas/N"}}}},
   "parameters": [{"name": "q", "in": "query", "schema": {"type": "array", "items": {"type": "integer"}, "uniqueItems": true}}],
   "responses": {"200": {"description": "ok"}}}},
 "/u": {"post": {"requestBody": {"content": {"application/json": {"schema": {"type": "array", "uniqueItems": true}}}}, "responses": {"200": {"description": "ok"}}}}},
"components": {"schemas": {"N": {"type": "object", "properties": {"c": {"anyOf": [{"$ref": "#/components/schemas/N"}, {"allOf": [{"$ref": "#/components/schemas/N"}, {"required": ["x"]}]}]}, "v": {"type": "integer"}}}}}}"##.to_string()
}

fn nested(depth: usize, leaf: &str) -> String {
    let mut s = leaf.to_string();
    for _ in 0..depth {
        s = format!(r#"{{"v": 1, "c": {s}}}"#);
    }
    s
}

fn deep_inline(levels: usize) -> String {
    let mut s = String::from(r#"{"type": "integer"}"#);
    for _ in 0..levels {
        s = format!(r#"{{"allOf": [{s}]}}"#);
    }
    format!(
        r#"{{"openapi": "3.1.0", "info": {{"title": "t", "version": "1"}}, "paths": {{"/d": {{"post": {{"requestBody": {{"content": {{"application/json": {{"schema": {s}}}}}}}, "responses": {{"200": {{"description": "ok"}}}}}}}}}}}}"#
    )
}

fn ref_chain(n: usize) -> String {
    let mut comps = String::new();
    for i in 0..n {
        comps.push_str(&format!(r##""S{i}": {{"allOf": [{{"$ref": "#/components/schemas/S{}"}}], "anyOf": [true, {{"$ref": "#/components/schemas/S{}"}}]}},"##, i + 1, i + 1));
    }
    comps.push_str(&format!(r#""S{n}": {{"type": "integer"}}"#));
    format!(
        r##"{{"openapi": "3.1.0", "info": {{"title": "t", "version": "1"}}, "paths": {{"/d": {{"post": {{"requestBody": {{"content": {{"application/json": {{"schema": {{"$ref": "#/components/schemas/S0"}}}}}}}}, "responses": {{"200": {{"description": "ok"}}}}}}}}}}, "components": {{"schemas": {{{comps}}}}}}}"##
    )
}

/// `R` reaches its own properties through `links` in-place hops.
fn recursive_chain(links: usize) -> String {
    let mut comps = String::new();
    for i in 0..links {
        comps.push_str(&format!(
            r##""L{i}": {{"allOf": [{{"$ref": "#/components/schemas/L{}"}}]}},"##,
            i + 1
        ));
    }
    comps.push_str(&format!(r##""L{links}": {{"type": "object", "properties": {{"c": {{"$ref": "#/components/schemas/L0"}}, "v": {{"type": "integer"}}}}}}"##));
    format!(
        r##"{{"openapi": "3.1.0", "info": {{"title": "t", "version": "1"}}, "paths": {{"/r": {{"post": {{"requestBody": {{"content": {{"application/json": {{"schema": {{"$ref": "#/components/schemas/L0"}}}}}}}}, "responses": {{"200": {{"description": "ok"}}}}}}}}}}, "components": {{"schemas": {{{comps}}}}}}}"##
    )
}

fn post(spec: &Spec, path: &str, body: &str) -> bool {
    spec.validate_request(&Request {
        method: "POST",
        path,
        headers: &[("Content-Type", "application/json")],
        body: body.as_bytes(),
        ..Request::default()
    })
    .is_valid()
}

fn run(stack: usize) -> bool {
    std::thread::Builder::new()
        .stack_size(stack)
        .spawn(|| {
            let spec = Spec::load(deep_inline(58).as_bytes(), Options::default()).unwrap();
            assert!(post(&spec, "/d", "1"));
            assert!(!post(&spec, "/d", "\"x\""));
            for n in [12, 30, 5000] {
                match Spec::load(ref_chain(n).as_bytes(), Options::default()) {
                    Ok(spec) => {
                        let ok = post(&spec, "/d", "1");
                        let bad = post(&spec, "/d", "\"x\"");
                        eprintln!("    ref chain {n}: ok={ok} bad={bad}");
                    }
                    Err(e) => eprintln!(
                        "    ref chain {n}: refused at load ({})",
                        &e.to_string()[..60]
                    ),
                }
            }
            // Worst case the limits allow: an in-place chain near the limit
            // at every level of a body near the depth limit.
            let spec = Spec::load(recursive_chain(31).as_bytes(), Options::default()).unwrap();
            for leaf in [r#"{"v": 1}"#, r#"{"v": "x"}"#] {
                let body = nested(126, leaf);
                std::hint::black_box(post(&spec, "/r", &body));
            }
            let spec = Spec::load(doc().as_bytes(), Options::default()).unwrap();
            let ok = nested(126, r#"{"v": 1}"#);
            let bad = nested(126, r#"{"v": "x"}"#);
            for body in [&ok, &bad] {
                let r = spec.validate_request(&Request {
                    method: "POST",
                    path: "/n",
                    headers: &[("Content-Type", "application/json")],
                    body: body.as_bytes(),
                    ..Request::default()
                });
                std::hint::black_box(r);
            }
            let arr: String = format!(
                "[{}]",
                (0..50_000)
                    .map(|i| i.to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            );
            let t = std::time::Instant::now();
            let r = spec.validate_request(&Request {
                method: "POST",
                path: "/u",
                headers: &[("Content-Type", "application/json")],
                body: arr.as_bytes(),
                ..Request::default()
            });
            assert!(r.is_valid());
            let q: String = (0..4000).map(|i| format!("q={i}&")).collect();
            let r2 = spec.validate_request(&Request {
                method: "POST",
                path: "/n",
                query: &q,
                ..Request::default()
            });
            std::hint::black_box(r2);
            eprintln!("    uniqueItems 50k + 4k query pairs: {:?}", t.elapsed());
        })
        .unwrap()
        .join()
        .is_ok()
}

fn main() {
    if let Some(k) = std::env::args().nth(1) {
        let kib: usize = k.parse().unwrap();
        std::process::exit(if run(kib * 1024) { 0 } else { 1 });
    }
    for kib in [8192, 2048, 1024, 512, 256, 128] {
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg(kib.to_string())
            .status()
            .unwrap();
        println!(
            "stack {kib:>5} KiB: {}",
            if child.success() { "ok" } else { "FAILED" }
        );
    }
}
