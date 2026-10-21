//! Non-JSON request and response bodies.
//!
//! - `application/x-www-form-urlencoded` (OpenAPI 3.1.1 §4.8.15.4, WHATWG
//!   URL §5.1): pairs become an object, each property read with its Encoding
//!   Object `style` and `explode` (default `form`, exploded).
//! - `multipart/form-data` (RFC 7578, OpenAPI 3.1.1 §4.8.15.5): each part
//!   becomes a property; JSON parts are parsed, others are text.
//! - `text/plain`: the text, read as the schema's type allows.
//! - XML: must be well-formed. Its content is not checked against the
//!   schema (the XML Object mapping is not implemented).

use serde_json::{Map, Value};

use crate::json;
use crate::media::MediaType;
use crate::params::{coerce, decode, split_query, Shape, Style};

/// Per-property serialization for form and multipart bodies.
#[derive(Debug, Clone)]
pub(crate) struct Encoding {
    pub name: String,
    pub content_type: Option<MediaType>,
    pub style: Style,
    pub explode: bool,
}

/// Declared properties of a form body with their shapes.
pub(crate) type FormProps = Vec<(String, Shape)>;

fn props_shape<'a>(props: &'a FormProps, name: &str) -> Option<&'a Shape> {
    props.iter().find(|(n, _)| n == name).map(|(_, s)| s)
}

fn insert_repeated(m: &mut Map<String, Value>, k: String, v: Value) {
    match m.get_mut(&k) {
        Some(Value::Array(a)) => a.push(v),
        Some(existing) => {
            let first = std::mem::take(existing);
            *existing = Value::Array(vec![first, v]);
        }
        None => {
            m.insert(k, v);
        }
    }
}

/// Parses a urlencoded form body into an object.
pub(crate) fn form(
    body: &str,
    props: &FormProps,
    encodings: &[Encoding],
    depth: usize,
) -> Result<Value, String> {
    let (pairs, bad) = split_query(body, true);
    if let Some(b) = bad.first() {
        return Err(format!(
            "the pair {:?} has an invalid percent-escape",
            crate::spec::truncate(b, 60)
        ));
    }
    let mut m = Map::new();
    let mut used: Vec<&str> = Vec::new();
    for (name, shape) in props {
        let enc = encodings.iter().find(|e| e.name == *name);
        let style = enc.map_or(Style::Form, |e| e.style);
        let explode = enc.is_none_or(|e| e.explode);
        if style == Style::DeepObject {
            let prefix = format!("{name}[");
            let mut obj = Map::new();
            for p in pairs.iter().filter(|p| p.name.starts_with(&prefix)) {
                let key = p.name[prefix.len()..].trim_end_matches(']').to_owned();
                let v = decode(p.raw, true)
                    .map_err(|_| format!("{name} has an invalid percent-escape"))?;
                obj.insert(key, Value::String(v));
            }
            if !obj.is_empty() {
                m.insert(name.clone(), Value::Object(obj));
            }
            continue;
        }
        let mine: Vec<_> = pairs.iter().filter(|p| p.name == *name).collect();
        if mine.is_empty() {
            continue;
        }
        used.push(name);
        let texts: Result<Vec<String>, ()> = mine.iter().map(|p| decode(p.raw, true)).collect();
        let texts = texts.map_err(|_| format!("{name} has an invalid percent-escape"))?;
        if enc
            .and_then(|e| e.content_type.as_ref())
            .is_some_and(MediaType::is_json)
        {
            let mut vals = Vec::new();
            for t in &texts {
                vals.push(
                    json::parse(t.as_bytes(), json::Limits { max_depth: depth })
                        .map_err(|e| format!("{name} is not valid JSON: {e}"))?,
                );
            }
            m.insert(
                name.clone(),
                if vals.len() == 1 {
                    vals.remove(0)
                } else {
                    Value::Array(vals)
                },
            );
            continue;
        }
        let value = match shape {
            Shape::Array(item) => {
                let delim = match (style, explode) {
                    (_, true) => None,
                    (Style::SpaceDelimited, false) => Some(' '),
                    (Style::PipeDelimited, false) => Some('|'),
                    _ => Some(','),
                };
                let mut items = Vec::new();
                if !(texts.len() == 1 && texts[0].is_empty()) {
                    for text in &texts {
                        match delim {
                            Some(d) => items.extend(text.split(d).map(|s| coerce(s, *item))),
                            None => items.push(coerce(text, *item)),
                        }
                    }
                }
                Value::Array(items)
            }
            Shape::Primitive(t) => {
                if texts.len() == 1 {
                    coerce(&texts[0], *t)
                } else {
                    Value::Array(texts.iter().map(|s| coerce(s, *t)).collect())
                }
            }
            Shape::Object(_) => Value::String(texts.join(",")),
        };
        m.insert(name.clone(), value);
    }
    // Undeclared fields are still properties of the form object, so
    // `additionalProperties` can judge them.
    for p in &pairs {
        if used.contains(&p.name.as_str()) || props_shape(props, &p.name).is_some() {
            continue;
        }
        if props
            .iter()
            .any(|(n, _)| p.name.starts_with(&format!("{n}[")))
        {
            continue;
        }
        let v =
            decode(p.raw, true).map_err(|_| format!("{} has an invalid percent-escape", p.name))?;
        insert_repeated(&mut m, p.name.clone(), Value::String(v));
    }
    Ok(Value::Object(m))
}

