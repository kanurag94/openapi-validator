//! OpenAPI Schema Objects rewritten as JSON Schema 2020-12.
//!
//! The rewritten schemas sit at the same JSON Pointers as in the source
//! document, inside a skeleton that holds nothing else, so every local
//! `$ref` keeps working unchanged. One skeleton is built per direction,
//! because `readOnly` and `writeOnly` change `required` differently for
//! requests and responses (OpenAPI 3.0.4 §4.7.24.1, 3.1.1 §4.8.24.4).
//!
//! OpenAPI 3.0 (Schema Object, JSON Schema Wright draft 00 subset):
//! - `nullable: true` adds `"null"` to `type`, only when `type` is in the
//!   same object (3.0.4 §4.7.24.1). `enum` keeps its own say: `null` must be
//!   listed to be allowed. `allOf`/`oneOf` structure is never rewritten.
//! - boolean `exclusiveMinimum`/`exclusiveMaximum` become the 2020-12
//!   numeric form;
//! - keywords beside `$ref` are ignored (3.0.4 §4.7.23);
//! - `integer` means the JSON integer production, so `1.0` and `1e2` are not
//!   integers (custom keyword `x-oav-int`);
//! - keywords that are not part of the 3.0 Schema Object are dropped (they
//!   would be unknown keywords, which constrain nothing).
//!
//! Both versions:
//! - count keywords written as numbers such as `2.0` or `1.79e308` are read
//!   as counts; negative or fractional counts are dropped with a warning;
//! - formats outside [`crate::format::ASSERTED`] are dropped (annotations);
//! - patterns are translated by [`crate::pattern`] and refused at load when
//!   they need backtracking;
//! - `discriminator` is enforced (see `discriminator`).
//! - annotations (`description`, `example`, `default`, ...) are dropped to
//!   keep the skeleton small. `enum` and `const` values are copied untouched.

use std::collections::{HashMap, HashSet};

use serde_json::{json, Map, Value};

use crate::doc::{self, LoadError, Version, Warning};
use crate::format;
use crate::number;
use crate::pattern;

/// Which way a message travels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    Request,
    Response,
}

/// Keywords of the OpenAPI 3.0 Schema Object (3.0.4 §4.7.24.1).
const OAS30_KEYWORDS: &[&str] = &[
    "title",
    "multipleOf",
    "maximum",
    "exclusiveMaximum",
    "minimum",
    "exclusiveMinimum",
    "maxLength",
    "minLength",
    "pattern",
    "maxItems",
    "minItems",
    "uniqueItems",
    "maxProperties",
    "minProperties",
    "required",
    "enum",
    "type",
    "allOf",
    "oneOf",
    "anyOf",
    "not",
    "items",
    "properties",
    "additionalProperties",
    "description",
    "format",
    "default",
    "nullable",
    "discriminator",
    "readOnly",
    "writeOnly",
    "xml",
    "externalDocs",
    "example",
    "deprecated",
    "$ref",
];

const COUNT_KEYWORDS: &[&str] = &[
    "maxLength",
    "minLength",
    "maxItems",
    "minItems",
    "maxProperties",
    "minProperties",
    "maxContains",
    "minContains",
];

/// Base URI of the translated document resource.
pub const DOC_URI: &str = "oav:///openapi.json";

pub struct Options {
    pub assert_formats: bool,
    pub discriminator_enforce: bool,
    pub lexical_integers: bool,
    pub relax_required: bool,
    /// Run lookaround and backreferences on the bounded engine instead of
    /// refusing them.
    pub allow_backtracking: bool,
}

/// The translated skeleton for one direction.
pub struct Translated {
    pub resource: Value,
    pub warnings: Vec<Warning>,
    /// Some pattern needs the bounded backtracking engine.
    pub needs_backtracking: bool,
}

/// Per-schema context passed down while translating.
#[derive(Clone)]
struct Ctx {
    /// Pointer (in the document) of the root being translated.
    root_ptr: String,
    /// Pointer of the innermost schema resource (`$id`), for `#/...` refs.
    resource_ptr: String,
    /// Absolute base URI of that resource (JSON Schema 2020-12 §8.2.1).
    base: String,
    /// Property flags `(readOnly, writeOnly)` visible from enclosing `allOf`
    /// siblings, for `required` filtering.
    inherited: HashMap<String, (bool, bool)>,
}

struct Translator<'a> {
    root: &'a Value,
    version: Version,
    opts: &'a Options,
    warnings: Vec<Warning>,
    pending: Vec<String>,
    done_roots: HashSet<String>,
    /// Discriminator base pointer -> pointers of schemas it can select.
    disc_targets: HashMap<String, HashSet<String>>,
    /// Base pointer -> pointer of its copy without dispatch.
    nodisc: HashMap<String, String>,
    synthetic: Vec<(String, Value)>,
    pattern_cache: HashMap<String, String>,
    needs_backtracking: bool,
    anchors: Option<HashSet<String>>,
    /// Absolute `$id` (without fragment) to the pointer of its schema.
    ids: Option<HashMap<String, String>>,
}

