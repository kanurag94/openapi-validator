//! Parameter Objects (OpenAPI 3.1.1 §4.8.12): serialization styles, value
//! parsing and checking.
//!
//! Parameter values arrive as text. Each style (§4.8.12.4) is undone into a
//! JSON value shaped like the parameter's schema, then that value is checked
//! against the schema. Text is turned into numbers or booleans only where the
//! schema allows them; when a value could be read two ways (a schema that
//! allows both strings and integers), it is valid if either reading is.

use std::collections::HashMap;

use serde_json::{Map, Number, Value};

use crate::doc::{self, LoadError, Version};
use crate::media::MediaType;
use crate::model::Builder;
use crate::number::Decimal;
use crate::schema::{Direction, SchemaId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum In {
    Path,
    Query,
    Header,
    Cookie,
}

impl In {
    pub fn as_str(self) -> &'static str {
        match self {
            In::Path => "path",
            In::Query => "query",
            In::Header => "header",
            In::Cookie => "cookie",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Style {
    Matrix,
    Label,
    Simple,
    Form,
    SpaceDelimited,
    PipeDelimited,
    DeepObject,
}

/// JSON types a schema admits, as bit flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Types(u8);

impl Types {
    pub const NULL: Types = Types(1);
    pub const BOOL: Types = Types(2);
    pub const INT: Types = Types(4);
    pub const NUM: Types = Types(8);
    pub const STR: Types = Types(16);
    pub const ARR: Types = Types(32);
    pub const OBJ: Types = Types(64);
    pub const ANY: Types = Types(127);

    pub fn has(self, t: Types) -> bool {
        self.0 & t.0 != 0
    }

    fn of(name: &str) -> Types {
        match name {
            "null" => Types::NULL,
            "boolean" => Types::BOOL,
            "integer" => Types::INT,
            "number" => Types::NUM,
            "string" => Types::STR,
            "array" => Types::ARR,
            "object" => Types::OBJ,
            _ => Types(0),
        }
    }

    fn union(self, o: Types) -> Types {
        Types(self.0 | o.0)
    }

    fn without_null(self) -> Types {
        Types(self.0 & !Types::NULL.0)
    }
}

/// The shape a parameter's text must be parsed into.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Shape {
    Primitive(Types),
    Array(Types),
    Object(Vec<(String, Types)>),
}

pub(crate) enum ParamValue {
    Schema {
        id: SchemaId,
        shape: Shape,
    },
    /// `content` with one media type (§4.8.12.3); a schema-less entry is
    /// unconstrained.
    Content {
        media: MediaType,
        id: Option<SchemaId>,
    },
    /// Neither `schema` nor `content`: any value.
    Any,
}

pub(crate) struct Param {
    pub name: String,
    pub location: In,
    pub required: bool,
    pub style: Style,
    pub explode: bool,
    pub value: ParamValue,
}

impl Param {
    pub fn label(&self) -> String {
        let name = if self.location == In::Header {
            self.name.to_ascii_lowercase()
        } else {
            self.name.clone()
        };
        format!("{}.{name}", self.location.as_str())
    }
}

// ---------------------------------------------------------------- shapes

/// Follows `$ref` (3.0 ignores siblings) up to the chain limit.
fn resolve<'a>(root: &'a Value, mut s: &'a Value) -> &'a Value {
    for _ in 0..doc::MAX_REF_CHAIN {
        match s
            .get("$ref")
            .and_then(Value::as_str)
            .and_then(|r| doc::lookup(root, r))
        {
            Some(t) => s = t,
            None => break,
        }
    }
    s
}

fn types_of(root: &Value, s: &Value, version: Version, depth: usize) -> Types {
    if depth > 16 {
        return Types::ANY;
    }
    let s = resolve(root, s);
    let Some(m) = s.as_object() else {
        return Types::ANY;
    };
    let mut t = match m.get("type") {
        Some(Value::String(n)) => Types::of(n),
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .fold(Types(0), |acc, n| acc.union(Types::of(n))),
        _ => Types(0),
    };
    if t.0 != 0 && version == Version::V30 && m.get("nullable") == Some(&Value::Bool(true)) {
        t = t.union(Types::NULL);
    }
    if t.0 == 0 {
        for key in ["allOf", "anyOf", "oneOf"] {
            for member in m.get(key).and_then(Value::as_array).into_iter().flatten() {
                t = t.union(types_of(root, member, version, depth + 1));
            }
        }
    }
    if t.0 == 0 {
        if m.contains_key("properties") {
            return Types::OBJ;
        }
        if m.contains_key("items") {
            return Types::ARR;
        }
        return Types::ANY;
    }
    if t.has(Types::INT) {
        t = t.union(Types::NUM);
    }
    t
}

fn collect_props(
    root: &Value,
    s: &Value,
    version: Version,
    out: &mut Vec<(String, Types)>,
    depth: usize,
) {
    if depth > 16 {
        return;
    }
    let s = resolve(root, s);
    if let Some(props) = s.get("properties").and_then(Value::as_object) {
        for (name, p) in props {
            if !out.iter().any(|(n, _)| n == name) {
                out.push((name.clone(), types_of(root, p, version, depth + 1)));
            }
        }
    }
    for key in ["allOf", "anyOf", "oneOf"] {
        for member in s.get(key).and_then(Value::as_array).into_iter().flatten() {
            collect_props(root, member, version, out, depth + 1);
        }
    }
}

pub(crate) fn shape_of(root: &Value, s: &Value, version: Version) -> Shape {
    let t = types_of(root, s, version, 0);
    let non_null = t.without_null();
    if non_null == Types::ARR {
        let r = resolve(root, s);
        let items = r
            .get("items")
            .map_or(Types::ANY, |i| types_of(root, i, version, 1));
        return Shape::Array(items);
    }
    if non_null == Types::OBJ {
        let mut props = Vec::new();
        collect_props(root, s, version, &mut props, 0);
        return Shape::Object(props);
    }
    Shape::Primitive(t)
}

// --------------------------------------------------------------- loading

fn default_style(location: In) -> Style {
    match location {
        In::Query | In::Cookie => Style::Form,
        In::Path | In::Header => Style::Simple,
    }
}

fn parse_style(s: &str) -> Option<Style> {
    Some(match s {
        "matrix" => Style::Matrix,
        "label" => Style::Label,
        "simple" => Style::Simple,
        "form" => Style::Form,
        "spaceDelimited" => Style::SpaceDelimited,
        "pipeDelimited" => Style::PipeDelimited,
        "deepObject" => Style::DeepObject,
        _ => return None,
    })
}

impl Builder<'_> {
    /// One Parameter Object (or Header Object when `header_name` is given).
    pub(crate) fn param(
        &mut self,
        v: &Value,
        ptr: &str,
        header_name: Option<&str>,
        dir: Direction,
        version: Version,
    ) -> Result<Option<Param>, LoadError> {
        let (p, pptr) = doc::deref(self.root, v, ptr)?;
        let (name, location) = match header_name {
            Some(n) => (n.to_owned(), In::Header),
            None => {
                let Some(name) = p.get("name").and_then(Value::as_str) else {
                    self.warn(&pptr, "parameter has no name; ignored");
                    return Ok(None);
                };
                let location = match p.get("in").and_then(Value::as_str) {
                    Some("path") => In::Path,
                    Some("query") => In::Query,
                    Some("header") => In::Header,
                    Some("cookie") => In::Cookie,
                    _ => {
                        self.warn(&pptr, "parameter has no valid `in`; ignored");
                        return Ok(None);
                    }
                };
                (name.to_owned(), location)
            }
        };
        let style = match p.get("style").and_then(Value::as_str) {
            Some(s) => match parse_style(s) {
                Some(st) => st,
                None => {
                    self.warn(&pptr, format!("unknown style {s:?}; using the default"));
                    default_style(location)
                }
            },
            None => default_style(location),
        };
        let explode = match p.get("explode") {
            Some(Value::Bool(b)) => *b,
            _ => style == Style::Form,
        };
        let value = if let Some(s) = p.get("schema") {
            let id = self.roots.id(dir, &doc::join(&pptr, "schema"));
            ParamValue::Schema {
                id,
                shape: shape_of(self.root, s, version),
            }
        } else if let Some(content) = p.get("content").and_then(Value::as_object) {
            match content.iter().next() {
                Some((key, media)) => {
                    let mt = MediaType::parse(key).unwrap_or(MediaType {
                        ty: "application".into(),
                        sub: "json".into(),
                        params: Vec::new(),
                    });
                    let id = media.get("schema").map(|_| {
                        self.roots.id(
                            dir,
                            &doc::join(&doc::join(&doc::join(&pptr, "content"), key), "schema"),
                        )
                    });
                    ParamValue::Content { media: mt, id }
                }
                None => ParamValue::Any,
            }
        } else {
            ParamValue::Any
        };
        Ok(Some(Param {
            name,
            location,
            // Path parameters are always required (§4.8.12.2).
            required: location == In::Path || p.get("required") == Some(&Value::Bool(true)),
            style,
            explode,
            value,
        }))
    }

    /// Path-item and operation parameters merged: an operation parameter
    /// replaces a path-item one with the same name and location
    /// (§4.8.10.1). Header parameters named `Accept`, `Content-Type` or
    /// `Authorization` are ignored (§4.8.12.2).
    pub(crate) fn params(
        &mut self,
        item: &Value,
        item_ptr: &str,
        op: &Value,
        op_ptr: &str,
        version: Version,
    ) -> Result<Vec<Param>, LoadError> {
        let mut out: Vec<Param> = Vec::new();
        for (container, cptr) in [(item, item_ptr), (op, op_ptr)] {
            let Some(list) = container.get("parameters").and_then(Value::as_array) else {
                continue;
            };
            let lptr = doc::join(cptr, "parameters");
            for (i, v) in list.iter().enumerate() {
                let Some(p) = self.param(
                    v,
                    &doc::join(&lptr, &i.to_string()),
                    None,
                    Direction::Request,
                    version,
                )?
                else {
                    continue;
                };
                if p.location == In::Header
                    && ["accept", "content-type", "authorization"]
                        .contains(&p.name.to_ascii_lowercase().as_str())
                {
                    continue;
                }
                let same = |q: &Param| {
                    q.location == p.location
                        && if p.location == In::Header {
                            q.name.eq_ignore_ascii_case(&p.name)
                        } else {
                            q.name == p.name
                        }
                };
                if let Some(pos) = out.iter().position(same) {
                    out[pos] = p;
                } else {
                    out.push(p);
                }
            }
        }
        Ok(out)
    }
}

// ---------------------------------------------------------- query parsing

/// Percent-decodes (RFC 3986 §2.1), optionally reading `+` as a space.
/// Fails on a malformed escape or a result that is not UTF-8.
pub(crate) fn decode(raw: &str, plus_as_space: bool) -> Result<String, ()> {
    let b = raw.as_bytes();
    if !b.iter().any(|&c| c == b'%' || (plus_as_space && c == b'+')) {
        return Ok(raw.to_owned());
    }
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => {
                let h = b.get(i + 1..i + 3).ok_or(())?;
                if !h.iter().all(u8::is_ascii_hexdigit) {
                    return Err(());
                }
                let s = std::str::from_utf8(h).map_err(|_| ())?;
                out.push(u8::from_str_radix(s, 16).map_err(|_| ())?);
                i += 3;
            }
            b'+' if plus_as_space => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|_| ())
}