/// One part of a multipart body.
struct Part<'a> {
    name: String,
    content_type: Option<MediaType>,
    file: bool,
    data: &'a [u8],
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from > hay.len() {
        return None;
    }
    hay[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

/// Splits a multipart body (RFC 2046 §5.1.1). Line breaks may be CRLF or,
/// leniently, LF.
fn parts<'a>(body: &'a [u8], boundary: &str) -> Result<Vec<Part<'a>>, String> {
    let delim = format!("--{boundary}");
    let d = delim.as_bytes();
    let mut out = Vec::new();
    let mut pos = match find(body, d, 0) {
        Some(p) if p == 0 || body[..p].ends_with(b"\n") => p,
        _ => return Err("the multipart body has no opening boundary".into()),
    };
    loop {
        let after = pos + d.len();
        if body[after..].starts_with(b"--") {
            return Ok(out);
        }
        let header_start = if body[after..].starts_with(b"\r\n") {
            after + 2
        } else if body[after..].starts_with(b"\n") {
            after + 1
        } else {
            return Err("a multipart boundary is not followed by a line break".into());
        };
        // The next delimiter must start a line.
        let mut search = header_start;
        let next = loop {
            let Some(n) = find(body, d, search) else {
                return Err("the multipart body has no closing boundary".into());
            };
            if body[..n].ends_with(b"\n") {
                break n;
            }
            search = n + 1;
        };
        let mut end = next;
        if body[..end].ends_with(b"\r\n") {
            end -= 2;
        } else if body[..end].ends_with(b"\n") {
            end -= 1;
        }
        let chunk = &body[header_start..end.max(header_start)];
        let (head, data) = match find(chunk, b"\r\n\r\n", 0) {
            Some(p) => (&chunk[..p], &chunk[p + 4..]),
            None => match find(chunk, b"\n\n", 0) {
                Some(p) => (&chunk[..p], &chunk[p + 2..]),
                None if chunk.starts_with(b"\r\n") => (&chunk[..0], &chunk[2..]),
                None => return Err("a multipart part has no blank line after its headers".into()),
            },
        };
        let head = std::str::from_utf8(head)
            .map_err(|_| "multipart part headers are not UTF-8".to_string())?;
        let mut name = None;
        let mut content_type = None;
        let mut file = false;
        for line in head.lines() {
            let Some((k, v)) = line.split_once(':') else {
                continue;
            };
            let k = k.trim();
            if k.eq_ignore_ascii_case("content-disposition") {
                for param in v.split(';').skip(1) {
                    let Some((pk, pv)) = param.split_once('=') else {
                        continue;
                    };
                    let pv = pv.trim().trim_matches('"').to_owned();
                    match pk.trim().to_ascii_lowercase().as_str() {
                        "name" => name = Some(pv),
                        "filename" | "filename*" => file = true,
                        _ => {}
                    }
                }
            } else if k.eq_ignore_ascii_case("content-type") {
                content_type = MediaType::parse(v.trim());
            }
        }
        let Some(name) = name else {
            return Err("a multipart part has no name in Content-Disposition".into());
        };
        out.push(Part {
            name,
            content_type,
            file,
            data,
        });
        pos = next;
    }
}

/// Parses a multipart/form-data body into an object.
pub(crate) fn multipart(
    body: &[u8],
    media: &MediaType,
    props: &FormProps,
    encodings: &[Encoding],
    depth: usize,
) -> Result<Value, String> {
    let Some(boundary) = media
        .param("boundary")
        .filter(|b| !b.is_empty() && b.len() <= 70)
    else {
        return Err("multipart/form-data needs a boundary parameter".into());
    };
    let mut m = Map::new();
    for part in parts(body, boundary)? {
        let declared_json = encodings
            .iter()
            .find(|e| e.name == part.name)
            .and_then(|e| e.content_type.as_ref())
            .is_some_and(MediaType::is_json);
        let shape = props_shape(props, &part.name);
        // Default part types (OpenAPI 3.0.4 §4.8.15.1): objects are JSON;
        // arrays take their items' type, so each item is its own part.
        let object = matches!(shape, Some(Shape::Object(_)));
        let is_json = part
            .content_type
            .as_ref()
            .map_or(declared_json || (object && !part.file), MediaType::is_json);
        let value = if is_json {
            json::parse(part.data, json::Limits { max_depth: depth })
                .map_err(|e| format!("part {:?} is not valid JSON: {e}", part.name))?
        } else {
            let text = String::from_utf8_lossy(part.data).into_owned();
            match shape {
                Some(Shape::Primitive(t)) if !part.file => coerce(&text, *t),
                Some(Shape::Array(t)) if !part.file => coerce(&text, *t),
                _ => Value::String(text),
            }
        };
        let repeated_array = matches!(shape, Some(Shape::Array(_))) && !is_json;
        if repeated_array {
            match m.get_mut(&part.name) {
                Some(Value::Array(a)) => a.push(value),
                _ => {
                    m.insert(part.name.clone(), Value::Array(vec![value]));
                }
            }
        } else {
            insert_repeated(&mut m, part.name.clone(), value);
        }
    }
    Ok(Value::Object(m))
}

