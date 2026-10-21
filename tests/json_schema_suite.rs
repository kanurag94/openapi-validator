//! Runs the official JSON-Schema-Test-Suite (draft 2020-12) through the
//! OpenAPI 3.1 path: each schema becomes a request body schema in a 3.1
//! document, and each test instance is sent as a JSON body.
//!
//! Skipped unless `JSON_SCHEMA_TEST_SUITE` names a checkout of
//! <https://github.com/json-schema-org/JSON-Schema-Test-Suite>. The suite
//! is never copied into this repository. Prints the pass rate per file and
//! overall; `JSON_SCHEMA_TEST_SUITE_REPORT` names a file for the details.
//!
//! The schema is given an `$id` (unless it has one) so that `#`
//! references resolve within it, as they would at a document root.
//! Remote references are refused by design (no network access), so the
//! groups that need them fail to load and count as failures.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use openapi_validator::{Options, Request, Spec};
use serde_json::{json, Value};

fn document(schema: &Value) -> Value {
    let mut schema = schema.clone();
    if let Value::Object(m) = &mut schema {
        m.entry("$id")
            .or_insert_with(|| json!("https://suite.test/root.json"));
    }
    json!({
        "openapi": "3.1.0",
        "info": {"title": "suite", "version": "1"},
        "paths": {"/t": {"post": {
            "requestBody": {"required": true, "content": {"application/json": {"schema": schema}}},
            "responses": {"200": {"description": "ok"}}
        }}}
    })
}

/// `(passed, total, details)` for one suite file.
fn run_file(path: &Path, opts: &Options, details: &mut String) -> (usize, usize) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return (0, 0);
    };
    let Ok(groups) = serde_json::from_str::<Vec<Value>>(&text) else {
        return (0, 0);
    };
    let (mut passed, mut total) = (0, 0);
    for g in &groups {
        let desc = g["description"].as_str().unwrap_or("");
        let tests = g["tests"].as_array().cloned().unwrap_or_default();
        let doc = document(&g["schema"]).to_string();
        let spec = Spec::load(doc.as_bytes(), opts.clone());
        for t in &tests {
            total += 1;
            let want = t["valid"].as_bool().unwrap_or(false);
            let got = match &spec {
                Ok(s) => {
                    let body = t["data"].to_string();
                    let r = s.validate_request(&Request {
                        method: "POST",
                        path: "/t",
                        headers: &[("Content-Type", "application/json")],
                        body: body.as_bytes(),
                        ..Request::default()
                    });
                    Ok(r.is_valid())
                }
                Err(e) => Err(e.to_string()),
            };
            match got {
                Ok(v) if v == want => passed += 1,
                Ok(v) => {
                    let _ = writeln!(
                        details,
                        "  FAIL {desc} / {}: expected {want}, got {v}",
                        t["description"].as_str().unwrap_or("")
                    );
                }
                Err(e) => {
                    let _ = writeln!(
                        details,
                        "  LOAD {desc} / {}: {e}",
                        t["description"].as_str().unwrap_or("")
                    );
                }
            }
        }
    }
    (passed, total)
}

fn json_files(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "json"))
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

#[test]
fn json_schema_test_suite() {
    let Some(root) = std::env::var_os("JSON_SCHEMA_TEST_SUITE").map(PathBuf::from) else {
        eprintln!("JSON_SCHEMA_TEST_SUITE is not set; skipping");
        return;
    };
    let dir = root.join("tests/draft2020-12");
    let mut details = String::new();
    let mut summary = String::new();
    let mut all = (0usize, 0usize);
    // Required tests: format is an annotation in 2020-12 by default.
    let required = Options {
        assert_formats: false,
        ..Options::default()
    };
    for f in json_files(&dir) {
        let _ = writeln!(
            details,
            "{}",
            f.file_name().unwrap_or_default().to_string_lossy()
        );
        let (p, t) = run_file(&f, &required, &mut details);
        all.0 += p;
        all.1 += t;
        let _ = writeln!(
            summary,
            "{:>4}/{:<4} {}",
            p,
            t,
            f.file_name().unwrap_or_default().to_string_lossy()
        );
    }
    // Optional format tests, with format assertion on.
    let asserting = Options::default();
    let mut fmt = (0usize, 0usize);
    for f in json_files(&dir.join("optional/format")) {
        let name = f
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let _ = writeln!(details, "optional/format/{name}");
        let (p, t) = run_file(&f, &asserting, &mut details);
        fmt.0 += p;
        fmt.1 += t;
        let _ = writeln!(summary, "{:>4}/{:<4} optional/format/{name}", p, t);
    }
    let pct = |p: usize, t: usize| {
        if t == 0 {
            0.0
        } else {
            100.0 * p as f64 / t as f64
        }
    };
    let head = format!(
        "draft2020-12 required: {} of {} ({:.1}%)\ndraft2020-12 optional/format: {} of {} ({:.1}%)\n",
        all.0,
        all.1,
        pct(all.0, all.1),
        fmt.0,
        fmt.1,
        pct(fmt.0, fmt.1)
    );
    println!("{head}{summary}");
    if let Some(p) = std::env::var_os("JSON_SCHEMA_TEST_SUITE_REPORT") {
        let _ = std::fs::write(p, format!("{head}\n{summary}\n{details}"));
    }
}
