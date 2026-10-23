//! Responses Object and Response Object (OpenAPI 3.1.1 §4.8.16, §4.8.17).

use openapi_validator::{Options, ProblemKind, Report, Request, Response, Spec};

const DOC: &str = r##"{"openapi": "3.0.3", "info": {"title": "t", "version": "1"},
"paths": {
  "/r": {"get": {"responses": {
    "200": {"description": "ok",
      "headers": {
        "X-Rate": {"required": true, "schema": {"type": "integer", "maximum": 100}},
        "X-Note": {"schema": {"type": "string", "maxLength": 5}},
        "X-Meta": {"content": {"application/json": {"schema": {"type": "object", "required": ["v"]}}}},
        "Content-Type": {"required": true, "schema": {"enum": ["never"]}}
      },
      "content": {"application/json": {"schema": {"$ref": "#/components/schemas/User"}}}},
    "2XX": {"description": "range", "content": {"application/json": {"schema": {"type": "array"}}}},
    "204": {"description": "empty", "content": {"application/json": {"schema": {"type": "object"}}}},
    "default": {"description": "error", "content": {"application/json": {"schema": {"type": "object", "required": ["code"]}}}}
  }}},
  "/n": {"get": {"responses": {"200": {"description": "ok"}}}}
},
"components": {"schemas": {
  "User": {"type": "object", "required": ["id", "password"], "properties": {
     "id": {"type": "string"}, "password": {"type": "string", "writeOnly": true}}}
}}}"##;

fn spec() -> Spec {
    Spec::load(DOC.as_bytes(), Options::default()).unwrap()
}

fn respond(
    s: &Spec,
    method: &str,
    path: &str,
    status: u16,
    headers: &[(&str, &str)],
    body: &str,
) -> Report {
    let req = Request {
        method,
        path,
        ..Request::default()
    };
    s.validate_response(
        &req,
        &Response {
            status,
            headers,
            body: body.as_bytes(),
        },
    )
}

const JSON: (&str, &str) = ("Content-Type", "application/json");

#[track_caller]
fn ok(r: Report) {
    assert!(r.is_valid(), "expected valid, got:\n{r}");
}

#[track_caller]
fn bad(r: Report, location: &str, kind: ProblemKind) {
    assert!(
        r.problems()
            .iter()
            .any(|p| p.location == location && p.kind == kind),
        "expected {kind} at {location}, got:\n{r}"
    );
}