/// Decodes a text body by its charset: UTF-8 (the default), US-ASCII or
/// ISO-8859-1.
pub(crate) fn text(body: &[u8], media: &MediaType) -> Result<String, String> {
    match media
        .param("charset")
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        None | Some("utf-8" | "utf8" | "us-ascii") => std::str::from_utf8(body)
            .map(str::to_owned)
            .map_err(|_| "the body is not valid UTF-8".to_string()),
        Some("iso-8859-1" | "latin1") => Ok(body.iter().map(|&b| char::from(b)).collect()),
        Some(other) => Err(format!("charset {other:?} is not supported")),
    }
}

/// Entity references nest at most this deep in `roxmltree`.
const XML_ENTITY_DEPTH: usize = 10;

fn skip_past(b: &[u8], from: usize, end: &[u8]) -> usize {
    find(b, end, from).map_or(b.len(), |p| p + end.len())
}

/// An upper bound on element nesting, found without recursion: the
/// nesting of the text itself, plus what entity expansion could add. The
/// XML parser recurses per level, so deeper input is refused before it
/// runs.
fn xml_depth_bound(b: &[u8]) -> usize {
    let mut depth = 0usize;
    let mut max = 0usize;
    let mut entity_tags = 0usize;
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'<' {
            i += 1;
            continue;
        }
        let rest = &b[i..];
        if rest.starts_with(b"<!--") {
            i = skip_past(b, i + 4, b"-->");
        } else if rest.starts_with(b"<![CDATA[") {
            i = skip_past(b, i + 9, b"]]>");
        } else if rest.starts_with(b"<?") {
            i = skip_past(b, i + 2, b"?>");
        } else if rest.starts_with(b"<!") {
            // A declaration, possibly with an internal subset. Count start
            // tags inside it (they can only come from entity values).
            let mut j = i + 2;
            let mut bracket = 0usize;
            let mut quote = 0u8;
            while j < b.len() {
                let c = b[j];
                if quote != 0 {
                    if c == quote {
                        quote = 0;
                    } else if c == b'<' && b.get(j + 1).is_some_and(|n| n.is_ascii_alphabetic()) {
                        entity_tags += 1;
                    }
                } else if c == b'"' || c == b'\'' {
                    quote = c;
                } else if c == b'[' {
                    bracket += 1;
                } else if c == b']' {
                    bracket = bracket.saturating_sub(1);
                } else if c == b'>' && bracket == 0 {
                    break;
                }
                j += 1;
            }
            i = j + 1;
        } else if rest.starts_with(b"</") {
            depth = depth.saturating_sub(1);
            i = skip_past(b, i + 2, b">");
        } else {
            let mut j = i + 1;
            let mut quote = 0u8;
            while j < b.len() {
                let c = b[j];
                if quote != 0 {
                    if c == quote {
                        quote = 0;
                    }
                } else if c == b'"' || c == b'\'' {
                    quote = c;
                } else if c == b'>' {
                    break;
                }
                j += 1;
            }
            if b.get(j.wrapping_sub(1)) != Some(&b'/') {
                depth += 1;
                max = max.max(depth);
            }
            i = j + 1;
        }
    }
    max.saturating_add(entity_tags.saturating_mul(XML_ENTITY_DEPTH))
}

/// Checks that an XML body is well-formed (XML 1.0 §2.1). Nesting deeper
/// than `max_depth` is refused before parsing.
pub(crate) fn xml(body: &[u8], media: &MediaType, max_depth: usize) -> Result<(), String> {
    let s = text(body, media)?;
    if xml_depth_bound(s.as_bytes()) > max_depth {
        return Err(format!(
            "the XML body nests deeper than {max_depth} elements"
        ));
    }
    let opts = roxmltree::ParsingOptions {
        allow_dtd: true,
        nodes_limit: 1_000_000,
        ..roxmltree::ParsingOptions::default()
    };
    roxmltree::Document::parse_with_options(&s, opts)
        .map(|_| ())
        .map_err(|e| format!("the body is not well-formed XML: {e}"))
}