/// One `name=value` pair of a query string, name decoded, value raw.
#[derive(Debug, Clone)]
pub(crate) struct Pair<'a> {
    pub name: String,
    pub raw: &'a str,
}

/// Splits a query string on `&` only (`;` stays inside values; RFC 3986
/// §3.4 gives it no meaning), with no limit on the number of pairs. Pairs
/// whose name cannot be decoded are returned in `bad`.
pub(crate) fn split_query(q: &str, plus_as_space: bool) -> (Vec<Pair<'_>>, Vec<&str>) {
    let mut pairs = Vec::new();
    let mut bad = Vec::new();
    for piece in q.split('&') {
        if piece.is_empty() {
            continue;
        }
        let (n, raw) = piece.split_once('=').unwrap_or((piece, ""));
        match decode(n, plus_as_space) {
            Ok(name) => pairs.push(Pair { name, raw }),
            Err(()) => bad.push(piece),
        }
    }
    (pairs, bad)
}

/// Cookie pairs from every `Cookie` header line (RFC 6265 §5.4).
pub(crate) fn split_cookies<'a>(lines: impl Iterator<Item = &'a str>) -> Vec<(&'a str, &'a str)> {
    let mut out = Vec::new();
    for line in lines {
        for part in line.split(';') {
            let part = part.trim_matches([' ', '\t']);
            if part.is_empty() {
                continue;
            }
            match part.split_once('=') {
                Some((n, v)) => out.push((n.trim(), v.trim())),
                None => out.push((part, "")),
            }
        }
    }
    out
}