/// Translates the schemas at `roots` (and everything they reference).
pub fn translate(
    root: &Value,
    version: Version,
    roots: &[String],
    opts: &Options,
) -> Result<Translated, LoadError> {
    let mut t = Translator {
        root,
        version,
        opts,
        warnings: Vec::new(),
        pending: roots.to_vec(),
        done_roots: HashSet::new(),
        disc_targets: HashMap::new(),
        nodisc: HashMap::new(),
        synthetic: Vec::new(),
        pattern_cache: HashMap::new(),
        needs_backtracking: false,
        anchors: None,
        ids: None,
    };
    t.find_discriminators();
    let mut skeleton = Value::Object(Map::new());
    let mut seen = HashSet::new();
    while let Some(ptr) = t.pending.pop() {
        if !seen.insert(ptr.clone()) || t.covered(&ptr) {
            continue;
        }
        let Some(src) = doc::lookup(root, &ptr) else {
            continue;
        };
        let ctx = Ctx {
            root_ptr: ptr.clone(),
            resource_ptr: "#".into(),
            base: DOC_URI.into(),
            inherited: HashMap::new(),
        };
        let out = t.schema(src, &ptr, &ctx)?;
        place(&mut skeleton, root, &ptr, out);
        t.done_roots.insert(ptr);
    }
    // Copies of discriminator bases without their dispatch.
    while let Some((ptr, src_ptr)) = t.next_nodisc() {
        let Some(src) = doc::lookup(root, &src_ptr) else {
            continue;
        };
        let ctx = Ctx {
            root_ptr: src_ptr.clone(),
            resource_ptr: "#".into(),
            base: DOC_URI.into(),
            inherited: HashMap::new(),
        };
        let mut out = t.schema_inner(src, &src_ptr, &ctx, false)?;
        if let Value::Object(m) = &mut out {
            m.remove("oneOf");
            m.remove("anyOf");
        }
        t.synthetic.push((ptr, out));
        // Anything referenced from the copy must be translated too.
        while let Some(p) = t.pending.pop() {
            if !seen.insert(p.clone()) || t.covered(&p) {
                continue;
            }
            let Some(src) = doc::lookup(root, &p) else {
                continue;
            };
            let ctx = Ctx {
                root_ptr: p.clone(),
                resource_ptr: "#".into(),
                base: DOC_URI.into(),
                inherited: HashMap::new(),
            };
            let out = t.schema(src, &p, &ctx)?;
            place(&mut skeleton, root, &p, out);
            t.done_roots.insert(p);
        }
    }
    let mut synth = Map::new();
    for (ptr, v) in std::mem::take(&mut t.synthetic) {
        if let Some(name) = ptr.strip_prefix("#/x-oav-nodisc/") {
            synth.insert(name.to_owned(), v);
        }
    }
    if !synth.is_empty() {
        if let Value::Object(m) = &mut skeleton {
            m.insert("x-oav-nodisc".into(), Value::Object(synth));
        }
    }
    Ok(Translated {
        resource: skeleton,
        warnings: t.warnings,
        needs_backtracking: t.needs_backtracking,
    })
}

/// Places `value` at `ptr` in `skeleton`, creating containers shaped like
/// the source document's.
fn place(skeleton: &mut Value, source: &Value, ptr: &str, value: Value) {
    let Some(body) = ptr.strip_prefix('#') else {
        return;
    };
    let tokens: Vec<String> = body
        .split('/')
        .skip(1)
        .map(|t| t.replace("~1", "/").replace("~0", "~"))
        .collect();
    if tokens.is_empty() {
        *skeleton = value;
        return;
    }
    let mut cur = skeleton;
    let mut src = Some(source);
    let last = tokens.len() - 1;
    for (i, tok) in tokens.iter().enumerate() {
        let src_child = src.and_then(|s| match s {
            Value::Object(m) => m.get(tok),
            Value::Array(a) => tok.parse::<usize>().ok().and_then(|n| a.get(n)),
            _ => None,
        });
        let is_array = matches!(src, Some(Value::Array(_)));
        if is_array && !cur.is_array() {
            *cur = Value::Array(Vec::new());
        } else if !is_array && !cur.is_object() {
            *cur = Value::Object(Map::new());
        }
        let slot = match cur {
            Value::Array(a) => {
                let Ok(n) = tok.parse::<usize>() else {
                    return;
                };
                if a.len() <= n {
                    a.resize(n + 1, Value::Null);
                }
                &mut a[n]
            }
            Value::Object(m) => m.entry(tok.clone()).or_insert(Value::Null),
            _ => return,
        };
        if i == last {
            *slot = value;
            return;
        }
        cur = slot;
        src = src_child;
    }
}

