//! YAML 1.2 loading into `serde_json::Value`.
//!
//! Plain scalars are resolved with the YAML 1.2 Core schema (§10.3.2), so
//! `yes`, `no`, `on`, `off`, `y` and `n` are strings, not booleans as they
//! were in YAML 1.1. Quoted scalars are always strings. Mapping keys keep
//! their written text (`200:` becomes the key `"200"`).
//!
//! The builder is iterative and bounded: nesting depth, the number of nodes
//! produced (aliases count each time they are expanded, so an alias bomb is
//! refused), and duplicate keys (YAML 1.2 §3.2.1.1 requires unique keys) are
//! all checked. `<<` merge keys are a YAML 1.1 feature and are read as an
//! ordinary key.

use std::collections::HashMap;
use std::fmt;

use num_bigint::BigUint;
use saphyr_parser::{Event, Parser, ScalarStyle, Span};
use serde_json::{Map, Number, Value};

use crate::number::Decimal;

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_depth: usize,
    /// Maximum number of nodes, counting every alias expansion.
    pub max_nodes: usize,
    /// Read `<<` keys as merge keys (YAML 1.1 merge type), as most YAML
    /// tooling still does. Off, `<<` is an ordinary key (YAML 1.2).
    pub merge_keys: bool,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_depth: 128,
            max_nodes: 4_000_000,
            merge_keys: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YamlError {
    pub line: usize,
    pub col: usize,
    pub message: String,
}

impl fmt::Display for YamlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "YAML line {}, column {}: {}",
            self.line, self.col, self.message
        )
    }
}

impl std::error::Error for YamlError {}

enum Frame {
    Seq {
        items: Vec<Value>,
        anchor: usize,
        nodes: usize,
        depth: usize,
    },
    Map {
        map: Map<String, Value>,
        key: Option<String>,
        anchor: usize,
        nodes: usize,
        depth: usize,
        /// Values of `<<` keys, in order.
        merges: Vec<Value>,
    },
}

/// A finished node with the number of nodes it contains, so aliases can be
/// charged their full expansion cost.
struct Built {
    value: Value,
    nodes: usize,
    depth: usize,
    /// Key text, when the node is a scalar that may be used as a key.
    key_text: Option<String>,
}

pub fn parse(input: &[u8], limits: Limits) -> Result<Value, YamlError> {
    let input = input.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(input);
    let text = std::str::from_utf8(input).map_err(|e| YamlError {
        line: 0,
        col: 0,
        message: format!("invalid UTF-8 at byte {}", e.valid_up_to()),
    })?;
    build(text, limits)
}

fn err(span: &Span, message: impl Into<String>) -> YamlError {
    YamlError {
        line: span.start.line(),
        col: span.start.col(),
        message: message.into(),
    }
}

