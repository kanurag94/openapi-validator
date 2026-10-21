//! Webhooks (OpenAPI 3.1.1 §4.8.1) and callbacks (§4.8.18): operations the
//! API calls rather than serves, named by the caller instead of routed.

use openapi_validator::{Options, Request, Response, Spec};

fn load(doc: &str) -> Spec {
    Spec::load(doc.as_bytes(), Options::default()).unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn the_report_names_the_webhook_and_method() {
    let s = load(
        "openapi: 3.1.0\ninfo: {title: t, version: '1'}\nwebhooks:\n  ping:\n    post:\n      operationId: sendPing\n      responses: {'200': {description: ok}}\n",
    );
    let r = s.validate_webhook_request(
        "ping",
        &Request {
            method: "POST",
            path: "/",
            ..Request::default()
        },
    );
    assert!(r.is_valid(), "{r}");
    let m = r.operation().expect("matched");
    assert_eq!(
        (
            m.path.as_str(),
            m.method.as_str(),
            m.operation_id.as_deref()
        ),
        ("ping", "post", Some("sendPing"))
    );
}

#[test]
fn webhooks_mean_nothing_in_30() {
    let s = load(
        "openapi: 3.0.3\ninfo: {title: t, version: '1'}\npaths: {}\nwebhooks:\n  ping:\n    post:\n      responses: {'200': {description: ok}}\n",
    );
    let r = s.validate_webhook_request(
        "ping",
        &Request {
            method: "POST",
            path: "/",
            ..Request::default()
        },
    );
    assert!(!r.is_valid());
}

// A callback whose operation declares the same callback by reference: the
// loader indexes it once and stops.
#[test]
fn self_referencing_callbacks_load() {
    let s = load(
        r##"
openapi: 3.1.0
info: {title: t, version: "1"}
paths:
  /s:
    post:
      operationId: sub
      responses: {"201": {description: ok}}
      callbacks:
        loop: {$ref: "#/components/callbacks/Loop"}
components:
  callbacks:
    Loop:
      "{$request.body#/u}":
        post:
          operationId: sub
          responses: {"200": {description: ok}}
          callbacks:
            loop: {$ref: "#/components/callbacks/Loop"}
"##,
    );
    let req = Request {
        method: "POST",
        path: "/",
        ..Request::default()
    };
    assert!(s.validate_callback_request("sub", "loop", &req).is_valid());
    let resp = Response {
        status: 200,
        ..Response::default()
    };
    assert!(s
        .validate_callback_response("sub", "loop", &req, &resp)
        .is_valid());
}

// Callbacks nest: a callback's operation can declare callbacks of its own.
#[test]
fn nested_callbacks_are_indexed() {
    let s = load(
        r##"
openapi: 3.1.0
info: {title: t, version: "1"}
paths:
  /s:
    post:
      operationId: outer
      responses: {"201": {description: ok}}
      callbacks:
        first:
          "{$request.body#/u}":
            post:
              operationId: inner
              responses: {"200": {description: ok}}
              callbacks:
                second:
                  "{$request.body#/v}":
                    put:
                      responses: {"204": {description: ok}}
"##,
    );
    let req = Request {
        method: "PUT",
        path: "/",
        ..Request::default()
    };
    assert!(s
        .validate_callback_request("inner", "second", &req)
        .is_valid());
}