// ------------------------------------------------------- value building

/// Turns one text item into JSON as the allowed types suggest.
pub(crate) fn coerce(s: &str, t: Types) -> Value {
    if (t.has(Types::INT) || t.has(Types::NUM)) && Decimal::parse(s).is_some() {
        if let Ok(n) = serde_json::from_str::<Number>(s) {
            return Value::Number(n);
        }
    }
    if t.has(Types::BOOL) && (s == "true" || s == "false") {
        return Value::Bool(s == "true");
    }
    if t.has(Types::NULL) && !t.has(Types::STR) && (s.is_empty() || s == "null") {
        return Value::Null;
    }
    Value::String(s.to_owned())
}

/// Splits raw text on a delimiter and decodes each piece.
fn split_decode(raw: &str, delims: &[&str], plus: bool) -> Result<Vec<String>, ()> {
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    let mut pieces = vec![raw];
    for d in delims {
        pieces = pieces.into_iter().flat_map(|p| p.split(d)).collect();
    }
    pieces.into_iter().map(|p| decode(p, plus)).collect()
}

/// `k,v,k,v` into an object.
fn pairs_to_object(items: Vec<String>, props: &[(String, Types)]) -> Value {
    let mut m = Map::new();
    let mut it = items.into_iter();
    while let Some(k) = it.next() {
        let v = it.next().unwrap_or_default();
        let t = props
            .iter()
            .find(|(n, _)| *n == k)
            .map_or(Types::ANY, |(_, t)| *t);
        m.insert(k, coerce(&v, t));
    }
    Value::Object(m)
}