impl Translator<'_> {
    fn warn(&mut self, location: &str, message: impl Into<String>) {
        self.warnings.push(Warning {
            location: location.to_owned(),
            message: message.into(),
        });
    }

    /// True when `ptr` lies inside an already translated root. Checks each
    /// pointer prefix, so the cost is the pointer's depth.
    fn covered(&self, ptr: &str) -> bool {
        if self.done_roots.contains(ptr) {
            return true;
        }
        let mut end = ptr.len();
        while let Some(slash) = ptr[..end].rfind('/') {
            if self.done_roots.contains(&ptr[..slash]) {
                return true;
            }
            end = slash;
        }
        false
    }

    fn next_nodisc(&mut self) -> Option<(String, String)> {
        // Synthetic copies are created on demand by `nodisc_ptr`; translate
        // the ones not yet produced.
        let produced: HashSet<&String> = self.synthetic.iter().map(|(p, _)| p).collect();
        let next = self
            .nodisc
            .iter()
            .find(|(_, syn)| !produced.contains(syn))
            .map(|(src, syn)| (syn.clone(), src.clone()));
        next
    }

    fn nodisc_ptr(&mut self, base: &str) -> String {
        if let Some(p) = self.nodisc.get(base) {
            return p.clone();
        }
        let p = format!("#/x-oav-nodisc/{}", self.nodisc.len());
        self.nodisc.insert(base.to_owned(), p.clone());
        p
    }

    /// Finds discriminator bases among component schemas and what each can
    /// select: explicit mapping targets, `oneOf`/`anyOf` branches, and
    /// component schemas that extend the base through `allOf`.
    fn find_discriminators(&mut self) {
        let Some(schemas) = self
            .root
            .pointer("/components/schemas")
            .and_then(Value::as_object)
        else {
            return;
        };
        for (name, s) in schemas {
            let ptr = doc::join("#/components/schemas", name);
            let Some(d) = s.get("discriminator").filter(|d| d.is_object()) else {
                continue;
            };
            let mut targets = HashSet::new();
            let enforce = self.opts.discriminator_enforce;
            if enforce {
                for (_, r) in mapping_entries(d) {
                    targets.insert(r);
                }
            }
            // As an annotation the discriminator changes nothing, so only a
            // real cycle (a oneOf/anyOf branch that extends this base) needs
            // the copy without dispatch; with enforcement, every target does.
            for key in ["oneOf", "anyOf"] {
                for b in s.get(key).and_then(Value::as_array).into_iter().flatten() {
                    if let Some(r) = b.get("$ref").and_then(Value::as_str) {
                        targets.insert(doc::canonical_pointer(r).unwrap_or_else(|| r.to_owned()));
                    }
                }
            }
            for (cname, c) in schemas {
                let extends = c
                    .get("allOf")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|m| m.get("$ref").and_then(Value::as_str))
                    .any(|r| doc::canonical_pointer(r).as_deref() == Some(ptr.as_str()));
                if extends && enforce {
                    targets.insert(doc::join("#/components/schemas", cname));
                }
            }
            self.disc_targets.insert(ptr, targets);
        }
    }

    fn schema(&mut self, v: &Value, ptr: &str, ctx: &Ctx) -> Result<Value, LoadError> {
        self.schema_inner(v, ptr, ctx, true)
    }

    fn check_ref(&mut self, r: &str, ptr: &str, ctx: &Ctx) -> Result<(), LoadError> {
        // The whole document named by its own URI (multi-document loads
        // write cross-document references this way inside `$id` scopes).
        if let Some(frag) = r.strip_prefix(DOC_URI).and_then(|f| f.strip_prefix('#')) {
            let target = doc::canonical_pointer(&format!("#{frag}")).unwrap_or_default();
            if doc::lookup(self.root, &target).is_none() {
                return Err(LoadError::BrokenRef {
                    reference: r.to_owned(),
                    location: ptr.to_owned(),
                });
            }
            self.pending.push(target);
            return Ok(());
        }
        if let Some(frag) = r.strip_prefix('#') {
            if frag.is_empty() || frag.starts_with('/') {
                let target = if ctx.resource_ptr == "#" {
                    doc::canonical_pointer(r).unwrap_or_else(|| r.to_owned())
                } else {
                    format!(
                        "{}{}",
                        ctx.resource_ptr,
                        doc::canonical_pointer(r)
                            .unwrap_or_default()
                            .trim_start_matches('#')
                    )
                };
                if doc::lookup(self.root, &target).is_none() {
                    return Err(LoadError::BrokenRef {
                        reference: r.to_owned(),
                        location: ptr.to_owned(),
                    });
                }
                self.pending.push(target);
                return Ok(());
            }
            // A plain-name fragment refers to an `$anchor` (JSON Schema
            // 2020-12 §8.2.2); only 3.1 schemas have anchors.
            if self.version != Version::V30 && self.anchor_exists(frag) {
                return Ok(());
            }
            return Err(LoadError::BrokenRef {
                reference: r.to_owned(),
                location: ptr.to_owned(),
            });
        }
        if self.version != Version::V30 && self.embedded_id_exists(&resolve(&ctx.base, r)) {
            return Ok(());
        }
        Err(LoadError::ExternalRef {
            reference: r.to_owned(),
            location: ptr.to_owned(),
        })
    }

    fn schema_inner(
        &mut self,
        v: &Value,
        ptr: &str,
        ctx: &Ctx,
        dispatch: bool,
    ) -> Result<Value, LoadError> {
        let m = match v {
            Value::Bool(_) => return Ok(v.clone()),
            Value::Object(m) => m,
            // Not a schema; constrain nothing rather than refuse.
            _ => {
                self.warn(
                    ptr,
                    "schema is neither an object nor a boolean; treated as true",
                );
                return Ok(Value::Bool(true));
            }
        };
        let v30 = self.version == Version::V30;
        if v30 {
            if let Some(r) = m.get("$ref") {
                let Some(r) = r.as_str() else {
                    return Err(LoadError::Invalid {
                        location: ptr.to_owned(),
                        message: "$ref must be a string".into(),
                    });
                };
                self.check_ref(r, ptr, ctx)?;
                return Ok(json!({ "$ref": r }));
            }
        }
        let mut ctx = ctx.clone();
        if !v30 {
            if let Some(id) = m.get("$id").and_then(Value::as_str) {
                if !id.starts_with('#') {
                    ctx.resource_ptr = ptr.to_owned();
                    ctx.base = resolve(&ctx.base, id);
                }
            }
        }
        // Flags of properties visible to `required` here.
        let mut visible = ctx.inherited.clone();
        if let Some(props) = m.get("properties").and_then(Value::as_object) {
            for (name, p) in props {
                visible.insert(name.clone(), self.access_flags(p, 0));
            }
        }
        let mut child_ctx = ctx.clone();
        child_ctx.inherited = HashMap::new();
        let mut allof_ctx = ctx.clone();
        if let Some(all) = m.get("allOf").and_then(Value::as_array) {
            let mut merged = visible.clone();
            for member in all {
                self.collect_props(member, &mut merged, 0);
            }
            allof_ctx.inherited = merged.clone();
            visible = merged;
        }

        let mut out = Map::new();
        for (k, val) in m {
            if v30 && !OAS30_KEYWORDS.contains(&k.as_str()) {
                continue;
            }
            let kp = doc::join(ptr, k);
            match k.as_str() {
                "$ref" => {
                    let Some(r) = val.as_str() else {
                        return Err(LoadError::Invalid {
                            location: kp,
                            message: "$ref must be a string".into(),
                        });
                    };
                    self.check_ref(r, ptr, &ctx)?;
                    out.insert(k.clone(), Value::String(r.to_owned()));
                }
                "$id" | "$anchor" | "$dynamicAnchor" | "$schema" | "$vocabulary" => {
                    out.insert(k.clone(), val.clone());
                }
                "$dynamicRef" => {
                    out.insert(k.clone(), val.clone());
                }
                "not"
                | "additionalProperties"
                | "propertyNames"
                | "contains"
                | "if"
                | "then"
                | "else"
                | "unevaluatedProperties"
                | "unevaluatedItems" => {
                    out.insert(k.clone(), self.schema(val, &kp, &child_ctx)?);
                }
                "items" => match val {
                    // An array `items` is draft 4 tuple form; 2020-12 calls it
                    // `prefixItems`.
                    Value::Array(a) => {
                        let mut items = Vec::with_capacity(a.len());
                        for (i, s) in a.iter().enumerate() {
                            items.push(self.schema(
                                s,
                                &doc::join(&kp, &i.to_string()),
                                &child_ctx,
                            )?);
                        }
                        out.insert("prefixItems".into(), Value::Array(items));
                    }
                    _ => {
                        out.insert(k.clone(), self.schema(val, &kp, &child_ctx)?);
                    }
                },
                "allOf" | "anyOf" | "oneOf" | "prefixItems" => {
                    let Some(a) = val.as_array() else {
                        self.warn(&kp, format!("{k} is not an array; ignored"));
                        continue;
                    };
                    let mut items = Vec::with_capacity(a.len());
                    for (i, s) in a.iter().enumerate() {
                        let c = if k == "allOf" { &allof_ctx } else { &child_ctx };
                        let ip = doc::join(&kp, &i.to_string());
                        let translated = if k == "allOf" {
                            self.allof_member(s, &ip, c)?
                        } else {
                            self.schema(s, &ip, c)?
                        };
                        items.push(translated);
                    }
                    out.insert(k.clone(), Value::Array(items));
                }
                "properties" | "$defs" | "definitions" | "dependentSchemas" => {
                    let Some(props) = val.as_object() else {
                        self.warn(&kp, format!("{k} is not an object; ignored"));
                        continue;
                    };
                    let mut o = Map::new();
                    for (name, s) in props {
                        o.insert(
                            name.clone(),
                            self.schema(s, &doc::join(&kp, name), &child_ctx)?,
                        );
                    }
                    out.insert(k.clone(), Value::Object(o));
                }
                "patternProperties" => {
                    let Some(props) = val.as_object() else {
                        continue;
                    };
                    let mut o = Map::new();
                    for (re, s) in props {
                        let translated = self.pattern(re, &kp)?;
                        o.insert(translated, self.schema(s, &doc::join(&kp, re), &child_ctx)?);
                    }
                    out.insert(k.clone(), Value::Object(o));
                }
                "pattern" => {
                    if let Some(p) = val.as_str() {
                        let translated = self.pattern(p, &kp)?;
                        if translated != p {
                            // Kept for messages only.
                            out.insert("x-oav-pattern-src".into(), Value::String(p.to_owned()));
                        }
                        out.insert(k.clone(), Value::String(translated));
                    }
                }
                "type" => {
                    let types = match val {
                        Value::String(_) => val.clone(),
                        Value::Array(a) if a.iter().all(Value::is_string) => val.clone(),
                        _ => {
                            self.warn(&kp, "type is not a string or array of strings; ignored");
                            continue;
                        }
                    };
                    out.insert(k.clone(), types);
                }
                "enum" => {
                    if val.is_array() {
                        out.insert(k.clone(), val.clone());
                    } else {
                        self.warn(&kp, "enum is not an array; ignored");
                    }
                }
                "const" => {
                    out.insert(k.clone(), val.clone());
                }
                "required" => {
                    let Some(names) = val.as_array() else {
                        continue;
                    };
                    // A readOnly property is required only in responses and a
                    // writeOnly one only in requests; both directions share
                    // one translation, so those names go to `x-oav-required`.
                    let mut always = Vec::new();
                    let mut request_only = Vec::new();
                    let mut response_only = Vec::new();
                    let mut neither = Vec::new();
                    for n in names.iter().filter_map(Value::as_str) {
                        let (ro, wo) = visible.get(n).copied().unwrap_or((false, false));
                        let v = Value::String(n.to_owned());
                        match (ro, wo) {
                            (false, false) => always.push(v),
                            (true, false) => response_only.push(v),
                            (false, true) => request_only.push(v),
                            (true, true) => neither.push(v),
                        }
                    }
                    if !self.opts.relax_required {
                        // OpenAPI 3.1: readOnly and writeOnly are annotations;
                        // `required` applies in both directions.
                        always.append(&mut request_only);
                        always.append(&mut response_only);
                        always.append(&mut neither);
                    }
                    out.insert(k.clone(), Value::Array(always));
                    if !request_only.is_empty() || !response_only.is_empty() {
                        out.insert(
                            "x-oav-required".into(),
                            json!({"request": request_only, "response": response_only}),
                        );
                    }
                }
                "dependentRequired" => {
                    if val.is_object() {
                        out.insert(k.clone(), val.clone());
                    }
                }
                "uniqueItems" => {
                    if val.is_boolean() {
                        out.insert(k.clone(), val.clone());
                    }
                }
                "minimum" | "maximum" | "multipleOf" => {
                    if val.is_number() {
                        out.insert(k.clone(), val.clone());
                    } else {
                        self.warn(&kp, format!("{k} is not a number; ignored"));
                    }
                }
                "exclusiveMinimum" | "exclusiveMaximum" => match val {
                    Value::Number(_) => {
                        out.insert(k.clone(), val.clone());
                    }
                    Value::Bool(true) => {
                        let bound = if k == "exclusiveMinimum" {
                            "minimum"
                        } else {
                            "maximum"
                        };
                        if !v30 {
                            self.warn(
                                &kp,
                                format!("boolean {k} is OpenAPI 3.0 syntax; read as 3.0"),
                            );
                        }
                        if let Some(b) = m.get(bound).filter(|b| b.is_number()) {
                            out.insert(k.clone(), b.clone());
                        }
                    }
                    _ => {}
                },
                c if COUNT_KEYWORDS.contains(&c) => {
                    if let Some(n) = self.count(val, &kp) {
                        out.insert(k.clone(), n);
                    }
                }
                "format" => {
                    let Some(f) = val.as_str() else {
                        continue;
                    };
                    if !self.opts.assert_formats || !format::ASSERTED.contains(&f) {
                        continue;
                    }
                    if format::is_numeric_format(f) {
                        out.insert("x-oav-int-format".into(), Value::String(f.to_owned()));
                    } else {
                        out.insert(k.clone(), val.clone());
                    }
                }
                // Validation-free annotations and the keywords consumed below.
                _ => {}
            }
        }
        // 3.0 boolean exclusive bounds replace the inclusive bound.
        for (ex, bound) in [
            ("exclusiveMinimum", "minimum"),
            ("exclusiveMaximum", "maximum"),
        ] {
            if m.get(ex) == Some(&Value::Bool(true)) && out.contains_key(ex) {
                out.remove(bound);
            }
        }
        if v30 {
            if m.get("nullable") == Some(&Value::Bool(true)) {
                match out.get_mut("type") {
                    Some(Value::String(t)) => {
                        let t = std::mem::take(t);
                        out.insert("type".into(), json!([t, "null"]));
                    }
                    Some(Value::Array(a)) if !a.iter().any(|t| t == "null") => {
                        a.push(Value::String("null".into()));
                    }
                    _ => {}
                }
            }
            let has = |t: &str| match out.get("type") {
                Some(Value::String(s)) => s == t,
                Some(Value::Array(a)) => a.iter().any(|x| x == t),
                _ => false,
            };
            if self.opts.lexical_integers && has("integer") && !has("number") {
                out.insert("x-oav-int".into(), Value::Bool(true));
            }
        }
        if dispatch && self.opts.discriminator_enforce {
            if let Some(d) = m.get("discriminator").filter(|d| d.is_object()) {
                self.discriminator(d, m, ptr, &ctx, &mut out)?;
            }
        }
        Ok(Value::Object(out))
    }

    /// An `allOf` member. A reference from a discriminator target to its base
    /// goes to the base's copy without dispatch, so inheritance does not
    /// loop back into the selection (see SPEC_DECISIONS.md).
    fn allof_member(&mut self, s: &Value, ptr: &str, ctx: &Ctx) -> Result<Value, LoadError> {
        if let Some(r) = s.get("$ref").and_then(Value::as_str) {
            let target = doc::canonical_pointer(r).unwrap_or_default();
            let is_child = self
                .disc_targets
                .get(&target)
                .is_some_and(|t| t.contains(&ctx.root_ptr));
            if is_child && ctx.resource_ptr == "#" {
                let mut out = self.schema(s, ptr, ctx)?;
                let syn = self.nodisc_ptr(&target);
                if let Value::Object(o) = &mut out {
                    o.insert("$ref".into(), Value::String(syn));
                }
                return Ok(out);
            }
        }
        self.schema(s, ptr, ctx)
    }

    fn pattern(&mut self, p: &str, location: &str) -> Result<String, LoadError> {
        if let Some(t) = self.pattern_cache.get(p) {
            return Ok(t.clone());
        }
        let to_err = |error| LoadError::Pattern {
            location: location.to_owned(),
            error,
        };
        let (translated, backtracking) = if self.opts.allow_backtracking {
            pattern::translate_any(p).map_err(to_err)?
        } else {
            (pattern::translate(p).map_err(to_err)?, false)
        };
        self.needs_backtracking |= backtracking;
        pattern::build_any(&translated, backtracking).map_err(|reason| LoadError::Pattern {
            location: location.to_owned(),
            error: pattern::PatternError {
                pattern: p.to_owned(),
                reason,
            },
        })?;
        self.pattern_cache.insert(p.to_owned(), translated.clone());
        Ok(translated)
    }

    fn count(&mut self, v: &Value, location: &str) -> Option<Value> {
        let lit = match v {
            Value::Number(n) => n.to_string(),
            _ => {
                self.warn(location, "count is not a number; ignored");
                return None;
            }
        };
        match number::as_count(&lit) {
            Some(Ok(n)) => Some(Value::from(n)),
            Some(Err(())) if lit.starts_with('-') => {
                self.warn(location, "negative count ignored");
                None
            }
            // Larger than any count: as good as unbounded for a maximum, and
            // unreachable for a minimum.
            Some(Err(())) => {
                if location
                    .rsplit('/')
                    .next()
                    .is_some_and(|k| k.starts_with("max"))
                {
                    None
                } else {
                    Some(Value::from(u64::MAX))
                }
            }
            None => {
                self.warn(location, "count is not an integer; ignored");
                None
            }
        }
    }

    /// `(readOnly, writeOnly)` of a property schema, following `$ref` and
    /// `allOf` (bounded).
    fn access_flags(&self, s: &Value, depth: usize) -> (bool, bool) {
        if depth > doc::MAX_REF_CHAIN {
            return (false, false);
        }
        let Some(m) = s.as_object() else {
            return (false, false);
        };
        let mut ro = m.get("readOnly") == Some(&Value::Bool(true));
        let mut wo = m.get("writeOnly") == Some(&Value::Bool(true));
        if let Some(r) = m.get("$ref").and_then(Value::as_str) {
            if self.version == Version::V30 {
                // Siblings of $ref are ignored in 3.0.
                ro = false;
                wo = false;
            }
            if let Some(t) = doc::lookup_ref(self.root, r) {
                let (a, b) = self.access_flags(t, depth + 1);
                ro |= a;
                wo |= b;
            }
        }
        if self.version != Version::V30 || !m.contains_key("$ref") {
            for member in m
                .get("allOf")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let (a, b) = self.access_flags(member, depth + 1);
                ro |= a;
                wo |= b;
            }
        }
        (ro, wo)
    }

    /// Collects property flags from an `allOf` member, following `$ref`.
    fn collect_props(&self, s: &Value, into: &mut HashMap<String, (bool, bool)>, depth: usize) {
        if depth > doc::MAX_REF_CHAIN {
            return;
        }
        let Some(m) = s.as_object() else {
            return;
        };
        if let Some(r) = m.get("$ref").and_then(Value::as_str) {
            if let Some(t) = doc::lookup_ref(self.root, r) {
                self.collect_props(t, into, depth + 1);
            }
            if self.version == Version::V30 {
                return;
            }
        }
        if let Some(props) = m.get("properties").and_then(Value::as_object) {
            for (name, p) in props {
                let f = self.access_flags(p, 0);
                let e = into.entry(name.clone()).or_insert((false, false));
                e.0 |= f.0;
                e.1 |= f.1;
            }
        }
        for member in m
            .get("allOf")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            self.collect_props(member, into, depth + 1);
        }
    }

    /// Enforces a Discriminator Object (OpenAPI 3.0.4 §4.7.25, 3.1.1
    /// §4.8.25): when the payload carries the property, its value must name
    /// a mapped schema and the payload is checked against that schema. When
    /// every `oneOf`/`anyOf` branch is mapped, the named branch alone decides
    /// (option `discriminator: Enforce`); without the property, the
    /// original `oneOf`/`anyOf` applies.
    fn discriminator(
        &mut self,
        d: &Value,
        m: &Map<String, Value>,
        ptr: &str,
        ctx: &Ctx,
        out: &mut Map<String, Value>,
    ) -> Result<(), LoadError> {
        let Some(prop) = d.get("propertyName").and_then(Value::as_str) else {
            self.warn(ptr, "discriminator without propertyName; ignored");
            return Ok(());
        };
        let mut values: Vec<(String, String)> = mapping_entries(d);
        let mapped: HashSet<String> = values.iter().map(|(_, r)| r.clone()).collect();
        let branch_key = ["oneOf", "anyOf"]
            .into_iter()
            .find(|k| m.get(*k).is_some_and(Value::is_array));
        let mut all_mapped = branch_key.is_some();
        if let Some(k) = branch_key {
            for b in m.get(k).and_then(Value::as_array).into_iter().flatten() {
                match b.get("$ref").and_then(Value::as_str) {
                    Some(r) => {
                        let target = doc::canonical_pointer(r).unwrap_or_else(|| r.to_owned());
                        if !mapped.contains(&target) {
                            match target.strip_prefix("#/components/schemas/") {
                                Some(name) if !name.contains('/') => {
                                    values
                                        .push((name.replace("~1", "/").replace("~0", "~"), target));
                                }
                                _ => all_mapped = false,
                            }
                        }
                    }
                    None => all_mapped = false,
                }
            }
        } else {
            // allOf-style: component schemas extending this one.
            let mut children: Vec<String> = self
                .disc_targets
                .get(&ctx.root_ptr)
                .map(|t| t.iter().cloned().collect())
                .unwrap_or_default();
            children.sort();
            for c in children {
                if mapped.contains(&c) {
                    continue;
                }
                if let Some(name) = c.strip_prefix("#/components/schemas/") {
                    if ptr == ctx.root_ptr && c != ctx.root_ptr {
                        values.push((name.replace("~1", "/").replace("~0", "~"), c));
                    }
                }
            }
        }
        if values.is_empty() {
            return Ok(());
        }
        for (_, target) in &values {
            if doc::lookup(self.root, target).is_none() {
                return Err(LoadError::BrokenRef {
                    reference: target.clone(),
                    location: doc::join(ptr, "discriminator"),
                });
            }
            self.pending.push(target.clone());
        }
        let mut rules = Vec::new();
        for (value, target) in &values {
            // A target that is this very schema needs no dispatch.
            if *target == ctx.root_ptr && ptr == ctx.root_ptr {
                continue;
            }
            rules.push(json!({
                "if": {"properties": {prop: {"const": value}}, "required": [prop]},
                "then": {"$ref": target}
            }));
        }
        let names: Vec<Value> = values
            .iter()
            .map(|(v, _)| Value::String(v.clone()))
            .collect();
        let selects = all_mapped;
        let mut then = Map::new();
        if branch_key.is_none() || all_mapped {
            let mut allowed = names;
            if branch_key.is_none() && ptr == ctx.root_ptr {
                // The base itself may be named by its own schema name.
                if let Some(own) = ptr.strip_prefix("#/components/schemas/") {
                    allowed.push(Value::String(own.replace("~1", "/").replace("~0", "~")));
                }
            }
            then.insert("properties".into(), json!({ prop: {"enum": allowed} }));
        }
        if !rules.is_empty() {
            then.insert("allOf".into(), Value::Array(rules));
        }
        let mut dispatch = Map::new();
        dispatch.insert("if".into(), json!({"required": [prop]}));
        dispatch.insert("then".into(), Value::Object(then));
        if selects {
            if let Some(k) = branch_key {
                if let Some(branches) = out.remove(k) {
                    let mut e = Map::new();
                    e.insert(k.to_owned(), branches);
                    dispatch.insert("else".into(), Value::Object(e));
                }
            }
        }
        let all = out
            .entry("allOf")
            .or_insert_with(|| Value::Array(Vec::new()));
        if let Value::Array(a) = all {
            a.push(Value::Object(dispatch));
        }
        Ok(())
    }
}

