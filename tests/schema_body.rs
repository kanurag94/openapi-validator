//! Schema semantics, checked through JSON request bodies.

use openapi_validator::{Options, ProblemKind, Report, Request, Spec};

fn doc(version: &str, schema: &str, components: &str) -> String {
    format!(
        r#"{{"openapi": "{version}", "info": {{"title": "t", "version": "1"}},
"paths": {{"/t": {{"post": {{
  "requestBody": {{"required": true, "content": {{"application/json": {{"schema": {schema}}}}}}},
  "responses": {{"200": {{"description": "ok"}}}}}}}}}},
"components": {{"schemas": {{{components}}}}}}}"#
    )
}

fn load(version: &str, schema: &str, components: &str) -> Spec {
    let d = doc(version, schema, components);
    Spec::load(d.as_bytes(), Options::default()).unwrap_or_else(|e| panic!("{e}\n{d}"))
}

/// The same, with `Options::gateway_profile()`.
fn load_gw(version: &str, schema: &str, components: &str) -> Spec {
    let d = doc(version, schema, components);
    Spec::load(d.as_bytes(), Options::gateway_profile()).unwrap_or_else(|e| panic!("{e}\n{d}"))
}

fn post(spec: &Spec, body: &str) -> Report {
    spec.validate_request(&Request {
        method: "POST",
        path: "/t",
        headers: &[("Content-Type", "application/json")],
        body: body.as_bytes(),
        ..Request::default()
    })
}

fn ok(spec: &Spec, body: &str) {
    let r = post(spec, body);
    assert!(r.is_valid(), "expected {body} to be valid, got:\n{r}");
}

fn bad(spec: &Spec, body: &str, location: &str) {
    let r = post(spec, body);
    assert!(!r.is_valid(), "expected {body} to be invalid");
    assert!(
        r.problems().iter().any(|p| p.location == location),
        "expected a problem at {location} for {body}, got:\n{r}"
    );
}

