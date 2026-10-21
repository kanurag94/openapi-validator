//! The loaded document and request routing.

use std::panic::{catch_unwind, AssertUnwindSafe};

use serde_json::Value;

use crate::doc::{self, LoadError, Version, Warning};
use crate::http::Request;
use crate::model::{Body, Builder};
use crate::options::{Options, ServerPaths};
use crate::params::Param;
use crate::path::{split_path, Match, PathIndex, RawSegment, Template, Vars};
use crate::report::{MatchedOperation, ProblemKind, Report};
use crate::schema::{SchemaStore, StoreOptions};
use crate::security::{self, Requirement, Schemes};
use crate::server::{base_paths, BasePath};
use crate::validate::Declared;

/// HTTP methods a Path Item can describe (OpenAPI 3.1.1 §4.8.9.1).
pub(crate) const METHODS: [&str; 8] = [
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];

pub(crate) struct PathItem {
    /// `(method key, operation index)`.
    pub ops: Vec<(&'static str, usize)>,
    /// Server base paths of the path item and its operations.
    pub bases: Vec<BasePath>,
}

pub(crate) struct Operation {
    pub template: usize,
    pub method: &'static str,
    pub operation_id: Option<String>,
    pub body: Option<Body>,
    pub params: Vec<Param>,
    /// `None`: no security applies.
    pub security: Option<Vec<Requirement>>,
    pub declared: Declared,
}

/// A validator for one OpenAPI document. Load once and share: `Spec` is
/// `Send + Sync`.
pub struct Spec {
    pub(crate) version: Version,
    pub(crate) options: Options,
    pub(crate) paths: PathIndex,
    pub(crate) items: Vec<PathItem>,
    pub(crate) ops: Vec<Operation>,
    /// Every distinct server base path, longest first.
    pub(crate) bases: Vec<BasePath>,
    pub(crate) store: SchemaStore,
    pub(crate) schemes: Schemes,
    warnings: Vec<Warning>,
}

/// The result of routing a request.
pub(crate) enum Route<'a> {
    Found {
        op: usize,
        vars: Vars<'a>,
        /// A HEAD request answered by the GET operation.
        head_as_get: bool,
    },
    PathNotFound,
    MethodNotAllowed {
        template: usize,
    },
    MalformedPath,
}

impl Spec {
    /// Parses and indexes an OpenAPI 3.0.x or 3.1.x document (JSON or YAML).
    pub fn load(bytes: &[u8], options: Options) -> Result<Spec, LoadError> {
        match catch_unwind(AssertUnwindSafe(|| Self::load_inner(bytes, options))) {
            Ok(r) => r,
            Err(_) => Err(LoadError::Invalid {
                location: "#".into(),
                message: "internal error while loading the document".into(),
            }),
        }
    }

