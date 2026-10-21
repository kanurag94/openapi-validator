//! Security Requirement and Security Scheme Objects (OpenAPI 3.1.1 §4.8.27,
//! §4.8.30).

use std::sync::Arc;

use openapi_validator::{LoadError, Options, ProblemKind, Report, Request, Spec};

const SCHEMES: &str = r#""key_q": {"type": "apiKey", "in": "query", "name": "api_key"},
"key_h": {"type": "apiKey", "in": "header", "name": "X-Key"},
"key_c": {"type": "apiKey", "in": "cookie", "name": "sid"},
"basic": {"type": "http", "scheme": "Basic"},
"bearer": {"type": "http", "scheme": "bearer", "bearerFormat": "JWT"},
"named_http": {"type": "http", "scheme": "bearer", "name": "ignored"},
"oauth": {"type": "oauth2", "flows": {"clientCredentials": {"tokenUrl": "https://t.example/token", "scopes": {"read": "r"}}}},
"oidc": {"type": "openIdConnect", "openIdConnectUrl": "https://t.example/.well-known/openid-configuration"},
"mtls": {"type": "mutualTLS"}"#;

fn spec_opts(doc_security: &str, op_security: Option<&str>, opts: Options) -> Spec {
    let op_sec = op_security
        .map(|s| format!(r#""security": {s},"#))
        .unwrap_or_default();
    let d = format!(
        r#"{{"openapi": "3.1.0", "info": {{"title": "t", "version": "1"}}, "security": {doc_security},
"paths": {{"/s": {{"get": {{{op_sec} "responses": {{"200": {{"description": "ok"}}}}}}}}}},
"components": {{"securitySchemes": {{{SCHEMES}}}}}}}"#
    );
    Spec::load(d.as_bytes(), opts).unwrap_or_else(|e| panic!("{e}\n{d}"))
}

fn spec(doc_security: &str, op_security: Option<&str>) -> Spec {
    spec_opts(doc_security, op_security, Options::default())
}

fn get(s: &Spec, query: &str, headers: &[(&str, &str)]) -> Report {
    s.validate_request(&Request {
        method: "GET",
        path: "/s",
        query,
        headers,
        ..Request::default()
    })
}

#[track_caller]
fn allowed(r: Report) {
    assert!(r.is_valid(), "expected allowed, got:\n{r}");
}

#[track_caller]
fn refused(r: Report) {
    assert_eq!(
        r.problems().iter().map(|p| p.kind).collect::<Vec<_>>(),
        vec![ProblemKind::MissingCredentials],
        "{r}"
    );
    assert_eq!(r.problems()[0].location, "security");
}

