//! The loaded document and request routing.

use std::collections::{HashMap, HashSet};
use std::panic::{catch_unwind, AssertUnwindSafe};

use serde_json::Value;

use crate::doc::{self, LoadError, Version, Warning};
use crate::http::{Request, Response};
use crate::model::{Body, Builder, Responses};
use crate::options::{
    DiscriminatorMode, FormatAssertion, IntegerSemantics, Options, PatternEngine,
    ReadWritePresence, ReadWriteRequired, ServerPaths,
};
use crate::params::Param;
use crate::path::{split_path, Match, PathIndex, RawSegment, Template, Vars};
use crate::report::{MatchedOperation, ProblemKind, Report};
use crate::schema::{SchemaStore, StoreOptions};
use crate::security::{self, Requirement, Schemes};
use crate::server::{base_paths, segment_matches, BasePath};
use crate::validate::Declared;

/// HTTP methods a Path Item can describe (OpenAPI 3.1.1 §4.8.9.1).
pub(crate) const METHODS: [&str; 8] = [
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];

/// One operation of a Path Item.
pub(crate) struct ItemOp<'a> {
    /// The field it is under: `get`, or an `additionalOperations` key.
    pub key: String,
    /// The method as sent in a request: `GET`, or the key as written.
    pub wire: String,
    pub op: &'a Value,
    pub ptr: String,
}

/// The operations of a Path Item, fixed fields first. OpenAPI 3.2 adds
/// `query` and `additionalOperations` (§4.10.1), whose keys are methods
/// with the capitalization sent; an entry for a method a fixed field
/// already describes is ignored.
pub(crate) fn item_operations<'a>(
    item: &'a Value,
    item_ptr: &str,
    version: Version,
) -> Vec<ItemOp<'a>> {
    let mut out: Vec<ItemOp<'a>> = Vec::new();
    let fixed: &[&str] = if version >= Version::V32 {
        &[
            "get", "put", "post", "delete", "options", "head", "patch", "trace", "query",
        ]
    } else {
        &METHODS
    };
    for &m in fixed {
        if let Some(op) = item.get(m).filter(|o| o.is_object()) {
            out.push(ItemOp {
                key: m.to_owned(),
                wire: m.to_ascii_uppercase(),
                op,
                ptr: doc::join(item_ptr, m),
            });
        }
    }
    if version >= Version::V32 {
        let extra_ptr = doc::join(item_ptr, "additionalOperations");
        for (k, op) in item
            .get("additionalOperations")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            if op.is_object() && !out.iter().any(|o| o.wire == *k) {
                out.push(ItemOp {
                    key: k.clone(),
                    wire: k.clone(),
                    op,
                    ptr: doc::join(&extra_ptr, k),
                });
            }
        }
    }
    out
}

pub(crate) struct PathItem {
    /// `(method as sent, operation index)`.
    pub ops: MethodOps,
    /// Server base paths of the path item and its operations.
    pub bases: Vec<BasePath>,
}

pub(crate) struct Operation {
    /// The path template; a webhook's name, or a callback's expression.
    pub path: String,
    /// The field the operation is under, such as `get`.
    pub method: String,
    /// The variable names of the operation's own template, when another
    /// template that differs only in names was read first.
    pub rename: Option<Vec<String>>,
    pub operation_id: Option<String>,
    /// Server base paths that serve this operation (OpenAPI 3.1.1
    /// §4.8.10.1: operation `servers` replace the path item's).
    pub bases: Vec<BasePath>,
    pub body: Option<Body>,
    pub params: Vec<Param>,
    /// `None`: no security applies.
    pub security: Option<Vec<Requirement>>,
    pub declared: Declared,
    pub responses: Responses,
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
    /// Webhook name to `(method key, operation index)` (OpenAPI 3.1.1 §4.8.1).
    pub(crate) webhooks: HashMap<String, MethodOps>,
    /// `(operationId, callback name)` to its expressions, each with
    /// `(method key, operation index)` (§4.8.18).
    pub(crate) callbacks: HashMap<(String, String), Vec<MethodOps>>,
    /// Encoding styles apply to multipart parts: OpenAPI 3.1 and later
    /// (§4.8.15.1), and Swagger 2.0 `collectionFormat` on `formData`.
    pub(crate) multipart_styles: bool,
    warnings: Vec<Warning>,
}

