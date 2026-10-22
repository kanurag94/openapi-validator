//! Test support: valid values and requests generated from a document.
//!
//! Used by the corpus test and the property tests. Generation is
//! deliberately independent of the library's own translation: it reads the
//! OpenAPI document directly, so a disagreement points at one side or the
//! other rather than at shared code.

#![allow(dead_code)]

use std::collections::BTreeSet;

use regex_syntax::hir::{Class, Hir, HirKind};
use serde_json::{json, Map, Number, Value};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dir {
    Request,
    Response,
}

pub struct Gen<'a> {
    pub root: &'a Value,
    pub v30: bool,
    pub dir: Dir,
    /// Varies generated values (for `uniqueItems` and property tests).
    pub salt: u64,
}

fn lookup<'a>(root: &'a Value, r: &str) -> Option<&'a Value> {
    let p = r.strip_prefix('#')?;
    let decoded = percent_decode(p);
    if decoded.is_empty() {
        return Some(root);
    }
    root.pointer(&decoded)
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = |c: u8| (c as char).to_digit(16);
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Generates a string matching a translated (`regex` syntax) pattern.
fn from_hir(h: &Hir, out: &mut String) {
    match h.kind() {
        HirKind::Empty | HirKind::Look(_) => {}
        HirKind::Literal(l) => out.push_str(&String::from_utf8_lossy(&l.0)),
        HirKind::Class(Class::Unicode(c)) => {
            let ranges = c.ranges();
            // Prefer a plain ASCII letter or digit when the class allows one.
            let pick = ranges
                .iter()
                .find_map(|r| {
                    ('a'..='z')
                        .chain('0'..='9')
                        .chain('A'..='Z')
                        .find(|ch| r.start() <= *ch && *ch <= r.end())
                })
                .or_else(|| ranges.first().map(|r| r.start()));
            if let Some(ch) = pick {
                out.push(ch);
            }
        }
        HirKind::Class(Class::Bytes(c)) => {
            if let Some(r) = c.ranges().first() {
                out.push(char::from(r.start()));
            }
        }
        HirKind::Repetition(r) => {
            let n = r.min.max(if r.max.is_none_or(|m| m >= 1) && r.min == 0 {
                0
            } else {
                r.min
            });
            for _ in 0..n.min(64) {
                from_hir(&r.sub, out);
            }
        }
        HirKind::Capture(c) => from_hir(&c.sub, out),
        HirKind::Concat(parts) => parts.iter().for_each(|p| from_hir(p, out)),
        HirKind::Alternation(alts) => {
            if let Some(a) = alts.first() {
                from_hir(a, out);
            }
        }
    }
}

pub fn string_for_pattern(pattern: &str) -> Option<String> {
    let translated = openapi_validator::pattern::translate(pattern).ok()?;
    let hir = regex_syntax::parse(&translated).ok()?;
    let mut s = String::new();
    from_hir(&hir, &mut s);
    let re = openapi_validator::pattern::compile(pattern).ok()?;
    if re.is_match(&s) {
        return Some(s);
    }
    // Unanchored patterns match anywhere; the generated core is enough.
    None
}

fn num(v: &Value) -> Option<f64> {
    v.as_f64()
}

