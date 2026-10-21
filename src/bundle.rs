//! Several documents loaded as one.
//!
//! The caller supplies every document with its URI; nothing is fetched. The
//! documents are merged into the entry document under a reserved key, and
//! each `$ref` is resolved against the URI of the document it appears in
//! (RFC 3986 §5.2) and rewritten to point inside the merged tree. Relative
//! `$id`s are made absolute the same way, so schema resources keep their
//! identity. A reference to a document that was not supplied is a load
//! error.

use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::doc::{escape_token, LoadError};

/// Where the extra documents live in the merged tree.
pub(crate) const DOCUMENTS_KEY: &str = "x-oav-documents";

/// Keys whose values are data, not OpenAPI objects or schemas; `$ref`
/// inside them is not a reference.
const DATA_KEYS: &[&str] = &["example", "examples", "enum", "const", "default"];

fn resolve(base: &str, reference: &str) -> Option<String> {
    let b = jsonschema::uri::from_str(base).ok()?;
    jsonschema::uri::resolve_against(&b.borrow(), reference)
        .ok()
        .map(|u| u.as_str().to_owned())
}

fn without_fragment(uri: &str) -> &str {
    uri.split('#').next().unwrap_or(uri)
}

/// A supplied document: its absolute URI, the pointer of its root in the
/// merged tree, and its `$anchor`s (name to pointer, relative to its root).
struct Doc {
    uri: String,
    root: String,
    anchors: HashMap<String, String>,
    /// `(pointer within the document, absolute $id)` for every `$id`.
    ids: Vec<(String, String)>,
}