/// `(value, target pointer)` pairs of an explicit `mapping`. A mapping value
/// that is a bare name means `#/components/schemas/<name>`.
fn mapping_entries(d: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Some(map) = d.get("mapping").and_then(Value::as_object) {
        for (value, target) in map {
            let Some(t) = target.as_str() else {
                continue;
            };
            let ptr = if t.starts_with('#') {
                doc::canonical_pointer(t).unwrap_or_else(|| t.to_owned())
            } else {
                doc::join("#/components/schemas", t)
            };
            out.push((value.clone(), ptr));
        }
    }
    out
}

/// Every `$anchor`/`$dynamicAnchor` name, and every `$id` without its
/// fragment, anywhere in the document.
fn collect_ids(root: &Value) -> (HashSet<String>, HashMap<String, String>) {
    let mut anchors = HashSet::new();
    let mut ids = HashMap::new();
    let mut stack = vec![(root, DOC_URI.to_owned(), "#".to_owned())];
    while let Some((v, base, ptr)) = stack.pop() {
        match v {
            Value::Object(m) => {
                for key in ["$anchor", "$dynamicAnchor"] {
                    if let Some(a) = m.get(key).and_then(Value::as_str) {
                        anchors.insert(a.to_owned());
                    }
                }
                let mut base = base;
                if let Some(id) = m
                    .get("$id")
                    .and_then(Value::as_str)
                    .filter(|i| !i.starts_with('#'))
                {
                    base = resolve(&base, id);
                    ids.entry(base.split('#').next().unwrap_or("").to_owned())
                        .or_insert_with(|| ptr.clone());
                }
                stack.extend(m.iter().map(|(k, c)| (c, base.clone(), doc::join(&ptr, k))));
            }
            Value::Array(a) => stack.extend(
                a.iter()
                    .enumerate()
                    .map(|(i, c)| (c, base.clone(), doc::join(&ptr, &i.to_string()))),
            ),
            _ => {}
        }
    }
    (anchors, ids)
}