fn types(s: &Map<String, Value>) -> Vec<String> {
    match s.get("type") {
        Some(Value::String(t)) => vec![t.clone()],
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

impl Gen<'_> {
    fn resolve<'b>(&'b self, mut s: &'b Value) -> &'b Value {
        for _ in 0..32 {
            match s
                .get("$ref")
                .and_then(Value::as_str)
                .and_then(|r| lookup(self.root, r))
            {
                Some(t) => s = t,
                None => break,
            }
        }
        s
    }

    /// Merges `allOf` members (and, in 3.1, `$ref` siblings) into one object.
    fn flatten(&self, s: &Value, depth: usize, out: &mut Map<String, Value>) {
        if depth > 12 {
            return;
        }
        let Some(m) = s.as_object() else {
            return;
        };
        if let Some(r) = m.get("$ref").and_then(Value::as_str) {
            if let Some(t) = lookup(self.root, r) {
                self.flatten(t, depth + 1, out);
            }
            if self.v30 {
                return;
            }
        }
        for (k, v) in m {
            match k.as_str() {
                "$ref" | "allOf" => {}
                "properties" => {
                    let e = out.entry("properties").or_insert_with(|| json!({}));
                    if let (Some(dst), Some(src)) = (e.as_object_mut(), v.as_object()) {
                        for (pk, pv) in src {
                            dst.entry(pk.clone()).or_insert_with(|| pv.clone());
                        }
                    }
                }
                "required" => {
                    let e = out.entry("required").or_insert_with(|| json!([]));
                    if let (Some(dst), Some(src)) = (e.as_array_mut(), v.as_array()) {
                        for n in src {
                            if !dst.contains(n) {
                                dst.push(n.clone());
                            }
                        }
                    }
                }
                _ => {
                    out.entry(k.clone()).or_insert_with(|| v.clone());
                }
            }
        }
        for member in m
            .get("allOf")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            self.flatten(member, depth + 1, out);
        }
    }

    fn access(&self, s: &Value) -> (bool, bool) {
        let mut m = Map::new();
        self.flatten(s, 0, &mut m);
        (
            m.get("readOnly") == Some(&Value::Bool(true)),
            m.get("writeOnly") == Some(&Value::Bool(true)),
        )
    }

    pub fn value(&self, s: &Value, depth: usize) -> Option<Value> {
        if depth > 10 {
            return None;
        }
        match s {
            Value::Bool(true) => return Some(json!("x")),
            Value::Bool(false) => return None,
            _ => {}
        }
        let mut m = Map::new();
        self.flatten(s, 0, &mut m);
        if let Some(c) = m.get("const") {
            return Some(c.clone());
        }
        if let Some(e) = m.get("enum").and_then(Value::as_array) {
            return e.iter().find(|v| !v.is_null()).or(e.first()).cloned();
        }
        for key in ["oneOf", "anyOf"] {
            if let Some(branches) = m.get(key).and_then(Value::as_array) {
                let disc = m.get("discriminator");
                for b in branches {
                    let mut merged = m.clone();
                    merged.remove("oneOf");
                    merged.remove("anyOf");
                    merged.remove("discriminator");
                    let mut bm = Map::new();
                    self.flatten(b, 0, &mut bm);
                    for (k, v) in bm {
                        match (merged.get_mut(&k), v) {
                            (Some(Value::Object(dst)), Value::Object(src)) => {
                                for (pk, pv) in src {
                                    dst.entry(pk).or_insert(pv);
                                }
                            }
                            (Some(Value::Array(dst)), Value::Array(src)) => {
                                for x in src {
                                    if !dst.contains(&x) {
                                        dst.push(x);
                                    }
                                }
                            }
                            (_, v) => {
                                merged.insert(k, v);
                            }
                        }
                    }
                    if let Some(mut v) = self.value(&Value::Object(merged), depth + 1) {
                        if let (Some(d), Some(obj)) = (disc, v.as_object_mut()) {
                            if let Some(p) = d.get("propertyName").and_then(Value::as_str) {
                                let name = self.discriminator_value(d, b);
                                if let Some(name) = name {
                                    obj.insert(p.to_owned(), Value::String(name));
                                }
                            }
                        }
                        return Some(v);
                    }
                }
                return None;
            }
        }
        let ts = types(&m);
        let t = ts
            .iter()
            .find(|t| *t != "null")
            .cloned()
            .or_else(|| {
                if m.contains_key("properties") || m.contains_key("required") {
                    Some("object".into())
                } else if m.contains_key("items") {
                    Some("array".into())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| "string".into());
        match t.as_str() {
            "object" => self.object(&m, depth),
            "array" => self.array(&m, depth),
            "integer" => self.number(&m, true),
            "number" => self.number(&m, false),
            "boolean" => Some(Value::Bool(true)),
            "null" => Some(Value::Null),
            _ => self.string(&m),
        }
    }

    fn discriminator_value(&self, d: &Value, branch: &Value) -> Option<String> {
        let r = branch.get("$ref").and_then(Value::as_str)?;
        if let Some(map) = d.get("mapping").and_then(Value::as_object) {
            for (k, v) in map {
                let v = v.as_str().unwrap_or("");
                if v == r || r.ends_with(&format!("/{v}")) {
                    return Some(k.clone());
                }
            }
        }
        r.rsplit('/')
            .next()
            .map(|n| n.replace("~1", "/").replace("~0", "~"))
    }

    fn object(&self, m: &Map<String, Value>, depth: usize) -> Option<Value> {
        let mut out = Map::new();
        let props = m.get("properties").and_then(Value::as_object);
        let required: Vec<&str> = m
            .get("required")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        for name in &required {
            let ps = props.and_then(|p| p.get(*name));
            if let Some(ps) = ps {
                let (ro, wo) = self.access(ps);
                if (self.dir == Dir::Request && ro) || (self.dir == Dir::Response && wo) {
                    continue;
                }
            }
            let v = match ps {
                Some(ps) => self.value(ps, depth + 1)?,
                None => match m.get("additionalProperties") {
                    Some(Value::Object(_)) => self.value(&m["additionalProperties"], depth + 1)?,
                    _ => json!("x"),
                },
            };
            out.insert((*name).to_owned(), v);
        }
        let min = m.get("minProperties").and_then(num).unwrap_or(0.0) as usize;
        if out.len() < min {
            for (name, ps) in props.into_iter().flatten() {
                if out.len() >= min {
                    break;
                }
                if !out.contains_key(name) {
                    out.insert(name.clone(), self.value(ps, depth + 1)?);
                }
            }
        }
        Some(Value::Object(out))
    }

    fn array(&self, m: &Map<String, Value>, depth: usize) -> Option<Value> {
        let min = m.get("minItems").and_then(num).unwrap_or(0.0).max(0.0) as usize;
        let items = m.get("items").cloned().unwrap_or(Value::Bool(true));
        let mut out = Vec::new();
        let mut seen = BTreeSet::new();
        for i in 0..min.min(8) {
            let g = Gen {
                root: self.root,
                v30: self.v30,
                dir: self.dir,
                salt: self.salt + i as u64,
            };
            let v = g.value(&items, depth + 1)?;
            if m.get("uniqueItems") == Some(&Value::Bool(true)) && !seen.insert(v.to_string()) {
                return None;
            }
            out.push(v);
        }
        Some(Value::Array(out))
    }

    fn number(&self, m: &Map<String, Value>, integer: bool) -> Option<Value> {
        let mut lo = m.get("minimum").and_then(num);
        let mut hi = m.get("maximum").and_then(num);
        let mut lo_ex = false;
        let mut hi_ex = false;
        match m.get("exclusiveMinimum") {
            Some(Value::Bool(true)) => lo_ex = true,
            Some(v) if v.is_number() => {
                lo = num(v);
                lo_ex = true;
            }
            _ => {}
        }
        match m.get("exclusiveMaximum") {
            Some(Value::Bool(true)) => hi_ex = true,
            Some(v) if v.is_number() => {
                hi = num(v);
                hi_ex = true;
            }
            _ => {}
        }
        let step = m.get("multipleOf").and_then(num).filter(|s| *s > 0.0);
        let fits = |x: f64| {
            lo.is_none_or(|l| if lo_ex { x > l } else { x >= l })
                && hi.is_none_or(|h| if hi_ex { x < h } else { x <= h })
        };
        let mut candidates = vec![1.0 + self.salt as f64, 0.0, 1.0];
        if let Some(l) = lo {
            candidates.extend([l, l + 1.0, l.ceil(), l.ceil() + 1.0]);
        }
        if let Some(h) = hi {
            candidates.extend([h, h - 1.0, h.floor(), h.floor() - 1.0]);
        }
        if let (Some(l), Some(h)) = (lo, hi) {
            candidates.push(((l + h) / 2.0).floor());
        }
        for c in candidates {
            let mut x = c;
            if let Some(s) = step {
                x = (x / s).ceil() * s;
            }
            if integer {
                x = x.ceil();
            }
            if fits(x)
                && x.is_finite()
                && step.is_none_or(|s| ((x / s) - (x / s).round()).abs() < 1e-9)
            {
                let n = if integer || x.fract() == 0.0 {
                    Number::from(x as i64)
                } else {
                    serde_json::from_str::<Number>(&format!("{x}")).ok()?
                };
                return Some(Value::Number(n));
            }
        }
        None
    }

    fn string(&self, m: &Map<String, Value>) -> Option<Value> {
        let min = m.get("minLength").and_then(num).unwrap_or(0.0) as usize;
        let max = m.get("maxLength").and_then(num).map(|x| x as usize);
        if let Some(p) = m.get("pattern").and_then(Value::as_str) {
            let re = openapi_validator::pattern::compile(p).ok()?;
            let format = m.get("format").and_then(Value::as_str).unwrap_or("");
            let fits = |e: &str| {
                re.is_match(e)
                    && (format.is_empty() || openapi_validator::format::check_str(format, e))
                    && e.chars().count() >= min
                    && max.is_none_or(|mx| e.chars().count() <= mx)
            };
            // A document example, or a canonical value of the format, is the
            // best witness when it fits.
            for key in ["example", "default"] {
                if let Some(Value::String(e)) = m.get(key) {
                    if fits(e) {
                        return Some(Value::String(e.clone()));
                    }
                }
            }
            let mut without = m.clone();
            without.remove("pattern");
            if let Some(Value::String(f)) = self.string(&without) {
                if fits(&f) {
                    return Some(Value::String(f));
                }
            }
            let mut s = string_for_pattern(p)?;
            if s.chars().count() < min {
                return None;
            }
            if max.is_some_and(|mx| s.chars().count() > mx) {
                s = s.chars().take(max.unwrap_or(0)).collect();
            }
            return Some(Value::String(s));
        }
        let format = m.get("format").and_then(Value::as_str).unwrap_or("");
        let base = match format {
            "date" => "2024-01-02".to_owned(),
            "date-time" => "2024-01-02T03:04:05Z".to_owned(),
            "time" => "03:04:05Z".to_owned(),
            "email" => "a@example.com".to_owned(),
            "uuid" => "2eb8aa08-aa98-11ea-b4aa-73b441d16380".to_owned(),
            "uri" => "https://example.com/x".to_owned(),
            "uri-reference" => "/x".to_owned(),
            "ipv4" => "10.0.0.1".to_owned(),
            "ipv6" => "::1".to_owned(),
            "hostname" => "example.com".to_owned(),
            "byte" => "AAAA".to_owned(),
            _ => format!("s{}", self.salt),
        };
        let mut s = base;
        while s.chars().count() < min {
            s.push('a');
        }
        if let Some(mx) = max {
            if s.chars().count() > mx {
                if !format.is_empty() && openapi_validator::format::ASSERTED.contains(&format) {
                    return None;
                }
                s = s.chars().take(mx).collect();
            }
        }
        Some(Value::String(s))
    }
}

// ------------------------------------------------------------ requests

/// An owned request, convertible to the borrowed form.
#[derive(Debug, Clone, Default)]
pub struct OwnedRequest {
    pub method: String,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    pub cookies: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl OwnedRequest {
    pub fn with<R>(&self, f: impl FnOnce(&openapi_validator::Request<'_>) -> R) -> R {
        let query = self
            .query
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&");
        let mut headers: Vec<(&str, &str)> = self
            .headers
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let cookie = self
            .cookies
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ");
        if !cookie.is_empty() {
            headers.push(("Cookie", cookie.as_str()));
        }
        f(&openapi_validator::Request {
            method: &self.method,
            path: &self.path,
            query: &query,
            headers: &headers,
            body: &self.body,
        })
    }
}

pub fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn scalar_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// A required parameter that a test can leave out.
#[derive(Debug, Clone)]
pub struct Omittable {
    /// `query`, `header` or `cookie`.
    pub location: String,
    pub name: String,
    /// Query pair names emitted for it (exploded objects use property names).
    pub pairs: Vec<String>,
}

pub struct Generated {
    pub request: OwnedRequest,
    pub omittable: Vec<Omittable>,
}

fn deref<'a>(root: &'a Value, v: &'a Value) -> &'a Value {
    let mut cur = v;
    for _ in 0..32 {
        match cur
            .get("$ref")
            .and_then(Value::as_str)
            .and_then(|r| lookup(root, r))
        {
            Some(t) => cur = t,
            None => break,
        }
    }
    cur
}

/// Builds one valid request for an operation, or says why it cannot.
pub fn request(root: &Value, path: &str, method: &str, salt: u64) -> Result<Generated, String> {
    let v30 = root
        .get("openapi")
        .and_then(Value::as_str)
        .is_some_and(|v| v.starts_with("3.0"));
    let g = Gen {
        root,
        v30,
        dir: Dir::Request,
        salt,
    };
    let item = deref(
        root,
        root.get("paths")
            .and_then(|p| p.get(path))
            .ok_or("no path item")?,
    );
    let op = item.get(method).ok_or("no operation")?;
    let mut params: Vec<&Value> = Vec::new();
    for container in [item, op] {
        for p in container
            .get("parameters")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let p = deref(root, p);
            let key = |q: &Value| {
                let n = q
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let i = q.get("in").and_then(Value::as_str).unwrap_or("").to_owned();
                if i == "header" {
                    (n.to_ascii_lowercase(), i)
                } else {
                    (n, i)
                }
            };
            let k = key(p);
            params.retain(|q| key(q) != k);
            params.push(p);
        }
    }
    let mut req = OwnedRequest {
        method: method.to_ascii_uppercase(),
        ..OwnedRequest::default()
    };
    let mut omittable = Vec::new();
    let mut path_out = path.to_owned();
    for p in &params {
        let name = p.get("name").and_then(Value::as_str).unwrap_or("");
        let loc = p.get("in").and_then(Value::as_str).unwrap_or("");
        let required = p.get("required") == Some(&Value::Bool(true)) || loc == "path";
        if loc == "header"
            && ["accept", "content-type", "authorization"]
                .contains(&name.to_ascii_lowercase().as_str())
        {
            continue;
        }
        if !required {
            continue;
        }
        let (value, json_content) = if let Some(s) = p.get("schema") {
            (
                g.value(s, 0)
                    .ok_or_else(|| format!("cannot generate {loc} parameter"))?,
                false,
            )
        } else if let Some((media, c)) = p
            .get("content")
            .and_then(Value::as_object)
            .and_then(|c| c.iter().next())
        {
            // Only JSON media types carry JSON text; others carry the value.
            let is_json = media.to_ascii_lowercase().contains("json");
            match c.get("schema") {
                Some(s) => (
                    g.value(s, 0).ok_or("cannot generate content parameter")?,
                    is_json,
                ),
                None => (json!("x"), false),
            }
        } else {
            (json!("x"), false)
        };
        let style = p.get("style").and_then(Value::as_str).unwrap_or(match loc {
            "query" | "cookie" => "form",
            _ => "simple",
        });
        let explode = p
            .get("explode")
            .and_then(Value::as_bool)
            .unwrap_or(style == "form");
        match loc {
            "path" => {
                let text = if json_content {
                    encode(&value.to_string())
                } else {
                    serialize_path(name, &value, style, explode)
                };
                path_out = path_out.replace(&format!("{{{name}}}"), &text);
            }
            "query" => {
                // An empty array is sent as `name=`; an empty exploded object
                // would be indistinguishable from absence (RFC 6570 §2.3), so
                // it gets one declared property, or one extra pair where the
                // schema allows them. With neither, a required one cannot be
                // sent at all.
                let value = match (&value, p.get("schema")) {
                    (Value::Object(o), Some(s)) if o.is_empty() && explode => {
                        let mut flat = Map::new();
                        g.flatten(s, 0, &mut flat);
                        let first = flat
                            .get("properties")
                            .and_then(Value::as_object)
                            .and_then(|pr| pr.iter().next());
                        match (first, flat.get("additionalProperties")) {
                            (Some((k, ps)), _) => {
                                json!({ k.clone(): g.value(ps, 1).unwrap_or(json!("x")) })
                            }
                            (None, Some(Value::Bool(false))) => {
                                if p.get("required") == Some(&Value::Bool(true)) {
                                    return Err(
                                        "a required exploded object that admits only {} cannot be sent"
                                            .into(),
                                    );
                                }
                                value.clone()
                            }
                            (None, extra) => {
                                let v = extra
                                    .filter(|e| e.is_object())
                                    .and_then(|e| g.value(e, 1))
                                    .unwrap_or(json!("x"));
                                json!({ "k": v })
                            }
                        }
                    }
                    _ => value.clone(),
                };
                let before = req.query.len();
                if matches!(&value, Value::Array(a) if a.is_empty()) {
                    req.query.push((encode(name), String::new()));
                } else if json_content {
                    req.query.push((encode(name), encode(&value.to_string())));
                } else {
                    req.query
                        .extend(serialize_query(name, &value, style, explode));
                }
                omittable.push(Omittable {
                    location: "query".into(),
                    name: name.to_owned(),
                    pairs: req.query[before..].iter().map(|(k, _)| k.clone()).collect(),
                });
            }
            "header" => {
                let text = if json_content {
                    value.to_string()
                } else {
                    simple(&value, explode)
                };
                req.headers.push((name.to_owned(), text));
                omittable.push(Omittable {
                    location: "header".into(),
                    name: name.to_owned(),
                    pairs: Vec::new(),
                });
            }
            "cookie" => {
                let text = if json_content {
                    value.to_string()
                } else {
                    simple(&value, false)
                };
                req.cookies.push((name.to_owned(), encode(&text)));
                omittable.push(Omittable {
                    location: "cookie".into(),
                    name: name.to_owned(),
                    pairs: Vec::new(),
                });
            }
            _ => {}
        }
    }
    req.path = path_out;
    credentials(root, op, &mut req);
    if let Some(rb) = op.get("requestBody") {
        let rb = deref(root, rb);
        if rb.get("required") == Some(&Value::Bool(true)) {
            body(&g, rb, &mut req)?;
        }
    }
    Ok(Generated {
        request: req,
        omittable,
    })
}

fn simple(v: &Value, explode: bool) -> String {
    match v {
        Value::Array(a) => a.iter().map(scalar_text).collect::<Vec<_>>().join(","),
        Value::Object(o) => o
            .iter()
            .map(|(k, x)| {
                if explode {
                    format!("{k}={}", scalar_text(x))
                } else {
                    format!("{k},{}", scalar_text(x))
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        other => scalar_text(other),
    }
}

fn serialize_path(name: &str, v: &Value, style: &str, explode: bool) -> String {
    let enc_items = |sep: &str| match v {
        Value::Array(a) => a
            .iter()
            .map(|x| encode(&scalar_text(x)))
            .collect::<Vec<_>>()
            .join(sep),
        Value::Object(o) => o
            .iter()
            .map(|(k, x)| {
                if explode {
                    format!("{}={}", encode(k), encode(&scalar_text(x)))
                } else {
                    format!("{},{}", encode(k), encode(&scalar_text(x)))
                }
            })
            .collect::<Vec<_>>()
            .join(if explode { sep } else { "," }),
        other => encode(&scalar_text(other)),
    };
    match style {
        "label" => format!(".{}", enc_items(if explode { "." } else { "," })),
        "matrix" => match v {
            Value::Array(a) if explode => a
                .iter()
                .map(|x| format!(";{}={}", name, encode(&scalar_text(x))))
                .collect::<String>(),
            Value::Object(_) if explode => format!(";{}", enc_items(";")),
            _ => format!(";{name}={}", enc_items(",")),
        },
        _ => enc_items(","),
    }
}

fn serialize_query(name: &str, v: &Value, style: &str, explode: bool) -> Vec<(String, String)> {
    let n = encode(name);
    match (v, style) {
        (Value::Object(o), "deepObject") => o
            .iter()
            .map(|(k, x)| (format!("{n}%5B{}%5D", encode(k)), encode(&scalar_text(x))))
            .collect(),
        (Value::Object(o), _) if explode => o
            .iter()
            .map(|(k, x)| (encode(k), encode(&scalar_text(x))))
            .collect(),
        (Value::Array(a), _) if explode => a
            .iter()
            .map(|x| (n.clone(), encode(&scalar_text(x))))
            .collect(),
        (Value::Array(a), "spaceDelimited") => {
            vec![(
                n,
                a.iter()
                    .map(|x| encode(&scalar_text(x)))
                    .collect::<Vec<_>>()
                    .join("%20"),
            )]
        }
        (Value::Array(a), "pipeDelimited") => {
            vec![(
                n,
                a.iter()
                    .map(|x| encode(&scalar_text(x)))
                    .collect::<Vec<_>>()
                    .join("|"),
            )]
        }
        (Value::Array(a), _) => vec![(
            n,
            a.iter()
                .map(|x| encode(&scalar_text(x)))
                .collect::<Vec<_>>()
                .join(","),
        )],
        (Value::Object(o), _) => vec![(
            n,
            o.iter()
                .map(|(k, x)| format!("{},{}", encode(k), encode(&scalar_text(x))))
                .collect::<Vec<_>>()
                .join(","),
        )],
        (other, _) => vec![(n, encode(&scalar_text(other)))],
    }
}

fn credentials(root: &Value, op: &Value, req: &mut OwnedRequest) {
    let reqs = op.get("security").or_else(|| root.get("security"));
    let Some(first) = reqs
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_object)
    else {
        return;
    };
    let schemes = root.pointer("/components/securitySchemes");
    for name in first.keys() {
        let Some(s) = schemes.and_then(|m| m.get(name)).map(|s| deref(root, s)) else {
            continue;
        };
        match s.get("type").and_then(Value::as_str) {
            Some("apiKey") => {
                let n = s
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("key")
                    .to_owned();
                match s.get("in").and_then(Value::as_str) {
                    Some("query") => req.query.push((encode(&n), "k3y".into())),
                    Some("cookie") => req.cookies.push((n, "k3y".into())),
                    _ => req.headers.push((n, "k3y".into())),
                }
            }
            Some("http") => {
                let sc = s.get("scheme").and_then(Value::as_str).unwrap_or("bearer");
                req.headers
                    .push(("Authorization".into(), format!("{sc} dXNlcjpwYXNz")));
            }
            Some("oauth2" | "openIdConnect") => req
                .headers
                .push(("Authorization".into(), "Bearer t0k3n".into())),
            _ => {}
        }
    }
}

fn body(g: &Gen<'_>, rb: &Value, req: &mut OwnedRequest) -> Result<(), String> {
    let content = rb
        .get("content")
        .and_then(Value::as_object)
        .ok_or("no content")?;
    let pick = content
        .iter()
        .find(|(k, _)| {
            let k = k.to_ascii_lowercase();
            k.starts_with("application/json") || k.contains("+json")
        })
        .or_else(|| {
            content
                .iter()
                .find(|(k, _)| k.starts_with("application/x-www-form-urlencoded"))
        })
        .or_else(|| content.iter().next())
        .ok_or("empty content")?;
    let (key, media) = pick;
    let ct = if key.contains('*') {
        "application/json".to_owned()
    } else {
        key.clone()
    };
    let value = match media.get("schema") {
        Some(s) => g.value(s, 0).ok_or("cannot generate body")?,
        None => json!({}),
    };
    let lower = ct.to_ascii_lowercase();
    if lower.starts_with("application/x-www-form-urlencoded") {
        let pairs: Vec<String> = value
            .as_object()
            .into_iter()
            .flatten()
            .flat_map(|(k, v)| match v {
                Value::Array(a) => a
                    .iter()
                    .map(|x| format!("{}={}", encode(k), encode(&scalar_text(x))))
                    .collect(),
                Value::Object(_) => vec![format!("{}={}", encode(k), encode(&v.to_string()))],
                other => vec![format!("{}={}", encode(k), encode(&scalar_text(other)))],
            })
            .collect();
        req.body = pairs.join("&").into_bytes();
    } else if lower.starts_with("multipart/form-data") {
        let mut b = String::new();
        for (k, v) in value.as_object().into_iter().flatten() {
            let items: Vec<&Value> = match v {
                Value::Array(a) => a.iter().collect(),
                other => vec![other],
            };
            for x in items {
                let (ctype, text) = match x {
                    Value::Object(_) | Value::Array(_) => ("application/json", x.to_string()),
                    other => ("text/plain", scalar_text(other)),
                };
                b.push_str(&format!(
                    "--BOUNDARY\r\nContent-Disposition: form-data; name=\"{k}\"\r\nContent-Type: {ctype}\r\n\r\n{text}\r\n"
                ));
            }
        }
        b.push_str("--BOUNDARY--\r\n");
        req.body = b.into_bytes();
        req.headers.push((
            "Content-Type".into(),
            "multipart/form-data; boundary=BOUNDARY".into(),
        ));
        return Ok(());
    } else if lower.contains("json") {
        req.body = value.to_string().into_bytes();
    } else if lower.contains("xml") {
        req.body = b"<value/>".to_vec();
    } else {
        req.body = scalar_text(&value).into_bytes();
        if req.body.is_empty() {
            req.body = b"x".to_vec();
        }
    }
    req.headers.push(("Content-Type".into(), ct));
    Ok(())
}
