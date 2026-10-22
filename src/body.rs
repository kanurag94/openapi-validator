//! Non-JSON request and response bodies.
//!
//! - `application/x-www-form-urlencoded` (OpenAPI 3.1.1 §4.8.14.4, WHATWG
//!   URL §5.1): pairs become an object, each property read with its Encoding
//!   Object `style` and `explode` (default `form`, exploded).
//! - `multipart/form-data` (RFC 7578, OpenAPI 3.1.1 §4.8.14.5): each part
//!   becomes a property; JSON parts are parsed, others are text.
//! - `text/plain`: the text, read as the schema's type allows.
//! - XML: must be well-formed. Its content is not checked against the
//!   schema (the XML Object mapping is not implemented).

use serde_json::{Map, Value};

use crate::json;
use crate::media::MediaType;
use crate::params::{
    bracket_keys, coerce, decode, insert_deep, pairs_to_object, split_query, Shape, Style, Types,
};

/// Per-property serialization for form and multipart bodies.
pub(crate) struct Encoding {
    pub name: String,
    /// The first listed type; decides how the part is parsed.
    pub content_type: Option<MediaType>,
    /// Every listed type or range (`contentType` may be a list).
    pub allowed: Vec<MediaType>,
    /// Header Objects for multipart parts (§4.8.15.1).
    pub headers: Vec<crate::params::Param>,
    pub style: Style,
    pub explode: bool,
    /// `style`, `explode` or `allowReserved` is written, so the value is
    /// serialized RFC 6570-style rather than by `contentType` (§4.8.15.1).
    pub rfc6570: bool,
    /// Nested positional encodings for a multipart part (OpenAPI 3.2).
    pub prefix: Vec<Encoding>,
    pub item: Option<Box<Encoding>>,
}

/// What a multipart part carried besides its value.
pub(crate) struct PartInfo {
    pub name: String,
    pub content_type: Option<MediaType>,
    pub headers: Vec<(String, String)>,
}

/// Declared properties of a form body with their shapes.
pub(crate) type FormProps = Vec<(String, Shape)>;

fn props_shape<'a>(props: &'a FormProps, name: &str) -> Option<&'a Shape> {
    props.iter().find(|(n, _)| n == name).map(|(_, s)| s)
}

pub(crate) fn insert_repeated(m: &mut Map<String, Value>, k: String, v: Value) {
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
    let mut used: Vec<String> = Vec::new();
    for (name, shape) in props {
        let enc = encodings.iter().find(|e| e.name == *name);
        let style = enc.map_or(Style::Form, |e| e.style);
        let explode = enc.is_none_or(|e| e.explode);
        if style == Style::DeepObject {
            let prefix = format!("{name}[");
            let mut obj = Map::new();
            for p in pairs.iter().filter(|p| p.name.starts_with(&prefix)) {
                let keys = bracket_keys(&p.name[name.len()..]);
                let v = decode(p.raw, true)
                    .map_err(|_| format!("{name} has an invalid percent-escape"))?;
                let t = match shape {
                    Shape::Object(nested) if keys.len() == 1 => nested
                        .iter()
                        .find(|(n, _)| Some(n) == keys.first())
                        .map_or(Types::ANY, |(_, t)| *t),
                    _ => Types::ANY,
                };
                insert_deep(&mut obj, &keys, coerce(&v, t));
            }
            if !obj.is_empty() {
                m.insert(name.clone(), Value::Object(obj));
            }
            continue;
        }
        // An exploded form object sends its own properties as top-level
        // pairs (OpenAPI 3.1.1 §4.8.12.4 style table).
        if let (Shape::Object(nested), Style::Form, true) = (shape, style, explode) {
            if !pairs.iter().any(|p| p.name == *name) {
                let mut obj = Map::new();
                for (n, t) in nested {
                    if let Some(p) = pairs.iter().find(|p| p.name == *n) {
                        let v = decode(p.raw, true)
                            .map_err(|_| format!("{n} has an invalid percent-escape"))?;
                        obj.insert(n.clone(), coerce(&v, *t));
                        used.push(n.clone());
                    }
                }
                if !obj.is_empty() {
                    m.insert(name.clone(), Value::Object(obj));
                }
                continue;
            }
        }
        let mine: Vec<_> = pairs.iter().filter(|p| p.name == *name).collect();
        if mine.is_empty() {
            continue;
        }
        used.push(name.clone());
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
            Shape::Object(nested) => {
                let first = texts.first().map_or("", String::as_str);
                match crate::json::parse(first.as_bytes(), crate::json::Limits { max_depth: depth })
                    .ok()
                    .filter(Value::is_object)
                {
                    Some(v) => v,
                    None => {
                        let d = match style {
                            Style::SpaceDelimited => ' ',
                            Style::PipeDelimited => '|',
                            _ => ',',
                        };
                        pairs_to_object(first.split(d).map(str::to_owned).collect(), nested)
                    }
                }
            }
        };
        m.insert(name.clone(), value);
    }
    // Undeclared fields are still properties of the form object, so
    // `additionalProperties` can judge them.
    for p in &pairs {
        if used.contains(&p.name) || props_shape(props, &p.name).is_some() {
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
pub(crate) struct Part<'a> {
    /// The `Content-Disposition` name; positional parts need none.
    pub name: Option<String>,
    pub content_type: Option<MediaType>,
    pub headers: Vec<(String, String)>,
    pub file: bool,
    pub data: &'a [u8],
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
pub(crate) fn parts<'a>(body: &'a [u8], boundary: &str) -> Result<Vec<Part<'a>>, String> {
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
        let mut headers = Vec::new();
        let mut file = false;
        for line in head.lines() {
            let Some((k, v)) = line.split_once(':') else {
                continue;
            };
            let k = k.trim();
            headers.push((k.to_owned(), v.trim().to_owned()));
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
        out.push(Part {
            name,
            content_type,
            headers,
            file,
            data,
        });
        pos = next;
    }
}

