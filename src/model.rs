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
    /// Shape of the whole value, for text bodies.
    pub shape: Shape,
    /// Declared properties, for form and multipart bodies.
    pub props: FormProps,
    pub encodings: Vec<Encoding>,
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
    ) -> Vec<(MediaType, MediaEntry)> {
        let mut out = Vec::new();
        let Some(map) = content.and_then(Value::as_object) else {
            return out;
        };
        for (key, media) in map {
            let mptr = doc::join(ptr, key);
            let Some(mt) = MediaType::parse(key) else {
                self.warn(&mptr, "not a media type or media range; ignored");
                continue;
            };
            let schema_v = media.get("schema");
            let schema = schema_v.map(|_| self.roots.id(dir, &doc::join(&mptr, "schema")));
            let shape = schema_v.map_or(Shape::Primitive(crate::params::Types::ANY), |s| {
                shape_of(self.root, s, self.version)
            });
            let props = schema_v.map(|s| self.form_props(s)).unwrap_or_default();
            let encodings = self.encodings(media.get("encoding"), &doc::join(&mptr, "encoding"));
            out.push((
                mt,
                MediaEntry {
                    key: key.clone(),
                    schema,
                    shape,
                    props,
                    encodings,
                },
            ));
        }
        out
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
        );
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
            let style = e.get("style").and_then(Value::as_str).map(|s| {
                parse_style(s).unwrap_or_else(|| {
                    self.warn(
                        &doc::join(ptr, name),
                        format!("unknown style {s:?}; using form"),
                    );
                    Style::Form
                })
            });
            let style = style.unwrap_or(Style::Form);
            let explode = match e.get("explode") {
                Some(Value::Bool(b)) => *b,
                _ => style == Style::Form,
            };
            // `contentType` may list several types; the first decides parsing.
            let content_type = e
                .get("contentType")
                .and_then(Value::as_str)
                .and_then(|c| c.split(',').next())
                .and_then(|c| MediaType::parse(c.trim()));
            out.push(Encoding {
                name: name.clone(),
                content_type,
                style,
                explode,
            });
        }
        out
    }
}