// OpenAPI 3.0.4 §4.7.24.1 `nullable`: only with `type` in the same object.
#[test]
fn oas30_nullable_applies_only_with_type() {
    let s = load("3.0.3", r#"{"type": "string", "nullable": true}"#, "");
    ok(&s, "null");
    ok(&s, r#""x""#);
    let s = load(
        "3.0.3",
        r#"{"nullable": true, "allOf": [{"type": "string"}]}"#,
        "",
    );
    bad(&s, "null", "body");
    let s = load("3.0.3", r#"{"type": "string"}"#, "");
    bad(&s, "null", "body");
}

// OpenAPI 3.0.4 §4.7.24.1: other keywords keep their say, so `enum` without
// `null` still refuses null.
#[test]
fn oas30_nullable_respects_enum() {
    let s = load(
        "3.0.3",
        r#"{"type": "string", "nullable": true, "enum": ["a", "b"]}"#,
        "",
    );
    bad(&s, "null", "body");
    ok(&s, r#""a""#);
    let s = load(
        "3.0.3",
        r#"{"type": "string", "nullable": true, "enum": ["a", null]}"#,
        "",
    );
    ok(&s, "null");
}

// `nullable` never rewrites `oneOf`, and never touches `enum` or `default`.
#[test]
fn oas30_nullable_leaves_composition_and_values_alone() {
    let s = load(
        "3.0.3",
        r#"{"type": "object", "nullable": true, "oneOf": [{"required": ["a"]}, {"required": ["b"]}], "default": {"nullable": true}}"#,
        "",
    );
    // `null` passes the type, then matches both branches (`required` only
    // constrains objects), so oneOf fails: nullable is not pushed into it.
    bad(&s, "null", "body");
    ok(&s, r#"{"a": 1}"#);
    bad(&s, r#"{"a": 1, "b": 2}"#, "body");
    let s = load(
        "3.0.3",
        r#"{"enum": [{"nullable": true, "type": "x"}]}"#,
        "",
    );
    ok(&s, r#"{"nullable": true, "type": "x"}"#);
}

// OpenAPI 3.1.1 §4.8.24: `nullable` is not a 3.1 keyword; it is an annotation.
#[test]
fn oas31_nullable_is_an_annotation() {
    let s = load("3.1.0", r#"{"type": "string", "nullable": true}"#, "");
    bad(&s, "null", "body");
    ok(&s, r#""x""#);
}

// OpenAPI 3.0.4 §4.7.23 Reference Object: siblings of `$ref` are ignored.
#[test]
fn oas30_ref_siblings_are_ignored() {
    let s = load(
        "3.0.3",
        r##"{"$ref": "#/components/schemas/S", "maxLength": 1}"##,
        r#""S": {"type": "string"}"#,
    );
    ok(&s, r#""long string""#);
    let s = load(
        "3.0.3",
        r##"{"type": "object", "properties": {"p": {"$ref": "#/components/schemas/S", "maxLength": 1}}}"##,
        r#""S": {"type": "string"}"#,
    );
    ok(&s, r#"{"p": "long string"}"#);
}

// JSON Schema 2020-12 §8.2.3.1: in 3.1, `$ref` siblings apply.
#[test]
fn oas31_ref_siblings_apply() {
    let s = load(
        "3.1.0",
        r##"{"$ref": "#/components/schemas/S", "maxLength": 1}"##,
        r#""S": {"type": "string"}"#,
    );
    bad(&s, r#""long""#, "body");
    let s = load(
        "3.1.0",
        r##"{"type": "object", "properties": {"p": {"$ref": "#/components/schemas/S", "maxLength": 1}}}"##,
        r#""S": {"type": "string"}"#,
    );
    bad(&s, r#"{"p": "long"}"#, "body/p");
}

// 3.0 integers are the JSON integer production; 3.1 integers are numbers
// with no fractional part (JSON Schema 2020-12 core §4.2.1).
#[test]
fn integer_meaning_by_version() {
    // OpenAPI 3.0.4 §4.4: integers are mathematical by default.
    let s = load("3.0.3", r#"{"type": "integer"}"#, "");
    ok(&s, "12");
    ok(&s, "1.0");
    bad(&s, "1.5", "body");
    // OpenAPI 3.0.3 §4.4 reading, through the gateway profile.
    let s = load_gw("3.0.3", r#"{"type": "integer"}"#, "");
    ok(&s, "12");
    bad(&s, "1.0", "body");
    bad(&s, "1e2", "body");
    let s = load("3.1.0", r#"{"type": "integer"}"#, "");
    ok(&s, "1.0");
    ok(&s, "1e2");
    bad(&s, "1.5", "body");
}

// JSON numbers are compared exactly: int64 bounds, values past 2^53.
#[test]
fn numbers_are_exact() {
    let s = load("3.0.3", r#"{"type": "integer", "format": "int64"}"#, "");
    ok(&s, "9223372036854775807");
    bad(&s, "9223372036854775808", "body");
    let s = load(
        "3.1.0",
        r#"{"type": "integer", "maximum": 9007199254740993}"#,
        "",
    );
    ok(&s, "9007199254740993");
    bad(&s, "9007199254740994", "body");
    let s = load("3.1.0", r#"{"multipleOf": 0.01}"#, "");
    ok(&s, "19.99");
    bad(&s, "19.999", "body");
    let s = load("3.1.0", r#"{"enum": [9007199254740993]}"#, "");
    bad(&s, "9007199254740992", "body");
    ok(&s, "9007199254740993.0");
    let s = load("3.0.3", r#"{"type": "integer", "format": "int32"}"#, "");
    bad(&s, "2147483648", "body");
}

// OpenAPI 3.0.4 §4.7.24.1: boolean exclusiveMinimum/exclusiveMaximum.
#[test]
fn oas30_boolean_exclusive_bounds() {
    let s = load(
        "3.0.3",
        r#"{"type": "number", "minimum": 1, "exclusiveMinimum": true, "maximum": 5, "exclusiveMaximum": false}"#,
        "",
    );
    bad(&s, "1", "body");
    ok(&s, "1.5");
    ok(&s, "5");
    bad(&s, "5.01", "body");
}

// Count keywords written as floats are read as counts; a negative count is
// dropped (with a load warning) rather than refusing the document.
#[test]
fn count_keywords_written_as_floats() {
    let s = load(
        "3.0.3",
        r#"{"type": "array", "maxItems": 2.0, "minItems": 1e0}"#,
        "",
    );
    ok(&s, "[1, 2]");
    bad(&s, "[1, 2, 3]", "body");
    bad(&s, "[]", "body");
    let s = load("3.0.3", r#"{"type": "array", "maxItems": 1.79e308}"#, "");
    ok(&s, "[1, 2, 3]");
    let d = doc("3.1.0", r#"{"type": "string", "minLength": -1}"#, "");
    let s = Spec::load(d.as_bytes(), Options::default()).unwrap();
    assert!(s.warnings().iter().any(|w| w.message.contains("negative")));
    ok(&s, r#""""#);
}

// JSON Schema 2020-12 §4.3.2: boolean schemas everywhere.
#[test]
fn boolean_schemas() {
    let s = load("3.1.0", "false", "");
    bad(&s, "{}", "body");
    let s = load("3.1.0", "true", "");
    ok(&s, r#"{"anything": [1]}"#);
    let s = load(
        "3.1.0",
        r#"{"properties": {"a": false, "b": true}, "patternProperties": {"^x": false}, "items": false}"#,
        "",
    );
    bad(&s, r#"{"a": 1}"#, "body/a");
    ok(&s, r#"{"b": 1}"#);
    bad(&s, r#"{"xy": 1}"#, "body/xy");
    bad(&s, "[1]", "body/0");
    ok(&s, "[]");
}

// OpenAPI 3.0.4 §4.7.24.1 and 3.1.1 §4.8.24.4: readOnly properties drop out
// of `required` for requests, including through `$ref`; sending one is
// allowed (SHOULD NOT, not MUST NOT).
#[test]
fn read_only_is_not_required_in_requests() {
    let comps = r##""Id": {"type": "string", "readOnly": true},
        "Pet": {"type": "object", "required": ["id", "name"], "properties": {"id": {"$ref": "#/components/schemas/Id"}, "name": {"type": "string"}}}"##;
    // 3.0 relaxes required by default; 3.1 only with the gateway profile
    // (OpenAPI 3.1.1 §4.8.24.3.2 keeps required in force).
    let s31 = load("3.1.0", r##"{"$ref": "#/components/schemas/Pet"}"##, comps);
    bad(&s31, r#"{"name": "rex"}"#, "body/id");
    for (v, gw) in [("3.0.3", false), ("3.1.0", true)] {
        let s = if gw {
            load_gw(v, r##"{"$ref": "#/components/schemas/Pet"}"##, comps)
        } else {
            load(v, r##"{"$ref": "#/components/schemas/Pet"}"##, comps)
        };
        ok(&s, r#"{"name": "rex"}"#);
        ok(&s, r#"{"id": "1", "name": "rex"}"#);
        bad(&s, r#"{"id": "1"}"#, "body/name");
    }
    let s = load_gw(
        "3.1.0",
        r#"{"allOf": [{"properties": {"id": {"type": "string", "readOnly": true}}}, {"required": ["id"]}]}"#,
        "",
    );
    ok(&s, "{}");
}

// Formats asserted by this library; unknown formats are annotations.
#[test]
fn formats() {
    let cases = [
        ("date", r#""2024-02-29""#, r#""2023-02-29""#),
        (
            "date-time",
            r#""2024-01-01T10:00:00Z""#,
            r#""2024-01-01 10:00:00""#,
        ),
        ("time", r#""10:00:00+01:00""#, r#""10:00""#),
        ("email", r#""a@example.com""#, r#""a@""#),
        (
            "uuid",
            r#""2eb8aa08-aa98-11ea-b4aa-73b441d16380""#,
            r#""2eb8aa08""#,
        ),
        (
            "uri",
            r#""https://a.example/x""#,
            r#""https://a.example/x y""#,
        ),
        ("uri-reference", r#""../x?y""#, r#""a b""#),
        ("ipv4", r#""10.0.0.1""#, r#""10.0.0.256""#),
        ("ipv6", r#""::1""#, r#""::g""#),
        ("hostname", r#""example.com""#, r#""-example.com""#),
    ];
    for (f, good, wrong) in cases {
        let s = load_gw(
            "3.1.0",
            &format!(r#"{{"type": "string", "format": "{f}"}}"#),
            "",
        );
        ok(&s, good);
        bad(&s, wrong, "body");
    }
    let s = load_gw("3.1.0", r#"{"type": "string", "format": "made-up"}"#, "");
    ok(&s, r#""anything""#);
    // OpenAPI 3.0.4 §4.4: byte is base64 (RFC 4648 §4).
    let s = load_gw("3.0.3", r#"{"type": "string", "format": "byte"}"#, "");
    bad(&s, r#""not base64!""#, "body");
    ok(&s, r#""aGVsbG8=""#);
}

// ECMA-262 patterns, with ASCII \d and linear-time matching.
#[test]
fn patterns() {
    let s = load("3.1.0", r#"{"type": "string", "pattern": "^\\d{3}$"}"#, "");
    ok(&s, r#""123""#);
    bad(&s, r#""١٢٣""#, "body");
    let s = load("3.1.0", r#"{"type": "string", "pattern": "^(a+)+$"}"#, "");
    let long = format!("\"{}!\"", "a".repeat(50_000));
    let t = std::time::Instant::now();
    bad(&s, &long, "body");
    assert!(t.elapsed().as_millis() < 500);
    // Lookahead runs on the bounded engine by default...
    let s = load("3.1.0", r#"{"type": "string", "pattern": "^(?=a)"}"#, "");
    ok(&s, r#""abc""#);
    bad(&s, r#""bc""#, "body");
    // ...and is refused at load, naming the pattern, by the gateway profile.
    let d = doc("3.1.0", r#"{"type": "string", "pattern": "^(?=a)"}"#, "");
    let e = Spec::load(d.as_bytes(), Options::gateway_profile())
        .err()
        .expect("lookahead refused");
    assert!(e.to_string().contains("(?=a)"), "{e}");
}

// OpenAPI 3.1.1 §4.8.25 Discriminator Object: the value must be mapped, and
// the payload is checked against the schema it names.
#[test]
fn discriminator_is_enforced() {
    let comps = r#""Cat": {"type": "object", "required": ["kind", "meow"], "properties": {"kind": {"type": "string"}, "meow": {"type": "boolean"}}},
        "Dog": {"type": "object", "required": ["kind", "bark"], "properties": {"kind": {"type": "string"}, "bark": {"type": "boolean"}}}"#;
    let schema = r##"{"oneOf": [{"$ref": "#/components/schemas/Cat"}, {"$ref": "#/components/schemas/Dog"}],
        "discriminator": {"propertyName": "kind", "mapping": {"cat": "#/components/schemas/Cat", "dog": "Dog"}}}"##;
    for v in ["3.0.3", "3.1.0"] {
        let s = load_gw(v, schema, comps);
        ok(&s, r#"{"kind": "cat", "meow": true}"#);
        ok(&s, r#"{"kind": "dog", "bark": true}"#);
        bad(&s, r#"{"kind": "cow", "meow": true}"#, "body/kind");
        bad(&s, r#"{"kind": "dog", "meow": true}"#, "body/bark");
        // Valid for both branches; the discriminator picks one.
        ok(&s, r#"{"kind": "cat", "meow": true, "bark": true}"#);
    }
}

// The common inheritance shape: the base lists its children in oneOf and
// each child extends the base through allOf.
#[test]
fn discriminator_with_inheritance_cycle() {
    let comps = r##""Pet": {"type": "object", "required": ["kind"], "properties": {"kind": {"type": "string"}},
            "oneOf": [{"$ref": "#/components/schemas/Dog"}, {"$ref": "#/components/schemas/Cat"}],
            "discriminator": {"propertyName": "kind"}},
        "Dog": {"allOf": [{"$ref": "#/components/schemas/Pet"}, {"type": "object", "properties": {"bark": {"type": "boolean"}}}]},
        "Cat": {"allOf": [{"$ref": "#/components/schemas/Pet"}, {"type": "object", "properties": {"meow": {"type": "boolean"}}}]}"##;
    for v in ["3.0.3", "3.1.0"] {
        let s = load_gw(v, r##"{"$ref": "#/components/schemas/Pet"}"##, comps);
        ok(&s, r#"{"kind": "Dog", "bark": true}"#);
        bad(&s, r#"{"kind": "Dog", "bark": "loud"}"#, "body/bark");
        bad(&s, r#"{"kind": "Cow"}"#, "body/kind");
        bad(&s, "{}", "body/kind");
    }
}

// A JSON `null` body is not an object.
#[test]
fn null_body_fails_object_schema() {
    let s = load("3.0.3", r#"{"type": "object"}"#, "");
    bad(&s, "null", "body");
}

// Problems are located with JSON Pointer paths under `body`.
#[test]
fn problem_locations() {
    let s = load(
        "3.1.0",
        r#"{"type": "object", "properties": {"items": {"type": "array", "items": {"type": "integer", "minimum": 0}}}, "additionalProperties": false}"#,
        "",
    );
    let r = post(&s, r#"{"items": [1, 2, 3, -4], "extra": 1}"#);
    let locs: Vec<&str> = r.problems().iter().map(|p| p.location.as_str()).collect();
    assert!(locs.contains(&"body/items/3"), "{r}");
    assert!(locs.contains(&"body/extra"), "{r}");
    assert!(r
        .problems()
        .iter()
        .all(|p| p.kind == ProblemKind::SchemaViolation));
}

// Bodies: missing required body, malformed JSON, media types.
#[test]
fn request_body_basics() {
    let s = load("3.1.0", r#"{"type": "object"}"#, "");
    let r = s.validate_request(&Request {
        method: "POST",
        path: "/t",
        ..Request::default()
    });
    assert_eq!(r.problems()[0].kind, ProblemKind::MissingBody);
    let r = post(&s, "{\"a\": 1, \"a\": 2}");
    assert_eq!(r.problems()[0].kind, ProblemKind::MalformedBody);
    let r = s.validate_request(&Request {
        method: "POST",
        path: "/t",
        headers: &[("content-type", "application/problem+json; charset=utf-8")],
        body: b"{}",
        ..Request::default()
    });
    assert!(
        r.is_valid() || r.problems()[0].kind == ProblemKind::UnsupportedMediaType,
        "{r}"
    );
    let r = s.validate_request(&Request {
        method: "POST",
        path: "/t",
        headers: &[("Content-Type", "text/csv")],
        body: b"a,b",
        ..Request::default()
    });
    assert_eq!(r.problems()[0].kind, ProblemKind::UnsupportedMediaType);
}

// Recursive anyOf must not cost 2^depth, valid or not.
#[test]
fn recursive_any_of_is_fast() {
    let comps = r##""N": {"anyOf": [
        {"type": "object", "properties": {"c": {"$ref": "#/components/schemas/N"}, "v": {"type": "integer"}}},
        {"type": "object", "properties": {"c": {"$ref": "#/components/schemas/N"}, "v": {"type": "number"}}}]}"##;
    let s = load("3.1.0", r##"{"$ref": "#/components/schemas/N"}"##, comps);
    let mut body = String::from(r#"{"v": "x"}"#);
    for _ in 0..24 {
        body = format!(r#"{{"v": 1, "c": {body}}}"#);
    }
    let t = std::time::Instant::now();
    let r = post(&s, &body);
    assert!(!r.is_valid());
    assert!(t.elapsed().as_millis() < 200, "took {:?}", t.elapsed());
}