    fn load_inner(bytes: &[u8], options: Options) -> Result<Spec, LoadError> {
        let root = doc::parse(bytes)?;
        let version = doc::version(&root)?;
        let mut warnings = Vec::new();
        let doc_bases = base_paths(root.get("servers"), "#/servers", &mut warnings);
        let mut b = Builder::new(&root);
        let schemes = b.schemes()?;
        let doc_security = b.requirements(root.get("security"), "#/security", &schemes)?;
        let mut paths = PathIndex::default();
        let mut items: Vec<PathItem> = Vec::new();
        let mut ops: Vec<Operation> = Vec::new();
        let mut all_bases: Vec<BasePath> = Vec::new();

        if let Some(path_map) = root.get("paths").and_then(Value::as_object) {
            for (key, item) in path_map {
                if key.starts_with("x-") {
                    continue;
                }
                let loc = doc::join("#/paths", key);
                if !key.starts_with('/') {
                    warnings.push(Warning {
                        location: loc,
                        message: "path does not start with /; ignored".into(),
                    });
                    continue;
                }
                let template = Template::parse(key).map_err(|message| LoadError::Invalid {
                    location: loc.clone(),
                    message,
                })?;
                let (item, item_ptr) = doc::deref(&root, item, &loc)?;
                let tid = paths
                    .insert(template)
                    .map_err(|first| LoadError::DuplicateTemplate {
                        first,
                        second: key.clone(),
                    })?;
                let item_bases = match item.get("servers") {
                    Some(s) => base_paths(Some(s), &format!("{item_ptr}/servers"), &mut warnings),
                    None => doc_bases.clone(),
                };
                let mut pi = PathItem {
                    ops: Vec::new(),
                    bases: Vec::new(),
                };
                for m in METHODS {
                    let Some(op) = item.get(m).filter(|o| o.is_object()) else {
                        continue;
                    };
                    let op_ptr = doc::join(&item_ptr, m);
                    let op_bases = match op.get("servers") {
                        Some(s) => base_paths(Some(s), &format!("{op_ptr}/servers"), &mut warnings),
                        None => item_bases.clone(),
                    };
                    for base in op_bases {
                        if !pi.bases.contains(&base) {
                            pi.bases.push(base);
                        }
                    }
                    let body =
                        b.request_body(op.get("requestBody"), &doc::join(&op_ptr, "requestBody"))?;
                    let params = b.params(item, &item_ptr, op, &op_ptr, version)?;
                    // Operation `security` replaces the document's, even when empty.
                    let sec = match b.requirements(
                        op.get("security"),
                        &doc::join(&op_ptr, "security"),
                        &schemes,
                    )? {
                        Some(r) => Some(r),
                        None => doc_security.clone(),
                    };
                    let declared = security::declared(&schemes, sec.as_deref());
                    pi.ops.push((m, ops.len()));
                    ops.push(Operation {
                        template: tid,
                        method: m,
                        operation_id: op
                            .get("operationId")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        body,
                        params,
                        security: sec,
                        declared,
                    });
                }
                if pi.bases.is_empty() {
                    pi.bases = item_bases;
                }
                for base in &pi.bases {
                    if !all_bases.contains(base) {
                        all_bases.push(base.clone());
                    }
                }
                debug_assert_eq!(tid, items.len());
                items.push(pi);
            }
        }
        all_bases.sort_by_key(|b| std::cmp::Reverse(b.len()));
        warnings.append(&mut b.warnings);
        let (store, mut schema_warnings) = SchemaStore::new(
            &root,
            version,
            std::mem::take(&mut b.roots.request),
            std::mem::take(&mut b.roots.response),
            &StoreOptions {
                assert_formats: options.assert_formats,
                discriminator_selects_branch: options.discriminator_selects_branch,
            },
        )?;
        warnings.append(&mut schema_warnings);
        Ok(Spec {
            version,
            options,
            paths,
            items,
            ops,
            bases: all_bases,
            store,
            schemes,
            warnings,
        })
    }

    /// The document's OpenAPI version family.
    pub fn version(&self) -> Version {
        self.version
    }

    /// Things the loader accepted but flagged.
    pub fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    pub fn options(&self) -> &Options {
        &self.options
    }

    pub(crate) fn new_report(&self) -> Report {
        Report::new(self.options.max_problems, self.options.max_report_bytes)
    }

