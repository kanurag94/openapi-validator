//! Locating problems in a value already known to be invalid.
//!
//! The walk visits each (schema node, value node) pair at most once and
//! stops after [`STEP_BUDGET`] keyword checks or [`MAX_DEPTH`] nested calls,
//! so its cost is bounded whatever the schema's shape. For `anyOf`, `oneOf`,
//! `not` and `if` it asks `jsonschema` for the branch verdicts instead of
//! descending into every branch, which is what makes other validators
//! exponential on recursive schemas.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use super::{DirStore, SchemaStore};
use crate::doc;
use crate::format;
use crate::number::{self, Decimal};

const STEP_BUDGET: usize = 20_000;
const MAX_DEPTH: usize = 96;

pub(crate) fn explain(
    store: &SchemaStore,
    dir: &DirStore,
    root_ptr: &str,
    instance: &Value,
    max: usize,
) -> (Vec<(String, String)>, usize) {
    let Some(resource) = store.resource() else {
        return (vec![(String::new(), "does not match its schema".into())], 0);
    };
    let mut e = Explainer {
        store,
        dir,
        resource,
        out: Vec::new(),
        more: 0,
        max: max.max(1),
        steps: 0,
        visited: HashSet::new(),
    };
    if let Some(s) = doc::lookup(resource, root_ptr) {
        e.node(s, root_ptr, instance, "", 0);
    }
    if e.out.is_empty() && e.more == 0 {
        e.out
            .push((String::new(), "does not match its schema".into()));
    }
    (e.out, e.more)
}

struct Explainer<'a> {
    store: &'a SchemaStore,
    dir: &'a DirStore,
    resource: &'a Value,
    out: Vec<(String, String)>,
    more: usize,
    max: usize,
    steps: usize,
    visited: HashSet<(usize, usize)>,
}

fn child(path: &str, token: &str) -> String {
    format!("{path}/{}", doc::escape_token(token))
}

fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

fn dec(v: &Value) -> Option<Decimal> {
    match v {
        Value::Number(n) => Decimal::parse(&n.to_string()),
        _ => None,
    }
}

/// JSON Schema type test (2020-12 §6.1.1; `integer` is any number with a
/// zero fractional part).
fn has_type(v: &Value, t: &str) -> bool {
    match t {
        "null" => v.is_null(),
        "boolean" => v.is_boolean(),
        "object" => v.is_object(),
        "array" => v.is_array(),
        "string" => v.is_string(),
        "number" => v.is_number(),
        "integer" => dec(v).is_some_and(|d| d.is_integer()),
        _ => false,
    }
}

/// Equality with numbers compared by value (2020-12 §4.2.2).
pub(crate) fn json_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(_), Value::Number(_)) => match (dec(a), dec(b)) {
            (Some(x), Some(y)) => x == y,
            _ => false,
        },
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| json_eq(p, q))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|w| json_eq(v, w)))
        }
        _ => a == b,
    }
}

/// A canonical text form: equal values (by [`json_eq`]) have equal keys.
pub(crate) fn canonical(v: &Value, out: &mut String) {
    match v {
        Value::Number(_) => {
            out.push('n');
            out.push_str(&dec(v).map(|d| d.to_canonical()).unwrap_or_default());
        }
        Value::Array(a) => {
            out.push('[');
            for x in a {
                canonical(x, out);
                out.push(',');
            }
            out.push(']');
        }
        Value::Object(m) => {
            out.push('{');
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            for k in keys {
                out.push_str(&Value::String(k.clone()).to_string());
                out.push(':');
                if let Some(x) = m.get(k) {
                    canonical(x, out);
                }
                out.push(',');
            }
            out.push('}');
        }
        other => out.push_str(&other.to_string()),
    }
}

fn short(v: &Value) -> String {
    let s = v.to_string();
    if s.len() > 40 {
        format!("{}…", crate::spec::truncate(&s, 40))
    } else {
        s
    }
}

impl<'a> Explainer<'a> {
    fn push(&mut self, path: &str, msg: String) {
        if self.out.len() >= self.max {
            self.more += 1;
        } else {
            self.out.push((path.to_owned(), msg));
        }
    }

    fn found(&self) -> usize {
        self.out.len() + self.more
    }

