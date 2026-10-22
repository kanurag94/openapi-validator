//! Optional run over a local corpus of OpenAPI documents.
//!
//! Skipped unless `OAS_CORPUS` names a directory. The corpus never enters
//! the repository, and this test never prints document names, paths, schema
//! names or field names: failures are named by file index and operation
//! index only. Details go to the report file (`OAS_CORPUS_REPORT`, default
//! `~/oas-corpus-report/report.txt`), with names replaced by placeholders.
//!
//! For each document:
//! - load it and record whether it was refused;
//! - generate one valid request per operation, which must be allowed;
//! - for each required query, header or cookie parameter, send that request
//!   without it, which must be refused with a missing-parameter problem.
//!
//! With `OAS_CORPUS_DUMP` set to a file outside the repository, every request
//! sent is also written there, one JSON object per line with the document's
//! file path and the expected verdict, so other validators can be run on the
//! same requests.

mod common;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use openapi_validator::{LoadError, Options, ProblemKind, Spec};
use serde_json::Value;

const METHODS: [&str; 8] = [
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];

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
            } else if matches!(
                p.extension().and_then(|x| x.to_str()),
                Some("json" | "yaml" | "yml")
            ) {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn load_kind(e: &LoadError) -> String {
    match e {
        LoadError::Parse(_) => "parse".into(),
        LoadError::UnsupportedVersion(_) => "unsupported-version".into(),
        LoadError::ExternalRef { .. } => "external-ref".into(),
        LoadError::BrokenRef { .. } => "broken-ref".into(),
        LoadError::RefCycle { .. } => "ref-cycle".into(),
        LoadError::Pattern { error, .. } => format!("pattern ({})", error.reason),
        LoadError::DuplicateTemplate { .. } => "duplicate-template".into(),
        LoadError::UnknownSecurityScheme { .. } => "unknown-security-scheme".into(),
        LoadError::Invalid { message, .. } => format!("invalid ({})", scrub(message)),
        _ => "other".into(),
    }
}

/// Removes quoted text and pointer tokens so no document names leak.
fn scrub(s: &str) -> String {
    let mut out = String::new();
    let mut in_quote = false;
    for c in s.chars() {
        if c == '"' {
            in_quote = !in_quote;
            if in_quote {
                out.push_str("\"…\"");
            }
            continue;
        }
        if !in_quote {
            out.push(c);
        }
    }
    out.split_whitespace()
        .map(|w| {
            if w.starts_with('#') || w.contains('/') {
                "<ptr>"
            } else {
                w
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A location with names replaced: `query.<name>/...` keeps its part and
/// its depth only.
/// One request for `OAS_CORPUS_DUMP`, as it goes on the wire.
fn write_dump(
    w: &mut impl std::io::Write,
    file: &Path,
    fi: usize,
    op: usize,
    valid: bool,
    req: &common::OwnedRequest,
) {
    let line = req.with(|r| {
        serde_json::json!({
            "doc": file.to_string_lossy(),
            "file": fi,
            "op": op,
            "valid": valid,
            "method": r.method,
            "path": r.path,
            "query": r.query,
            "headers": r.headers,
            "body": String::from_utf8_lossy(r.body),
        })
    });
    let _ = writeln!(w, "{line}");
}

fn anon_location(loc: &str) -> String {
    let (head, rest) = loc.split_once(['.', '/']).unwrap_or((loc, ""));
    let depth = rest.matches('/').count() + usize::from(!rest.is_empty());
    format!("{head}[{depth}]")
}

#[test]
fn corpus() {
    let Some(dir) = std::env::var_os("OAS_CORPUS").map(PathBuf::from) else {
        eprintln!("OAS_CORPUS is not set; skipping");
        return;
    };
    let report_path = std::env::var_os("OAS_CORPUS_REPORT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(std::env::temp_dir);
            home.join("oas-corpus-report").join("report.txt")
        });
    let mut report = String::new();
    let mut dump = std::env::var_os("OAS_CORPUS_DUMP").map(|p| {
        std::io::BufWriter::new(std::fs::File::create(p).expect("OAS_CORPUS_DUMP is writable"))
    });
    let (mut loaded, mut refused) = (0usize, 0usize);
    let (mut valid_total, mut valid_ok, mut valid_skipped) = (0usize, 0usize, 0usize);
    let (mut missing_total, mut missing_ok) = (0usize, 0usize);

    for (fi, path) in files(&dir).iter().enumerate() {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let spec = match Spec::load(&bytes, Options::default()) {
            Ok(s) => s,
            Err(e) => {
                refused += 1;
                let _ = writeln!(report, "file {fi}: refused: {}", load_kind(&e));
                continue;
            }
        };
        loaded += 1;
        let Ok(root) = openapi_validator::json::parse(&bytes, Default::default())
            .map_err(|_| ())
            .or_else(|_| {
                openapi_validator::yaml::parse(&bytes, Default::default()).map_err(|_| ())
            })
        else {
            continue;
        };
        let mut paths: Vec<&String> = root
            .get("paths")
            .and_then(Value::as_object)
            .map(|m| m.keys().collect())
            .unwrap_or_default();
        paths.sort();
        let mut oi = 0usize;
        for p in paths {
            if !p.starts_with('/') {
                continue;
            }
            for m in METHODS {
                let item = &root["paths"][p.as_str()];
                let item = match item.get("$ref").and_then(Value::as_str) {
                    Some(r) => root.pointer(r.trim_start_matches('#')).unwrap_or(item),
                    None => item,
                };
                if item.get(m).is_none() {
                    continue;
                }
                let op = oi;
                oi += 1;
                valid_total += 1;
                let g = match common::request(&root, p, m, 0) {
                    Ok(g) => g,
                    Err(why) => {
                        valid_skipped += 1;
                        let _ = writeln!(
                            report,
                            "file {fi} op {op}: generation skipped: {}",
                            scrub(&why)
                        );
                        continue;
                    }
                };
                if let Some(w) = dump.as_mut() {
                    write_dump(w, path, fi, op, true, &g.request);
                }
                let r = g.request.with(|req| spec.validate_request(req));
                if r.is_valid() {
                    valid_ok += 1;
                } else {
                    let what: Vec<String> = r
                        .problems()
                        .iter()
                        .map(|pr| {
                            format!(
                                "{} at {}: {}",
                                pr.kind,
                                anon_location(&pr.location),
                                scrub(&pr.message)
                            )
                        })
                        .collect();
                    let elsewhere = r
                        .operation()
                        .is_some_and(|mo| mo.path != *p || mo.method != m);
                    let note = if elsewhere {
                        " (routed to another operation)"
                    } else {
                        ""
                    };
                    let _ = writeln!(
                        report,
                        "file {fi} op {op}: valid request refused{note}: {}",
                        what.join("; ")
                    );
                }
                for (k, o) in g.omittable.iter().enumerate() {
                    missing_total += 1;
                    let mut req = g.request.clone();
                    match o.location.as_str() {
                        "query" => req.query.retain(|(n, _)| !o.pairs.contains(n)),
                        "header" => req
                            .headers
                            .retain(|(n, _)| !n.eq_ignore_ascii_case(&o.name)),
                        _ => req.cookies.retain(|(n, _)| *n != o.name),
                    }
                    if let Some(w) = dump.as_mut() {
                        write_dump(w, path, fi, op, false, &req);
                    }
                    let r = req.with(|q| spec.validate_request(q));
                    if r.problems()
                        .iter()
                        .any(|pr| pr.kind == ProblemKind::MissingParameter)
                    {
                        missing_ok += 1;
                    } else {
                        let _ = writeln!(
                            report,
                            "file {fi} op {op} required parameter {k} ({}): omission not refused",
                            o.location
                        );
                    }
                }
            }
        }
    }
    let summary = format!(
        "documents: {loaded} loaded, {refused} refused\nvalid requests: {valid_ok} of {} allowed ({valid_skipped} not generated)\nmissing required parameters: {missing_ok} of {missing_total} refused\n",
        valid_total - valid_skipped
    );
    if let Some(parent) = report_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&report_path, format!("{summary}\n{report}"));
    println!("{summary}");
}
