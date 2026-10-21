//! The indexed form of operations: what validation needs, nothing more.

use std::collections::HashMap;

use serde_json::Value;

use crate::body::{Encoding, FormProps};
use crate::doc::{self, LoadError, Version, Warning};
use crate::media::MediaType;
use crate::params::{parse_style, shape_of, Shape, Style};
use crate::schema::{Direction, SchemaId};

/// One `content` entry (Media Type Object).
pub(crate) struct MediaEntry {
    pub key: String,
    pub schema: Option<SchemaId>,
    /// `itemSchema` (OpenAPI 3.2 §4.14.1): each item of a sequential body.
    pub item_schema: Option<SchemaId>,
    /// Shape of the whole value, for text bodies.
    pub shape: Shape,
    /// Declared properties, for form and multipart bodies.
    pub props: FormProps,
    pub encodings: Vec<Encoding>,
    /// `prefixEncoding` and `itemEncoding` (OpenAPI 3.2 §4.14.5.2).
    pub prefix_encoding: Vec<Encoding>,
    pub item_encoding: Option<Box<Encoding>>,
    /// How each position of a positional multipart body is read, from the
    /// schema's `prefixItems` and then `items` (or `itemSchema`).
    pub parts: PartShapes,
    /// XML mapping of the schema, for XML media types.
    pub xml: Option<crate::xml::XmlShape>,
}

pub(crate) struct Body {
    pub required: bool,
    pub content: Vec<(MediaType, MediaEntry)>,
}

/// Collects schema roots and hands out their ids.
pub(crate) struct Roots {
    pub request: Vec<String>,
    pub response: Vec<String>,
    index: HashMap<(Direction, String), usize>,
}

impl Roots {
    pub fn new() -> Roots {
        Roots {
            request: Vec::new(),
            response: Vec::new(),
            index: HashMap::new(),
        }
    }

    pub fn id(&mut self, dir: Direction, ptr: &str) -> SchemaId {
        let key = (dir, ptr.to_owned());
        if let Some(&index) = self.index.get(&key) {
            return SchemaId { dir, index };
        }
        let list = match dir {
            Direction::Request => &mut self.request,
            Direction::Response => &mut self.response,
        };
        let index = list.len();
        list.push(ptr.to_owned());
        self.index.insert(key, index);
        SchemaId { dir, index }
    }
}

pub(crate) struct Builder<'a> {
    pub root: &'a Value,
    pub version: Version,
    pub roots: Roots,
    pub warnings: Vec<Warning>,
}

