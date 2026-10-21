//! XML bodies read through the XML Object (OpenAPI 3.1.1 §4.8.26).
//!
//! The schema says how the XML maps onto a JSON value: a property is a
//! child element named by `xml.name` or the property name, or an attribute
//! when `xml.attribute` is true; an array is a run of repeated elements, or
//! with `xml.wrapped` a wrapper element holding them; `xml.namespace` must
//! match the element's namespace when given. The value built this way is
//! then checked against the schema like a JSON body. Elements the schema
//! does not declare are kept, so `additionalProperties` still applies.

use serde_json::{Map, Value};

use crate::doc::{self, Version};
use crate::params::{coerce, Types};

/// How one schema maps onto XML.
#[derive(Debug, Clone)]
pub(crate) struct XmlShape {
    pub name: Option<String>,
    pub namespace: Option<String>,
    pub attribute: bool,
    pub wrapped: bool,
    pub kind: Kind,
}

#[derive(Debug, Clone)]
pub(crate) enum Kind {
    Object(Vec<(String, XmlShape)>),
    Array(Box<XmlShape>),
    Primitive(Types),
    Any,
}

const MAX_DEPTH: usize = 24;

fn resolve<'a>(root: &'a Value, mut s: &'a Value) -> &'a Value {
    for _ in 0..doc::MAX_REF_CHAIN {
        match s
            .get("$ref")
            .and_then(Value::as_str)
            .and_then(|r| doc::lookup_ref(root, r))
        {
            Some(t) => s = t,
            None => break,
        }
    }
    s
}

/// Builds the mapping for a schema. `name` is the name the schema has
/// where it is used (a property name, or a component name for a root).
pub(crate) fn shape(
    root: &Value,
    s: &Value,
    version: Version,
    name: Option<&str>,
    depth: usize,
) -> XmlShape {
    let own_xml = s.get("xml");
    let target = resolve(root, s);
    // A component reached through `$ref` is named after the component.
    let ref_name = s
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|r| r.rsplit('/').next())
        .map(|n| n.replace("~1", "/").replace("~0", "~"));
    let xml = own_xml.or_else(|| target.get("xml"));
    let text = |k: &str| {
        xml.and_then(|x| x.get(k))
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    let flag = |k: &str| xml.and_then(|x| x.get(k)) == Some(&Value::Bool(true));
    let mut out = XmlShape {
        name: text("name")
            .or_else(|| name.map(str::to_owned))
            .or(ref_name),
        namespace: text("namespace"),
        attribute: flag("attribute"),
        wrapped: flag("wrapped"),
        kind: Kind::Any,
    };
    if depth > MAX_DEPTH {
        return out;
    }
    let (types, object) = match crate::params::shape_of(root, target, version) {
        crate::params::Shape::Primitive(t) => (Some(t), t.has(Types::OBJ) && !t.has(Types::STR)),
        crate::params::Shape::Object(_) => (None, true),
        crate::params::Shape::Array(_) => (None, false),
    };
    let mut props: Vec<(String, XmlShape)> = Vec::new();
    let mut stack = vec![target];
    let mut guard = 0;
    while let Some(sch) = stack.pop() {
        guard += 1;
        if guard > 64 {
            break;
        }
        let sch = resolve(root, sch);
        for (p, ps) in sch
            .get("properties")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            if !props.iter().any(|(n, _)| n == p) {
                props.push((p.clone(), shape(root, ps, version, Some(p), depth + 1)));
            }
        }
        for key in ["allOf", "anyOf", "oneOf"] {
            stack.extend(sch.get(key).and_then(Value::as_array).into_iter().flatten());
        }
    }
    let is_array = target.get("type").and_then(Value::as_str) == Some("array")
        || target.get("items").is_some();
    out.kind = if is_array {
        let items = target.get("items").unwrap_or(&Value::Bool(true));
        Kind::Array(Box::new(shape(
            root,
            items,
            version,
            out.name.as_deref(),
            depth + 1,
        )))
    } else if !props.is_empty() || object {
        // An object, declared properties or not: its children are kept.
        Kind::Object(props)
    } else if let Some(t) = types.filter(|t| *t != Types::ANY) {
        Kind::Primitive(t)
    } else {
        Kind::Any
    };
    out
}

fn matches(node: &roxmltree::Node<'_, '_>, name: &str, namespace: Option<&str>) -> bool {
    node.is_element()
        && node.tag_name().name() == name
        && namespace.is_none_or(|ns| node.tag_name().namespace() == Some(ns))
}

fn text(node: &roxmltree::Node<'_, '_>) -> String {
    node.descendants()
        .filter(|n| n.is_text())
        .filter_map(|n| n.text())
        .collect()
}

/// An element without a schema: text, or an object of its children.
fn untyped(node: &roxmltree::Node<'_, '_>, depth: usize) -> Value {
    let children: Vec<_> = node.children().filter(|c| c.is_element()).collect();
    if children.is_empty() || depth > MAX_DEPTH {
        return Value::String(text(node));
    }
    let mut m = Map::new();
    for c in children {
        let v = untyped(&c, depth + 1);
        let k = c.tag_name().name().to_owned();
        match m.get_mut(&k) {
            Some(Value::Array(a)) => a.push(v),
            Some(prev) => {
                let first = std::mem::take(prev);
                *prev = Value::Array(vec![first, v]);
            }
            None => {
                m.insert(k, v);
            }
        }
    }
    Value::Object(m)
}

