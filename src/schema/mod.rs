//! Schema compilation and validation.
//!
//! The verdict for a value always comes from a `jsonschema` validator
//! compiled from the translated schema. Validators are compiled lazily, once
//! per schema root and direction, against one registry that holds the whole
//! translated document, so building a `Spec` never compiles a schema.
//! Requests and responses share the translation; they differ only in
//! `x-oav-required`, whose keyword is registered per direction.
//!
//! `jsonschema` reports errors by collecting every failing branch of every
//! `anyOf`, `oneOf` and `allOf`, which is exponential on recursive schemas.
//! So it is only asked "valid or not" (`is_valid`, which stays fast). When
//! the answer is no, [`explain`] walks the schema and the value together,
//! with memoisation and a step budget, to say where and why; it asks
//! `jsonschema` again for the branches of `anyOf`, `oneOf`, `not` and `if`.
//! The explanation never changes the verdict.

mod depth;
pub(crate) mod explain;
pub(crate) mod translate;

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

use jsonschema::{Draft, Keyword, PatternOptions, Registry, ValidationError, Validator};
use serde_json::{json, Map, Value};

use crate::doc::{LoadError, Version};
use crate::format;
use crate::number::{self, Decimal};
pub(crate) use translate::Direction;
use translate::DOC_URI;

/// Index of a compiled-on-demand schema root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct SchemaId {
    pub dir: Direction,
    pub index: usize,
}

struct Slot {
    pointer: String,
    compiled: OnceLock<Result<Validator, String>>,
}

/// Per-direction compiled validators.
pub(crate) struct DirStore {
    dir: Direction,
    slots: Vec<Slot>,
    /// Validators for sub-schemas, used only while explaining a failure.
    sub: RwLock<HashMap<String, Arc<Result<Validator, String>>>>,
    pub(crate) assert_formats: bool,
}

pub(crate) struct SchemaStore {
    registry: Registry<'static>,
    request: DirStore,
    response: DirStore,
    /// Compiled patterns by translated text, shared by both directions.
    patterns: RwLock<HashMap<String, Arc<regex::Regex>>>,
    /// Longest in-place applicator chain in the document.
    in_place_depth: usize,
}

pub(crate) struct StoreOptions {
    pub assert_formats: bool,
    pub discriminator_selects_branch: bool,
}