/// Resolves a URI reference against a base (RFC 3986 §5.2); on failure the
/// reference is returned as is.
fn resolve(base: &str, reference: &str) -> String {
    jsonschema::uri::from_str(base)
        .ok()
        .and_then(|b| jsonschema::uri::resolve_against(&b.borrow(), reference).ok())
        .map_or_else(|| reference.to_owned(), |u| u.as_str().to_owned())
}

impl Translator<'_> {
    fn ensure_ids(&mut self) {
        if self.anchors.is_none() {
            let (a, i) = collect_ids(self.root);
            self.anchors = Some(a);
            self.ids = Some(i);
        }
    }

    fn anchor_exists(&mut self, name: &str) -> bool {
        self.ensure_ids();
        self.anchors.as_ref().is_some_and(|a| a.contains(name))
    }

    /// True when an absolute reference names a schema resource the engine
    /// holds without retrieval: an `$id` declared in the document, or one
    /// of the JSON Schema metaschemas, which are built in.
    fn embedded_id_exists(&mut self, absolute: &str) -> bool {
        let (base, _) = absolute.split_once('#').unwrap_or((absolute, ""));
        if base.is_empty() {
            return false;
        }
        if base.starts_with("https://json-schema.org/")
            || base.starts_with("http://json-schema.org/")
        {
            return true;
        }
        self.ensure_ids();
        // The resource must be translated too, or the engine cannot find it.
        match self.ids.as_ref().and_then(|ids| ids.get(base)).cloned() {
            Some(ptr) => {
                self.pending.push(ptr);
                true
            }
            None => false,
        }
    }
}