/// What an operation's Path Item is reached by.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reached {
    /// A path template: requests are routed to it.
    Path,
    /// A webhook or a callback: the caller names it, and there are no path
    /// parameters to read.
    Named,
}

/// The parts of the document every operation is built against.
struct Ctx<'r> {
    root: &'r Value,
    version: Version,
    schemes: Schemes,
    doc_security: Option<Vec<Requirement>>,
    ops: Vec<Operation>,
    /// Callback objects still to index: `(owner operationId, operation, pointer)`.
    pending: Vec<(String, &'r Value, String)>,
}

/// `(method key, operation index)` for each method of a Path Item.
type MethodOps = Vec<(String, usize)>;

/// The Path Items a webhook or callback name selects, if declared.
type Named<'a> = Option<Vec<&'a [(String, usize)]>>;

/// A problem to report: kind, location, message.
type Problem = (ProblemKind, &'static str, String);

/// Bounds on callbacks nested in callbacks.
const MAX_CALLBACK_OPS: usize = 10_000;

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

impl<'r> Ctx<'r> {
    /// Builds the operation under `method` of a Path Item, and queues its
    /// callbacks. Returns its index.
    #[allow(clippy::too_many_arguments)]
    fn operation(
        &mut self,
        b: &mut Builder<'r>,
        path: &str,
        item: &'r Value,
        item_ptr: &str,
        it: &ItemOp<'r>,
        bases: Vec<BasePath>,
        reached: Reached,
    ) -> Result<usize, LoadError> {
        let (op, op_ptr) = (it.op, &it.ptr);
        // OpenAPI 3.0.4 §4.7.10.1: on GET, HEAD and DELETE, where HTTP gives a
        // body no defined meaning, requestBody SHALL be ignored. 3.1 and
        // later describe such bodies.
        let ignored =
            self.version == Version::V30 && matches!(it.key.as_str(), "get" | "head" | "delete");
        let body = if ignored {
            None
        } else {
            b.request_body(op.get("requestBody"), &doc::join(op_ptr, "requestBody"))?
        };
        let mut params = b.params(item, item_ptr, op, op_ptr, self.version)?;
        if reached == Reached::Named {
            params.retain(|p| p.location != crate::params::In::Path);
        }
        // Operation `security` replaces the document's, even when empty.
        let security = match b.requirements(
            op.get("security"),
            &doc::join(op_ptr, "security"),
            &self.schemes,
        )? {
            Some(r) => Some(r),
            None => self.doc_security.clone(),
        };
        let declared = security::declared(&self.schemes, security.as_deref());
        let responses = b.responses(op.get("responses"), &doc::join(op_ptr, "responses"))?;
        let operation_id = op
            .get("operationId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if let (Some(id), Some(cbs)) = (&operation_id, op.get("callbacks")) {
            self.pending
                .push((id.clone(), cbs, doc::join(op_ptr, "callbacks")));
        }
        self.ops.push(Operation {
            path: path.to_owned(),
            method: it.key.clone(),
            rename: None,
            operation_id,
            bases,
            body,
            params,
            security,
            declared,
            responses,
        });
        Ok(self.ops.len() - 1)
    }

    /// Builds every operation of a webhook's or a callback's Path Item.
    fn named_item(
        &mut self,
        b: &mut Builder<'r>,
        label: &str,
        item: &'r Value,
        item_ptr: &str,
    ) -> Result<MethodOps, LoadError> {
        let mut out = Vec::new();
        for it in item_operations(item, item_ptr, self.version) {
            let o = self.operation(b, label, item, item_ptr, &it, Vec::new(), Reached::Named)?;
            out.push((it.wire, o));
        }
        Ok(out)
    }

    /// Indexes the callbacks of every operation built so far, and of the
    /// operations inside them. A callback reached twice is indexed once.
    fn callbacks(
        &mut self,
        b: &mut Builder<'r>,
    ) -> Result<HashMap<(String, String), Vec<MethodOps>>, LoadError> {
        let root = self.root;
        let mut out: HashMap<(String, String), Vec<MethodOps>> = HashMap::new();
        let mut seen: HashSet<String> = HashSet::new();
        while let Some((owner, cbs, ptr)) = self.pending.pop() {
            let (cbs, ptr) = doc::deref(root, cbs, &ptr)?;
            let Some(cbs) = cbs.as_object() else { continue };
            for (name, cb) in cbs {
                if name.starts_with("x-") {
                    continue;
                }
                let (cb, cb_ptr) = doc::deref(root, cb, &doc::join(&ptr, name))?;
                let key = (owner.clone(), name.clone());
                if out.contains_key(&key) || self.ops.len() > MAX_CALLBACK_OPS {
                    continue;
                }
                let mut exprs = Vec::new();
                for (expr, item) in cb.as_object().into_iter().flatten() {
                    if expr.starts_with("x-") {
                        continue;
                    }
                    let (item, item_ptr) = doc::deref(root, item, &doc::join(&cb_ptr, expr))?;
                    // A Path Item shared by several callbacks is built each
                    // time, but its own callbacks are followed once.
                    let first = seen.insert(item_ptr.clone());
                    let before = self.pending.len();
                    exprs.push(self.named_item(b, expr, item, &item_ptr)?);
                    if !first {
                        self.pending.truncate(before);
                    }
                }
                out.insert(key, exprs);
            }
        }
        Ok(out)
    }
}

impl Spec {
    /// Parses and indexes an OpenAPI 3.0.x, 3.1.x or 3.2.x document, or a
    /// Swagger 2.0 one, in JSON or YAML.
    pub fn load(bytes: &[u8], options: Options) -> Result<Spec, LoadError> {
        guard_load(|| {
            let root = doc::parse(bytes, options.yaml_merge_keys)?;
            // A 3.2 document can name itself with `$self` (§4.8.1), and
            // references may use that name.
            let root = match root.get("$self").and_then(Value::as_str) {
                Some(uri) if doc::version(&root) == Ok(Version::V32) => {
                    let uri = uri.to_owned();
                    crate::bundle::merge(&uri, root, Vec::new())?
                }
                _ => root,
            };
            Self::load_value(root, options)
        })
    }

    /// Loads a description spread over several documents. `entry` is the
    /// OpenAPI document at `entry_uri`; `others` are the documents it refers
    /// to, each with the URI references use for it (relative URIs resolve
    /// against `entry_uri`). Nothing is fetched: a reference to a document
    /// not supplied is a load error.
    pub fn load_documents(
        entry_uri: &str,
        entry: &[u8],
        others: &[(&str, &[u8])],
        options: Options,
    ) -> Result<Spec, LoadError> {
        guard_load(|| {
            let root = doc::parse(entry, options.yaml_merge_keys)?;
            let mut parsed = Vec::with_capacity(others.len());
            for (uri, bytes) in others {
                let v = doc::parse(bytes, options.yaml_merge_keys).map_err(|e| match e {
                    LoadError::Parse(m) => LoadError::Parse(format!("{uri}: {m}")),
                    other => other,
                })?;
                parsed.push(((*uri).to_owned(), v));
            }
            Self::load_value(crate::bundle::merge(entry_uri, root, parsed)?, options)
        })
    }

    fn load_value(root: Value, options: Options) -> Result<Spec, LoadError> {
        // Swagger 2.0 is read as the OpenAPI 3.0 it converts to.
        let from_swagger = root.get("swagger").is_some();
        let (root, mut warnings) = if from_swagger {
            crate::swagger::convert(root)?
        } else {
            (root, Vec::new())
        };
        let version = doc::version(&root)?;
        let doc_bases = base_paths(root.get("servers"), "#/servers", &mut warnings);
        let mut b = Builder::new(&root, version);
        let schemes = b.schemes()?;
        let doc_security = b.requirements(root.get("security"), "#/security", &schemes)?;
        let mut cx = Ctx {
            root: &root,
            version,
            schemes,
            doc_security,
            ops: Vec::new(),
            pending: Vec::new(),
        };
        let mut paths = PathIndex::default();
        let mut items: Vec<PathItem> = Vec::new();
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
                let names = template.var_names();
                // §4.8.8.2: templates that differ only in variable names
                // MUST NOT exist. They are one path: the second's operations
                // join the first's, each reading its own variable names.
                let (tid, rename) = match paths.insert(template) {
                    Ok(t) => (t, None),
                    Err(first) => {
                        warnings.push(Warning {
                            location: loc.clone(),
                            message: format!(
                                "path template {:?} differs from {:?} only in parameter names, which MUST NOT happen (§4.8.8.2); read as one path",
                                key,
                                paths.template(first).text
                            ),
                        });
                        (first, Some(names))
                    }
                };
                let item_bases = match item.get("servers") {
                    Some(s) => base_paths(Some(s), &format!("{item_ptr}/servers"), &mut warnings),
                    None => doc_bases.clone(),
                };
                let mut pi = PathItem {
                    ops: Vec::new(),
                    bases: Vec::new(),
                };
                for it in item_operations(item, &item_ptr, version) {
                    if rename.is_some() && items[tid].ops.iter().any(|(w, _)| *w == it.wire) {
                        warnings.push(Warning {
                            location: it.ptr.clone(),
                            message: format!(
                                "{} is already described for {:?}; ignored",
                                it.wire,
                                paths.template(tid).text
                            ),
                        });
                        continue;
                    }
                    let op_bases = match it.op.get("servers") {
                        Some(s) => {
                            base_paths(Some(s), &format!("{}/servers", it.ptr), &mut warnings)
                        }
                        None => item_bases.clone(),
                    };
                    for base in &op_bases {
                        if !pi.bases.contains(base) {
                            pi.bases.push(base.clone());
                        }
                    }
                    let o =
                        cx.operation(&mut b, key, item, &item_ptr, &it, op_bases, Reached::Path)?;
                    cx.ops[o].rename = rename.clone();
                    pi.ops.push((it.wire, o));
                }
                if pi.bases.is_empty() {
                    pi.bases = item_bases;
                }
                for base in &pi.bases {
                    if !all_bases.contains(base) {
                        all_bases.push(base.clone());
                    }
                }
                if rename.is_some() {
                    let first = &mut items[tid];
                    first.ops.append(&mut pi.ops);
                    for base in pi.bases {
                        if !first.bases.contains(&base) {
                            first.bases.push(base);
                        }
                    }
                } else {
                    debug_assert_eq!(tid, items.len());
                    items.push(pi);
                }
            }
        }
        all_bases.sort_by_key(|b| std::cmp::Reverse(b.len()));
        let mut webhooks = HashMap::new();
        // `webhooks` is new in 3.1; in 3.0 the key means nothing.
        let hooks = root.get("webhooks").filter(|_| version != Version::V30);
        if let Some(hooks) = hooks.and_then(Value::as_object) {
            for (name, item) in hooks {
                if name.starts_with("x-") {
                    continue;
                }
                let (item, item_ptr) = doc::deref(&root, item, &doc::join("#/webhooks", name))?;
                let ops = cx.named_item(&mut b, name, item, &item_ptr)?;
                webhooks.insert(name.clone(), ops);
            }
        }
        let callbacks = cx.callbacks(&mut b)?;
        let Ctx { schemes, ops, .. } = cx;
        warnings.append(&mut b.warnings);
        let (store, mut schema_warnings) = SchemaStore::new(
            &root,
            version,
            std::mem::take(&mut b.roots.request),
            std::mem::take(&mut b.roots.response),
            &StoreOptions {
                assert_formats: match options.formats {
                    FormatAssertion::Always => true,
                    FormatAssertion::Never => false,
                    FormatAssertion::ByVersion => version == Version::V30,
                },
                discriminator_enforce: options.discriminator == DiscriminatorMode::Enforce,
                lexical_integers: options.integers == IntegerSemantics::LexicalIn30,
                relax_required: version == Version::V30
                    || options.read_write_required == ReadWriteRequired::RelaxAlways,
                allow_backtracking: options.patterns == PatternEngine::Bounded,
                forbid_read_write: options.read_write_presence == ReadWritePresence::Refuse,
            },
        )?;
        warnings.append(&mut schema_warnings);
        warnings.extend(crate::lint::check(&root, version));
        if options.strict_document {
            if let Some(w) = warnings.first() {
                return Err(LoadError::Invalid {
                    location: w.location.clone(),
                    message: format!("{} (strict_document)", w.message),
                });
            }
        }
        Ok(Spec {
            version,
            options,
            paths,
            items,
            ops,
            bases: all_bases,
            store,
            schemes,
            webhooks,
            callbacks,
            multipart_styles: from_swagger || version >= Version::V31,
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
    /// Finds the path item for a raw request path, and the server base path
    /// that was stripped (`None` when matched without one).
    fn match_path<'a>(&self, segs: &[RawSegment<'a>]) -> Option<(Match<'a>, Option<usize>)> {
        let mut best: Option<(Match<'a>, usize, Option<usize>)> = None;
        let consider =
            |m: Match<'a>,
             base: Option<usize>,
             best: &mut Option<(Match<'a>, usize, Option<usize>)>| {
                let base_len = base.and_then(|i| self.bases.get(i)).map_or(0, Vec::len);
                let better = match best {
                    None => true,
                    Some((b, bl, _)) => {
                        let (rn, rb) = (
                            self.paths.rank_of(m.template),
                            self.paths.rank_of(b.template),
                        );
                        rn > rb || (rn == rb && base_len > *bl)
                    }
                };
                if better {
                    *best = Some((m, base_len, base));
                }
            };
        if self.options.server_paths != ServerPaths::Ignored {
            for (bi, base) in self.bases.iter().enumerate() {
                if base.is_empty() || base.len() > segs.len() {
                    continue;
                }
                if !base
                    .iter()
                    .zip(segs)
                    .all(|(b, s)| segment_matches(b, &s.text))
                {
                    continue;
                }
                for m in self.paths.find(&segs[base.len()..]) {
                    if self.items[m.template].bases.contains(base) {
                        consider(m, Some(bi), &mut best);
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
                consider(m, None, &mut best);
                break;
            }
        }
        best.map(|(m, _, b)| (m, b))
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
        let Some((m, base)) = found else {
            return Route::PathNotFound;
        };
        let item = &self.items[m.template];
        // The server path used must serve the chosen operation itself.
        let served = |op: usize| match base {
            Some(i) => self
                .bases
                .get(i)
                .is_some_and(|b| self.ops[op].bases.contains(b)),
            None => {
                self.options.server_paths != ServerPaths::Required
                    || self.ops[op].bases.iter().any(Vec::is_empty)
            }
        };
        // Methods are case-sensitive (RFC 9110 §9.1): `GET` is the `get` key.
        let find = |k: &str| item.ops.iter().find(|(mk, _)| mk == k).map(|(_, o)| *o);
        let hit = match find(req.method) {
            Some(op) => Some((op, false)),
            None if req.method == "HEAD" => find("GET").map(|op| (op, true)),
            None => None,
        };
        match hit {
            Some((op, head_as_get)) if served(op) => Route::Found {
                op,
                vars: match &self.ops[op].rename {
                    Some(names) => m
                        .vars
                        .into_iter()
                        .zip(names)
                        .map(|((_, raw), n)| (n.clone(), raw))
                        .collect(),
                    None => m.vars,
                },
                head_as_get,
            },
            Some(_) => Route::PathNotFound,
            None => Route::MethodNotAllowed {
                template: m.template,
            },
        }
    }

    pub(crate) fn matched(&self, op: usize) -> MatchedOperation {
        let o = &self.ops[op];
        MatchedOperation {
            path: o.path.clone(),
            method: o.method.clone(),
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
                    .map(|(m, _)| m.clone())
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
        self.guarded("request", |r| {
            if let Some((op, vars, _head)) = self.route_into(req, r) {
                self.check_request_op(op, &vars, req, r);
            }
        })
    }

    /// Validates a response against the operation its request matches.
    pub fn validate_response(&self, req: &Request<'_>, resp: &Response<'_>) -> Report {
        self.guarded("response", |r| {
            let mut scratch = self.new_report();
            let Some((op, _vars, head)) = self.route_into(req, &mut scratch) else {
                let why = scratch
                    .problems()
                    .first()
                    .map(|p| p.message.clone())
                    .unwrap_or_else(|| "the request matches no operation".into());
                r.push(
                    ProblemKind::UndeclaredOperation,
                    "request",
                    format!("the response is for an undeclared operation: {why}"),
                );
                return;
            };
            if let Some(m) = scratch.operation() {
                r.set_operation(m.clone());
            }
            self.check_response_op(op, req, head, resp, r);
        })
    }

    /// Validates a request the API sends to a webhook (OpenAPI 3.1.1
    /// §4.8.1 `webhooks`). The method selects the operation; the path is
    /// not checked, since the receiver chooses it.
    pub fn validate_webhook_request(&self, name: &str, req: &Request<'_>) -> Report {
        self.guarded("request", |r| {
            let found = self.webhooks.get(name).map(|ops| vec![ops.as_slice()]);
            self.named_request(found, "webhook", name, req, r);
        })
    }

    /// Validates the response a webhook receiver returned.
    pub fn validate_webhook_response(
        &self,
        name: &str,
        req: &Request<'_>,
        resp: &Response<'_>,
    ) -> Report {
        self.guarded("response", |r| {
            let found = self.webhooks.get(name).map(|ops| vec![ops.as_slice()]);
            self.named_response(found, "webhook", name, req, resp, r);
        })
    }

    /// Validates a request the API sends for the callback `callback` of the
    /// operation `operation_id` (OpenAPI 3.1.1 §4.8.18). The callback URL
    /// is the caller's to resolve, so the path is not checked; when the
    /// callback has several expressions, any whose operation accepts the
    /// request is enough.
    pub fn validate_callback_request(
        &self,
        operation_id: &str,
        callback: &str,
        req: &Request<'_>,
    ) -> Report {
        self.guarded("request", |r| {
            let found = self.callback(operation_id, callback);
            self.named_request(found, "callback", callback, req, r);
        })
    }

    /// Validates the response a callback receiver returned.
    pub fn validate_callback_response(
        &self,
        operation_id: &str,
        callback: &str,
        req: &Request<'_>,
        resp: &Response<'_>,
    ) -> Report {
        self.guarded("response", |r| {
            let found = self.callback(operation_id, callback);
            self.named_response(found, "callback", callback, req, resp, r);
        })
    }

    fn callback(&self, operation_id: &str, callback: &str) -> Named<'_> {
        self.callbacks
            .get(&(operation_id.to_owned(), callback.to_owned()))
            .map(|exprs| exprs.iter().map(Vec::as_slice).collect())
    }

    /// Runs `f` on a fresh report; a panic becomes an internal problem.
    fn guarded(&self, what: &str, f: impl FnOnce(&mut Report)) -> Report {
        let guarded = catch_unwind(AssertUnwindSafe(|| {
            let mut r = self.new_report();
            f(&mut r);
            r
        }));
        guarded.unwrap_or_else(|_| {
            let mut r = self.new_report();
            r.push(
                ProblemKind::Internal,
                what,
                "internal error during validation",
            );
            r
        })
    }

    fn check_request_op(&self, op: usize, vars: &Vars<'_>, req: &Request<'_>, r: &mut Report) {
        let o = &self.ops[op];
        if self.options.check_security {
            if let Some(reqs) = &o.security {
                if let Some(why) = security::check(&self.schemes, reqs, req, &self.options) {
                    r.push(ProblemKind::MissingCredentials, "security", why);
                }
            }
        }
        crate::validate::check_single_headers(req.headers, r);
        self.check_params(&o.params, vars, req, &o.declared, r);
        self.check_request_body(o.body.as_ref(), req, r);
    }

    fn check_response_op(
        &self,
        op: usize,
        req: &Request<'_>,
        head: bool,
        resp: &Response<'_>,
        r: &mut Report,
    ) {
        match self.ops[op].responses.find(resp.status) {
            Some(obj) => self.check_response(obj, req, head, resp, r),
            None => r.push(
                ProblemKind::UndeclaredStatus,
                "status",
                format!("status {} is not declared for this operation", resp.status),
            ),
        }
    }

    /// The operations a named Path Item offers for the request's method,
    /// one per expression; HEAD falls back to GET. `Err` is the problem.
    fn named_ops(
        &self,
        found: Named<'_>,
        kind: &str,
        name: &str,
        req: &Request<'_>,
    ) -> Result<Vec<(usize, bool)>, Problem> {
        let Some(items) = found else {
            return Err((
                ProblemKind::UndeclaredOperation,
                if kind == "webhook" {
                    "webhook"
                } else {
                    "callback"
                },
                format!(
                    "the document declares no {kind} named {:?}",
                    truncate(name, 200)
                ),
            ));
        };
        let mut out = Vec::new();
        for ops in &items {
            let find = |k: &str| ops.iter().find(|(m, _)| m == k).map(|(_, o)| *o);
            match find(req.method) {
                Some(o) => out.push((o, false)),
                None if req.method == "HEAD" => out.extend(find("GET").map(|o| (o, true))),
                None => {}
            }
        }
        if out.is_empty() {
            let mut allowed: Vec<String> = items
                .iter()
                .flat_map(|ops| ops.iter().map(|(m, _)| m.clone()))
                .collect();
            allowed.dedup();
            return Err((
                ProblemKind::MethodNotAllowed,
                "method",
                format!(
                    "{} is not allowed on the {kind} {:?}; allowed: {}",
                    truncate(req.method, 32),
                    truncate(name, 200),
                    allowed.join(", ")
                ),
            ));
        }
        Ok(out)
    }

    /// Checks against each candidate; the first that accepts wins, and
    /// otherwise the first candidate's problems are reported.
    fn first_accepting(
        &self,
        candidates: &[(usize, bool)],
        r: &mut Report,
        check: impl Fn(usize, bool, &mut Report),
    ) {
        let mut first: Option<Report> = None;
        for &(op, head) in candidates {
            let mut c = self.new_report();
            c.set_operation(self.matched(op));
            check(op, head, &mut c);
            if c.is_valid() {
                *r = c;
                return;
            }
            first.get_or_insert(c);
        }
        if let Some(c) = first {
            *r = c;
        }
    }

    fn named_request(
        &self,
        found: Named<'_>,
        kind: &str,
        name: &str,
        req: &Request<'_>,
        r: &mut Report,
    ) {
        match self.named_ops(found, kind, name, req) {
            Err((k, loc, msg)) => r.push(k, loc, msg),
            Ok(c) => self.first_accepting(&c, r, |op, _, c| {
                self.check_request_op(op, &Vars::default(), req, c)
            }),
        }
    }

    fn named_response(
        &self,
        found: Named<'_>,
        kind: &str,
        name: &str,
        req: &Request<'_>,
        resp: &Response<'_>,
        r: &mut Report,
    ) {
        match self.named_ops(found, kind, name, req) {
            Err((k, loc, msg)) => r.push(
                ProblemKind::UndeclaredOperation,
                if k == ProblemKind::UndeclaredOperation {
                    loc
                } else {
                    "request"
                },
                format!("the response is for an undeclared operation: {msg}"),
            ),
            Ok(c) => self.first_accepting(&c, r, |op, head, c| {
                self.check_response_op(op, req, head, resp, c)
            }),
        }
    }
}

fn guard_load(f: impl FnOnce() -> Result<Spec, LoadError>) -> Result<Spec, LoadError> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(_) => Err(LoadError::Invalid {
            location: "#".into(),
            message: "internal error while loading the document".into(),
        }),
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
        let strict = Options {
            strict_document: true,
            ..Options::default()
        };
        assert!(Spec::load(dup.as_bytes(), strict).is_err());
        let s = Spec::load(dup.as_bytes(), Options::default()).unwrap();
        assert!(s
            .warnings()
            .iter()
            .any(|w| w.message.contains("only in parameter names")));
        assert!(matches!(
            Spec::load(b"swagger: '1.2'", Options::default()),
            Err(LoadError::UnsupportedVersion(_))
        ));
        assert!(Spec::load(
            b"swagger: '2.0'\ninfo: {title: t, version: '1'}\npaths: {}\n",
            Options::default()
        )
        .is_ok());
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