    /// The schema a `$ref` at `sptr` names, and its pointer. `#` references
    /// resolve within the innermost `$id` resource (JSON Schema Core 2020-12
    /// §8.2.1); others through the `$id` index. Anchors are not followed.
    fn resolve_ref(&self, r: &str, sptr: &str) -> Option<(String, &'a Value)> {
        let scope = self.store.scope_of(sptr);
        let (base_ptr, fragment) = if let Some(f) = r.strip_prefix('#') {
            (scope.map_or("#", |(p, _)| p).to_owned(), f.to_owned())
        } else {
            let base = scope.map_or(super::DOC_URI, |(_, u)| u);
            let abs = super::resolve_uri(base, r)?;
            let (uri, f) = abs.split_once('#').unwrap_or((abs.as_str(), ""));
            let p = if uri == super::DOC_URI {
                "#"
            } else {
                self.store.id_pointer(uri)?
            };
            (p.to_owned(), f.to_owned())
        };
        if !(fragment.is_empty() || fragment.starts_with('/')) {
            return None;
        }
        let rel = doc::canonical_pointer(&format!("#{fragment}"))?;
        let ptr = if base_ptr == "#" {
            rel
        } else {
            format!("{base_ptr}{}", &rel[1..])
        };
        doc::lookup(self.resource, &ptr).map(|t| (ptr, t))
    }

    /// Properties a schema evaluates for an object (JSON Schema Core 2020-12
    /// §11.3): its own `properties`, `patternProperties` and
    /// `additionalProperties`, and those of applicators that apply.
    fn evaluated_props(
        &self,
        s: &'a Value,
        sptr: &str,
        inst: &Value,
        depth: usize,
    ) -> HashSet<String> {
        let mut out = HashSet::new();
        let (Some(m), Some(o)) = (s.as_object(), inst.as_object()) else {
            return out;
        };
        if depth > MAX_DEPTH {
            return o.keys().cloned().collect();
        }
        // A nested `unevaluatedProperties` evaluates the rest; the schema
        // being explained (depth 0) does not count its own.
        if m.contains_key("additionalProperties")
            || (depth > 0 && m.contains_key("unevaluatedProperties"))
        {
            return o.keys().cloned().collect();
        }
        if let Some(p) = m.get("properties").and_then(Value::as_object) {
            out.extend(o.keys().filter(|k| p.contains_key(*k)).cloned());
        }
        for re in m
            .get("patternProperties")
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(|p| p.keys())
        {
            if let Some(rx) = self.store.regex(re) {
                out.extend(o.keys().filter(|k| rx.is_match(k) == Some(true)).cloned());
            }
        }
        for (sub, sp) in self.applied(m, sptr, inst) {
            out.extend(self.evaluated_props(sub, &sp, inst, depth + 1));
        }
        out
    }

    /// Items a schema evaluates for an array (§11.2).
    fn evaluated_items(
        &self,
        s: &'a Value,
        sptr: &str,
        inst: &Value,
        depth: usize,
    ) -> HashSet<usize> {
        let mut out = HashSet::new();
        let (Some(m), Some(a)) = (s.as_object(), inst.as_array()) else {
            return out;
        };
        if depth > MAX_DEPTH
            || m.contains_key("items")
            || (depth > 0 && m.contains_key("unevaluatedItems"))
        {
            return (0..a.len()).collect();
        }
        let n = m
            .get("prefixItems")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        out.extend(0..n.min(a.len()));
        if m.contains_key("contains") {
            let cp = doc::join(sptr, "contains");
            out.extend((0..a.len()).filter(|&i| self.valid_at(&cp, &a[i]) == Some(true)));
        }
        for (sub, sp) in self.applied(m, sptr, inst) {
            out.extend(self.evaluated_items(sub, &sp, inst, depth + 1));
        }
        out
    }