fn build(text: &str, limits: Limits) -> Result<Value, YamlError> {
    let mut stack: Vec<Frame> = Vec::new();
    let mut anchors: HashMap<usize, (Value, usize, usize)> = HashMap::new();
    let mut total_nodes = 0usize;
    let mut result: Option<Value> = None;
    let mut documents = 0usize;

    for ev in Parser::new_from_str(text) {
        let (ev, span) = ev.map_err(|e| YamlError {
            line: e.marker().line(),
            col: e.marker().col(),
            message: e.info().to_owned(),
        })?;
        let built = match ev {
            Event::StreamStart | Event::StreamEnd | Event::DocumentEnd | Event::Nothing => continue,
            Event::DocumentStart(_) => {
                documents += 1;
                if documents > 1 {
                    return Err(err(&span, "more than one YAML document in the stream"));
                }
                continue;
            }
            Event::SequenceStart(anchor, _) => {
                if stack.len() >= limits.max_depth {
                    return Err(err(
                        &span,
                        format!("nesting deeper than {}", limits.max_depth),
                    ));
                }
                stack.push(Frame::Seq {
                    items: Vec::new(),
                    anchor,
                    nodes: 1,
                    depth: 0,
                });
                continue;
            }
            Event::MappingStart(anchor, _) => {
                if stack.len() >= limits.max_depth {
                    return Err(err(
                        &span,
                        format!("nesting deeper than {}", limits.max_depth),
                    ));
                }
                stack.push(Frame::Map {
                    map: Map::new(),
                    key: None,
                    anchor,
                    nodes: 1,
                    depth: 0,
                    merges: Vec::new(),
                });
                continue;
            }
            Event::SequenceEnd => match stack.pop() {
                Some(Frame::Seq {
                    items,
                    anchor,
                    nodes,
                    depth,
                }) => finish(Value::Array(items), anchor, nodes, depth + 1, &mut anchors),
                _ => return Err(err(&span, "unbalanced sequence end")),
            },
            Event::MappingEnd => match stack.pop() {
                Some(Frame::Map {
                    mut map,
                    key,
                    anchor,
                    nodes,
                    depth,
                    merges,
                }) => {
                    if key.is_some() {
                        return Err(err(&span, "mapping key without a value"));
                    }
                    // Explicit keys win; among merged mappings, earlier ones
                    // win (YAML 1.1 merge type).
                    for source in merges {
                        let maps = match source {
                            Value::Object(m) => vec![m],
                            Value::Array(a) => {
                                let mut v = Vec::new();
                                for x in a {
                                    match x {
                                        Value::Object(m) => v.push(m),
                                        _ => return Err(err(&span, "a merge key needs mappings")),
                                    }
                                }
                                v
                            }
                            _ => {
                                return Err(err(
                                    &span,
                                    "a merge key needs a mapping or a list of mappings",
                                ))
                            }
                        };
                        for m in maps {
                            for (k, v) in m {
                                map.entry(k).or_insert(v);
                            }
                        }
                    }
                    finish(Value::Object(map), anchor, nodes, depth + 1, &mut anchors)
                }
                _ => return Err(err(&span, "unbalanced mapping end")),
            },
            Event::Scalar(raw, style, anchor, tag) => {
                let tag = tag.map(|t| format!("{}{}", t.handle, t.suffix));
                let value =
                    resolve_scalar(&raw, style, tag.as_deref()).map_err(|m| err(&span, m))?;
                let key_text = Some(raw.into_owned());
                let b = Built {
                    value,
                    nodes: 1,
                    depth: 0,
                    key_text,
                };
                if anchor > 0 {
                    anchors.insert(anchor, (b.value.clone(), 1, 0));
                }
                b
            }
            Event::Alias(id) => {
                let Some((v, nodes, depth)) = anchors.get(&id) else {
                    return Err(err(&span, "alias to an unknown anchor"));
                };
                if stack.len() + depth > limits.max_depth {
                    return Err(err(
                        &span,
                        format!("nesting deeper than {}", limits.max_depth),
                    ));
                }
                let key_text = match v {
                    Value::String(s) => Some(s.clone()),
                    Value::Number(n) => Some(n.to_string()),
                    Value::Bool(b) => Some(b.to_string()),
                    Value::Null => Some("null".into()),
                    _ => None,
                };
                Built {
                    value: v.clone(),
                    nodes: *nodes,
                    depth: *depth,
                    key_text,
                }
            }
        };
        total_nodes = total_nodes.saturating_add(built.nodes);
        if total_nodes > limits.max_nodes {
            return Err(err(
                &span,
                format!("document expands to more than {} nodes", limits.max_nodes),
            ));
        }
        match stack.last_mut() {
            None => {
                if result.is_some() {
                    return Err(err(&span, "more than one root node"));
                }
                result = Some(built.value);
            }
            Some(Frame::Seq {
                items,
                nodes,
                depth,
                ..
            }) => {
                *nodes = nodes.saturating_add(built.nodes);
                *depth = (*depth).max(built.depth);
                items.push(built.value);
            }
            Some(Frame::Map {
                map,
                key,
                nodes,
                depth,
                merges,
                ..
            }) => {
                *nodes = nodes.saturating_add(built.nodes);
                *depth = (*depth).max(built.depth);
                match key.take() {
                    None => {
                        let Some(k) = built.key_text.filter(|_| is_scalar(&built.value)) else {
                            return Err(err(&span, "mapping keys must be scalars"));
                        };
                        if map.contains_key(&k) {
                            return Err(err(&span, format!("duplicate mapping key {k:?}")));
                        }
                        *key = Some(k);
                    }
                    Some(k) if k == "<<" && limits.merge_keys => merges.push(built.value),
                    Some(k) => {
                        map.insert(k, built.value);
                    }
                }
            }
        }
    }
    Ok(result.unwrap_or(Value::Null))
}

fn is_scalar(v: &Value) -> bool {
    !matches!(v, Value::Array(_) | Value::Object(_))
}

fn finish(
    value: Value,
    anchor: usize,
    nodes: usize,
    depth: usize,
    anchors: &mut HashMap<usize, (Value, usize, usize)>,
) -> Built {
    if anchor > 0 {
        anchors.insert(anchor, (value.clone(), nodes, depth));
    }
    Built {
        value,
        nodes,
        depth,
        key_text: None,
    }
}