/// `k=v` items into an object.
fn kv_to_object(items: Vec<String>, props: &[(String, Types)]) -> Value {
    let mut m = Map::new();
    for item in items {
        let (k, v) = item.split_once('=').unwrap_or((item.as_str(), ""));
        let t = props
            .iter()
            .find(|(n, _)| n == k)
            .map_or(Types::ANY, |(_, t)| *t);
        m.insert(k.to_owned(), coerce(v, t));
    }
    Value::Object(m)
}

/// Builds the JSON value of a simple/label/matrix/form-style text whose
/// prefix has already been removed. `sep` separates array items or object
/// entries when exploded.
fn build(text: &str, shape: &Shape, explode: bool, sep: &str, plus: bool) -> Result<Value, ()> {
    Ok(match shape {
        Shape::Primitive(t) => coerce(&decode(text, plus)?, *t),
        Shape::Array(t) => {
            let delim = if explode { sep } else { "," };
            Value::Array(
                split_decode(text, &[delim], plus)?
                    .iter()
                    .map(|s| coerce(s, *t))
                    .collect(),
            )
        }
        Shape::Object(props) => {
            if explode {
                kv_to_object(split_decode(text, &[sep], plus)?, props)
            } else {
                pairs_to_object(split_decode(text, &[","], plus)?, props)
            }
        }
    })
}