/// Converts an element along its mapping.
pub(crate) fn to_json(node: &roxmltree::Node<'_, '_>, shape: &XmlShape, depth: usize) -> Value {
    if depth > MAX_DEPTH {
        return untyped(node, depth);
    }
    match &shape.kind {
        Kind::Any => untyped(node, depth),
        Kind::Primitive(t) => coerce(&text(node), *t),
        Kind::Array(item) => {
            let name = item.name.clone().unwrap_or_default();
            Value::Array(
                node.children()
                    .filter(|c| {
                        c.is_element()
                            && (name.is_empty() || matches(c, &name, item.namespace.as_deref()))
                    })
                    .map(|c| to_json(&c, item, depth + 1))
                    .collect(),
            )
        }
        Kind::Object(props) => {
            let mut m = Map::new();
            let mut used: Vec<roxmltree::NodeId> = Vec::new();
            for (prop, ps) in props {
                let name = ps.name.clone().unwrap_or_else(|| prop.clone());
                if ps.attribute {
                    if let Some(v) = node.attributes().find(|a| {
                        a.name() == name
                            && ps
                                .namespace
                                .as_deref()
                                .is_none_or(|ns| a.namespace() == Some(ns))
                    }) {
                        let t = match &ps.kind {
                            Kind::Primitive(t) => *t,
                            _ => Types::ANY,
                        };
                        m.insert(prop.clone(), coerce(v.value(), t));
                    }
                    continue;
                }
                match &ps.kind {
                    Kind::Array(item) if !ps.wrapped => {
                        // Unwrapped: the items repeat directly in this element.
                        let item_name = item.name.clone().unwrap_or_else(|| name.clone());
                        let items: Vec<_> = node
                            .children()
                            .filter(|c| matches(c, &item_name, item.namespace.as_deref()))
                            .collect();
                        if !items.is_empty() {
                            used.extend(items.iter().map(|c| c.id()));
                            m.insert(
                                prop.clone(),
                                Value::Array(
                                    items.iter().map(|c| to_json(c, item, depth + 1)).collect(),
                                ),
                            );
                        }
                    }
                    _ => {
                        if let Some(c) = node
                            .children()
                            .find(|c| matches(c, &name, ps.namespace.as_deref()))
                        {
                            used.push(c.id());
                            m.insert(prop.clone(), to_json(&c, ps, depth + 1));
                        }
                    }
                }
            }
            // Undeclared children stay, so additionalProperties can judge them.
            for c in node
                .children()
                .filter(|c| c.is_element() && !used.contains(&c.id()))
            {
                let k = c.tag_name().name().to_owned();
                if props
                    .iter()
                    .any(|(p, s)| *p == k || s.name.as_deref() == Some(&k))
                {
                    continue;
                }
                m.entry(k).or_insert_with(|| untyped(&c, depth + 1));
            }
            Value::Object(m)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn convert(schema: Value, xml: &str) -> Value {
        let root = json!({"components": {"schemas": {"Pet": schema}}});
        let sh = shape(
            &root,
            &json!({"$ref": "#/components/schemas/Pet"}),
            Version::V31,
            None,
            0,
        );
        let d = roxmltree::Document::parse(xml).unwrap();
        to_json(&d.root_element(), &sh, 0)
    }

    #[test]
    fn oas311_s4_8_26_elements_attributes_and_arrays() {
        let schema = json!({
            "type": "object",
            "properties": {
                "id": {"type": "integer", "xml": {"attribute": true}},
                "name": {"type": "string", "xml": {"name": "fullName"}},
                "tags": {"type": "array", "items": {"type": "string", "xml": {"name": "tag"}}},
                "photos": {"type": "array", "xml": {"wrapped": true}, "items": {"type": "string", "xml": {"name": "url"}}}
            }
        });
        let v = convert(
            schema,
            r#"<Pet id="7"><fullName>Rex</fullName><tag>a</tag><tag>b</tag><photos><url>x</url><url>y</url></photos><extra>1</extra></Pet>"#,
        );
        assert_eq!(
            v,
            json!({"id": 7, "name": "Rex", "tags": ["a", "b"], "photos": ["x", "y"], "extra": "1"})
        );
    }

    #[test]
    fn namespaces_must_match_when_declared() {
        let schema = json!({"type": "object", "properties": {
            "a": {"type": "string", "xml": {"namespace": "urn:x"}}}});
        let v = convert(
            schema.clone(),
            r#"<Pet xmlns:p="urn:x"><p:a>yes</p:a></Pet>"#,
        );
        assert_eq!(v["a"], "yes");
        // A different namespace is a different element: the property is absent.
        let v = convert(schema, r#"<Pet xmlns:p="urn:y"><p:a>no</p:a></Pet>"#);
        assert!(v.get("a").is_none(), "{v}");
    }
}
