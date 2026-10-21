//! Request bodies by media type (OpenAPI 3.1.1 §4.8.13–§4.8.15).

use openapi_validator::{Options, ProblemKind, Report, Request, Spec};

fn spec_opts(content: &str, required: bool, opts: Options) -> Spec {
    let d = format!(
        r#"{{"openapi": "3.1.0", "info": {{"title": "t", "version": "1"}},
"paths": {{"/b": {{"post": {{"requestBody": {{"required": {required}, "content": {{{content}}}}},
  "responses": {{"200": {{"description": "ok"}}}}}}}}}}}}"#
    );
    Spec::load(d.as_bytes(), opts).unwrap_or_else(|e| panic!("{e}\n{d}"))
}

fn spec(content: &str) -> Spec {
    spec_opts(content, true, Options::default())
}

fn post(s: &Spec, ct: Option<&str>, body: &[u8]) -> Report {
    let headers: Vec<(&str, &str)> = ct.map(|c| vec![("Content-Type", c)]).unwrap_or_default();
    s.validate_request(&Request {
        method: "POST",
        path: "/b",
        headers: &headers,
        body,
        ..Request::default()
    })
}

#[track_caller]
fn ok(r: Report) {
    assert!(r.is_valid(), "expected valid, got:\n{r}");
}

#[track_caller]
fn bad(r: Report, location: &str) {
    assert!(
        r.problems().iter().any(|p| p.location == location),
        "expected a problem at {location}, got:\n{r}"
    );
}

#[track_caller]
fn kind(r: &Report, k: ProblemKind) {
    assert!(
        r.problems().iter().any(|p| p.kind == k),
        "expected {k}, got:\n{r}"
    );
}

const FORM_SCHEMA: &str = r#"{"type": "object", "required": ["name"], "properties": {
    "name": {"type": "string", "pattern": "^[a-z/ ]+$"},
    "age": {"type": "integer", "minimum": 0},
    "tags": {"type": "array", "items": {"type": "string"}, "maxItems": 2},
    "ids": {"type": "array", "items": {"type": "integer"}},
    "meta": {"type": "object", "properties": {"k": {"type": "integer"}}}
}, "additionalProperties": false}"#;

// §4.8.14.4 urlencoded forms, with Encoding Object style and explode.
#[test]
fn urlencoded_form() {
    let s = spec(&format!(
        r#""application/x-www-form-urlencoded": {{"schema": {FORM_SCHEMA},
            "encoding": {{"ids": {{"style": "form", "explode": false}}, "meta": {{"contentType": "application/json"}}}}}}"#
    ));
    let ct = Some("application/x-www-form-urlencoded");
    ok(post(&s, ct, b"name=rex&age=3&tags=a&tags=b&ids=1,2,3"));
    ok(post(&s, ct, b"name=a%2Fb+c"));
    ok(post(&s, ct, b"name=a/b"));
    ok(post(&s, ct, br#"name=x&meta=%7B%22k%22%3A1%7D"#));
    bad(
        post(&s, ct, br#"name=x&meta=%7B%22k%22%3A%22no%22%7D"#),
        "body/meta/k",
    );
    bad(post(&s, ct, b"age=3"), "body/name");
    bad(post(&s, ct, b"name=rex&age=-1"), "body/age");
    bad(post(&s, ct, b"name=rex&tags=a&tags=b&tags=c"), "body/tags");
    bad(post(&s, ct, b"name=rex&ids=1,x"), "body/ids/1");
    bad(post(&s, ct, b"name=rex&other=1"), "body/other");
    kind(&post(&s, ct, b"name=%zz"), ProblemKind::MalformedBody);
}

// RFC 7578 multipart/form-data: required parts, and each part against its
// property.
#[test]
fn multipart_form() {
    let s = spec(
        r#""multipart/form-data": {"schema": {"type": "object", "required": ["id", "file"], "properties": {
            "id": {"type": "integer"},
            "meta": {"type": "object", "required": ["a"]},
            "file": {"type": "string", "format": "binary"},
            "tags": {"type": "array", "items": {"type": "string"}, "maxItems": 2}
        }}}"#,
    );
    let ct = Some("multipart/form-data; boundary=XyZ");
    let body = b"--XyZ\r\nContent-Disposition: form-data; name=\"id\"\r\n\r\n42\r\n\
--XyZ\r\nContent-Disposition: form-data; name=\"meta\"\r\nContent-Type: application/json\r\n\r\n{\"a\": 1}\r\n\
--XyZ\r\nContent-Disposition: form-data; name=\"file\"; filename=\"x.bin\"\r\nContent-Type: application/octet-stream\r\n\r\n\x00\x01\xff\r\n\
--XyZ\r\nContent-Disposition: form-data; name=\"tags\"\r\n\r\na\r\n\
--XyZ\r\nContent-Disposition: form-data; name=\"tags\"\r\n\r\nb\r\n\
--XyZ--\r\n";
    ok(post(&s, ct, body));
    let missing = b"--XyZ\r\nContent-Disposition: form-data; name=\"id\"\r\n\r\n42\r\n--XyZ--";
    bad(post(&s, ct, missing), "body/file");
    let wrong = b"--XyZ\r\nContent-Disposition: form-data; name=\"id\"\r\n\r\nnope\r\n\
--XyZ\r\nContent-Disposition: form-data; name=\"file\"; filename=\"f\"\r\n\r\nx\r\n--XyZ--";
    bad(post(&s, ct, wrong), "body/id");
    let bad_json = b"--XyZ\r\nContent-Disposition: form-data; name=\"meta\"\r\nContent-Type: application/json\r\n\r\n{}\r\n\
--XyZ\r\nContent-Disposition: form-data; name=\"id\"\r\n\r\n1\r\n\
--XyZ\r\nContent-Disposition: form-data; name=\"file\"; filename=\"f\"\r\n\r\nx\r\n--XyZ--";
    bad(post(&s, ct, bad_json), "body/meta/a");
    kind(
        &post(&s, ct, b"no boundary here"),
        ProblemKind::MalformedBody,
    );
    kind(
        &post(&s, Some("multipart/form-data"), b"--x\r\n"),
        ProblemKind::MalformedBody,
    );
    let lf = b"--XyZ\nContent-Disposition: form-data; name=\"id\"\n\n7\n--XyZ\nContent-Disposition: form-data; name=\"file\"; filename=\"f\"\n\nx\n--XyZ--\n";
    ok(post(&s, ct, lf));
}