/// The JSON value of a path parameter from its raw (encoded) text.
pub(crate) fn path_value(p: &Param, raw: &str, shape: &Shape) -> Result<Value, String> {
    let bad = |what: &str| format!("is not a valid {what}-style value");
    match p.style {
        Style::Label => {
            let rest = raw.strip_prefix('.').ok_or_else(|| bad("label"))?;
            build(rest, shape, p.explode, ".", false).map_err(|_| bad("label"))
        }
        Style::Matrix => {
            let rest = raw.strip_prefix(';').ok_or_else(|| bad("matrix"))?;
            let prefix = format!("{}=", p.name);
            match shape {
                Shape::Object(_) if p.explode => {
                    build(rest, shape, true, ";", false).map_err(|_| bad("matrix"))
                }
                Shape::Array(_) if p.explode => {
                    let mut items = Vec::new();
                    for part in rest.split(';') {
                        let v = part.strip_prefix(&prefix).ok_or_else(|| bad("matrix"))?;
                        items.push(v.to_owned());
                    }
                    let joined = items.join(",");
                    build(&joined, shape, false, ",", false).map_err(|_| bad("matrix"))
                }
                _ => {
                    if rest == p.name {
                        return build("", shape, p.explode, ",", false).map_err(|_| bad("matrix"));
                    }
                    let v = rest.strip_prefix(&prefix).ok_or_else(|| bad("matrix"))?;
                    build(v, shape, false, ",", false).map_err(|_| bad("matrix"))
                }
            }
        }
        _ => build(raw, shape, p.explode, ",", false).map_err(|_| bad("simple")),
    }
}

/// What a query or cookie parameter contributed: its value, or that it was
/// absent, and the pair names it consumed.
pub(crate) enum Found {
    Absent,
    Values(Vec<Value>),
    Malformed(String),
}