impl<'a> Builder<'a> {
    pub fn new(root: &'a Value, version: Version) -> Builder<'a> {
        Builder {
            root,
            version,
            roots: Roots::new(),
            warnings: Vec::new(),
        }
    }

    pub fn warn(&mut self, location: &str, message: impl Into<String>) {
        self.warnings.push(Warning {
            location: location.to_owned(),
            message: message.into(),
        });
    }

    /// A `content` map (OpenAPI 3.1.1 §4.8.14).
    pub fn content(
        &mut self,
        content: Option<&Value>,
        ptr: &str,
        dir: Direction,
    ) -> Result<Vec<(MediaType, MediaEntry)>, LoadError> {
        let mut out = Vec::new();
        let Some(map) = content.and_then(Value::as_object) else {
            return Ok(out);
        };
        for (key, media) in map {
            let mptr = doc::join(ptr, key);
            let Some(mt) = MediaType::parse(key) else {
                self.warn(&mptr, "not a media type or media range; ignored");
                continue;
            };
            let (media, mptr) = self.media(media, &mptr)?;
            let schema_v = media.get("schema");
            let schema = schema_v.map(|_| self.roots.id(dir, &doc::join(&mptr, "schema")));
            let item_schema = media
                .get("itemSchema")
                .filter(|_| self.version >= Version::V32)
                .map(|_| self.roots.id(dir, &doc::join(&mptr, "itemSchema")));
            // Shapes and encodings matter only for bodies parsed from text.
            let textual = mt.is_form() || mt.is_multipart_form() || mt.ty == "text" || mt.ty == "*";
            let shape = match schema_v {
                Some(s) if textual => shape_of(self.root, s, self.version),
                _ => Shape::Primitive(crate::params::Types::ANY),
            };
            let props = match schema_v {
                Some(s) if mt.is_form() || mt.is_multipart_form() || mt.ty == "*" => {
                    self.form_props(s)
                }
                _ => Vec::new(),
            };
            let encodings = self.encodings(media.get("encoding"), &doc::join(&mptr, "encoding"));
            let (prefix_encoding, item_encoding) = self.positional(media, &mptr, 0);
            let parts = if mt.ty == "multipart" && self.version >= Version::V32 {
                let rest = media.get("itemSchema");
                part_shapes(self.root, media.get("schema"), rest, self.version, 0)
            } else {
                PartShapes::default()
            };
            let xml = match schema_v {
                Some(s) if mt.is_xml() => {
                    Some(crate::xml::shape(self.root, s, self.version, None, 0))
                }
                _ => None,
            };
            out.push((
                mt,
                MediaEntry {
                    key: key.clone(),
                    schema,
                    item_schema,
                    shape,
                    props,
                    encodings,
                    prefix_encoding,
                    item_encoding,
                    parts,
                    xml,
                },
            ));
        }
        Ok(out)
    }

    /// A Media Type Object, which OpenAPI 3.2 lets be a Reference Object
    /// (§4.7 `mediaTypes`).
    pub fn media(&self, media: &'a Value, ptr: &str) -> Result<(&'a Value, String), LoadError> {
        if self.version >= Version::V32 {
            doc::deref(self.root, media, ptr)
        } else {
            Ok((media, ptr.to_owned()))
        }
    }

    /// A Request Body Object, following Reference Objects.
    pub fn request_body(
        &mut self,
        v: Option<&'a Value>,
        ptr: &str,
    ) -> Result<Option<Body>, LoadError> {
        let Some(v) = v else {
            return Ok(None);
        };
        let (body, bptr) = doc::deref(self.root, v, ptr)?;
        let content = self.content(
            body.get("content"),
            &doc::join(&bptr, "content"),
            Direction::Request,
        )?;
        Ok(Some(Body {
            required: body.get("required") == Some(&Value::Bool(true)),
            content,
        }))
    }

    /// Declared properties of an object schema, each with its shape,
    /// following `$ref` and `allOf`.
    fn form_props(&self, s: &Value) -> FormProps {
        let mut out: FormProps = Vec::new();
        let mut stack = vec![(s, 0usize)];
        while let Some((s, depth)) = stack.pop() {
            if depth > 16 {
                continue;
            }
            let s = crate::params::resolve(self.root, s);
            if let Some(props) = s.get("properties").and_then(Value::as_object) {
                for (name, p) in props {
                    if !out.iter().any(|(n, _)| n == name) {
                        out.push((name.clone(), shape_of(self.root, p, self.version)));
                    }
                }
            }
            for key in ["allOf", "anyOf", "oneOf"] {
                for m in s.get(key).and_then(Value::as_array).into_iter().flatten() {
                    stack.push((m, depth + 1));
                }
            }
        }
        out
    }

    /// Encoding Objects of a Media Type Object (§4.8.15).
    fn encodings(&mut self, v: Option<&Value>, ptr: &str) -> Vec<Encoding> {
        let mut out = Vec::new();
        for (name, e) in v.and_then(Value::as_object).into_iter().flatten() {
            out.push(self.encoding(name, e, &doc::join(ptr, name), 0));
        }
        out
    }

    /// One Encoding Object. In OpenAPI 3.2 it may nest `prefixEncoding`
    /// and `itemEncoding` for a nested multipart part (§4.15.2).
    fn encoding(&mut self, name: &str, e: &Value, eptr: &str, depth: usize) -> Encoding {
        let style = e.get("style").and_then(Value::as_str).map(|s| {
            parse_style(s).unwrap_or_else(|| {
                self.warn(eptr, format!("unknown style {s:?}; using form"));
                Style::Form
            })
        });
        let style = style.unwrap_or(Style::Form);
        let explode = match e.get("explode") {
            Some(Value::Bool(b)) => *b,
            _ => style == Style::Form,
        };
        // `contentType` may list several types and ranges; the first
        // decides parsing, any of them may be sent.
        let allowed: Vec<MediaType> = e
            .get("contentType")
            .and_then(Value::as_str)
            .map(|c| {
                c.split(',')
                    .filter_map(|t| MediaType::parse(t.trim()))
                    .collect()
            })
            .unwrap_or_default();
        let content_type = allowed.first().cloned();
        let mut headers = Vec::new();
        if let Some(hs) = e.get("headers").and_then(Value::as_object) {
            let hptr = doc::join(eptr, "headers");
            for (h, hv) in hs {
                // §4.8.15.1: a Content-Type header here SHALL be ignored.
                if h.eq_ignore_ascii_case("content-type") {
                    continue;
                }
                if let Ok(Some(p)) = self.param(
                    hv,
                    &doc::join(&hptr, h),
                    Some(h),
                    Direction::Request,
                    self.version,
                ) {
                    headers.push(p);
                }
            }
        }
        let (prefix, item) = if depth < MAX_ENCODING_NESTING {
            self.positional(e, eptr, depth + 1)
        } else {
            (Vec::new(), None)
        };
        Encoding {
            name: name.to_owned(),
            content_type,
            allowed,
            headers,
            style,
            explode,
            prefix,
            item,
        }
    }

    /// `prefixEncoding` and `itemEncoding` of a Media Type or Encoding
    /// Object (OpenAPI 3.2 §4.14.5.2).
    pub fn positional(
        &mut self,
        v: &Value,
        ptr: &str,
        depth: usize,
    ) -> (Vec<Encoding>, Option<Box<Encoding>>) {
        if self.version < Version::V32 {
            return (Vec::new(), None);
        }
        let pptr = doc::join(ptr, "prefixEncoding");
        let prefix = v
            .get("prefixEncoding")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(i, e)| self.encoding("", e, &doc::join(&pptr, &i.to_string()), depth))
            .collect();
        let item = v
            .get("itemEncoding")
            .map(|e| Box::new(self.encoding("", e, &doc::join(ptr, "itemEncoding"), depth)));
        (prefix, item)
    }
}