/// Every `$id` of a document, with its pointer, resolved against the
/// enclosing base.
fn ids(v: &Value, uri: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut stack: Vec<(&Value, String, String)> = vec![(v, String::new(), uri.to_owned())];
    while let Some((v, ptr, base)) = stack.pop() {
        match v {
            Value::Object(m) => {
                let mut base = base;
                if let Some(id) = m
                    .get("$id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.starts_with('#'))
                {
                    if let Some(abs) = resolve(&base, id) {
                        base = without_fragment(&abs).to_owned();
                        out.push((ptr.clone(), base.clone()));
                    }
                }
                for (k, c) in m {
                    if !DATA_KEYS.contains(&k.as_str()) {
                        stack.push((c, format!("{ptr}/{}", escape_token(k)), base.clone()));
                    }
                }
            }
            Value::Array(a) => {
                for (i, c) in a.iter().enumerate() {
                    stack.push((c, format!("{ptr}/{i}"), base.clone()));
                }
            }
            _ => {}
        }
    }
    out
}

/// `$anchor`s of a document that belong to its own resource (not to an
/// embedded resource with a different `$id`).
fn anchors(v: &Value, uri: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut stack: Vec<(&Value, String)> = vec![(v, String::new())];
    while let Some((v, ptr)) = stack.pop() {
        match v {
            Value::Object(m) => {
                if !ptr.is_empty() {
                    let own_resource = m
                        .get("$id")
                        .and_then(Value::as_str)
                        .filter(|id| !id.starts_with('#'))
                        .is_none_or(|id| {
                            resolve(uri, id).as_deref().map(without_fragment) == Some(uri)
                        });
                    if !own_resource {
                        continue;
                    }
                }
                if let Some(a) = m.get("$anchor").and_then(Value::as_str) {
                    out.entry(a.to_owned()).or_insert_with(|| ptr.clone());
                }
                for (k, c) in m {
                    if !DATA_KEYS.contains(&k.as_str()) {
                        stack.push((c, format!("{ptr}/{}", escape_token(k))));
                    }
                }
            }
            Value::Array(a) => {
                for (i, c) in a.iter().enumerate() {
                    stack.push((c, format!("{ptr}/{i}")));
                }
            }
            _ => {}
        }
    }
    out
}

/// Merges `entry` (at `entry_uri`) with `others` into one document.
pub(crate) fn merge(
    entry_uri: &str,
    entry: Value,
    others: Vec<(String, Value)>,
) -> Result<Value, LoadError> {
    let entry_base = resolve("oav:///", entry_uri).unwrap_or_else(|| entry_uri.to_owned());
    let entry_uri = without_fragment(&entry_base).to_owned();
    let mut docs = vec![Doc {
        anchors: anchors(&entry, &entry_uri),
        ids: ids(&entry, &entry_uri),
        uri: entry_uri,
        root: "#".to_owned(),
    }];
    for (i, (uri, v)) in others.iter().enumerate() {
        let abs = resolve(&entry_base, uri).unwrap_or_else(|| uri.clone());
        let abs = without_fragment(&abs).to_owned();
        docs.push(Doc {
            anchors: anchors(v, &abs),
            ids: ids(v, &abs),
            uri: abs,
            root: format!("#/{}/{i}", escape_token(DOCUMENTS_KEY)),
        });
    }
    let mut entry = entry;
    rewrite(&mut entry, 0, &docs)?;
    let mut store = Map::new();
    for (i, (_, mut v)) in others.into_iter().enumerate() {
        rewrite(&mut v, i + 1, &docs)?;
        store.insert(i.to_string(), v);
    }
    if let Value::Object(m) = &mut entry {
        if !store.is_empty() {
            m.insert(DOCUMENTS_KEY.to_owned(), Value::Object(store));
        }
    }
    Ok(entry)
}

/// Rewrites references in document `this`.
fn rewrite(v: &mut Value, this: usize, docs: &[Doc]) -> Result<(), LoadError> {
    let doc_uri = docs[this].uri.clone();
    // Iterative: (value, base URI in force, inside an `$id` scope).
    let mut stack: Vec<(&mut Value, String, bool)> = vec![(v, doc_uri.clone(), false)];
    while let Some((v, base, scoped)) = stack.pop() {
        match v {
            Value::Object(m) => {
                let mut base = base;
                let mut scoped = scoped;
                if let Some(Value::String(id)) = m.get_mut("$id") {
                    if !id.starts_with('#') {
                        if let Some(abs) = resolve(&base, id) {
                            *id = abs.clone();
                            base = without_fragment(&abs).to_owned();
                            scoped = true;
                        }
                    }
                }
                if let Some(Value::String(r)) = m.get_mut("$ref") {
                    *r = target(r, &base, scoped, docs, &docs[this].root)?;
                }
                for (k, child) in m.iter_mut() {
                    if DATA_KEYS.contains(&k.as_str()) || k.starts_with("x-") {
                        continue;
                    }
                    stack.push((child, base.clone(), scoped));
                }
            }
            Value::Array(a) => stack.extend(a.iter_mut().map(|c| (c, base.clone(), scoped))),
            _ => {}
        }
    }
    Ok(())
}

/// The rewritten form of one reference.
fn target(
    r: &str,
    base: &str,
    scoped: bool,
    docs: &[Doc],
    _here: &str,
) -> Result<String, LoadError> {
    // A reference that cannot be resolved here is left as written; it only
    // matters if a schema in use reaches it, and the loader judges it then.
    let Some(abs) = resolve(base, r) else {
        return Ok(r.to_owned());
    };
    let (uri, fragment) = match abs.split_once('#') {
        Some((u, f)) => (u.to_owned(), f.to_owned()),
        None => (abs.clone(), String::new()),
    };
    // Inside an `$id` scope, a reference to the scope's own resource keeps
    // its fragment form: `#` there means the resource.
    if scoped && uri == base {
        return Ok(format!("#{fragment}"));
    }
    let Some(doc) = docs.iter().find(|d| d.uri == uri) else {
        // Not a supplied document: kept (an embedded `$id`, a metaschema)
        // for the loader to judge.
        return Ok(if !scoped && r.starts_with('#') {
            r.to_owned()
        } else {
            abs
        });
    };
    // Anchors in supplied documents become pointers.
    let fragment = if fragment.is_empty() || fragment.starts_with('/') {
        fragment
    } else {
        match doc.anchors.get(&fragment) {
            Some(p) => p.clone(),
            // A `$dynamicAnchor` or an anchor of an embedded resource: left
            // for the schema engine, which resolves it by URI.
            None => {
                return Ok(if !scoped && uri == base {
                    format!("#{fragment}")
                } else {
                    abs
                })
            }
        }
    };
    // A pointer that passes through a schema with its own `$id` continues
    // from that resource's URI, so references inside it resolve there.
    let decoded = crate::doc::canonical_pointer(&format!("#{fragment}"))
        .map(|p| p[1..].to_owned())
        .unwrap_or_else(|| fragment.clone());
    let through = doc
        .ids
        .iter()
        .filter(|(p, _)| decoded == *p || decoded.starts_with(&format!("{p}/")))
        .max_by_key(|(p, _)| p.len());
    if let Some((p, id)) = through {
        let rest = &decoded[p.len()..];
        return Ok(if rest.is_empty() {
            id.clone()
        } else {
            format!("{id}#{}", crate::schema::fragment(&format!("#{rest}")))
        });
    }
    let pointer = format!(
        "{}{fragment}",
        if doc.root == "#" {
            "#"
        } else {
            doc.root.as_str()
        }
    );
    // Inside a scope, `#` means the scope's resource, so the merged
    // document is named in full.
    Ok(if scoped {
        format!("{}{pointer}", crate::schema::DOC_URI)
    } else {
        pointer
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rfc3986_s5_2_relative_references_resolve_per_document() {
        let entry = json!({"paths": {"/p": {"$ref": "paths.yaml#/p"}},
                           "components": {"schemas": {"A": {"$ref": "schemas/a.json"}}}});
        let paths = json!({"p": {"get": {"responses": {"200": {"$ref": "#/r"}}}}, "r": {"description": "ok"}});
        let a = json!({"type": "object", "properties": {"b": {"$ref": "b.json#/defs/b"}}, "example": {"$ref": "not a ref"}});
        let b = json!({"defs": {"b": {"type": "string"}}});
        let merged = merge(
            "https://api.example.com/spec/openapi.yaml",
            entry,
            vec![
                ("paths.yaml".into(), paths),
                ("schemas/a.json".into(), a),
                ("https://api.example.com/spec/schemas/b.json".into(), b),
            ],
        )
        .unwrap();
        assert_eq!(merged["paths"]["/p"]["$ref"], "#/x-oav-documents/0/p");
        assert_eq!(
            merged["x-oav-documents"]["0"]["p"]["get"]["responses"]["200"]["$ref"],
            "#/x-oav-documents/0/r"
        );
        assert_eq!(
            merged["components"]["schemas"]["A"]["$ref"],
            "#/x-oav-documents/1"
        );
        assert_eq!(
            merged["x-oav-documents"]["1"]["properties"]["b"]["$ref"],
            "#/x-oav-documents/2/defs/b"
        );
        assert_eq!(
            merged["x-oav-documents"]["1"]["example"]["$ref"], "not a ref",
            "data is not rewritten"
        );
    }

    #[test]
    fn unsupplied_documents_stay_external() {
        let merged = merge(
            "https://x/o.yaml",
            json!({"a": {"$ref": "missing.yaml#/x"}}),
            vec![],
        )
        .unwrap();
        assert_eq!(merged["a"]["$ref"], "https://x/missing.yaml#/x");
    }
}