    /// Subschemas whose annotations reach this schema for this value: every
    /// `allOf` member, `anyOf`/`oneOf` branches that pass, the `if`/`then`
    /// or `else` that applies, `dependentSchemas` of present names, and the
    /// `$ref` target.
    fn applied(
        &self,
        m: &'a serde_json::Map<String, Value>,
        sptr: &str,
        inst: &Value,
    ) -> Vec<(&'a Value, String)> {
        let mut out = Vec::new();
        for k in ["allOf", "anyOf", "oneOf"] {
            for (i, sub) in m
                .get(k)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .enumerate()
            {
                let sp = doc::join(&doc::join(sptr, k), &i.to_string());
                if k == "allOf" || self.valid_at(&sp, inst) == Some(true) {
                    out.push((sub, sp));
                }
            }
        }
        if let Some(cond) = m.get("if") {
            let ip = doc::join(sptr, "if");
            if self.valid_at(&ip, inst) == Some(true) {
                out.push((cond, ip));
                if let Some(t) = m.get("then") {
                    out.push((t, doc::join(sptr, "then")));
                }
            } else if let Some(e) = m.get("else") {
                out.push((e, doc::join(sptr, "else")));
            }
        }
        if let (Some(ds), Some(o)) = (
            m.get("dependentSchemas").and_then(Value::as_object),
            inst.as_object(),
        ) {
            for (k, sub) in ds {
                if o.contains_key(k) {
                    out.push((sub, doc::join(&doc::join(sptr, "dependentSchemas"), k)));
                }
            }
        }
        if let Some((p, t)) = m
            .get("$ref")
            .and_then(Value::as_str)
            .and_then(|r| self.resolve_ref(r, sptr))
        {
            out.push((t, p));
        }
        out
    }

    /// `jsonschema`'s verdict for the schema at `ptr`.
    fn valid_at(&self, ptr: &str, inst: &Value) -> Option<bool> {
        match &*self.store.sub_validator(self.dir, ptr) {
            Ok(v) => Some(v.is_valid(inst)),
            Err(_) => None,
        }
    }

