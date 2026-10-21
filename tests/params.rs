//! Parameter Objects (OpenAPI 3.1.1 §4.8.12; 3.0.4 §4.8.12).

use openapi_validator::{Options, ProblemKind, Report, Request, Spec};

fn spec_with(version: &str, path: &str, params: &str, opts: Options) -> Spec {
    let d = format!(
        r#"{{"openapi": "{version}", "info": {{"title": "t", "version": "1"}},
"paths": {{"{path}": {{"get": {{"parameters": [{params}], "responses": {{"200": {{"description": "ok"}}}}}}}}}}}}"#
    );
    Spec::load(d.as_bytes(), opts).unwrap_or_else(|e| panic!("{e}\n{d}"))
}

fn spec(path: &str, params: &str) -> Spec {
    spec_with("3.1.0", path, params, Options::default())
}

fn get(s: &Spec, path: &str, query: &str, headers: &[(&str, &str)]) -> Report {
    s.validate_request(&Request {
        method: "GET",
        path,
        query,
        headers,
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

// §4.8.12.4 style examples, path parameters.
#[test]
fn path_styles() {
    let arr = r#"{"type": "array", "items": {"type": "integer"}, "maxItems": 3}"#;
    let obj = r#"{"type": "object", "properties": {"role": {"type": "string"}, "n": {"type": "integer"}}, "required": ["role"]}"#;
    let cases: &[(&str, bool, &str, &str, bool)] = &[
        ("simple", false, arr, "3,4,5", true),
        ("simple", false, arr, "3,4,5,6", false),
        ("simple", false, arr, "3,x", false),
        ("simple", true, obj, "role=admin,n=5", true),
        ("simple", false, obj, "role,admin,n,5", true),
        ("simple", false, obj, "n,5", false),
        ("label", false, arr, ".3,4,5", true),
        ("label", true, arr, ".3.4.5", true),
        ("label", true, obj, ".role=admin.n=5", true),
        ("label", false, arr, "3,4", false),
        ("matrix", false, arr, ";id=3,4,5", true),
        ("matrix", true, arr, ";id=3;id=4;id=5", true),
        ("matrix", true, obj, ";role=admin;n=5", true),
        ("matrix", false, obj, ";id=role,admin,n,5", true),
        ("matrix", false, arr, ";other=3", false),
    ];
    for (style, explode, schema, raw, valid) in cases {
        let s = spec(
            "/p/{id}",
            &format!(
                r#"{{"name": "id", "in": "path", "required": true, "style": "{style}", "explode": {explode}, "schema": {schema}}}"#
            ),
        );
        let r = get(&s, &format!("/p/{raw}"), "", &[]);
        assert_eq!(r.is_valid(), *valid, "{style} explode={explode} {raw}: {r}");
        if !valid {
            assert!(
                r.problems()
                    .iter()
                    .all(|p| p.location.starts_with("path.id")),
                "{r}"
            );
        }
    }
}

// Percent-encoded commas stay inside an item.
#[test]
fn path_simple_array_keeps_encoded_delimiters() {
    let s = spec(
        "/p/{id}",
        r#"{"name": "id", "in": "path", "required": true, "schema": {"type": "array", "items": {"enum": ["a,b", "c"]}}}"#,
    );
    ok(get(&s, "/p/a%2Cb,c", "", &[]));
    bad(get(&s, "/p/a,b,c", "", &[]), "path.id/0");
}

// §4.8.12.4 query styles.
#[test]
fn query_styles() {
    let arr = r#"{"type": "array", "items": {"type": "integer"}, "minItems": 2, "maxItems": 3}"#;
    let cases: &[(&str, bool, &str, bool)] = &[
        ("form", true, "id=3&id=4&id=5", true),
        ("form", true, "id=3&id=4&id=5&id=6", false),
        ("form", true, "id=3", false),
        ("form", false, "id=3,4,5", true),
        ("form", false, "id=3,4,5,6", false),
        ("spaceDelimited", false, "id=3%204%205", true),
        ("spaceDelimited", false, "id=3+4", true),
        ("pipeDelimited", false, "id=3|4|5", true),
        ("pipeDelimited", false, "id=3%7C4", true),
        ("pipeDelimited", false, "id=3|4|5|6", false),
    ];
    for (style, explode, q, valid) in cases {
        let s = spec(
            "/q",
            &format!(
                r#"{{"name": "id", "in": "query", "style": "{style}", "explode": {explode}, "schema": {arr}}}"#
            ),
        );
        let r = get(&s, "/q", q, &[]);
        assert_eq!(r.is_valid(), *valid, "{style} explode={explode} {q}: {r}");
    }
}

// `list=` is an empty array, which then meets minItems.
#[test]
fn empty_query_array() {
    for explode in [true, false] {
        let s = spec(
            "/q",
            &format!(
                r#"{{"name": "list", "in": "query", "explode": {explode}, "schema": {{"type": "array", "items": {{"type": "integer"}}}}}}"#
            ),
        );
        ok(get(&s, "/q", "list=", &[]));
        let s = spec(
            "/q",
            &format!(
                r#"{{"name": "list", "in": "query", "explode": {explode}, "schema": {{"type": "array", "minItems": 1}}}}"#
            ),
        );
        bad(get(&s, "/q", "list=", &[]), "query.list");
    }
}

// Exploded objects take only their declared properties, and an absent
// object never stops later parameters from being checked.
#[test]
fn exploded_object_takes_declared_properties() {
    let s = spec(
        "/q",
        r#"{"name": "filter", "in": "query", "schema": {"type": "object", "properties": {"color": {"type": "string"}, "size": {"type": "integer"}}, "additionalProperties": false}},
           {"name": "limit", "in": "query", "required": true, "schema": {"type": "integer", "maximum": 10}}"#,
    );
    ok(get(&s, "/q", "color=red&size=3&limit=5&other=1", &[]));
    bad(get(&s, "/q", "size=big&limit=5", &[]), "query.filter/size");
    bad(get(&s, "/q", "limit=50", &[]), "query.limit");
    bad(get(&s, "/q", "", &[]), "query.limit");
}

// §4.8.12.4 deepObject.
#[test]
fn deep_object() {
    let s = spec(
        "/q",
        r#"{"name": "f", "in": "query", "style": "deepObject", "explode": true, "schema": {"type": "object", "properties": {"min": {"type": "integer"}, "max": {"type": "integer"}}, "required": ["min"]}}"#,
    );
    ok(get(&s, "/q", "f[min]=1&f[max]=5", &[]));
    ok(get(&s, "/q", "f%5Bmin%5D=1", &[]));
    bad(get(&s, "/q", "f[min]=x", &[]), "query.f/min");
    bad(get(&s, "/q", "f[max]=5", &[]), "query.f/min");
}

// RFC 3986 §3.4: only `&` separates pairs; a bad escape is a problem; no
// silent pair limit.
#[test]
fn query_parsing() {
    let s = spec(
        "/q",
        r#"{"name": "a", "in": "query", "schema": {"type": "string", "pattern": "^x;y$"}}"#,
    );
    ok(get(&s, "/q", "a=x;y", &[]));
    let r = get(&s, "/q", "a=%zz", &[]);
    assert_eq!(r.problems()[0].kind, ProblemKind::MalformedQuery, "{r}");
    let r = get(&s, "/q", "%zz=1", &[]);
    assert_eq!(r.problems()[0].kind, ProblemKind::MalformedQuery, "{r}");
    let s = spec(
        "/q",
        r#"{"name": "last", "in": "query", "required": true, "schema": {"type": "integer"}}"#,
    );
    let mut q: String = (0..3000).map(|i| format!("k{i}=v&")).collect();
    q.push_str("last=x");
    bad(get(&s, "/q", &q, &[]), "query.last");
}

// Repeated primitive query values are each checked.
#[test]
fn repeated_primitive_query_values() {
    let s = spec(
        "/q",
        r#"{"name": "n", "in": "query", "schema": {"type": "integer"}}"#,
    );
    ok(get(&s, "/q", "n=1&n=2", &[]));
    bad(get(&s, "/q", "n=1&n=x", &[]), "query.n");
}

// Coercion follows the schema: "5" is an integer, "05" is not, and a schema
// that allows strings accepts the text as is.
#[test]
fn coercion() {
    let s = spec(
        "/q",
        r#"{"name": "n", "in": "query", "schema": {"type": "integer"}}"#,
    );
    ok(get(&s, "/q", "n=5", &[]));
    bad(get(&s, "/q", "n=05", &[]), "query.n");
    bad(get(&s, "/q", "n=5.5", &[]), "query.n");
    let s = spec(
        "/q",
        r#"{"name": "n", "in": "query", "schema": {"type": ["string", "integer"], "maxLength": 1}}"#,
    );
    ok(get(&s, "/q", "n=123", &[]));
    let s = spec(
        "/q",
        r#"{"name": "b", "in": "query", "schema": {"type": "boolean"}}"#,
    );
    ok(get(&s, "/q", "b=true", &[]));
    bad(get(&s, "/q", "b=yes", &[]), "query.b");
    let s = spec_with(
        "3.0.3",
        "/q",
        r#"{"name": "n", "in": "query", "schema": {"type": "integer"}}"#,
        Options::default(),
    );
    bad(get(&s, "/q", "n=1.0", &[]), "query.n");
}

// Boolean schemas and schema-less content on parameters.
#[test]
fn unconstrained_parameters() {
    let s = spec(
        "/q",
        r#"{"name": "q", "in": "query", "required": true, "schema": true}"#,
    );
    ok(get(&s, "/q", "q=1", &[]));
    bad(get(&s, "/q", "", &[]), "query.q");
    let s = spec("/q", r#"{"name": "q", "in": "query", "schema": false}"#);
    bad(get(&s, "/q", "q=1", &[]), "query.q");
    ok(get(&s, "/q", "", &[]));
    let s = spec(
        "/q",
        r#"{"name": "q", "in": "query", "content": {"application/json": {}}}"#,
    );
    ok(get(&s, "/q", "q=not-json", &[]));
    let s = spec(
        "/q",
        r#"{"name": "q", "in": "query", "schema": {"type": "array"}}"#,
    );
    ok(get(&s, "/q", "q=a&q=1", &[]));
}

// §4.8.12.3 parameters described by `content`, in query, header and cookie.
#[test]
fn content_parameters() {
    let schema = r#"{"application/json": {"schema": {"type": "object", "required": ["lat"], "properties": {"lat": {"type": "number"}}}}}"#;
    let s = spec(
        "/q",
        &format!(r#"{{"name": "c", "in": "query", "content": {schema}}}"#),
    );
    ok(get(&s, "/q", "c=%7B%22lat%22%3A1.5%7D", &[]));
    bad(get(&s, "/q", "c=%7B%7D", &[]), "query.c/lat");
    bad(get(&s, "/q", "c=nope", &[]), "query.c");
    let s = spec(
        "/q",
        &format!(r#"{{"name": "X-Pos", "in": "header", "content": {schema}}}"#),
    );
    ok(get(&s, "/q", "", &[("x-pos", r#"{"lat": 2}"#)]));
    bad(
        get(&s, "/q", "", &[("X-Pos", r#"{"lat": "n"}"#)]),
        "header.x-pos/lat",
    );
    let s = spec(
        "/q",
        &format!(r#"{{"name": "pos", "in": "cookie", "content": {schema}}}"#),
    );
    ok(get(&s, "/q", "", &[("Cookie", r#"a=b; pos={"lat":3}"#)]));
    bad(
        get(&s, "/q", "", &[("Cookie", r#"pos={}"#)]),
        "cookie.pos/lat",
    );
}

// Headers: every line of a repeated header is checked; names compare
// case-insensitively (RFC 9110 §5.1).
#[test]
fn header_parameters() {
    let s = spec(
        "/q",
        r#"{"name": "X-Tag", "in": "header", "required": true, "schema": {"type": "string", "maxLength": 3}}"#,
    );
    ok(get(&s, "/q", "", &[("x-tag", "abc")]));
    bad(
        get(&s, "/q", "", &[("X-TAG", "abc"), ("x-tag", "toolong")]),
        "header.x-tag",
    );
    bad(get(&s, "/q", "", &[]), "header.x-tag");
    let s = spec(
        "/q",
        r#"{"name": "X-Ids", "in": "header", "schema": {"type": "array", "items": {"type": "integer"}, "maxItems": 2}}"#,
    );
    ok(get(&s, "/q", "", &[("X-Ids", "1, 2")]));
    bad(get(&s, "/q", "", &[("X-Ids", "1,2,3")]), "header.x-ids");
}

// §4.8.12.2: header parameters named Accept, Content-Type or Authorization
// are ignored.
#[test]
fn reserved_header_parameters_are_ignored() {
    for name in ["Accept", "Content-Type", "Authorization"] {
        let s = spec(
            "/q",
            &format!(
                r#"{{"name": "{name}", "in": "header", "required": true, "schema": {{"enum": ["never"]}}}}"#
            ),
        );
        ok(get(&s, "/q", "", &[]));
        ok(get(&s, "/q", "", &[(name, "something")]));
    }
}

// Cookies, including arrays with maxItems.
#[test]
fn cookie_parameters() {
    let s = spec(
        "/q",
        r#"{"name": "ids", "in": "cookie", "explode": false, "schema": {"type": "array", "items": {"type": "integer"}, "maxItems": 2}},
           {"name": "sid", "in": "cookie", "required": true, "schema": {"type": "string", "minLength": 2}}"#,
    );
    ok(get(&s, "/q", "", &[("Cookie", "ids=1,2; sid=ab")]));
    bad(
        get(&s, "/q", "", &[("Cookie", "ids=1,2,3; sid=ab")]),
        "cookie.ids",
    );
    bad(get(&s, "/q", "", &[("Cookie", "ids=1")]), "cookie.sid");
    ok(get(
        &s,
        "/q",
        "",
        &[("Cookie", "ids=1"), ("Cookie", "sid=xy")],
    ));
}

// §4.8.10.1: an operation parameter replaces the path item's one with the
// same name and location.
#[test]
fn operation_parameter_overrides_path_item() {
    let d = r#"{"openapi": "3.1.0", "info": {"title": "t", "version": "1"},
"paths": {"/o": {"parameters": [{"name": "n", "in": "query", "required": true, "schema": {"type": "integer"}},
                                {"name": "X-A", "in": "header", "schema": {"type": "integer"}}],
  "get": {"parameters": [{"name": "n", "in": "query", "schema": {"type": "string"}},
                         {"name": "x-a", "in": "header", "schema": {"type": "string"}}],
          "responses": {"200": {"description": "ok"}}}}}}"#;
    let s = Spec::load(d.as_bytes(), Options::default()).unwrap();
    ok(get(&s, "/o", "", &[]));
    ok(get(&s, "/o", "n=abc", &[("X-A", "text")]));
}

// Undeclared query parameters, headers and cookies, when not allowed.
#[test]
fn unknown_parameters() {
    let opts = Options {
        allow_unknown_query: false,
        allow_unknown_headers: false,
        allow_unknown_cookies: false,
        ..Options::default()
    };
    let s = spec_with(
        "3.1.0",
        "/q",
        r#"{"name": "a", "in": "query", "schema": {"type": "string"}},
           {"name": "f", "in": "query", "style": "deepObject", "schema": {"type": "object"}},
           {"name": "X-A", "in": "header", "schema": {"type": "string"}},
           {"name": "c", "in": "cookie", "schema": {"type": "string"}}"#,
        opts,
    );
    ok(get(
        &s,
        "/q",
        "a=1&f[x]=2",
        &[
            ("X-A", "1"),
            ("Host", "h"),
            ("User-Agent", "u"),
            ("Accept", "*/*"),
            ("Content-Length", "0"),
            ("Connection", "close"),
            ("Transfer-Encoding", "chunked"),
            ("Authorization", "x"),
            ("Content-Type", "x/y"),
            ("Cookie", "c=1"),
        ],
    ));
    bad(get(&s, "/q", "b=1", &[]), "query.b");
    bad(get(&s, "/q", "", &[("X-B", "1")]), "header.x-b");
    bad(get(&s, "/q", "", &[("Cookie", "d=1")]), "cookie.d");
    let s = spec_with(
        "3.1.0",
        "/q",
        r#"{"name": "a", "in": "query", "schema": {"type": "string"}}"#,
        Options {
            allow_unknown_headers: false,
            ignored_headers: vec!["x-trace".into()],
            ..Options::default()
        },
    );
    ok(get(&s, "/q", "", &[("X-Trace", "1")]));
    bad(get(&s, "/q", "", &[("Host", "h")]), "header.host");
}