// §4.8.16.1: exact code, then range, then default.
#[test]
fn status_lookup_order() {
    let s = spec();
    ok(respond(
        &s,
        "GET",
        "/r",
        200,
        &[JSON, ("X-Rate", "5")],
        r#"{"id": "1"}"#,
    ));
    ok(respond(&s, "GET", "/r", 201, &[JSON], "[]"));
    bad(
        respond(&s, "GET", "/r", 201, &[JSON], "{}"),
        "body",
        ProblemKind::SchemaViolation,
    );
    ok(respond(&s, "GET", "/r", 500, &[JSON], r#"{"code": 1}"#));
    bad(
        respond(&s, "GET", "/r", 404, &[JSON], "{}"),
        "body/code",
        ProblemKind::SchemaViolation,
    );
    bad(
        respond(&s, "GET", "/n", 500, &[], ""),
        "status",
        ProblemKind::UndeclaredStatus,
    );
}

// writeOnly properties drop out of `required` in responses.
#[test]
fn write_only_is_not_required_in_responses() {
    let s = spec();
    ok(respond(
        &s,
        "GET",
        "/r",
        200,
        &[JSON, ("X-Rate", "1")],
        r#"{"id": "1"}"#,
    ));
    bad(
        respond(
            &s,
            "GET",
            "/r",
            200,
            &[JSON, ("X-Rate", "1")],
            r#"{"password": "x"}"#,
        ),
        "body/id",
        ProblemKind::SchemaViolation,
    );
}

// A JSON null body fails a non-nullable object schema.
#[test]
fn null_body_fails() {
    let s = spec();
    bad(
        respond(&s, "GET", "/r", 500, &[JSON], "null"),
        "body",
        ProblemKind::SchemaViolation,
    );
}

// Response headers: required, literal text in simple style (never re-read as
// JSON), `content`-described, and Content-Type ignored (§4.8.17.1).
#[test]
fn response_headers() {
    let s = spec();
    bad(
        respond(&s, "GET", "/r", 200, &[JSON], r#"{"id": "1"}"#),
        "header.x-rate",
        ProblemKind::MissingParameter,
    );
    bad(
        respond(
            &s,
            "GET",
            "/r",
            200,
            &[JSON, ("X-Rate", "101")],
            r#"{"id": "1"}"#,
        ),
        "header.x-rate",
        ProblemKind::InvalidParameter,
    );
    ok(respond(
        &s,
        "GET",
        "/r",
        200,
        &[JSON, ("X-Rate", "1"), ("X-Note", "\"ab\"")],
        r#"{"id": "1"}"#,
    ));
    bad(
        respond(
            &s,
            "GET",
            "/r",
            200,
            &[JSON, ("X-Rate", "1"), ("X-Note", "\"abcd\"")],
            r#"{"id": "1"}"#,
        ),
        "header.x-note",
        ProblemKind::InvalidParameter,
    );
    ok(respond(
        &s,
        "GET",
        "/r",
        200,
        &[JSON, ("X-Rate", "1"), ("X-Meta", r#"{"v": 1}"#)],
        r#"{"id": "1"}"#,
    ));
    bad(
        respond(
            &s,
            "GET",
            "/r",
            200,
            &[JSON, ("X-Rate", "1"), ("X-Meta", "{}")],
            r#"{"id": "1"}"#,
        ),
        "header.x-meta/v",
        ProblemKind::InvalidParameter,
    );
    bad(
        respond(
            &s,
            "GET",
            "/r",
            200,
            &[JSON, ("X-Rate", "1"), ("X-Rate", "500")],
            r#"{"id": "1"}"#,
        ),
        "header.x-rate",
        ProblemKind::InvalidParameter,
    );
}

// An empty body where content is declared is a problem, except for HEAD,
// 204 and 304 (RFC 9110 §9.3.2, §15.3.5, §15.4.5).
#[test]
fn empty_bodies() {
    let s = spec();
    bad(
        respond(&s, "GET", "/r", 200, &[("X-Rate", "1")], ""),
        "body",
        ProblemKind::MissingBody,
    );
    ok(respond(&s, "HEAD", "/r", 200, &[("X-Rate", "1")], ""));
    ok(respond(&s, "GET", "/r", 204, &[], ""));
    ok(respond(&s, "GET", "/n", 200, &[], ""));
    let d = DOC.replace(r#""204": {"#, r#""304": {"#);
    let s = Spec::load(d.as_bytes(), Options::default()).unwrap();
    ok(respond(&s, "GET", "/r", 304, &[], ""));
}

// A response to a request that matches no operation is a problem.
#[test]
fn undeclared_operation() {
    let s = spec();
    bad(
        respond(&s, "GET", "/nowhere", 200, &[], ""),
        "request",
        ProblemKind::UndeclaredOperation,
    );
    bad(
        respond(&s, "POST", "/r", 200, &[], ""),
        "request",
        ProblemKind::UndeclaredOperation,
    );
}

#[test]
fn response_media_types() {
    let s = spec();
    let html = respond(
        &s,
        "GET",
        "/r",
        200,
        &[("Content-Type", "text/html"), ("X-Rate", "1")],
        "<p>",
    );
    assert!(
        html.is_valid(),
        "an undescribed type is not checked:\n{html}"
    );
    let strict = Spec::load(
        DOC.as_bytes(),
        Options {
            strict_mode: true,
            ..Options::default()
        },
    )
    .unwrap();
    bad(
        respond(
            &strict,
            "GET",
            "/r",
            200,
            &[("Content-Type", "text/html"), ("X-Rate", "1")],
            "<p>",
        ),
        "header.content-type",
        ProblemKind::UnsupportedMediaType,
    );
    bad(
        respond(&s, "GET", "/r", 200, &[JSON, ("X-Rate", "1")], "{bad"),
        "body",
        ProblemKind::MalformedBody,
    );
}