/// Percent-encodes a JSON Pointer for use as a URI fragment (RFC 6901 §6).
pub(crate) fn fragment(ptr: &str) -> String {
    let body = ptr.strip_prefix('#').unwrap_or(ptr);
    let mut out = String::with_capacity(body.len() + 8);
    for &b in body.as_bytes() {
        let keep = b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'-' | b'.'
                    | b'_'
                    | b'~'
                    | b'!'
                    | b'$'
                    | b'&'
                    | b'\''
                    | b'('
                    | b')'
                    | b'*'
                    | b'+'
                    | b','
                    | b';'
                    | b'='
                    | b':'
                    | b'@'
                    | b'/'
                    | b'?'
            );
        if keep {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

struct LexicalInteger;

impl<'i> Keyword<'i> for LexicalInteger {
    fn validate(&self, instance: &'i Value) -> Result<(), ValidationError<'i>> {
        if self.is_valid(instance) {
            Ok(())
        } else {
            Err(ValidationError::custom("must be an integer"))
        }
    }

    fn is_valid(&self, instance: &'i Value) -> bool {
        match instance {
            Value::Number(n) => number::is_lexical_integer(&n.to_string()),
            _ => true,
        }
    }
}

/// `required` names that apply in one direction only (readOnly and
/// writeOnly properties).
struct DirRequired(Vec<String>);

impl<'i> Keyword<'i> for DirRequired {
    fn validate(&self, instance: &'i Value) -> Result<(), ValidationError<'i>> {
        match self.missing(instance) {
            None => Ok(()),
            Some(n) => Err(ValidationError::custom(format!("{n} is required"))),
        }
    }

    fn is_valid(&self, instance: &'i Value) -> bool {
        self.missing(instance).is_none()
    }
}

impl DirRequired {
    fn missing(&self, instance: &Value) -> Option<&str> {
        let o = instance.as_object()?;
        self.0
            .iter()
            .find(|n| !o.contains_key(n.as_str()))
            .map(String::as_str)
    }
}

/// `enum` and `const` with JSON Schema equality (2020-12 §4.2.2): numbers
/// by value, objects regardless of member order.
struct Enum(Vec<Value>);

impl<'i> Keyword<'i> for Enum {
    fn validate(&self, instance: &'i Value) -> Result<(), ValidationError<'i>> {
        if self.is_valid(instance) {
            Ok(())
        } else {
            Err(ValidationError::custom("is not one of the allowed values"))
        }
    }

    fn is_valid(&self, instance: &'i Value) -> bool {
        self.0.iter().any(|v| explain::json_eq(v, instance))
    }
}

/// `uniqueItems: true` with the same equality, by canonical form.
struct UniqueItems;

impl<'i> Keyword<'i> for UniqueItems {
    fn validate(&self, instance: &'i Value) -> Result<(), ValidationError<'i>> {
        if self.is_valid(instance) {
            Ok(())
        } else {
            Err(ValidationError::custom("items must be unique"))
        }
    }

    fn is_valid(&self, instance: &'i Value) -> bool {
        let Some(items) = instance.as_array() else {
            return true;
        };
        let mut seen = std::collections::HashSet::with_capacity(items.len());
        items.iter().all(|x| {
            let mut key = String::new();
            explain::canonical(x, &mut key);
            seen.insert(key)
        })
    }
}

struct AlwaysValid;

impl<'i> Keyword<'i> for AlwaysValid {
    fn validate(&self, _: &'i Value) -> Result<(), ValidationError<'i>> {
        Ok(())
    }

    fn is_valid(&self, _: &'i Value) -> bool {
        true
    }
}

struct IntFormat(&'static str);

impl<'i> Keyword<'i> for IntFormat {
    fn validate(&self, instance: &'i Value) -> Result<(), ValidationError<'i>> {
        if self.is_valid(instance) {
            Ok(())
        } else {
            Err(ValidationError::custom(format!("must be an {}", self.0)))
        }
    }

    fn is_valid(&self, instance: &'i Value) -> bool {
        match instance {
            Value::Number(n) => {
                Decimal::parse(&n.to_string()).is_some_and(|d| format::check_int_range(self.0, &d))
            }
            _ => true,
        }
    }
}

impl DirStore {
    fn new(dir: Direction, pointers: Vec<String>, assert_formats: bool) -> DirStore {
        DirStore {
            dir,
            slots: pointers
                .into_iter()
                .map(|pointer| Slot {
                    pointer,
                    compiled: OnceLock::new(),
                })
                .collect(),
            sub: RwLock::new(HashMap::new()),
            assert_formats,
        }
    }

    pub(crate) fn direction(&self) -> Direction {
        self.dir
    }

    pub(crate) fn pointer(&self, index: usize) -> &str {
        self.slots.get(index).map_or("#", |s| s.pointer.as_str())
    }
}

/// The direction's name inside `x-oav-required`.
fn dir_key(d: Direction) -> &'static str {
    match d {
        Direction::Request => "request",
        Direction::Response => "response",
    }
}

/// Outcome of checking one value.
pub(crate) enum Outcome {
    Valid,
    /// `(instance path, message)` pairs, and how many more were found.
    Invalid(Vec<(String, String)>, usize),
    /// The schema could not be compiled.
    SchemaError(String),
}

impl SchemaStore {
    pub(crate) fn new(
        root: &Value,
        version: Version,
        request_roots: Vec<String>,
        response_roots: Vec<String>,
        opts: &StoreOptions,
    ) -> Result<(SchemaStore, Vec<crate::doc::Warning>), LoadError> {
        let topts = translate::Options {
            assert_formats: opts.assert_formats,
            discriminator_selects_branch: opts.discriminator_selects_branch,
        };
        let mut roots = request_roots.clone();
        roots.extend(response_roots.iter().cloned());
        let t = translate::translate(root, version, &roots, &topts)?;
        let starts: Vec<&Value> = roots
            .iter()
            .filter_map(|p| crate::doc::lookup(&t.resource, p))
            .collect();
        let m = depth::measure(&t.resource, &starts);
        if m.depth > depth::MAX_IN_PLACE {
            return Err(LoadError::Invalid {
                location: "#".into(),
                message: format!(
                    "schemas nest {} in-place applicators ($ref, allOf, anyOf, ...) deep; at most {} are allowed",
                    m.depth,
                    depth::MAX_IN_PLACE
                ),
            });
        }
        if m.work > depth::MAX_IN_PLACE_WORK {
            return Err(LoadError::Invalid {
                location: "#".into(),
                message: format!(
                    "schemas apply up to {} subschemas to one value through repeated references; at most {} are allowed",
                    m.work,
                    depth::MAX_IN_PLACE_WORK
                ),
            });
        }
        let registry = Registry::new()
            .draft(Draft::Draft202012)
            .add(DOC_URI, t.resource)
            .and_then(|b| b.prepare())
            .map_err(|e| LoadError::Invalid {
                location: "#".into(),
                message: format!("cannot index schemas: {e}"),
            })?;
        Ok((
            SchemaStore {
                registry,
                request: DirStore::new(Direction::Request, request_roots, opts.assert_formats),
                response: DirStore::new(Direction::Response, response_roots, opts.assert_formats),
                patterns: RwLock::new(HashMap::new()),
                in_place_depth: m.depth,
            },
            t.warnings,
        ))
    }

    /// The deepest value that can be checked within the frame budget, so
    /// that recursion stays bounded whatever the schemas' shape.
    pub(crate) fn depth_limit(&self, configured: usize) -> usize {
        configured
            .min(depth::FRAME_BUDGET / (self.in_place_depth + 3))
            .max(1)
    }

    pub(crate) fn dir(&self, d: Direction) -> &DirStore {
        match d {
            Direction::Request => &self.request,
            Direction::Response => &self.response,
        }
    }

    /// The translated document.
    pub(crate) fn resource(&self) -> Option<&Value> {
        let base = jsonschema::uri::from_str(DOC_URI).ok()?;
        self.registry
            .resolver(base)
            .lookup("")
            .ok()
            .map(|r| r.contents())
    }

    fn compile(&self, dir: &DirStore, pointer: &str) -> Result<Validator, String> {
        let key = dir_key(dir.dir);
        let mut opts = jsonschema::options()
            .with_draft(Draft::Draft202012)
            .with_registry(&self.registry)
            .offline()
            .with_pattern_options(PatternOptions::regex())
            .should_validate_formats(dir.assert_formats)
            .should_ignore_unknown_formats(true)
            .with_keyword("x-oav-int", |_: &Map<String, Value>, _: &Value, _| {
                Ok(Box::new(LexicalInteger) as Box<dyn for<'i> Keyword<'i>>)
            })
            .with_keyword("enum", |_: &Map<String, Value>, v: &Value, _| {
                let values = v.as_array().cloned().unwrap_or_default();
                Ok(Box::new(Enum(values)) as Box<dyn for<'i> Keyword<'i>>)
            })
            .with_keyword("const", |_: &Map<String, Value>, v: &Value, _| {
                Ok(Box::new(Enum(vec![v.clone()])) as Box<dyn for<'i> Keyword<'i>>)
            })
            .with_keyword("uniqueItems", |_: &Map<String, Value>, v: &Value, _| {
                let k: Box<dyn for<'i> Keyword<'i>> = if v == &Value::Bool(true) {
                    Box::new(UniqueItems)
                } else {
                    Box::new(AlwaysValid)
                };
                Ok(k)
            })
            .with_keyword(
                "x-oav-int-format",
                |_: &Map<String, Value>, v: &Value, _| {
                    let f = if v.as_str() == Some("int32") {
                        "int32"
                    } else {
                        "int64"
                    };
                    Ok(Box::new(IntFormat(f)) as Box<dyn for<'i> Keyword<'i>>)
                },
            )
            .with_keyword(
                "x-oav-required",
                move |_: &Map<String, Value>, v: &Value, _| {
                    let names = v
                        .get(key)
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(Value::as_str)
                                .map(str::to_owned)
                                .collect()
                        })
                        .unwrap_or_default();
                    Ok(Box::new(DirRequired(names)) as Box<dyn for<'i> Keyword<'i>>)
                },
            );
        for name in format::ASSERTED {
            if !format::is_numeric_format(name) {
                opts = opts.with_format(*name, move |s: &str| format::check_str(name, s));
            }
        }
        let root = json!({ "$ref": format!("{DOC_URI}#{}", fragment(pointer)) });
        opts.build(&root).map_err(|e| e.to_string())
    }

    fn root_validator<'d>(
        &self,
        dir: &'d DirStore,
        index: usize,
    ) -> Result<&'d Validator, &'d str> {
        let Some(slot) = dir.slots.get(index) else {
            return Err("unknown schema");
        };
        slot.compiled
            .get_or_init(|| self.compile(dir, &slot.pointer))
            .as_ref()
            .map_err(String::as_str)
    }

    /// A validator for any pointer, cached; used while explaining.
    pub(crate) fn sub_validator(
        &self,
        dir: &DirStore,
        pointer: &str,
    ) -> Arc<Result<Validator, String>> {
        if let Ok(map) = dir.sub.read() {
            if let Some(v) = map.get(pointer) {
                return Arc::clone(v);
            }
        }
        let v = Arc::new(self.compile(dir, pointer));
        if let Ok(mut map) = dir.sub.write() {
            map.insert(pointer.to_owned(), Arc::clone(&v));
        }
        v
    }

    pub(crate) fn regex(&self, translated: &str) -> Option<Arc<regex::Regex>> {
        if let Ok(map) = self.patterns.read() {
            if let Some(r) = map.get(translated) {
                return Some(Arc::clone(r));
            }
        }
        let r = Arc::new(crate::pattern::build(translated).ok()?);
        if let Ok(mut map) = self.patterns.write() {
            map.insert(translated.to_owned(), Arc::clone(&r));
        }
        Some(r)
    }

    /// Fast verdict only.
    pub(crate) fn is_valid(&self, id: SchemaId, instance: &Value) -> Result<bool, String> {
        self.root_validator(self.dir(id.dir), id.index)
            .map(|v| v.is_valid(instance))
            .map_err(str::to_owned)
    }

    /// Verdict and, when invalid, an explanation bounded by `max`.
    pub(crate) fn check(&self, id: SchemaId, instance: &Value, max: usize) -> Outcome {
        match self.is_valid(id, instance) {
            Ok(true) => Outcome::Valid,
            Ok(false) => {
                let dir = self.dir(id.dir);
                let (found, more) =
                    explain::explain(self, dir, dir.pointer(id.index), instance, max);
                Outcome::Invalid(found, more)
            }
            Err(e) => Outcome::SchemaError(e),
        }
    }
}