/// Encoding Objects nest at most this deep (§4.15.2 requires one level).
const MAX_ENCODING_NESTING: usize = 4;

/// A Response Object (§4.8.17).
pub(crate) struct Resp {
    pub headers: Vec<crate::params::Param>,
    pub content: Vec<(MediaType, MediaEntry)>,
}

/// Responses Object (§4.8.16): exact codes, `1XX`–`5XX` ranges, `default`.
#[derive(Default)]
pub(crate) struct Responses {
    pub exact: Vec<(u16, Resp)>,
    pub ranges: Vec<(u16, Resp)>,
    pub default: Option<Resp>,
}

impl Responses {
    /// The response for a status: exact code, then range, then `default`.
    pub fn find(&self, status: u16) -> Option<&Resp> {
        self.exact
            .iter()
            .find(|(c, _)| *c == status)
            .or_else(|| self.ranges.iter().find(|(c, _)| *c == status / 100))
            .map(|(_, r)| r)
            .or(self.default.as_ref())
    }
}

impl Builder<'_> {
    fn response(&mut self, v: &Value, ptr: &str) -> Result<Resp, LoadError> {
        let (r, rptr) = doc::deref(self.root, v, ptr)?;
        let mut headers = Vec::new();
        if let Some(map) = r.get("headers").and_then(Value::as_object) {
            let hptr = doc::join(&rptr, "headers");
            for (name, h) in map {
                // §4.8.17.1: a header named Content-Type is ignored.
                if name.eq_ignore_ascii_case("content-type") {
                    continue;
                }
                if let Some(p) = self.param(
                    h,
                    &doc::join(&hptr, name),
                    Some(name),
                    Direction::Response,
                    self.version,
                )? {
                    headers.push(p);
                }
            }
        }
        let content = self.content(
            r.get("content"),
            &doc::join(&rptr, "content"),
            Direction::Response,
        )?;
        Ok(Resp { headers, content })
    }

    pub(crate) fn responses(
        &mut self,
        v: Option<&Value>,
        ptr: &str,
    ) -> Result<Responses, LoadError> {
        let mut out = Responses::default();
        let Some(map) = v.and_then(Value::as_object) else {
            return Ok(out);
        };
        for (key, r) in map {
            if key.starts_with("x-") {
                continue;
            }
            let rp = doc::join(ptr, key);
            let resp = self.response(r, &rp)?;
            let k = key.as_bytes();
            if key == "default" {
                out.default = Some(resp);
            } else if k.len() == 3
                && (b'1'..=b'5').contains(&k[0])
                && k[1..].eq_ignore_ascii_case(b"xx")
            {
                out.ranges.push((u16::from(k[0] - b'0'), resp));
            } else if let Ok(code) = key.parse::<u16>() {
                out.exact.push((code, resp));
            } else {
                self.warn(&rp, "not a status code, range or default; ignored");
            }
        }
        Ok(out)
    }
}