/// YAML 1.2 Core schema resolution (§10.3.2) for one scalar.
fn resolve_scalar(raw: &str, style: ScalarStyle, tag: Option<&str>) -> Result<Value, String> {
    match tag {
        Some("tag:yaml.org,2002:str" | "!!str") => return Ok(Value::String(raw.to_owned())),
        Some("tag:yaml.org,2002:null" | "!!null") => return Ok(Value::Null),
        Some("tag:yaml.org,2002:bool" | "!!bool") => {
            return core_bool(raw)
                .map(Value::Bool)
                .ok_or_else(|| format!("{raw:?} is not a YAML boolean"))
        }
        Some("tag:yaml.org,2002:int" | "!!int" | "tag:yaml.org,2002:float" | "!!float") => {
            return core_number(raw).unwrap_or_else(|| Err(format!("{raw:?} is not a YAML number")))
        }
        _ => {}
    }
    if style != ScalarStyle::Plain {
        return Ok(Value::String(raw.to_owned()));
    }
    if matches!(raw, "" | "~" | "null" | "Null" | "NULL") {
        return Ok(Value::Null);
    }
    if let Some(b) = core_bool(raw) {
        return Ok(Value::Bool(b));
    }
    match core_number(raw) {
        Some(n) => n,
        None => Ok(Value::String(raw.to_owned())),
    }
}

fn core_bool(raw: &str) -> Option<bool> {
    match raw {
        "true" | "True" | "TRUE" => Some(true),
        "false" | "False" | "FALSE" => Some(false),
        _ => None,
    }
}

