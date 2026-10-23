//! Hard limits: no stack overflow and no exponential work for any document
//! the loader accepts. Checks run on a 256 KiB thread, a small stack.

use openapi_validator::{LoadError, Options, ProblemKind, Request, Spec};

fn doc(comps: &str) -> String {
    format!(
        r##"{{"openapi": "3.1.0", "info": {{"title": "t", "version": "1"}},
"paths": {{"/r": {{"post": {{"requestBody": {{"content": {{"application/json": {{"schema": {{"$ref": "#/components/schemas/L0"}}}}}}}}, "responses": {{"200": {{"description": "ok"}}}}}}}}}},
"components": {{"schemas": {{{comps}}}}}}}"##
    )
}

/// `L0 -> ... -> Ln` through `allOf`, optionally also through `anyOf`, with
/// `Ln` recursing into `L0` through a property.
fn chain(n: usize, fan_out: bool) -> String {
    let mut c = String::new();
    for i in 0..n {
        let next = format!("#/components/schemas/L{}", i + 1);
        if fan_out {
            c.push_str(&format!(r#""L{i}": {{"allOf": [{{"$ref": "{next}"}}], "anyOf": [{{"$ref": "{next}"}}, true]}},"#));
        } else {
            c.push_str(&format!(r#""L{i}": {{"allOf": [{{"$ref": "{next}"}}]}},"#));
        }
    }
    c.push_str(&format!(
        r##""L{n}": {{"type": "object", "properties": {{"c": {{"$ref": "#/components/schemas/L0"}}, "v": {{"type": "integer"}}}}}}"##
    ));
    doc(&c)
}

fn nested(depth: usize, leaf: &str) -> String {
    let mut s = leaf.to_string();
    for _ in 0..depth {
        s = format!(r#"{{"v": 1, "c": {s}}}"#);
    }
    s
}

fn on_small_stack(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .expect("completes on a 256 KiB stack");
}

#[test]
fn long_in_place_chains_are_refused_at_load() {
    let e = Spec::load(chain(5000, false).as_bytes(), Options::default())
        .err()
        .unwrap();
    assert!(matches!(e, LoadError::Invalid { .. }), "{e}");
    assert!(e.to_string().contains("in-place"), "{e}");
}

#[test]
fn exponential_fan_out_is_refused_at_load() {
    let t = std::time::Instant::now();
    let e = Spec::load(chain(30, true).as_bytes(), Options::default())
        .err()
        .unwrap();
    assert!(e.to_string().contains("subschemas to one value"), "{e}");
    assert!(t.elapsed().as_secs() < 5);
    // A small fan-out is fine.
    assert!(Spec::load(chain(8, true).as_bytes(), Options::default()).is_ok());
}

#[test]
fn deepest_accepted_schemas_and_bodies_fit_a_small_stack() {
    if cfg!(debug_assertions) {
        // Debug frames are several times larger; the guarantee is for
        // release builds (see examples/stack_probe.rs).
        return;
    }
    on_small_stack(|| {
        let s = Spec::load(chain(31, false).as_bytes(), Options::default()).unwrap();
        for leaf in [r#"{"v": 1}"#, r#"{"v": "x"}"#] {
            for depth in [10, 30, 126] {
                let body = nested(depth, leaf);
                let r = s.validate_request(&Request {
                    method: "POST",
                    path: "/r",
                    headers: &[("Content-Type", "application/json")],
                    body: body.as_bytes(),
                    ..Request::default()
                });
                if depth == 126 {
                    // Beyond the frame budget for this document: refused, not crashed.
                    assert_eq!(r.problems()[0].kind, ProblemKind::MalformedBody, "{r}");
                }
            }
        }
    });
}

#[test]
fn typical_documents_keep_the_full_body_depth() {
    let s = Spec::load(chain(1, false).as_bytes(), Options::default()).unwrap();
    let body = nested(120, r#"{"v": 1}"#);
    let r = s.validate_request(&Request {
        method: "POST",
        path: "/r",
        headers: &[("Content-Type", "application/json")],
        body: body.as_bytes(),
        ..Request::default()
    });
    assert!(r.is_valid(), "{r}");
}

/// Many operations reaching one densely linked set of schemas, as large
/// public APIs have: each schema is compiled once, not once per operation.
#[test]
fn operations_share_the_schemas_they_reach() {
    if cfg!(debug_assertions) {
        return;
    }
    let (n, ops) = (150, 400);
    let mut comps = Vec::new();
    for i in 0..n {
        let props: Vec<String> = (0..25)
            .map(|j| {
                let to = (i * 7 + j * 13) % n;
                format!(r##""p{j}": {{"anyOf": [{{"type": "string"}}, {{"$ref": "#/components/schemas/C{to}"}}]}}"##)
            })
            .collect();
        comps.push(format!(
            r#""C{i}": {{"type": "object", "properties": {{{}}}}}"#,
            props.join(",")
        ));
    }
    let paths: Vec<String> = (0..ops)
        .map(|i| {
            format!(r##""/o{i}": {{"get": {{"responses": {{"200": {{"description": "ok", "content": {{"application/json": {{"schema": {{"$ref": "#/components/schemas/C{}"}}}}}}}}}}}}}}"##, i % n)
        })
        .collect();
    let d = format!(
        r#"{{"openapi": "3.1.0", "info": {{"title": "t", "version": "1"}}, "paths": {{{}}}, "components": {{"schemas": {{{}}}}}}}"#,
        paths.join(","),
        comps.join(",")
    );
    let s = Spec::load(d.as_bytes(), Options::default()).unwrap();
    let body = br#"{"p0": "x", "p1": {"p2": "y"}}"#;
    let t = std::time::Instant::now();
    for i in 0..ops {
        let path = format!("/o{i}");
        let req = Request {
            method: "GET",
            path: &path,
            ..Request::default()
        };
        let resp = openapi_validator::Response {
            status: 200,
            headers: &[("Content-Type", "application/json")],
            body,
        };
        let r = s.validate_response(&req, &resp);
        assert!(r.is_valid(), "{r}");
    }
    assert!(t.elapsed().as_millis() < 500, "took {:?}", t.elapsed());
}
