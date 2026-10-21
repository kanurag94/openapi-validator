//! The indexed form of operations: what validation needs, nothing more.

use std::collections::HashMap;

use serde_json::Value;

use crate::doc::{self, LoadError, Warning};
use crate::media::MediaType;
use crate::schema::{Direction, SchemaId};

/// One `content` entry (Media Type Object).
pub(crate) struct MediaEntry {
    pub key: String,
    pub schema: Option<SchemaId>,
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
    pub roots: Roots,
    pub warnings: Vec<Warning>,
}

impl<'a> Builder<'a> {
    pub fn new(root: &'a Value) -> Builder<'a> {
        Builder {
            root,
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
            let schema = media
                .get("schema")
                .map(|_| self.roots.id(dir, &doc::join(&mptr, "schema")));
            out.push((
                mt,
                MediaEntry {
                    key: key.clone(),
                    schema,
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
}