/// `Some(Ok)` for a Core-schema number, `Some(Err)` for one that cannot be
/// held exactly, `None` when the scalar is not a number at all.
fn core_number(raw: &str) -> Option<Result<Value, String>> {
    let to_value = |lit: String| -> Result<Value, String> {
        if Decimal::parse(&lit).is_none() {
            return Err(format!("number {raw} is too large to hold exactly"));
        }
        serde_json::from_str::<Number>(&lit)
            .map(Value::Number)
            .map_err(|_| format!("number {raw} is not representable"))
    };
    if let Some(h) = raw.strip_prefix("0x") {
        if !h.is_empty() && h.bytes().all(|c| c.is_ascii_hexdigit()) {
            return BigUint::parse_bytes(h.as_bytes(), 16).map(|n| to_value(n.to_string()));
        }
        return None;
    }
    if let Some(o) = raw.strip_prefix("0o") {
        if !o.is_empty() && o.bytes().all(|c| (b'0'..=b'7').contains(&c)) {
            return BigUint::parse_bytes(o.as_bytes(), 8).map(|n| to_value(n.to_string()));
        }
        return None;
    }
    let (neg, body) = match raw.as_bytes().first() {
        Some(b'-') => (true, &raw[1..]),
        Some(b'+') => (false, &raw[1..]),
        _ => (false, raw),
    };
    if matches!(body, ".inf" | ".Inf" | ".INF") || matches!(raw, ".nan" | ".NaN" | ".NAN") {
        // Not representable in JSON. Kept as text so the document still
        // loads; no OpenAPI keyword needs a non-finite number.
        return Some(Ok(Value::String(raw.to_owned())));
    }
    // [-+]? ( \. [0-9]+ | [0-9]+ ( \. [0-9]* )? ) ( [eE] [-+]? [0-9]+ )?
    let (mant, exp) = match body.find(['e', 'E']) {
        Some(p) => (&body[..p], Some(&body[p + 1..])),
        None => (body, None),
    };
    let (int, frac) = match mant.find('.') {
        Some(p) => (&mant[..p], Some(&mant[p + 1..])),
        None => (mant, None),
    };
    let digits = |s: &str| s.bytes().all(|c| c.is_ascii_digit());
    let ok_mant = match frac {
        None => !int.is_empty() && digits(int),
        Some(f) => digits(int) && digits(f) && (!int.is_empty() || !f.is_empty()),
    };
    let ok_exp = exp.is_none_or(|e| {
        let e = e.strip_prefix(['+', '-']).unwrap_or(e);
        !e.is_empty() && digits(e)
    });
    if !ok_mant || !ok_exp || (frac == Some("") && int.is_empty()) {
        return None;
    }
    let int = int.trim_start_matches('0');
    let mut lit = String::new();
    if neg {
        lit.push('-');
    }
    lit.push_str(if int.is_empty() { "0" } else { int });
    if let Some(f) = frac.filter(|f| !f.is_empty()) {
        lit.push('.');
        lit.push_str(f);
    } else if frac.is_some() {
        lit.push_str(".0");
    }
    if let Some(e) = exp {
        lit.push('e');
        lit.push_str(e);
    }
    Some(to_value(lit))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn y(s: &str) -> Result<Value, String> {
        parse(s.as_bytes(), Limits::default()).map_err(|e| e.message)
    }

    #[test]
    fn yaml12_core_schema_s10_3_2_booleans() {
        let v =
            y("a: yes\nb: no\nc: on\nd: off\ne: y\nf: n\ng: true\nh: False\ni: 'true'").unwrap();
        assert_eq!(
            v,
            json!({"a":"yes","b":"no","c":"on","d":"off","e":"y","f":"n","g":true,"h":false,"i":"true"})
        );
    }

    #[test]
    fn yaml12_core_schema_s10_3_2_null_and_numbers() {
        let v = y("a: ~\nb:\nc: null\nd: 007\ne: +12\nf: 0x1F\ng: 0o17\nh: 1.\ni: .5\nj: -1e3\nk: 1_000\nl: .inf\nm: 12:30").unwrap();
        assert_eq!(v["a"], Value::Null);
        assert_eq!(v["b"], Value::Null);
        assert_eq!(v["c"], Value::Null);
        assert_eq!(v["d"].to_string(), "7");
        assert_eq!(v["e"].to_string(), "12");
        assert_eq!(v["f"].to_string(), "31");
        assert_eq!(v["g"].to_string(), "15");
        assert_eq!(v["h"].to_string(), "1.0");
        assert_eq!(v["i"].to_string(), "0.5");
        assert_eq!(v["j"].to_string(), "-1e+3");
        assert_eq!(v["k"], "1_000", "underscores are YAML 1.1 only");
        assert_eq!(v["l"], ".inf");
        assert_eq!(v["m"], "12:30", "sexagesimal is YAML 1.1 only");
    }

    #[test]
    fn keys_keep_written_text() {
        let v = y("responses:\n  200: {description: ok}\n  2XX: {}\n  true: 1").unwrap();
        assert!(v["responses"]["200"].is_object());
        assert!(v["responses"]["2XX"].is_object());
        assert_eq!(v["responses"]["true"], 1);
    }

    #[test]
    fn yaml12_s3_2_1_1_duplicate_keys_are_refused() {
        assert!(y("a: 1\na: 2").unwrap_err().contains("duplicate"));
        assert!(y("a: 1\n'a': 2").unwrap_err().contains("duplicate"));
    }

    #[test]
    fn anchors_aliases_and_merge_key() {
        let v = y("x: &a {k: 1}\ny: *a\nz:\n  <<: *a").unwrap();
        assert_eq!(v["y"], json!({"k": 1}));
        assert_eq!(v["z"], json!({"k": 1}), "merge key");
        let v = y("a: &a {k: 1, m: 1}\nb: &b {k: 2, n: 2}\nc:\n  k: 0\n  <<: [*a, *b]").unwrap();
        assert_eq!(
            v["c"],
            json!({"k": 0, "m": 1, "n": 2}),
            "explicit keys and earlier sources win"
        );
        let off = parse(
            b"x: &a {k: 1}\nz:\n  <<: *a",
            Limits {
                merge_keys: false,
                ..Limits::default()
            },
        )
        .unwrap();
        assert_eq!(off["z"], json!({"<<": {"k": 1}}), "YAML 1.2 reading");
    }

    #[test]
    fn alias_bomb_is_refused() {
        let mut s = String::from("a0: &a0 [x, x, x, x, x, x, x, x, x, x]\n");
        for i in 1..12 {
            let p = i - 1;
            s.push_str(&format!(
                "a{i}: &a{i} [*a{p}, *a{p}, *a{p}, *a{p}, *a{p}, *a{p}, *a{p}, *a{p}, *a{p}, *a{p}]\n"
            ));
        }
        assert!(y(&s).unwrap_err().contains("expands"));
    }

    #[test]
    fn depth_is_bounded() {
        let deep = "[".repeat(10_000);
        assert!(y(&deep).is_err());
        let s = format!("{}1{}", "[".repeat(200), "]".repeat(200));
        assert!(y(&s).unwrap_err().contains("deeper"));
    }

    #[test]
    fn complex_keys_and_multiple_documents_are_refused() {
        assert!(y("? [a]\n: 1").is_err());
        assert!(y("a: 1\n---\nb: 2").is_err());
    }

    #[test]
    fn bom_and_utf8() {
        assert_eq!(
            parse(b"\xEF\xBB\xBFa: 1", Limits::default()).unwrap(),
            json!({"a": 1})
        );
        assert!(parse(b"a: \xff", Limits::default()).is_err());
    }
}