/// Query (or cookie) parameter value(s) from decoded-name pairs.
pub(crate) fn query_value(
    p: &Param,
    pairs: &[Pair<'_>],
    shape: &Shape,
    plus: bool,
    consumed: &mut Vec<String>,
) -> Found {
    let mine: Vec<&Pair<'_>> = pairs.iter().filter(|q| q.name == p.name).collect();
    let malformed = || Found::Malformed("has an invalid percent-escape".into());
    match (p.style, shape) {
        (Style::DeepObject, _) => {
            let prefix = format!("{}[", p.name);
            let mut root = Map::new();
            let mut any = false;
            for q in pairs.iter().filter(|q| q.name.starts_with(&prefix)) {
                consumed.push(q.name.clone());
                any = true;
                let Ok(v) = decode(q.raw, plus) else {
                    return malformed();
                };
                let keys = bracket_keys(&q.name[p.name.len()..]);
                let types = match shape {
                    Shape::Object(props) => keys
                        .first()
                        .and_then(|k| props.iter().find(|(n, _)| n == k))
                        .map_or(Types::ANY, |(_, t)| *t),
                    _ => Types::ANY,
                };
                insert_deep(&mut root, &keys, coerce(&v, types));
            }
            if any {
                Found::Values(vec![Value::Object(root)])
            } else {
                Found::Absent
            }
        }
        (_, Shape::Object(props)) if p.explode => {
            // Exploded objects take only their declared properties.
            let mut m = Map::new();
            for (name, t) in props {
                if let Some(q) = pairs.iter().find(|q| q.name == *name) {
                    consumed.push(name.clone());
                    let Ok(v) = decode(q.raw, plus) else {
                        return malformed();
                    };
                    m.insert(name.clone(), coerce(&v, *t));
                }
            }
            if m.is_empty() {
                Found::Absent
            } else {
                Found::Values(vec![Value::Object(m)])
            }
        }
        (style, Shape::Array(t)) => {
            if mine.is_empty() {
                return Found::Absent;
            }
            consumed.push(p.name.clone());
            let delims: &[&str] = match (style, p.explode) {
                (_, true) => &[],
                (Style::SpaceDelimited, false) => &["%20", " ", "+"],
                (Style::PipeDelimited, false) => &["|", "%7C", "%7c"],
                _ => &[","],
            };
            let mut items = Vec::new();
            // `list=` alone is an empty array.
            let only_empty = mine.len() == 1 && mine[0].raw.is_empty();
            if !only_empty {
                for q in &mine {
                    let pieces = if delims.is_empty() {
                        decode(q.raw, plus).map(|s| vec![s])
                    } else {
                        split_decode(q.raw, delims, plus)
                    };
                    let Ok(pieces) = pieces else {
                        return malformed();
                    };
                    items.extend(pieces.iter().map(|s| coerce(s, *t)));
                }
            }
            Found::Values(vec![Value::Array(items)])
        }
        (style, Shape::Object(props)) => {
            let Some(q) = mine.first() else {
                return Found::Absent;
            };
            consumed.push(p.name.clone());
            let delims: &[&str] = match style {
                Style::SpaceDelimited => &["%20", " ", "+"],
                Style::PipeDelimited => &["|", "%7C", "%7c"],
                _ => &[","],
            };
            match split_decode(q.raw, delims, plus) {
                Ok(items) => Found::Values(vec![pairs_to_object(items, props)]),
                Err(()) => malformed(),
            }
        }
        (_, Shape::Primitive(t)) => {
            if mine.is_empty() {
                return Found::Absent;
            }
            consumed.push(p.name.clone());
            let mut vals = Vec::new();
            for q in &mine {
                let Ok(v) = decode(q.raw, plus) else {
                    return malformed();
                };
                vals.push(coerce(&v, *t));
            }
            Found::Values(vals)
        }
    }
}

/// `[a][b]` into `["a", "b"]`; text outside brackets ends the list.
fn bracket_keys(s: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut rest = s;
    while let Some(inner) = rest.strip_prefix('[') {
        let Some(end) = inner.find(']') else {
            keys.push(inner.to_owned());
            break;
        };
        keys.push(inner[..end].to_owned());
        rest = &inner[end + 1..];
    }
    keys
}

/// Inserts a deepObject value, nesting for `a[b][c]` (one level is all
/// OpenAPI defines; deeper levels nest, and `[]` appends to an array).
fn insert_deep(root: &mut Map<String, Value>, keys: &[String], v: Value) {
    let Some((first, rest)) = keys.split_first() else {
        return;
    };
    if rest.is_empty() {
        root.insert(first.clone(), v);
        return;
    }
    if rest.len() == 1 && rest[0].is_empty() {
        let e = root
            .entry(first.clone())
            .or_insert_with(|| Value::Array(Vec::new()));
        if let Value::Array(a) = e {
            a.push(v);
        }
        return;
    }
    let e = root
        .entry(first.clone())
        .or_insert_with(|| Value::Object(Map::new()));
    if let Value::Object(m) = e {
        insert_deep(m, rest, v);
    }
}

/// The value of a header parameter from one header line (simple style,
/// §4.8.12.4). Array items and object entries are trimmed of the optional
/// whitespace that list-valued headers commonly carry (RFC 9110 §5.6.1).
pub(crate) fn header_value(line: &str, shape: &Shape, explode: bool) -> Value {
    match shape {
        Shape::Primitive(t) => coerce(line, *t),
        Shape::Array(t) => {
            if line.is_empty() {
                return Value::Array(Vec::new());
            }
            Value::Array(line.split(',').map(|s| coerce(s.trim(), *t)).collect())
        }
        Shape::Object(props) => {
            let items: Vec<String> = line.split(',').map(|s| s.trim().to_owned()).collect();
            if explode {
                kv_to_object(items, props)
            } else {
                pairs_to_object(items, props)
            }
        }
    }
}

/// Every reading of a value, primary first: the typed reading, then (when
/// it differs and strings are allowed) the all-strings reading.
pub(crate) fn readings(v: Value, shape: &Shape) -> Vec<Value> {
    let allows_str = match shape {
        Shape::Primitive(t) | Shape::Array(t) => t.has(Types::STR),
        Shape::Object(_) => true,
    };
    let s = stringify(&v);
    if allows_str && s != v {
        vec![v, s]
    } else {
        vec![v]
    }
}

fn stringify(v: &Value) -> Value {
    match v {
        Value::Number(n) => Value::String(n.to_string()),
        Value::Bool(b) => Value::String(b.to_string()),
        Value::Array(a) => Value::Array(a.iter().map(stringify).collect()),
        Value::Object(m) => {
            Value::Object(m.iter().map(|(k, x)| (k.clone(), stringify(x))).collect())
        }
        other => other.clone(),
    }
}

/// Indexes header lines by lower-cased name.
pub(crate) fn header_index<'a>(headers: &'a [(&'a str, &'a str)]) -> HashMap<String, Vec<&'a str>> {
    let mut m: HashMap<String, Vec<&'a str>> = HashMap::new();
    for (n, v) in headers {
        m.entry(n.to_ascii_lowercase())
            .or_default()
            .push(v.trim_matches([' ', '\t']));
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3986_s2_1_decoding() {
        assert_eq!(decode("a%20b", false), Ok("a b".into()));
        assert_eq!(decode("a+b", true), Ok("a b".into()));
        assert_eq!(decode("a+b", false), Ok("a+b".into()));
        assert_eq!(decode("%2F", false), Ok("/".into()));
        assert!(decode("%zz", false).is_err());
        assert!(decode("%2", false).is_err());
        assert!(decode("%FF", false).is_err());
    }

    #[test]
    fn query_splits_only_on_ampersand() {
        let (pairs, bad) = split_query("a=1;b=2&c&&d=%zz&%zz=1&e=", true);
        let names: Vec<&str> = pairs.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["a", "c", "d", "e"]);
        assert_eq!(pairs[0].raw, "1;b=2");
        assert_eq!(bad, ["%zz=1"]);
        let many: String = (0..5000).map(|i| format!("k{i}=v&")).collect();
        assert_eq!(
            split_query(&many, true).0.len(),
            5000,
            "no silent pair limit"
        );
    }

    #[test]
    fn coercion_follows_types() {
        assert_eq!(coerce("5", Types::INT.union(Types::NUM)), Value::from(5));
        assert_eq!(coerce("05", Types::INT), Value::String("05".into()));
        assert_eq!(coerce("true", Types::BOOL), Value::Bool(true));
        assert_eq!(coerce("true", Types::STR), Value::String("true".into()));
        assert_eq!(coerce("", Types::NULL.union(Types::INT)), Value::Null);
    }

    #[test]
    fn deep_object_keys() {
        assert_eq!(bracket_keys("[a][b]"), vec!["a", "b"]);
        let mut m = Map::new();
        insert_deep(&mut m, &["a".into(), "b".into()], Value::from(1));
        insert_deep(&mut m, &["l".into(), "".into()], Value::from(2));
        assert_eq!(
            Value::Object(m),
            serde_json::json!({"a": {"b": 1}, "l": [2]})
        );
    }
}