    fn node(&mut self, s: &'a Value, sptr: &str, inst: &Value, ipath: &str, depth: usize) {
        if depth > MAX_DEPTH || self.steps > STEP_BUDGET || self.out.len() >= self.max {
            return;
        }
        if !self
            .visited
            .insert((s as *const Value as usize, inst as *const Value as usize))
        {
            return;
        }
        let m = match s {
            Value::Bool(false) => {
                self.push(ipath, "is not allowed here".into());
                return;
            }
            Value::Object(m) => m,
            _ => return,
        };
        let before = self.found();
        let mut opaque = false;
        for (k, kv) in m {
            self.steps += 1;
            let kp = doc::join(sptr, k);
            match k.as_str() {
                "$ref" => match kv.as_str().and_then(|r| self.resolve_ref(r, sptr)) {
                    Some((target_ptr, t)) => self.node(t, &target_ptr, inst, ipath, depth + 1),
                    None => opaque = true,
                },
                "$dynamicRef" => opaque = true,
                "unevaluatedProperties" => {
                    let Some(o) = inst.as_object() else {
                        continue;
                    };
                    let seen = self.evaluated_props(s, sptr, inst, 0);
                    for (name, x) in o {
                        if seen.contains(name.as_str()) {
                            continue;
                        }
                        match kv {
                            Value::Bool(false) => self.push(
                                &child(ipath, name),
                                "is not allowed (unevaluatedProperties)".into(),
                            ),
                            sch => self.node(sch, &kp, x, &child(ipath, name), depth + 1),
                        }
                    }
                }
                "unevaluatedItems" => {
                    let Some(a) = inst.as_array() else {
                        continue;
                    };
                    let seen = self.evaluated_items(s, sptr, inst, 0);
                    for (i, x) in a.iter().enumerate() {
                        if seen.contains(&i) {
                            continue;
                        }
                        let at = child(ipath, &i.to_string());
                        match kv {
                            Value::Bool(false) => {
                                self.push(&at, "is not allowed (unevaluatedItems)".into())
                            }
                            sch => self.node(sch, &kp, x, &at, depth + 1),
                        }
                    }
                }
                "type" => {
                    let types: Vec<&str> = match kv {
                        Value::String(t) => vec![t.as_str()],
                        Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
                        _ => continue,
                    };
                    if !types.iter().any(|t| has_type(inst, t)) {
                        let want = types
                            .iter()
                            .map(|t| match *t {
                                "integer" => "an integer",
                                "number" => "a number",
                                "string" => "a string",
                                "boolean" => "a boolean",
                                "object" => "an object",
                                "array" => "an array",
                                _ => "null",
                            })
                            .collect::<Vec<_>>()
                            .join(" or ");
                        self.push(ipath, format!("must be {want}, not {}", kind(inst)));
                    }
                }
                "x-oav-int" => {
                    if let Value::Number(n) = inst {
                        if !number::is_lexical_integer(&n.to_string()) {
                            self.push(
                                ipath,
                                "must be an integer written without a fraction or exponent".into(),
                            );
                        }
                    }
                }
                "x-oav-int-format" => {
                    if let (Some(d), Some(f)) = (dec(inst), kv.as_str()) {
                        if !format::check_number(f, &d) {
                            let msg = match f {
                                "float" | "double" => format!("must be within the {f} range"),
                                _ => format!("must be an integer that fits {f}"),
                            };
                            self.push(ipath, msg);
                        }
                    }
                }
                "enum" => {
                    if let Some(a) = kv.as_array() {
                        if !a.iter().any(|x| json_eq(x, inst)) {
                            let shown: Vec<String> = a.iter().take(5).map(short).collect();
                            let tail = if a.len() > 5 { ", …" } else { "" };
                            self.push(ipath, format!("must be one of {}{tail}", shown.join(", ")));
                        }
                    }
                }
                "const" => {
                    if !json_eq(kv, inst) {
                        self.push(ipath, format!("must equal {}", short(kv)));
                    }
                }
                "minimum" | "maximum" | "exclusiveMinimum" | "exclusiveMaximum" | "multipleOf" => {
                    let (Some(x), Some(b)) = (dec(inst), dec(kv)) else {
                        continue;
                    };
                    let bad = match k.as_str() {
                        "minimum" => x < b,
                        "maximum" => x > b,
                        "exclusiveMinimum" => x <= b,
                        "exclusiveMaximum" => x >= b,
                        _ => !x.is_multiple_of(&b),
                    };
                    if bad {
                        let what = match k.as_str() {
                            "minimum" => "at least",
                            "maximum" => "at most",
                            "exclusiveMinimum" => "greater than",
                            "exclusiveMaximum" => "less than",
                            _ => "a multiple of",
                        };
                        self.push(ipath, format!("must be {what} {}", b.to_canonical()));
                    }
                }
                "minLength" | "maxLength" => {
                    let (Some(s), Some(n)) = (inst.as_str(), kv.as_u64()) else {
                        continue;
                    };
                    let len = s.chars().count() as u64;
                    if k == "minLength" && len < n {
                        self.push(ipath, format!("must be at least {n} characters long"));
                    } else if k == "maxLength" && len > n {
                        self.push(ipath, format!("must be at most {n} characters long"));
                    }
                }
                "pattern" => {
                    let (Some(s), Some(p)) = (inst.as_str(), kv.as_str()) else {
                        continue;
                    };
                    if let Some(re) = self.store.regex(p) {
                        if re.is_match(s).is_none() {
                            self.push(
                                ipath,
                                "could not be checked against its pattern within the step limit"
                                    .into(),
                            );
                        } else if re.is_match(s) == Some(false) {
                            let src = m
                                .get("x-oav-pattern-src")
                                .and_then(Value::as_str)
                                .unwrap_or(p);
                            self.push(
                                ipath,
                                format!(
                                    "must match the pattern {}",
                                    short(&Value::String(src.to_owned()))
                                ),
                            );
                        }
                    }
                }
                "format" => {
                    let (Some(s), Some(f)) = (inst.as_str(), kv.as_str()) else {
                        continue;
                    };
                    if self.dir.assert_formats && !format::check_str(f, s) {
                        self.push(ipath, format!("must be a valid {f}"));
                    }
                }
                "minItems" | "maxItems" => {
                    let (Some(a), Some(n)) = (inst.as_array(), kv.as_u64()) else {
                        continue;
                    };
                    let len = a.len() as u64;
                    if k == "minItems" && len < n {
                        self.push(ipath, format!("must have at least {n} items"));
                    } else if k == "maxItems" && len > n {
                        self.push(ipath, format!("must have at most {n} items"));
                    }
                }
                "uniqueItems" => {
                    let Some(a) = inst.as_array() else {
                        continue;
                    };
                    if kv != &Value::Bool(true) {
                        continue;
                    }
                    let mut seen: HashMap<String, usize> = HashMap::new();
                    for (i, x) in a.iter().enumerate() {
                        let mut key = String::new();
                        canonical(x, &mut key);
                        if let Some(j) = seen.insert(key, i) {
                            self.push(ipath, format!("items {j} and {i} must not be equal"));
                            break;
                        }
                    }
                }
                "prefixItems" => {
                    let (Some(a), Some(schemas)) = (inst.as_array(), kv.as_array()) else {
                        continue;
                    };
                    for (i, (x, sch)) in a.iter().zip(schemas).enumerate() {
                        let sp = doc::join(&kp, &i.to_string());
                        self.node(sch, &sp, x, &child(ipath, &i.to_string()), depth + 1);
                    }
                }
                "items" => {
                    let Some(a) = inst.as_array() else {
                        continue;
                    };
                    let skip = m
                        .get("prefixItems")
                        .and_then(Value::as_array)
                        .map_or(0, Vec::len);
                    for (i, x) in a.iter().enumerate().skip(skip) {
                        self.node(kv, &kp, x, &child(ipath, &i.to_string()), depth + 1);
                        if self.out.len() >= self.max {
                            break;
                        }
                    }
                }
                "contains" => {
                    let Some(a) = inst.as_array() else {
                        continue;
                    };
                    let min = m.get("minContains").and_then(Value::as_u64).unwrap_or(1);
                    let max = m.get("maxContains").and_then(Value::as_u64);
                    let mut n = 0u64;
                    for x in a {
                        if self.valid_at(&kp, x) == Some(true) {
                            n += 1;
                        }
                    }
                    if n < min {
                        self.push(ipath, format!("must contain at least {min} matching items"));
                    }
                    if max.is_some_and(|mx| n > mx) {
                        self.push(
                            ipath,
                            format!("must contain at most {} matching items", max.unwrap_or(0)),
                        );
                    }
                }
                "x-oav-required" => {
                    let key = match self.dir.direction() {
                        super::Direction::Request => "request",
                        super::Direction::Response => "response",
                    };
                    let (Some(o), Some(names)) =
                        (inst.as_object(), kv.get(key).and_then(Value::as_array))
                    else {
                        continue;
                    };
                    for n in names.iter().filter_map(Value::as_str) {
                        if !o.contains_key(n) {
                            self.push(&child(ipath, n), "is required".into());
                        }
                    }
                }
                "x-oav-forbid" => {
                    let (key, what) = match self.dir.direction() {
                        super::Direction::Request => {
                            ("request", "is readOnly and must not be sent in a request")
                        }
                        super::Direction::Response => (
                            "response",
                            "is writeOnly and must not be sent in a response",
                        ),
                    };
                    let (Some(o), Some(names)) =
                        (inst.as_object(), kv.get(key).and_then(Value::as_array))
                    else {
                        continue;
                    };
                    for n in names.iter().filter_map(Value::as_str) {
                        if o.contains_key(n) {
                            self.push(&child(ipath, n), what.into());
                        }
                    }
                }
                "required" => {
                    let (Some(o), Some(names)) = (inst.as_object(), kv.as_array()) else {
                        continue;
                    };
                    for n in names.iter().filter_map(Value::as_str) {
                        if !o.contains_key(n) {
                            self.push(&child(ipath, n), "is required".into());
                        }
                    }
                }
                "dependentRequired" => {
                    let (Some(o), Some(deps)) = (inst.as_object(), kv.as_object()) else {
                        continue;
                    };
                    for (trigger, needs) in deps {
                        if !o.contains_key(trigger) {
                            continue;
                        }
                        for n in needs
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(Value::as_str)
                        {
                            if !o.contains_key(n) {
                                self.push(
                                    &child(ipath, n),
                                    format!("is required when {trigger} is present"),
                                );
                            }
                        }
                    }
                }
                "minProperties" | "maxProperties" => {
                    let (Some(o), Some(n)) = (inst.as_object(), kv.as_u64()) else {
                        continue;
                    };
                    let len = o.len() as u64;
                    if k == "minProperties" && len < n {
                        self.push(ipath, format!("must have at least {n} properties"));
                    } else if k == "maxProperties" && len > n {
                        self.push(ipath, format!("must have at most {n} properties"));
                    }
                }
                "properties" => {
                    let (Some(o), Some(props)) = (inst.as_object(), kv.as_object()) else {
                        continue;
                    };
                    for (name, sch) in props {
                        if let Some(x) = o.get(name) {
                            self.node(
                                sch,
                                &doc::join(&kp, name),
                                x,
                                &child(ipath, name),
                                depth + 1,
                            );
                        }
                    }
                }
                "patternProperties" => {
                    let (Some(o), Some(props)) = (inst.as_object(), kv.as_object()) else {
                        continue;
                    };
                    for (re, sch) in props {
                        let Some(rx) = self.store.regex(re) else {
                            continue;
                        };
                        for (name, x) in o {
                            if rx.is_match(name) == Some(true) {
                                self.node(
                                    sch,
                                    &doc::join(&kp, re),
                                    x,
                                    &child(ipath, name),
                                    depth + 1,
                                );
                            }
                        }
                    }
                }
                "additionalProperties" => {
                    let Some(o) = inst.as_object() else {
                        continue;
                    };
                    let props = m.get("properties").and_then(Value::as_object);
                    let pats: Vec<_> = m
                        .get("patternProperties")
                        .and_then(Value::as_object)
                        .map(|p| p.keys().filter_map(|re| self.store.regex(re)).collect())
                        .unwrap_or_default();
                    for (name, x) in o {
                        if props.is_some_and(|p| p.contains_key(name))
                            || pats.iter().any(|r| r.is_match(name) == Some(true))
                        {
                            continue;
                        }
                        if kv == &Value::Bool(false) {
                            self.push(&child(ipath, name), "is not an allowed property".into());
                        } else {
                            self.node(kv, &kp, x, &child(ipath, name), depth + 1);
                        }
                    }
                }
                "propertyNames" => {
                    let Some(o) = inst.as_object() else {
                        continue;
                    };
                    for name in o.keys() {
                        if self.valid_at(&kp, &Value::String(name.clone())) == Some(false) {
                            self.push(
                                &child(ipath, name),
                                "is not an allowed property name".into(),
                            );
                        }
                    }
                }
                "dependentSchemas" => {
                    let (Some(o), Some(deps)) = (inst.as_object(), kv.as_object()) else {
                        continue;
                    };
                    for (trigger, sch) in deps {
                        if o.contains_key(trigger) {
                            self.node(sch, &doc::join(&kp, trigger), inst, ipath, depth + 1);
                        }
                    }
                }
                "allOf" => {
                    for (i, sch) in kv.as_array().into_iter().flatten().enumerate() {
                        self.node(sch, &doc::join(&kp, &i.to_string()), inst, ipath, depth + 1);
                    }
                }
                "anyOf" | "oneOf" => {
                    let Some(branches) = kv.as_array() else {
                        continue;
                    };
                    let mut valid = Vec::new();
                    for i in 0..branches.len() {
                        if self.valid_at(&doc::join(&kp, &i.to_string()), inst) == Some(true) {
                            valid.push(i);
                            if k == "anyOf" || valid.len() > 1 {
                                break;
                            }
                        }
                    }
                    if k == "oneOf" && valid.len() > 1 {
                        self.push(
                            ipath,
                            format!(
                                "must match exactly one schema in oneOf, but matches {} and {}",
                                valid[0], valid[1]
                            ),
                        );
                    } else if valid.is_empty() {
                        // Descend into the one branch whose type fits, if any,
                        // for a precise location.
                        let fitting: Vec<usize> = (0..branches.len())
                            .filter(|&i| self.type_fits(&branches[i], inst))
                            .collect();
                        let n = self.found();
                        if let [only] = fitting.as_slice() {
                            self.node(
                                &branches[*only],
                                &doc::join(&kp, &only.to_string()),
                                inst,
                                ipath,
                                depth + 1,
                            );
                        }
                        if self.found() == n {
                            self.push(ipath, format!("must match at least one schema in {k}"));
                        }
                    }
                }
                "not" => {
                    if self.valid_at(&kp, inst) == Some(true) {
                        self.push(ipath, "must not match the schema in not".into());
                    }
                }
                "if" => {
                    let branch = match self.valid_at(&kp, inst) {
                        Some(true) => "then",
                        Some(false) => "else",
                        None => continue,
                    };
                    if let Some(sch) = m.get(branch) {
                        self.node(sch, &doc::join(sptr, branch), inst, ipath, depth + 1);
                    }
                }
                _ => {}
            }
        }
        if opaque && self.found() == before && self.valid_at(sptr, inst) == Some(false) {
            self.push(ipath, "does not match its schema".into());
        }
    }

    /// Whether a branch's declared `type` (following one `$ref`) admits the
    /// value. A branch without `type` fits anything.
    fn type_fits(&self, s: &Value, inst: &Value) -> bool {
        let mut s = s;
        for _ in 0..doc::MAX_REF_CHAIN {
            match s
                .get("$ref")
                .and_then(Value::as_str)
                .and_then(|r| doc::lookup_ref(self.resource, r))
            {
                Some(t) if s.get("type").is_none() => s = t,
                _ => break,
            }
        }
        match s.get("type") {
            Some(Value::String(t)) => has_type(inst, t),
            Some(Value::Array(a)) => a
                .iter()
                .filter_map(Value::as_str)
                .any(|t| has_type(inst, t)),
            _ => !matches!(s, Value::Bool(false)),
        }
    }
}
