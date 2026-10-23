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
pub(crate) use translate::OAS30_KEYWORDS;

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

use jsonschema::{Draft, Keyword, PatternOptions, Registry, ValidationError, Validator};
use serde_json::{json, Map, Value};

use crate::doc::{LoadError, Version};
use crate::format;
use crate::number::{self, Decimal};
pub(crate) use translate::Direction;
pub(crate) use translate::DOC_URI;

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

/// What compiling a validator needs. Every validator it compiles checks a
/// [`SHARED_REF`] through it, so the target is compiled once per direction.
struct Compiler {
    registry: Registry<'static>,
    /// Some pattern needs the bounded backtracking engine.
    backtracking: bool,
    /// Validators of [`SHARED_REF`] targets by pointer: requests, responses.
    shared: [RwLock<HashMap<String, Arc<SharedSlot>>>; 2],
}

type SharedSlot = OnceLock<Result<Validator, String>>;

pub(crate) struct SchemaStore {
    compiler: Arc<Compiler>,
    request: DirStore,
    response: DirStore,
    /// Compiled patterns by translated text, shared by both directions.
    patterns: RwLock<HashMap<String, Arc<crate::pattern::Matcher>>>,
    /// Longest in-place applicator chain in the document.
    in_place_depth: usize,
    /// `(pointer, absolute URI)` of every schema resource with its own `$id`,
    /// innermost first when one holds another; built when first needed.
    ids: OnceLock<Vec<(String, String)>>,
}

pub(crate) struct StoreOptions {
    pub assert_formats: bool,
    pub discriminator_enforce: bool,
    pub lexical_integers: bool,
    pub relax_required: bool,
    pub allow_backtracking: bool,
    pub forbid_read_write: bool,
}

