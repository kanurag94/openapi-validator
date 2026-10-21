//! Document parsing, JSON Pointers and Reference Objects.

use std::fmt;

use serde_json::Value;

use crate::pattern::PatternError;

/// The OpenAPI version family of a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Version {
    V30,
    V31,
}

/// Why a document was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LoadError {
    /// The bytes are not valid JSON or YAML.
    Parse(String),
    /// The `openapi` field is missing or not 3.0.x / 3.1.x.
    UnsupportedVersion(String),
    /// A `$ref` points outside the document. Validation never reads files or
    /// the network.
    ExternalRef { reference: String, location: String },
    /// A `$ref` points at nothing.
    BrokenRef { reference: String, location: String },
    /// Reference Objects refer to each other in a loop, or chain too deep.
    RefCycle { location: String },
    /// A `pattern` cannot be run in linear time, or is not a valid ECMA-262
    /// pattern.
    Pattern {
        location: String,
        error: PatternError,
    },
    /// Two path templates differ only in parameter names
    /// (OpenAPI 3.1.1 §4.8.8.2).
    DuplicateTemplate { first: String, second: String },
    /// A security requirement names a scheme not in
    /// `components.securitySchemes` (OpenAPI 3.1.1 §4.8.30.1).
    UnknownSecurityScheme { name: String, location: String },
    /// Any other structural problem.
    Invalid { location: String, message: String },
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Parse(m) => write!(f, "cannot parse document: {m}"),
            LoadError::UnsupportedVersion(v) => {
                write!(f, "unsupported OpenAPI version {v:?}; 3.0.x and 3.1.x are supported")
            }
            LoadError::ExternalRef {
                reference,
                location,
            } => write!(
                f,
                "{location}: external reference {reference:?} is not allowed; bundle the document first"
            ),
            LoadError::BrokenRef {
                reference,
                location,
            } => write!(f, "{location}: reference {reference:?} points at nothing"),
            LoadError::RefCycle { location } => {
                write!(f, "{location}: references form a loop or chain too deep")
            }
            LoadError::Pattern { location, error } => write!(f, "{location}: {error}"),
            LoadError::DuplicateTemplate { first, second } => write!(
                f,
                "path templates {first:?} and {second:?} differ only in parameter names"
            ),
            LoadError::UnknownSecurityScheme { name, location } => {
                write!(f, "{location}: security scheme {name:?} is not defined")
            }
            LoadError::Invalid { location, message } => write!(f, "{location}: {message}"),
        }
    }
}

impl std::error::Error for LoadError {}

/// Something the loader accepted but that may not mean what the author
/// intended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Warning {
    pub location: String,
    pub message: String,
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.location, self.message)
    }
}

/// Maximum nesting of the document itself.
pub const MAX_DOC_DEPTH: usize = 128;
/// Maximum length of a chain of Reference Objects.
pub const MAX_REF_CHAIN: usize = 32;

/// Parses JSON (when the first character is `{`) or YAML 1.2.
pub fn parse(bytes: &[u8], yaml_merge_keys: bool) -> Result<Value, LoadError> {
    let trimmed = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let first = trimmed.iter().find(|c| !c.is_ascii_whitespace());
    if first == Some(&b'{') {
        crate::json::parse(
            bytes,
            crate::json::Limits {
                max_depth: MAX_DOC_DEPTH,
            },
        )
        .map_err(|e| LoadError::Parse(e.to_string()))
    } else {
        crate::yaml::parse(
            bytes,
            crate::yaml::Limits {
                max_depth: MAX_DOC_DEPTH,
                merge_keys: yaml_merge_keys,
                ..crate::yaml::Limits::default()
            },
        )
        .map_err(|e| LoadError::Parse(e.to_string()))
    }
}

/// Reads the `openapi` field.
pub fn version(root: &Value) -> Result<Version, LoadError> {
    let Some(obj) = root.as_object() else {
        return Err(LoadError::Invalid {
            location: "#".into(),
            message: "the document is not an object".into(),
        });
    };
    if obj.contains_key("swagger") {
        return Err(LoadError::UnsupportedVersion(
            obj.get("swagger").map(text).unwrap_or_default(),
        ));
    }
    let v = obj.get("openapi").map(text).unwrap_or_default();
    let mut parts = v.split('.');
    match (parts.next(), parts.next()) {
        (Some("3"), Some("0")) => Ok(Version::V30),
        (Some("3"), Some("1")) => Ok(Version::V31),
        _ => Err(LoadError::UnsupportedVersion(v)),
    }
}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Escapes one JSON Pointer reference token (RFC 6901 §3).
pub fn escape_token(t: &str) -> String {
    t.replace('~', "~0").replace('/', "~1")
}

/// Appends a token to a pointer that starts with `#`.
pub fn join(ptr: &str, token: &str) -> String {
    let mut s = String::with_capacity(ptr.len() + token.len() + 1);
    s.push_str(ptr);
    s.push('/');
    s.push_str(&escape_token(token));
    s
}