/// A form-data part, which always has a name.
struct NamedPart<'a> {
    name: String,
    part: Part<'a>,
}

impl<'a> std::ops::Deref for NamedPart<'a> {
    type Target = Part<'a>;
    fn deref(&self) -> &Part<'a> {
        &self.part
    }
}

/// The boundary of a multipart media type (RFC 2046 §5.1.1).
pub(crate) fn boundary(media: &MediaType) -> Result<&str, String> {
    media
        .param("boundary")
        .filter(|b| !b.is_empty() && b.len() <= 70)
        .ok_or_else(|| format!("{}/{} needs a boundary parameter", media.ty, media.sub))
}

/// Parses a multipart/form-data body into an object.
pub(crate) fn multipart(
    body: &[u8],
    media: &MediaType,
    props: &FormProps,
    encodings: &[Encoding],
    depth: usize,
    styled: bool,
) -> Result<(Value, Vec<PartInfo>), String> {
    let Some(boundary) = media
        .param("boundary")
        .filter(|b| !b.is_empty() && b.len() <= 70)
    else {
        return Err("multipart/form-data needs a boundary parameter".into());
    };
    let mut m = Map::new();
    let mut infos = Vec::new();
    for part in parts(body, boundary)? {
        let Some(name) = part.name.clone() else {
            return Err("a multipart part has no name in Content-Disposition".into());
        };
        let part = NamedPart { name, part };
        infos.push(PartInfo {
            name: part.name.clone(),
            content_type: part.content_type.clone(),
            headers: part.headers.clone(),
        });
        let declared_json = encodings
            .iter()
            .find(|e| e.name == part.name)
            .and_then(|e| e.content_type.as_ref())
            .is_some_and(MediaType::is_json);
        let shape = props_shape(props, &part.name);
        // Default part types (OpenAPI 3.0.4 §4.7.15.1): objects are JSON;
        // arrays take their items' type, so each item is its own part.
        let object = matches!(shape, Some(Shape::Object(_)));
        let is_json = part
            .content_type
            .as_ref()
            .map_or(declared_json || (object && !part.file), MediaType::is_json);
        // OpenAPI 3.1 §4.8.15.1: a part whose Encoding Object writes style,
        // explode or allowReserved is serialized as that style, and an
        // unexploded array or object is one delimited part.
        let enc = encodings.iter().find(|e| e.name == part.name);
        let delimited = styled
            && !is_json
            && enc.is_some_and(|e| e.rfc6570 && !e.explode)
            && matches!(shape, Some(Shape::Array(_) | Shape::Object(_)));
        let value = if delimited {
            let text = String::from_utf8_lossy(part.data).into_owned();
            let delim = match enc.map(|e| e.style) {
                Some(Style::SpaceDelimited) => ' ',
                Some(Style::PipeDelimited) => '|',
                _ => ',',
            };
            let pieces: Vec<&str> = if text.is_empty() {
                Vec::new()
            } else {
                text.split(delim).collect()
            };
            match shape {
                Some(Shape::Array(t)) => {
                    Value::Array(pieces.iter().map(|x| coerce(x, *t)).collect())
                }
                Some(Shape::Object(nested)) => {
                    let mut o = Map::new();
                    for kv in pieces.chunks(2) {
                        let t = nested
                            .iter()
                            .find(|(n, _)| n == kv[0])
                            .map_or(Types::ANY, |(_, t)| *t);
                        o.insert(
                            kv[0].to_owned(),
                            coerce(kv.get(1).copied().unwrap_or(""), t),
                        );
                    }
                    Value::Object(o)
                }
                _ => Value::String(text),
            }
        } else if is_json {
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
        let repeated_array = matches!(shape, Some(Shape::Array(_))) && !is_json && !delimited;
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
    Ok((Value::Object(m), infos))
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

/// The items of a sequential media type body (OpenAPI 3.2 §4.14.3.1).
pub(crate) fn sequence(
    body: &[u8],
    media: &MediaType,
    seq: crate::media::Sequence,
    max_depth: usize,
) -> Result<Vec<Value>, String> {
    use crate::media::Sequence;
    let limits = json::Limits { max_depth };
    let item = |i: usize, b: &[u8]| {
        json::parse(b, limits).map_err(|e| format!("item {i} is not valid JSON: {e}"))
    };
    match seq {
        // Blank lines are skipped (NDJSON allows ignoring them).
        Sequence::JsonLines => body
            .split(|&c| c == b'\n')
            .map(|l| l.strip_suffix(b"\r").unwrap_or(l))
            .filter(|l| !l.iter().all(|c| matches!(c, b' ' | b'\t')))
            .enumerate()
            .map(|(i, l)| item(i, l))
            .collect(),
        // RFC 7464 §2.1: each record follows RS; empty records are not
        // elements.
        Sequence::JsonSeq => body
            .split(|&c| c == 0x1E)
            .skip(1)
            .filter(|r| !r.iter().all(u8::is_ascii_whitespace))
            .enumerate()
            .map(|(i, r)| item(i, r))
            .collect(),
        Sequence::EventStream => {
            let t = text(body, media)?;
            Ok(events(&t))
        }
    }
}

/// Server-Sent Events parsed as the WHATWG event stream rules say
/// (HTML §9.2.6), one object per event: comments and unknown fields are
/// dropped, `data` lines are joined with LF, `retry` is an integer when it
/// is all digits (and ignored otherwise), and an `id` with NUL is ignored
/// (OpenAPI 3.2 §4.14.4). A block without `data` is no event. A final
/// event without a blank line is kept.
pub(crate) fn events(t: &str) -> Vec<Value> {
    let t = t.strip_prefix('\u{feff}').unwrap_or(t);
    let mut out = Vec::new();
    let mut cur = Map::new();
    let mut data: Option<String> = None;
    // HTML §9.2.6: a block with no `data` line dispatches no event.
    let mut flush = |cur: &mut Map<String, Value>, data: &mut Option<String>| match data.take() {
        Some(d) => {
            cur.insert("data".into(), Value::String(d));
            out.push(Value::Object(std::mem::take(cur)));
        }
        None => cur.clear(),
    };
    // Lines end with CRLF, LF or CR.
    let mut rest = t;
    while !rest.is_empty() {
        let end = rest.find(['\r', '\n']).unwrap_or(rest.len());
        let line = &rest[..end];
        rest = &rest[end..];
        if let Some(r) = rest.strip_prefix("\r\n") {
            rest = r;
        } else if !rest.is_empty() {
            rest = &rest[1..];
        }
        if line.is_empty() {
            flush(&mut cur, &mut data);
            continue;
        }
        if line.starts_with(':') {
            continue;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        match field {
            "data" => match &mut data {
                Some(d) => {
                    d.push('\n');
                    d.push_str(value);
                }
                None => data = Some(value.to_owned()),
            },
            "event" => {
                cur.insert("event".into(), Value::String(value.to_owned()));
            }
            "id" if !value.contains('\0') => {
                cur.insert("id".into(), Value::String(value.to_owned()));
            }
            "retry" if !value.is_empty() && value.bytes().all(|c| c.is_ascii_digit()) => {
                if let Ok(n) = value.parse::<serde_json::Number>() {
                    cur.insert("retry".into(), Value::Number(n));
                }
            }
            _ => {}
        }
    }
    flush(&mut cur, &mut data);
    out
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
pub(crate) fn xml(
    body: &[u8],
    media: &MediaType,
    max_depth: usize,
    shape: Option<&crate::xml::XmlShape>,
) -> Result<Option<Value>, String> {
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
    let d = roxmltree::Document::parse_with_options(&s, opts)
        .map_err(|e| format!("the body is not well-formed XML: {e}"))?;
    Ok(shape.map(|sh| crate::xml::to_json(&d.root_element(), sh, 0)))
}

/// Undoes `Content-Encoding` (RFC 9110 §8.4): codings are listed in the
/// order they were applied and undone in reverse. gzip, x-gzip, deflate
/// (zlib, or raw deflate as some senders use) and br are decoded; output
/// stops at `limit` bytes, so a compression bomb cannot exhaust memory.
pub(crate) fn decode_content(body: &[u8], codings: &str, limit: usize) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let list: Vec<String> = codings
        .split(',')
        .map(|c| c.trim().to_ascii_lowercase())
        .filter(|c| !c.is_empty() && c != "identity")
        .collect();
    let mut data = body.to_vec();
    for coding in list.iter().rev() {
        let mut out = Vec::new();
        let cap = limit as u64 + 1;
        let read = match coding.as_str() {
            "gzip" | "x-gzip" => flate2::read::MultiGzDecoder::new(&data[..])
                .take(cap)
                .read_to_end(&mut out),
            "deflate" => {
                let zlib = flate2::read::ZlibDecoder::new(&data[..])
                    .take(cap)
                    .read_to_end(&mut out);
                if zlib.is_err() {
                    out.clear();
                    flate2::read::DeflateDecoder::new(&data[..])
                        .take(cap)
                        .read_to_end(&mut out)
                } else {
                    zlib
                }
            }
            "br" => brotli_decompressor::Decompressor::new(&data[..], 4096)
                .take(cap)
                .read_to_end(&mut out),
            other => {
                return Err(format!(
                    "content coding {other:?} is not supported, so the body cannot be checked"
                ))
            }
        };
        read.map_err(|e| format!("the body is not valid {coding}: {e}"))?;
        if out.len() > limit {
            return Err(format!("the decoded body is larger than {limit} bytes"));
        }
        data = out;
    }
    Ok(data)
}

#[cfg(test)]
mod sequence_tests {
    use super::*;
    use serde_json::json;

    // OpenAPI 3.2.0 §4.14.6.3: the stream and its JSON Lines equivalent.
    #[test]
    fn oas32_s4_14_6_3_event_stream() {
        let t = "event: addString\ndata: This data is formatted\ndata: across two lines\nretry: 5\n\nevent: addInt64\ndata: 1234.5678\nunknownField: this is ignored\n\n: This is a comment\nevent: addJSON\ndata: {\"foo\": 42}\n";
        assert_eq!(
            events(t),
            vec![
                json!({"event": "addString", "data": "This data is formatted\nacross two lines", "retry": 5}),
                json!({"event": "addInt64", "data": "1234.5678"}),
                json!({"event": "addJSON", "data": "{\"foo\": 42}"}),
            ]
        );
    }

    #[test]
    fn event_stream_line_endings_and_ignored_values() {
        let t = "\u{feff}data\r\nretry: 1x\rid: a\0b\r\n\r\n:only a comment\n\nretry: 5\nid: 7\n\n";
        assert_eq!(events(t), vec![json!({"data": ""})]);
    }

    #[test]
    fn json_lines_and_sequences() {
        let jsonl = MediaType::parse("application/jsonl").unwrap();
        let v = sequence(
            b"{\"a\":1}\r\n\n  \n[2]\n",
            &jsonl,
            jsonl.sequence().unwrap(),
            64,
        )
        .unwrap();
        assert_eq!(v, vec![json!({"a": 1}), json!([2])]);
        let seq = MediaType::parse("application/geo+json-seq").unwrap();
        let v = sequence(
            b"\x1e{\"a\":1}\n\x1e\x1e3\n",
            &seq,
            seq.sequence().unwrap(),
            64,
        )
        .unwrap();
        assert_eq!(v, vec![json!({"a": 1}), json!(3)]);
        assert!(sequence(b"{}\nnope\n", &jsonl, jsonl.sequence().unwrap(), 64).is_err());
    }
}