    /// Finds the path item for a raw request path.
    fn match_path<'a>(&self, segs: &[RawSegment<'a>]) -> Option<Match<'a>> {
        let mut best: Option<(Match<'a>, usize)> = None;
        let consider = |m: Match<'a>, base_len: usize, best: &mut Option<(Match<'a>, usize)>| {
            let better = match best {
                None => true,
                Some((b, bl)) => {
                    let (rn, rb) = (
                        self.paths.rank_of(m.template),
                        self.paths.rank_of(b.template),
                    );
                    rn > rb || (rn == rb && base_len > *bl)
                }
            };
            if better {
                *best = Some((m, base_len));
            }
        };
        if self.options.server_paths != ServerPaths::Ignored {
            for base in &self.bases {
                if base.is_empty() || base.len() > segs.len() {
                    continue;
                }
                if !base.iter().zip(segs).all(|(b, s)| *b == s.text) {
                    continue;
                }
                for m in self.paths.find(&segs[base.len()..]) {
                    if self.items[m.template].bases.contains(base) {
                        consider(m, base.len(), &mut best);
                        break;
                    }
                }
            }
        }
        let unprefixed_ok = |t: usize| match self.options.server_paths {
            ServerPaths::Ignored | ServerPaths::Either => true,
            ServerPaths::Required => self.items[t].bases.iter().any(Vec::is_empty),
        };
        for m in self.paths.find(segs) {
            if unprefixed_ok(m.template) {
                consider(m, 0, &mut best);
                break;
            }
        }
        best.map(|(m, _)| m)
    }

    pub(crate) fn route<'a>(&self, req: &Request<'a>) -> Route<'a> {
        let raw = if req.path.is_empty() { "/" } else { req.path };
        let Ok(mut segs) = split_path(raw) else {
            return Route::MalformedPath;
        };
        let mut found = self.match_path(&segs);
        // A trailing slash is retried without it (see SPEC_DECISIONS.md).
        if found.is_none() && segs.len() > 1 && segs.last().is_some_and(|s| s.raw.is_empty()) {
            segs.pop();
            found = self.match_path(&segs);
        }
        let Some(m) = found else {
            return Route::PathNotFound;
        };
        let item = &self.items[m.template];
        // Methods are case-sensitive (RFC 9110 §9.1): `GET` is the `get` key.
        let key = METHODS
            .iter()
            .find(|k| k.to_ascii_uppercase() == req.method)
            .copied();
        let find = |k: &str| item.ops.iter().find(|(mk, _)| *mk == k).map(|(_, o)| *o);
        match key {
            Some(k) => {
                if let Some(op) = find(k) {
                    return Route::Found {
                        op,
                        vars: m.vars,
                        head_as_get: false,
                    };
                }
                if k == "head" {
                    if let Some(op) = find("get") {
                        return Route::Found {
                            op,
                            vars: m.vars,
                            head_as_get: true,
                        };
                    }
                }
                Route::MethodNotAllowed {
                    template: m.template,
                }
            }
            None => Route::MethodNotAllowed {
                template: m.template,
            },
        }
    }

    pub(crate) fn matched(&self, op: usize) -> MatchedOperation {
        let o = &self.ops[op];
        MatchedOperation {
            path: self.paths.template(o.template).text.clone(),
            method: o.method.to_owned(),
            operation_id: o.operation_id.clone(),
        }
    }

    /// Records a routing failure; returns the operation when found.
    pub(crate) fn route_into<'a>(
        &self,
        req: &Request<'a>,
        report: &mut Report,
    ) -> Option<(usize, Vars<'a>, bool)> {
        match self.route(req) {
            Route::Found {
                op,
                vars,
                head_as_get,
            } => {
                report.set_operation(self.matched(op));
                Some((op, vars, head_as_get))
            }
            Route::PathNotFound => {
                report.push(
                    ProblemKind::PathNotFound,
                    "path",
                    format!(
                        "no path in the document matches {:?}",
                        truncate(req.path, 200)
                    ),
                );
                None
            }
            Route::MalformedPath => {
                report.push(
                    ProblemKind::MalformedPath,
                    "path",
                    "the path has an invalid percent-escape or is not UTF-8",
                );
                None
            }
            Route::MethodNotAllowed { template } => {
                let allowed: Vec<String> = self.items[template]
                    .ops
                    .iter()
                    .map(|(m, _)| m.to_ascii_uppercase())
                    .collect();
                report.push(
                    ProblemKind::MethodNotAllowed,
                    "method",
                    format!(
                        "{} is not allowed on {}; allowed: {}",
                        truncate(req.method, 32),
                        self.paths.template(template).text,
                        allowed.join(", ")
                    ),
                );
                None
            }
        }
    }

    /// Validates a request against the operation it matches.
    pub fn validate_request(&self, req: &Request<'_>) -> Report {
        let mut report = self.new_report();
        let guarded = catch_unwind(AssertUnwindSafe(|| {
            let mut r = self.new_report();
            if let Some((op, vars, _head)) = self.route_into(req, &mut r) {
                let o = &self.ops[op];
                if self.options.check_security {
                    if let Some(reqs) = &o.security {
                        if let Some(why) = security::check(&self.schemes, reqs, req, &self.options)
                        {
                            r.push(ProblemKind::MissingCredentials, "security", why);
                        }
                    }
                }
                self.check_params(&o.params, &vars, req, &o.declared, &mut r);
                self.check_request_body(o.body.as_ref(), req, &mut r);
            }
            r
        }));
        match guarded {
            Ok(r) => report = r,
            Err(_) => report.push(
                ProblemKind::Internal,
                "request",
                "internal error during validation",
            ),
        }
        report
    }
}