#[test]
fn text_plain() {
    let s = spec(r#""text/plain": {"schema": {"type": "string", "maxLength": 5}}"#);
    ok(post(&s, Some("text/plain"), b"hello"));
    bad(
        post(&s, Some("text/plain; charset=utf-8"), b"hello world"),
        "body",
    );
    kind(
        &post(&s, Some("text/plain"), b"\xff"),
        ProblemKind::MalformedBody,
    );
    ok(post(&s, Some("text/plain; charset=iso-8859-1"), b"caf\xe9"));
    let s = spec(r#""text/plain": {"schema": {"type": "integer"}}"#);
    ok(post(&s, Some("text/plain"), b"5"));
    bad(post(&s, Some("text/plain"), b"five"), "body");
}

// XML must be well-formed; well-formed XML is never refused.
#[test]
fn xml_must_be_well_formed() {
    let s =
        spec(r#""application/xml": {"schema": {"type": "object"}}, "application/atom+xml": {}"#);
    ok(post(
        &s,
        Some("application/xml"),
        b"<?xml version=\"1.0\"?><pet id=\"1\"><name>rex</name></pet>",
    ));
    ok(post(
        &s,
        Some("application/atom+xml"),
        b"<feed xmlns=\"http://www.w3.org/2005/Atom\"/>",
    ));
    kind(
        &post(&s, Some("application/xml"), b"<pet><name>rex</pet>"),
        ProblemKind::MalformedBody,
    );
    kind(
        &post(&s, Some("application/xml"), b"<a/><b/>"),
        ProblemKind::MalformedBody,
    );
    let bomb = b"<?xml version=\"1.0\"?><!DOCTYPE l [<!ENTITY a \"aaaaaaaaaa\"><!ENTITY b \"&a;&a;&a;&a;&a;&a;&a;&a;&a;&a;\"><!ENTITY c \"&b;&b;&b;&b;&b;&b;&b;&b;&b;&b;\"><!ENTITY d \"&c;&c;&c;&c;&c;&c;&c;&c;&c;&c;\"><!ENTITY e \"&d;&d;&d;&d;&d;&d;&d;&d;&d;&d;\"><!ENTITY f \"&e;&e;&e;&e;&e;&e;&e;&e;&e;&e;\"><!ENTITY g \"&f;&f;&f;&f;&f;&f;&f;&f;&f;&f;\">]><l>&g;</l>";
    let t = std::time::Instant::now();
    let _ = post(&s, Some("application/xml"), bomb);
    assert!(t.elapsed().as_secs() < 2);
    let deep = format!("{}{}", "<a>".repeat(100_000), "</a>".repeat(100_000));
    let _ = post(&s, Some("application/xml"), deep.as_bytes());
}

// §4.8.14: media ranges, `+json`, and charset parameters.
#[test]
fn media_ranges() {
    let s = spec(
        r#""application/json": {"schema": {"type": "object"}}, "application/*": {"schema": {"type": "string"}}, "*/*": {}"#,
    );
    ok(post(&s, Some("application/json; charset=utf-8"), b"{}"));
    bad(post(&s, Some("application/json"), b"[]"), "body");
    ok(post(&s, Some("application/vnd.x+json"), b"\"s\""));
    ok(post(&s, Some("image/png"), b"\x89PNG"));
    let s = spec(r#""application/json": {"schema": {"type": "object"}}"#);
    ok(post(&s, Some("Application/JSON"), b"{}"));
    ok(post(&s, None, b"{}"));
    kind(
        &post(&s, Some("text/plain"), b"{}"),
        ProblemKind::UnsupportedMediaType,
    );
    kind(
        &post(&s, Some("nonsense"), b"{}"),
        ProblemKind::UnsupportedMediaType,
    );
}

// §4.8.13: a required body must be present; an empty optional body is fine.
#[test]
fn required_and_optional_bodies() {
    let s = spec_opts(
        r#""application/json": {"schema": {"type": "object"}}"#,
        false,
        Options::default(),
    );
    ok(post(&s, None, b""));
    ok(post(&s, Some("application/json"), b""));
    let s = spec(r#""application/json": {"schema": {"type": "object"}}"#);
    kind(&post(&s, None, b""), ProblemKind::MissingBody);
}

#[test]
fn body_limits_and_switch() {
    let s = spec_opts(
        r#""application/json": {"schema": {"type": "object"}}"#,
        true,
        Options {
            max_body_bytes: 8,
            ..Options::default()
        },
    );
    kind(
        &post(&s, Some("application/json"), b"{\"a\": 12345}"),
        ProblemKind::BodyTooLarge,
    );
    let s = spec_opts(
        r#""application/json": {"schema": {"type": "object"}}"#,
        true,
        Options {
            validate_body: false,
            ..Options::default()
        },
    );
    ok(post(&s, Some("application/json"), b"[]"));
    ok(post(&s, None, b""));
    let s = spec(r#""application/json": {"schema": {"type": "array"}}"#);
    let deep = format!("{}{}", "[".repeat(200), "]".repeat(200));
    kind(
        &post(&s, Some("application/json"), deep.as_bytes()),
        ProblemKind::MalformedBody,
    );
}

fn gzip(data: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

fn post_encoded(s: &Spec, coding: &str, body: &[u8]) -> Report {
    s.validate_request(&Request {
        method: "POST",
        path: "/b",
        headers: &[
            ("Content-Type", "application/json"),
            ("Content-Encoding", coding),
        ],
        body,
        ..Request::default()
    })
}

// RFC 9110 §8.4: the body is checked after undoing its content codings.
#[test]
fn content_encoding_is_decoded() {
    use std::io::Write;
    let s = spec(r#""application/json": {"schema": {"type": "object", "required": ["a"]}}"#);
    ok(post_encoded(&s, "gzip", &gzip(br#"{"a": 1}"#)));
    bad(post_encoded(&s, "gzip", &gzip(b"{}")), "body/a");
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    z.write_all(br#"{"a": 1}"#).unwrap();
    ok(post_encoded(&s, "deflate", &z.finish().unwrap()));
    // Codings are undone in reverse order of application.
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    z.write_all(&gzip(br#"{"a": 1}"#)).unwrap();
    ok(post_encoded(&s, "gzip, deflate", &z.finish().unwrap()));
    kind(
        &post_encoded(&s, "zstd", b"\x28\xb5\x2f\xfd"),
        ProblemKind::UnsupportedMediaType,
    );
    kind(
        &post_encoded(&s, "gzip", b"not gzip"),
        ProblemKind::MalformedBody,
    );
    kind(
        &post_encoded(&s, "br", b"not brotli at all"),
        ProblemKind::MalformedBody,
    );
    // `{"a": 1}` compressed with brotli (RFC 7932).
    let br = [
        0x8f, 0x03, 0x80, 0x7b, 0x22, 0x61, 0x22, 0x3a, 0x20, 0x31, 0x7d, 0x03,
    ];
    ok(post_encoded(&s, "br", &br));
}

#[test]
fn compression_bombs_stop_at_the_limit() {
    let s = spec_opts(
        r#""application/json": {"schema": {"type": "array"}}"#,
        true,
        Options {
            max_body_bytes: 1 << 20,
            ..Options::default()
        },
    );
    let bomb = gzip(&vec![b' '; 8 << 20]);
    assert!(bomb.len() < 1 << 20, "small on the wire");
    let t = std::time::Instant::now();
    kind(&post_encoded(&s, "gzip", &bomb), ProblemKind::BodyTooLarge);
    assert!(t.elapsed().as_secs() < 2);
}
