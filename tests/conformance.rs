//! Conformance suite: every `tests/cases/**/*.yaml` file is one case written
//! from the specification text.
//!
//! ```yaml
//! title: An exploded array keeps maxItems across repeated pairs
//! ref: OpenAPI 3.1.1 §4.8.12.4
//! options:            # optional; Options fields by name
//!   allow_unknown_query: false
//! document: |         # the OpenAPI document (YAML or JSON)
//!   openapi: 3.1.0
//!   ...
//! request:            # method, path, query, headers, body (all optional
//!   method: GET       # but method and path)
//!   path: /items
//!   query: ids=1&ids=2
//!   headers: [["X-Tag", "a"]]
//!   body: '{"a": 1}'
//! response:           # optional: validates the response for the request
//!   status: 200
//!   headers: [["Content-Type", "application/json"]]
//!   body: '{}'
//! expect:
//!   valid: false      # or `load_error: true` for a refused document
//!   kind: invalid_parameter   # optional: some problem has this kind
//!   location: query.ids       # optional: some problem has this location
//! ```

use std::path::{Path, PathBuf};

use openapi_validator::{
    DiscriminatorMode, FormatAssertion, IntegerSemantics, Options, PatternEngine,
    ReadWriteRequired, Request, Response, ServerPaths, Spec,
};
use serde_json::Value;