pub(crate) fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"
openapi: 3.0.3
info: {title: t, version: "1"}
servers:
  - url: https://api.example.com/v1
paths:
  /pets:
    get: {responses: {"200": {description: ok}}}
    post: {responses: {"201": {description: ok}}}
  /pets/{id}:
    get: {responses: {"200": {description: ok}}}
  /admin:
    servers: [{url: /internal}]
    get: {responses: {"200": {description: ok}}}
"#;

    fn spec(opts: Options) -> Spec {
        Spec::load(DOC.as_bytes(), opts).unwrap()
    }

    fn kinds(s: &Spec, method: &str, path: &str) -> Vec<ProblemKind> {
        let r = s.validate_request(&Request {
            method,
            path,
            ..Request::default()
        });
        r.problems().iter().map(|p| p.kind).collect()
    }

    #[test]
    fn spec_is_send_and_sync() {
        fn assert<T: Send + Sync>() {}
        assert::<Spec>();
    }

    #[test]
    fn oas311_s4_8_5_server_path_is_stripped_as_whole_segments() {
        let s = spec(Options::default());
        assert!(kinds(&s, "GET", "/v1/pets").is_empty());
        assert!(
            kinds(&s, "GET", "/pets").is_empty(),
            "Either: unprefixed also matches"
        );
        assert_eq!(kinds(&s, "GET", "/v1pets"), vec![ProblemKind::PathNotFound]);
        let s = spec(Options {
            server_paths: ServerPaths::Required,
            ..Options::default()
        });
        assert!(kinds(&s, "GET", "/v1/pets").is_empty());
        assert_eq!(kinds(&s, "GET", "/pets"), vec![ProblemKind::PathNotFound]);
    }

    #[test]
    fn oas311_s4_8_9_path_item_servers_replace_document_servers() {
        let s = spec(Options {
            server_paths: ServerPaths::Required,
            ..Options::default()
        });
        assert!(kinds(&s, "GET", "/internal/admin").is_empty());
        assert_eq!(
            kinds(&s, "GET", "/v1/admin"),
            vec![ProblemKind::PathNotFound]
        );
    }

    #[test]
    fn rfc9110_s9_1_methods_are_case_sensitive() {
        let s = spec(Options::default());
        assert_eq!(
            kinds(&s, "get", "/pets"),
            vec![ProblemKind::MethodNotAllowed]
        );
        assert_eq!(
            kinds(&s, "DELETE", "/pets"),
            vec![ProblemKind::MethodNotAllowed]
        );
        assert!(kinds(&s, "POST", "/pets").is_empty());
    }

    #[test]
    fn rfc9110_s9_3_2_head_is_checked_as_get() {
        let s = spec(Options::default());
        let r = s.validate_request(&Request {
            method: "HEAD",
            path: "/pets/1",
            ..Request::default()
        });
        assert!(r.is_valid());
        assert_eq!(r.operation().unwrap().method, "get");
        assert_eq!(
            kinds(&s, "HEAD", "/admin/x"),
            vec![ProblemKind::PathNotFound]
        );
    }

    #[test]
    fn trailing_slash_is_retried_and_bad_escapes_reported() {
        let s = spec(Options::default());
        assert!(kinds(&s, "GET", "/pets/").is_empty());
        assert_eq!(
            kinds(&s, "GET", "/pets/%zz"),
            vec![ProblemKind::MalformedPath]
        );
    }

    #[test]
    fn load_errors() {
        let dup =
            "openapi: 3.1.0\ninfo: {title: t, version: '1'}\npaths:\n  /a/{x}: {}\n  /a/{y}: {}\n";
        assert!(matches!(
            Spec::load(dup.as_bytes(), Options::default()),
            Err(LoadError::DuplicateTemplate { .. })
        ));
        assert!(matches!(
            Spec::load(b"swagger: '2.0'", Options::default()),
            Err(LoadError::UnsupportedVersion(_))
        ));
        assert!(matches!(
            Spec::load(b"{", Options::default()),
            Err(LoadError::Parse(_))
        ));
        let ext = "openapi: 3.1.0\ninfo: {title: t, version: '1'}\npaths:\n  /a: {$ref: 'other.yaml#/a'}\n";
        assert!(matches!(
            Spec::load(ext.as_bytes(), Options::default()),
            Err(LoadError::ExternalRef { .. })
        ));
    }
}