/// The default part media type for a schema (OpenAPI 3.2 §4.15.1 table):
/// objects and arrays are JSON, strings and other primitives text, a
/// string with `contentEncoding` or a schema without `type` octets.
fn default_part_type(root: &Value, s: Option<&Value>) -> MediaType {
    let s = s.map(|s| {
        s.get("$ref")
            .and_then(Value::as_str)
            .and_then(|r| doc::lookup_ref(root, r))
            .unwrap_or(s)
    });
    let ty = s.and_then(|s| s.get("type"));
    let has = |t: &str| match ty {
        Some(Value::String(x)) => x == t,
        Some(Value::Array(a)) => a.iter().any(|x| x == t),
        _ => false,
    };
    let media = if has("object") || has("array") {
        "application/json"
    } else if has("string") && s.is_some_and(|s| s.get("contentEncoding").is_some()) {
        "application/octet-stream"
    } else if has("string") || has("number") || has("integer") || has("boolean") {
        "text/plain"
    } else {
        "application/octet-stream"
    };
    MediaType::parse(media).expect("a valid media type")
}

/// How the parts of a positional multipart body are read.
#[derive(Default)]
pub(crate) struct PartShapes {
    pub prefix: Vec<PartShape>,
    pub rest: Option<Box<PartShape>>,
}

/// One position: its default media type, the shape of text values, the
/// XML mapping, and the positions of a nested multipart part.
pub(crate) struct PartShape {
    pub default: MediaType,
    pub shape: Shape,
    pub xml: crate::xml::XmlShape,
    pub nested: PartShapes,
}

impl PartShapes {
    pub fn at(&self, i: usize) -> Option<&PartShape> {
        self.prefix.get(i).or(self.rest.as_deref())
    }
}

fn part_shape(root: &Value, s: Option<&Value>, version: Version, depth: usize) -> PartShape {
    let any = Value::Bool(true);
    let schema = s.unwrap_or(&any);
    PartShape {
        default: default_part_type(root, s),
        shape: shape_of(root, schema, version),
        xml: crate::xml::shape(root, schema, version, None, 0),
        nested: if depth < MAX_ENCODING_NESTING {
            part_shapes(root, Some(schema), None, version, depth + 1)
        } else {
            PartShapes::default()
        },
    }
}

/// Positions from an array schema's `prefixItems` and `items`; `rest`
/// (an `itemSchema`) takes the place of `items`.
fn part_shapes(
    root: &Value,
    schema: Option<&Value>,
    rest: Option<&Value>,
    version: Version,
    depth: usize,
) -> PartShapes {
    let schema = schema.map(|s| {
        s.get("$ref")
            .and_then(Value::as_str)
            .and_then(|r| doc::lookup_ref(root, r))
            .unwrap_or(s)
    });
    let prefix = schema
        .and_then(|s| s.get("prefixItems"))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|p| part_shape(root, Some(p), version, depth))
                .collect()
        })
        .unwrap_or_default();
    let rest = rest.or_else(|| schema.and_then(|s| s.get("items")));
    PartShapes {
        prefix,
        rest: Some(Box::new(part_shape(root, rest, version, depth))),
    }
}