fn files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|x| x.to_str()) == Some("yaml") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn options(v: Option<&Value>) -> Result<Options, String> {
    let mut o = Options::default();
    let Some(m) = v.and_then(Value::as_object) else {
        return Ok(o);
    };
    // `profile: gateway` starts from Options::gateway_profile(); other
    // options then adjust it.
    match m.get("profile").map(|p| p.as_str()) {
        None => {}
        Some(Some("gateway")) => o = Options::gateway_profile(),
        Some(_) => return Err("profile must be gateway".into()),
    }
    for (k, x) in m {
        let b = || {
            x.as_bool()
                .ok_or_else(|| format!("option {k} must be a boolean"))
        };
        let n = || {
            x.as_u64()
                .map(|n| n as usize)
                .ok_or_else(|| format!("option {k} must be a number"))
        };
        match k.as_str() {
            "validate_body" => o.validate_body = b()?,
            "allow_unknown_query" => o.allow_unknown_query = b()?,
            "allow_unknown_headers" => o.allow_unknown_headers = b()?,
            "allow_unknown_cookies" => o.allow_unknown_cookies = b()?,
            "strict_document" => o.strict_document = b()?,
            "check_security" => o.check_security = b()?,
            "profile" => {} // applied first, below
            "formats" => {
                o.formats = match x.as_str() {
                    Some("by-version") => FormatAssertion::ByVersion,
                    Some("always") => FormatAssertion::Always,
                    Some("never") => FormatAssertion::Never,
                    _ => return Err("formats must be by-version, always or never".into()),
                }
            }
            "discriminator" => {
                o.discriminator = match x.as_str() {
                    Some("annotation") => DiscriminatorMode::Annotation,
                    Some("enforce") => DiscriminatorMode::Enforce,
                    _ => return Err("discriminator must be annotation or enforce".into()),
                }
            }
            "integers" => {
                o.integers = match x.as_str() {
                    Some("mathematical") => IntegerSemantics::Mathematical,
                    Some("lexical-in-30") => IntegerSemantics::LexicalIn30,
                    _ => return Err("integers must be mathematical or lexical-in-30".into()),
                }
            }
            "patterns" => {
                o.patterns = match x.as_str() {
                    Some("bounded") => PatternEngine::Bounded,
                    Some("linear-only") => PatternEngine::LinearOnly,
                    _ => return Err("patterns must be bounded or linear-only".into()),
                }
            }
            "read_write_required" => {
                o.read_write_required = match x.as_str() {
                    Some("by-version") => ReadWriteRequired::ByVersion,
                    Some("relax-always") => ReadWriteRequired::RelaxAlways,
                    _ => {
                        return Err("read_write_required must be by-version or relax-always".into())
                    }
                }
            }
            "query_plus_as_space" => o.query_plus_as_space = b()?,
            "max_problems" => o.max_problems = n()?,
            "max_report_bytes" => o.max_report_bytes = n()?,
            "max_body_bytes" => o.max_body_bytes = n()?,
            "max_body_depth" => o.max_body_depth = n()?,
            "ignored_headers" => {
                o.ignored_headers = x
                    .as_array()
                    .ok_or("ignored_headers must be a list")?
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            }
            "server_paths" => {
                o.server_paths = match x.as_str() {
                    Some("either") => ServerPaths::Either,
                    Some("required") => ServerPaths::Required,
                    Some("ignored") => ServerPaths::Ignored,
                    _ => return Err("server_paths must be either, required or ignored".into()),
                }
            }
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(o)
}

fn text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

fn headers(v: Option<&Value>) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    for h in v.and_then(Value::as_array).into_iter().flatten() {
        let pair = h
            .as_array()
            .filter(|a| a.len() == 2)
            .ok_or("headers are [name, value] pairs")?;
        out.push((text(pair.first()), text(pair.get(1))));
    }
    Ok(out)
}

/// Runs one case; `Err` explains a failure.
fn run(case: &Value) -> Result<(), String> {
    let doc = case
        .get("document")
        .and_then(Value::as_str)
        .ok_or("missing document")?;
    let expect = case.get("expect").ok_or("missing expect")?;
    let opts = options(case.get("options"))?;
    let loaded = Spec::load(doc.as_bytes(), opts);
    if expect.get("load_error") == Some(&Value::Bool(true)) {
        return match loaded {
            Err(_) => Ok(()),
            Ok(_) => Err("expected the document to be refused, but it loaded".into()),
        };
    }
    let spec = loaded.map_err(|e| format!("document refused: {e}"))?;
    let req_v = case.get("request").ok_or("missing request")?;
    let method = text(req_v.get("method"));
    let path = text(req_v.get("path"));
    let query = text(req_v.get("query"));
    let req_headers = headers(req_v.get("headers"))?;
    let req_body = text(req_v.get("body"));
    let hs: Vec<(&str, &str)> = req_headers
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let req = Request {
        method: &method,
        path: &path,
        query: &query,
        headers: &hs,
        body: req_body.as_bytes(),
    };
    let report = match case.get("response") {
        Some(r) => {
            let status = r
                .get("status")
                .and_then(Value::as_u64)
                .ok_or("response needs a status")? as u16;
            let rh = headers(r.get("headers"))?;
            let rhs: Vec<(&str, &str)> = rh.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
            let body = text(r.get("body"));
            spec.validate_response(
                &req,
                &Response {
                    status,
                    headers: &rhs,
                    body: body.as_bytes(),
                },
            )
        }
        None => spec.validate_request(&req),
    };
    let want_valid = expect
        .get("valid")
        .and_then(Value::as_bool)
        .ok_or("expect.valid must be a boolean")?;
    if report.is_valid() != want_valid {
        return Err(format!("expected valid={want_valid}, got:\n{report}"));
    }
    if let Some(k) = expect.get("kind").and_then(Value::as_str) {
        if !report.problems().iter().any(|p| p.kind.as_str() == k) {
            return Err(format!("expected a {k} problem, got:\n{report}"));
        }
    }
    if let Some(l) = expect.get("location").and_then(Value::as_str) {
        if !report.problems().iter().any(|p| p.location == l) {
            return Err(format!("expected a problem at {l}, got:\n{report}"));
        }
    }
    Ok(())
}

#[test]
fn conformance() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/cases");
    let only = std::env::var("CASE").ok();
    let mut failures = Vec::new();
    let mut total = 0;
    for f in files(&dir) {
        let rel = f.strip_prefix(&dir).unwrap_or(&f).display().to_string();
        if only.as_deref().is_some_and(|o| !rel.contains(o)) {
            continue;
        }
        total += 1;
        let bytes = std::fs::read(&f).unwrap();
        let outcome = openapi_validator::yaml::parse(&bytes, Default::default())
            .map_err(|e| format!("case file is not valid YAML: {e}"))
            .and_then(|case| run(&case));
        if let Err(e) = outcome {
            failures.push(format!("{rel}: {e}"));
        }
    }
    println!(
        "conformance: {} of {total} cases pass",
        total - failures.len()
    );
    assert!(
        failures.is_empty(),
        "{} failing case(s):\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}