/// Every `$id` in the translated document with its pointer, resolved against
/// the enclosing base (JSON Schema Core 2020-12 §8.2.1); longest pointer
/// first, so the innermost resource is found first.
fn id_index(resource: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut stack: Vec<(&Value, String, String)> = vec![(resource, "#".into(), DOC_URI.into())];
    while let Some((v, ptr, base)) = stack.pop() {
        match v {
            Value::Object(m) => {
                let mut base = base;
                if let Some(id) = m
                    .get("$id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.starts_with('#'))
                {
                    if let Some(abs) = resolve_uri(&base, id) {
                        base = abs.split('#').next().unwrap_or(&abs).to_owned();
                        out.push((ptr.clone(), base.clone()));
                    }
                }
                for (k, c) in m {
                    if !matches!(
                        k.as_str(),
                        "enum" | "const" | "default" | "examples" | "example"
                    ) {
                        stack.push((c, crate::doc::join(&ptr, k), base.clone()));
                    }
                }
            }
            Value::Array(a) => {
                for (i, c) in a.iter().enumerate() {
                    stack.push((c, crate::doc::join(&ptr, &i.to_string()), base.clone()));
                }
            }
            _ => {}
        }
    }
    out.sort_by_key(|(p, _)| std::cmp::Reverse(p.len()));
    out
}

/// `reference` resolved against `base` (RFC 3986 §5.2).
pub(crate) fn resolve_uri(base: &str, reference: &str) -> Option<String> {
    let b = jsonschema::uri::from_str(base).ok()?;
    jsonschema::uri::resolve_against(&b.borrow(), reference)
        .ok()
        .map(|u| u.as_str().to_owned())
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

/// The keyword a translated OpenAPI 3.0 `pattern` is checked under: its
/// input is read as UTF-16 code units (see [`crate::pattern::utf16_view`]).
pub(crate) const PATTERN_UTF16: &str = "x-oav-pattern16";

struct PatternUtf16(Option<crate::pattern::Matcher>);

impl<'i> Keyword<'i> for PatternUtf16 {
    fn validate(&self, instance: &'i Value) -> Result<(), ValidationError<'i>> {
        if self.is_valid(instance) {
            Ok(())
        } else {
            Err(ValidationError::custom("must match the pattern"))
        }
    }

    /// A match that hits the step limit is invalid, never accepted.
    fn is_valid(&self, instance: &'i Value) -> bool {
        match (instance, &self.0) {
            (Value::String(s), Some(m)) => m.is_match(&crate::pattern::utf16_view(s)) == Some(true),
            _ => true,
        }
    }
}

/// The keyword a local `$ref` is rewritten to (see [`share_in`]): the
/// value is checked by the target's validator, compiled once and shared.
pub(crate) const SHARED_REF: &str = "x-oav-ref";

/// Holds only weak references, so dropping a `Spec` frees validators that
/// refer to one another.
struct SharedRef {
    compiler: std::sync::Weak<Compiler>,
    slot: std::sync::Weak<SharedSlot>,
    dir: Direction,
    assert_formats: bool,
    pointer: String,
}

impl SharedRef {
    /// Out of line: building a validator takes a large frame, which must
    /// not sit on the stack at every level of a deep value.
    #[cold]
    #[inline(never)]
    fn compile_into<'s>(
        &self,
        c: &Arc<Compiler>,
        slot: &'s SharedSlot,
    ) -> &'s Result<Validator, String> {
        slot.get_or_init(|| c.compile(self.dir, self.assert_formats, &self.pointer))
    }
}

impl<'i> Keyword<'i> for SharedRef {
    fn validate(&self, instance: &'i Value) -> Result<(), ValidationError<'i>> {
        if self.is_valid(instance) {
            Ok(())
        } else {
            Err(ValidationError::custom(
                "does not match the referenced schema",
            ))
        }
    }

    /// A reference reached again for the same value, with no value
    /// consumed in between, is an in-place cycle such as `{anyOf: [S]}`
    /// inside S; it holds, as `jsonschema` decides such cycles.
    fn is_valid(&self, instance: &'i Value) -> bool {
        let (Some(c), Some(slot)) = (self.compiler.upgrade(), self.slot.upgrade()) else {
            return false;
        };
        let key = (
            Arc::as_ptr(&slot) as usize,
            instance as *const Value as usize,
        );
        let known = CHECKS.with(|c| {
            let c = c.borrow();
            c.done
                .get(&key)
                .copied()
                .or(c.active.contains(&key).then_some(true))
        });
        if let Some(ok) = known {
            return ok;
        }
        CHECKS.with(|c| c.borrow_mut().active.push(key));
        let compiled = match slot.get() {
            Some(v) => v,
            None => self.compile_into(&c, &slot),
        };
        let ok = compiled.as_ref().is_ok_and(|v| v.is_valid(instance));
        CHECKS.with(|c| {
            let mut c = c.borrow_mut();
            c.active.pop();
            if c.active.is_empty() {
                c.done.clear();
            } else if instance.is_object() || instance.is_array() {
                c.done.insert(key, ok);
            }
        });
        ok
    }
}

/// Shared references being checked on this thread, and the outcomes found
/// so far within the outermost one, by (slot, value) address. Objects and
/// arrays are parts of the checked value, which does not move during a
/// check, so their addresses identify them until it ends; strings may be
/// temporaries (`propertyNames`), so they are not remembered. The memo keeps
/// recursive `anyOf` and `oneOf` linear, as `jsonschema` does for the
/// references it resolves itself.
#[derive(Default)]
struct Checks {
    active: Vec<(usize, usize)>,
    done: HashMap<(usize, usize), bool>,
}

thread_local! {
    static CHECKS: std::cell::RefCell<Checks> = std::cell::RefCell::new(Checks::default());
}

/// Rewrites `$ref`s to local pointers in a translated schema as
/// [`SHARED_REF`]. `jsonschema` inlines what a `$ref` reaches into each
/// validator, so a schema that many operations reach was compiled once per
/// operation; shared, it is compiled once.
///
/// Only references below a child value (`properties`, `items`, ...) are
/// shared: each shared step then checks a smaller value, so the stack grows
/// with the value's depth, as it does for `jsonschema`'s own references,
/// while in-place chains (`allOf`, `$ref` on the same value) stay inlined
/// within the limits the loader measures. A shared reference reports no
/// annotations, so one is not shared when a schema between it and the
/// child boundary has `unevaluatedProperties` or `unevaluatedItems`. The
/// translator does not call this for documents with `$id` or dynamic
/// references, whose scopes would cross validators.
///
/// `below`: a child boundary lies between the root schema and `v`. `clean`:
/// no schema since the last boundary has `unevaluated*`.
pub(crate) fn share_in(v: &mut Value, below: bool, clean: bool, depth: usize) {
    let Value::Object(m) = v else {
        return;
    };
    if depth > 512 {
        return;
    }
    let clean =
        clean && !m.contains_key("unevaluatedProperties") && !m.contains_key("unevaluatedItems");
    if below
        && clean
        && m.get("$ref")
            .and_then(Value::as_str)
            .is_some_and(|r| r.starts_with("#/"))
    {
        if let Some(r) = m.remove("$ref") {
            m.insert(SHARED_REF.into(), r);
        }
    }
    for (k, sub) in m.iter_mut() {
        // (below and clean for the subschemas, value is a map of schemas)
        let (below, clean, map) = match k.as_str() {
            "properties" | "patternProperties" => (true, true, true),
            // Definitions apply only where they are referenced.
            "$defs" | "definitions" => (false, true, true),
            "dependentSchemas" => (below, clean, true),
            "additionalProperties"
            | "items"
            | "prefixItems"
            | "contains"
            | "propertyNames"
            | "unevaluatedProperties"
            | "unevaluatedItems"
            | "additionalItems" => (true, true, false),
            "allOf" | "anyOf" | "oneOf" | "not" | "if" | "then" | "else" => (below, clean, false),
            _ => continue,
        };
        match sub {
            Value::Object(o) if map => {
                for x in o.values_mut() {
                    share_in(x, below, clean, depth + 1);
                }
            }
            Value::Array(a) => {
                for x in a {
                    share_in(x, below, clean, depth + 1);
                }
            }
            x => share_in(x, below, clean, depth + 1),
        }
    }
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

/// Property names that must not be present in one direction (readOnly in
/// requests, writeOnly in responses, with `ReadWritePresence::Refuse`).
struct DirForbid(Vec<String>);

impl<'i> Keyword<'i> for DirForbid {
    fn validate(&self, instance: &'i Value) -> Result<(), ValidationError<'i>> {
        match self.present(instance) {
            None => Ok(()),
            Some(n) => Err(ValidationError::custom(format!("{n} must not be sent"))),
        }
    }

    fn is_valid(&self, instance: &'i Value) -> bool {
        self.present(instance).is_none()
    }
}

impl DirForbid {
    fn present(&self, instance: &Value) -> Option<&str> {
        let o = instance.as_object()?;
        self.0
            .iter()
            .find(|n| o.contains_key(n.as_str()))
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

/// `multipleOf` with exact decimals (JSON Schema Validation 2020-12
/// §6.2.1): `0.30000000000000004` is not a multiple of `0.1`.
struct MultipleOf(Decimal);

impl<'i> Keyword<'i> for MultipleOf {
    fn validate(&self, instance: &'i Value) -> Result<(), ValidationError<'i>> {
        if self.is_valid(instance) {
            Ok(())
        } else {
            Err(ValidationError::custom(format!(
                "must be a multiple of {}",
                self.0.to_canonical()
            )))
        }
    }

    fn is_valid(&self, instance: &'i Value) -> bool {
        match instance {
            Value::Number(n) => {
                Decimal::parse(&n.to_string()).is_some_and(|d| d.is_multiple_of(&self.0))
            }
            _ => true,
        }
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
                Decimal::parse(&n.to_string()).is_some_and(|d| format::check_number(self.0, &d))
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

/// `(absolute $id, schema)` for every `$id` not nested in another one.
fn outermost_ids(resource: &Value) -> Vec<(String, Value)> {
    let Ok(base) = jsonschema::uri::from_str(DOC_URI) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut stack = vec![resource];
    while let Some(v) = stack.pop() {
        match v {
            Value::Object(m) => {
                if let Some(id) = m
                    .get("$id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.starts_with('#'))
                {
                    if let Ok(abs) = jsonschema::uri::resolve_against(&base.borrow(), id) {
                        let uri = abs.as_str().split('#').next().unwrap_or("").to_owned();
                        if !out.iter().any(|(u, _)| *u == uri) {
                            out.push((uri, v.clone()));
                        }
                        continue;
                    }
                }
                stack.extend(m.values());
            }
            Value::Array(a) => stack.extend(a),
            _ => {}
        }
    }
    out
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
            discriminator_enforce: opts.discriminator_enforce,
            lexical_integers: opts.lexical_integers,
            relax_required: opts.relax_required,
            allow_backtracking: opts.allow_backtracking,
            forbid_read_write: opts.forbid_read_write,
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
        // Schemas with their own `$id` sit under OpenAPI objects, which are
        // not schema keywords, so the registry would not find them while
        // indexing the document; each outermost one is added on its own.
        let embedded = outermost_ids(&t.resource);
        let resource = t.resource;
        let registry = Registry::new()
            .draft(Draft::Draft202012)
            .add(DOC_URI, resource)
            .and_then(|b| b.extend(embedded))
            .and_then(|b| b.prepare())
            .map_err(|e| LoadError::Invalid {
                location: "#".into(),
                message: format!("cannot index schemas: {e}"),
            })?;
        Ok((
            SchemaStore {
                compiler: Arc::new(Compiler {
                    registry,
                    backtracking: t.needs_backtracking,
                    shared: [RwLock::new(HashMap::new()), RwLock::new(HashMap::new())],
                }),
                request: DirStore::new(Direction::Request, request_roots, opts.assert_formats),
                response: DirStore::new(Direction::Response, response_roots, opts.assert_formats),
                patterns: RwLock::new(HashMap::new()),
                in_place_depth: m.depth,
                ids: OnceLock::new(),
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
    /// The innermost `$id` resource holding `ptr`: its pointer and URI.
    pub(crate) fn scope_of(&self, ptr: &str) -> Option<(&str, &str)> {
        self.id_list()
            .iter()
            .find(|(p, _)| {
                ptr == p
                    || ptr
                        .strip_prefix(p.as_str())
                        .is_some_and(|r| r.starts_with('/'))
            })
            .map(|(p, u)| (p.as_str(), u.as_str()))
    }

    /// The pointer of the resource whose `$id` is `uri`.
    pub(crate) fn id_pointer(&self, uri: &str) -> Option<&str> {
        self.id_list()
            .iter()
            .find(|(_, u)| u == uri)
            .map(|(p, _)| p.as_str())
    }

    fn id_list(&self) -> &[(String, String)] {
        self.ids
            .get_or_init(|| self.resource().map(id_index).unwrap_or_default())
    }

    pub(crate) fn resource(&self) -> Option<&Value> {
        self.compiler.resource()
    }

    fn compile(&self, dir: &DirStore, pointer: &str) -> Result<Validator, String> {
        self.compiler.compile(dir.dir, dir.assert_formats, pointer)
    }
}

impl Compiler {
    fn resource(&self) -> Option<&Value> {
        let base = jsonschema::uri::from_str(DOC_URI).ok()?;
        self.registry
            .resolver(base)
            .lookup("")
            .ok()
            .map(|r| r.contents())
    }

    /// The slot of the shared validator for `pointer`.
    fn slot(&self, dir: Direction, pointer: &str) -> Arc<SharedSlot> {
        let map = &self.shared[dir as usize];
        if let Some(s) = map.read().ok().and_then(|m| m.get(pointer).cloned()) {
            return s;
        }
        let mut m = map.write().unwrap_or_else(|e| e.into_inner());
        Arc::clone(m.entry(pointer.to_owned()).or_default())
    }

    fn compile(
        self: &Arc<Self>,
        dir: Direction,
        assert_formats: bool,
        pointer: &str,
    ) -> Result<Validator, String> {
        let key = dir_key(dir);
        let weak = Arc::downgrade(self);
        let mut opts = jsonschema::options()
            .with_draft(Draft::Draft202012)
            .with_registry(&self.registry)
            .offline()
            .should_validate_formats(assert_formats)
            .should_ignore_unknown_formats(true)
            .with_keyword(SHARED_REF, move |_: &Map<String, Value>, v: &Value, _| {
                let pointer = v
                    .as_str()
                    .and_then(crate::doc::canonical_pointer)
                    .unwrap_or_default();
                let slot = weak
                    .upgrade()
                    .map(|c| Arc::downgrade(&c.slot(dir, &pointer)))
                    .unwrap_or_default();
                Ok(Box::new(SharedRef {
                    compiler: weak.clone(),
                    slot,
                    dir,
                    assert_formats,
                    pointer,
                }) as Box<dyn for<'i> Keyword<'i>>)
            })
            .with_keyword(PATTERN_UTF16, |_: &Map<String, Value>, v: &Value, _| {
                let m = v.as_str().and_then(|t| {
                    crate::pattern::build_any(t, false)
                        .or_else(|_| crate::pattern::build_any(t, true))
                        .ok()
                });
                Ok(Box::new(PatternUtf16(m)) as Box<dyn for<'i> Keyword<'i>>)
            })
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
                "x-oav-multiple-of",
                |_: &Map<String, Value>, v: &Value, _| {
                    let d = v
                        .as_number()
                        .and_then(|n| Decimal::parse(&n.to_string()))
                        .unwrap_or_else(|| Decimal::parse("1").expect("a decimal"));
                    Ok(Box::new(MultipleOf(d)) as Box<dyn for<'i> Keyword<'i>>)
                },
            )
            .with_keyword(
                "x-oav-int-format",
                |_: &Map<String, Value>, v: &Value, _| {
                    let f = match v.as_str() {
                        Some("int32") => "int32",
                        Some("float") => "float",
                        Some("double") => "double",
                        _ => "int64",
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
            )
            .with_keyword(
                "x-oav-forbid",
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
                    Ok(Box::new(DirForbid(names)) as Box<dyn for<'i> Keyword<'i>>)
                },
            );
        // Linear-time matching unless the document has patterns that need
        // backtracking; those run with a step limit.
        opts = if self.backtracking {
            opts.with_pattern_options(
                PatternOptions::fancy_regex().backtrack_limit(crate::pattern::BACKTRACK_LIMIT),
            )
        } else {
            opts.with_pattern_options(PatternOptions::regex())
        };
        for name in format::ASSERTED {
            if !format::is_numeric_format(name) {
                opts = opts.with_format(*name, move |s: &str| format::check_str(name, s));
            }
        }
        let root = json!({ "$ref": self.root_uri(pointer) });
        opts.build(&root).map_err(|e| e.to_string())
    }

    /// The URI of the schema at `pointer`, relative to the nearest enclosing
    /// schema resource: a pointer that crosses an `$id` must continue from
    /// that resource's URI (JSON Schema 2020-12 §8.2.1, §9.2.1), or `#`
    /// references inside it would resolve against the document root.
    fn root_uri(&self, pointer: &str) -> String {
        let plain = || format!("{DOC_URI}#{}", fragment(pointer));
        let Some(mut cur) = self.resource() else {
            return plain();
        };
        let Ok(mut base) = jsonschema::uri::from_str(DOC_URI) else {
            return plain();
        };
        let mut rest: Vec<String> = Vec::new();
        let body = pointer.strip_prefix('#').unwrap_or(pointer);
        for raw in body.split('/').skip(1) {
            let token = raw.replace("~1", "/").replace("~0", "~");
            let next = match cur {
                Value::Object(m) => m.get(&token),
                Value::Array(a) => token.parse::<usize>().ok().and_then(|i| a.get(i)),
                _ => None,
            };
            let Some(next) = next else {
                return plain();
            };
            cur = next;
            rest.push(raw.to_owned());
            if let Some(id) = cur
                .get("$id")
                .and_then(Value::as_str)
                .filter(|id| !id.starts_with('#'))
            {
                if let Ok(b) = jsonschema::uri::resolve_against(&base.borrow(), id) {
                    base = b;
                    rest.clear();
                }
            }
        }
        let base_text = base.as_str().split('#').next().unwrap_or("").to_owned();
        if rest.is_empty() {
            return base_text;
        }
        format!("{base_text}#{}", fragment(&format!("#/{}", rest.join("/"))))
    }
}

impl SchemaStore {
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

    pub(crate) fn regex(&self, translated: &str) -> Option<Arc<crate::pattern::Matcher>> {
        if let Ok(map) = self.patterns.read() {
            if let Some(r) = map.get(translated) {
                return Some(Arc::clone(r));
            }
        }
        let m = crate::pattern::build_any(translated, false)
            .or_else(|_| crate::pattern::build_any(translated, true))
            .ok()?;
        let r = Arc::new(m);
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