#[test]
fn api_key_in_query_header_and_cookie() {
    let s = spec(r#"[{"key_q": []}]"#, None);
    allowed(get(&s, "api_key=abc", &[]));
    refused(get(&s, "api_key=", &[]));
    refused(get(&s, "", &[]));
    let s = spec(r#"[{"key_h": []}]"#, None);
    allowed(get(&s, "", &[("x-key", "abc")]));
    refused(get(&s, "", &[("X-Key", "")]));
    let s = spec(r#"[{"key_c": []}]"#, None);
    allowed(get(&s, "", &[("Cookie", "a=1; sid=xyz")]));
    refused(get(&s, "", &[("Cookie", "sid=")]));
}

// §4.8.30.1: AND within a requirement, OR across requirements.
#[test]
fn and_within_or_across() {
    let s = spec(r#"[{"key_q": [], "key_h": []}, {"basic": []}]"#, None);
    allowed(get(&s, "api_key=1", &[("X-Key", "2")]));
    refused(get(&s, "api_key=1", &[]));
    allowed(get(&s, "", &[("Authorization", "Basic dTpw")]));
}

// §4.8.30.1: `{}` makes security optional; operation `security: []`
// replaces the document's requirements.
#[test]
fn optional_and_overridden() {
    let s = spec(r#"[{"key_q": []}, {}]"#, None);
    allowed(get(&s, "", &[]));
    let s = spec(r#"[{"key_q": []}]"#, Some("[]"));
    allowed(get(&s, "", &[]));
    let s = spec("[]", Some(r#"[{"key_h": []}]"#));
    refused(get(&s, "", &[]));
}

// RFC 9110 §11.4: the auth-scheme is a whole, case-insensitive token
// followed by a non-empty credential.
#[test]
fn http_schemes_match_whole_tokens() {
    let s = spec(r#"[{"bearer": []}]"#, None);
    allowed(get(&s, "", &[("Authorization", "Bearer t0k3n")]));
    allowed(get(&s, "", &[("authorization", "BEARER t0k3n")]));
    refused(get(&s, "", &[("Authorization", "Bearer")]));
    refused(get(&s, "", &[("Authorization", "Bearer   ")]));
    refused(get(&s, "", &[("Authorization", "Bearerx t0k3n")]));
    refused(get(&s, "", &[("Authorization", "Basic dTpw")]));
    // RFC 9110 §5.3: Authorization takes one value; two lines are a problem.
    let r = get(
        &s,
        "",
        &[
            ("Authorization", "Basic dTpw"),
            ("Authorization", "Bearer t"),
        ],
    );
    assert!(
        r.problems()
            .iter()
            .any(|p| p.location == "header.authorization"),
        "{r}"
    );
    let s = spec(r#"[{"basic": []}]"#, None);
    allowed(get(&s, "", &[("Authorization", "basic dTpw")]));
    refused(get(&s, "", &[("Authorization", "Bearer t")]));
}

// An http scheme carrying an unrelated `name` field still loads.
#[test]
fn http_scheme_with_extra_fields_loads() {
    let s = spec(r#"[{"named_http": []}]"#, None);
    allowed(get(&s, "", &[("Authorization", "Bearer x")]));
}

// oauth2 and openIdConnect need a bearer token (RFC 6750 §2.1).
#[test]
fn oauth2_and_oidc_need_a_bearer_token() {
    for name in ["oauth", "oidc"] {
        let s = spec(&format!(r#"[{{"{name}": ["read"]}}]"#), None);
        allowed(get(&s, "", &[("Authorization", "Bearer abc")]));
        refused(get(&s, "", &[]));
        refused(get(&s, "", &[("Authorization", "Basic abc")]));
    }
}

// mutualTLS is not visible in the HTTP message and is not refused.
#[test]
fn mutual_tls_is_not_refused() {
    let s = spec(r#"[{"mtls": []}]"#, None);
    allowed(get(&s, "", &[]));
}

// §4.8.30.2: every scheme name must be defined.
#[test]
fn unknown_scheme_is_a_load_error() {
    let d = r#"{"openapi": "3.1.0", "info": {"title": "t", "version": "1"}, "security": [{"nope": []}], "paths": {}}"#;
    assert!(matches!(
        Spec::load(d.as_bytes(), Options::default()),
        Err(LoadError::UnknownSecurityScheme { .. })
    ));
}

#[test]
fn hook_sees_credentials_and_can_refuse() {
    let opts = Options {
        security_hook: Some(Arc::new(|c: &openapi_validator::Credential<'_>| {
            if c.scheme_name == "bearer" && c.value == "good" {
                Ok(())
            } else {
                Err(format!("{} rejected", c.scheme_type))
            }
        })),
        ..Options::default()
    };
    let s = spec_opts(r#"[{"bearer": []}]"#, None, opts);
    allowed(get(&s, "", &[("Authorization", "Bearer good")]));
    let r = get(&s, "", &[("Authorization", "Bearer bad")]);
    refused(r.clone());
    assert!(r.problems()[0].message.contains("http rejected"), "{r}");
}

#[test]
fn checking_can_be_turned_off() {
    let s = spec_opts(
        r#"[{"key_q": []}]"#,
        None,
        Options {
            check_security: false,
            ..Options::default()
        },
    );
    allowed(get(&s, "", &[]));
}

// Security schemes' names count as declared for the unknown-parameter modes.
#[test]
fn scheme_names_count_as_declared() {
    let s = spec_opts(
        r#"[{"key_q": [], "key_h": [], "key_c": []}]"#,
        None,
        Options {
            allow_unknown_query: false,
            allow_unknown_headers: false,
            allow_unknown_cookies: false,
            ..Options::default()
        },
    );
    allowed(get(&s, "api_key=1", &[("X-Key", "2"), ("Cookie", "sid=3")]));
}