fn percent_decode_fragment(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let h = b.get(i + 1..i + 3)?;
            let h = std::str::from_utf8(h).ok()?;
            out.push(u8::from_str_radix(h, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Looks up a plain JSON Pointer (RFC 6901 §5) written after `#`, as built
/// by [`join`]. It is not percent-decoded; use [`lookup_ref`] for `$ref`
/// values, which are URI fragments.
pub fn lookup<'a>(root: &'a Value, ptr: &str) -> Option<&'a Value> {
    let ptr = ptr.strip_prefix('#')?;
    if ptr.is_empty() {
        return Some(root);
    }
    let ptr = ptr.strip_prefix('/')?;
    let mut cur = root;
    for token in ptr.split('/') {
        let token = token.replace("~1", "/").replace("~0", "~");
        cur = match cur {
            Value::Object(m) => m.get(&token)?,
            Value::Array(a) => {
                if token.starts_with('+') || (token.len() > 1 && token.starts_with('0')) {
                    return None;
                }
                a.get(token.parse::<usize>().ok()?)?
            }
            _ => return None,
        };
    }
    Some(cur)
}

/// Looks up a `$ref` value that is a `#` fragment (RFC 6901 §6): the
/// fragment is percent-decoded once, then read as a JSON Pointer.
pub fn lookup_ref<'a>(root: &'a Value, reference: &str) -> Option<&'a Value> {
    lookup(root, &canonical_pointer(reference)?)
}

/// Canonical `#`-pointer text (percent-decoded) for a reference, so that
/// pointers built by [`join`] and pointers written in `$ref` compare equal.
pub fn canonical_pointer(fragment: &str) -> Option<String> {
    let ptr = fragment.strip_prefix('#')?;
    let ptr = percent_decode_fragment(ptr)?;
    Some(format!("#{ptr}"))
}

/// Follows Reference Objects (`{"$ref": ...}`) for a non-schema object until
/// a real object is reached. Returns the object and its pointer.
pub fn deref<'a>(
    root: &'a Value,
    value: &'a Value,
    location: &str,
) -> Result<(&'a Value, String), LoadError> {
    let mut cur = value;
    let mut ptr = location.to_owned();
    for _ in 0..MAX_REF_CHAIN {
        let Some(r) = cur.get("$ref") else {
            return Ok((cur, ptr));
        };
        let Some(r) = r.as_str() else {
            return Err(LoadError::Invalid {
                location: ptr,
                message: "$ref must be a string".into(),
            });
        };
        if !r.starts_with('#') {
            return Err(LoadError::ExternalRef {
                reference: r.to_owned(),
                location: ptr,
            });
        }
        let Some(next) = lookup_ref(root, r) else {
            return Err(LoadError::BrokenRef {
                reference: r.to_owned(),
                location: ptr,
            });
        };
        cur = next;
        ptr = canonical_pointer(r).unwrap_or_else(|| r.to_owned());
    }
    Err(LoadError::RefCycle { location: ptr })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rfc6901_pointers() {
        let doc = json!({"a/b": {"m~n": [10, 20]}, "": 1, "p q": 2});
        assert_eq!(lookup(&doc, "#/a~1b/m~0n/1"), Some(&json!(20)));
        assert_eq!(lookup(&doc, "#/"), Some(&json!(1)));
        assert_eq!(lookup_ref(&doc, "#/p%20q"), Some(&json!(2)));
        assert_eq!(
            lookup(&doc, "#/p%20q"),
            None,
            "plain pointers are not decoded"
        );
        assert_eq!(lookup(&doc, "#"), Some(&doc));
        assert_eq!(lookup(&doc, "#/a~1b/m~0n/01"), None);
        assert_eq!(lookup(&doc, "#/missing"), None);
        assert_eq!(join("#/paths", "/pets/{id}"), "#/paths/~1pets~1{id}");
    }

    #[test]
    fn versions() {
        assert_eq!(version(&json!({"openapi": "3.0.3"})), Ok(Version::V30));
        assert_eq!(version(&json!({"openapi": "3.1.0"})), Ok(Version::V31));
        assert!(version(&json!({"swagger": "2.0"})).is_err());
        assert!(version(&json!({"openapi": "3.2.0"})).is_err());
        assert!(version(&json!({})).is_err());
    }

    #[test]
    fn reference_objects() {
        let doc = json!({
            "components": {"parameters": {
                "a": {"$ref": "#/components/parameters/b"},
                "b": {"name": "x", "in": "query"},
                "loop": {"$ref": "#/components/parameters/loop"},
                "ext": {"$ref": "other.yaml#/x"},
                "gone": {"$ref": "#/nowhere"}
            }}
        });
        let p = |n: &str| lookup(&doc, &format!("#/components/parameters/{n}")).unwrap();
        let (v, ptr) = deref(&doc, p("a"), "#/components/parameters/a").unwrap();
        assert_eq!(v["name"], "x");
        assert_eq!(ptr, "#/components/parameters/b");
        assert!(matches!(
            deref(&doc, p("loop"), "l"),
            Err(LoadError::RefCycle { .. })
        ));
        assert!(matches!(
            deref(&doc, p("ext"), "e"),
            Err(LoadError::ExternalRef { .. })
        ));
        assert!(matches!(
            deref(&doc, p("gone"), "g"),
            Err(LoadError::BrokenRef { .. })
        ));
    }
}
