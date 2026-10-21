//! XML bodies read through the XML Object (OpenAPI 3.1.1 §4.8.26, 3.2.0
//! §4.26).
//!
//! The schema says how the XML maps onto a JSON value. Each schema is a
//! node (3.2 `nodeType`): an element, an attribute, the text of its
//! parent element, or no node at all, in which case what it describes
//! sits directly in the parent. Before 3.2, `xml.attribute` makes an
//! attribute and `xml.wrapped` an array's wrapper element. A property is
//! named by `xml.name` or the property name; an array is a run of
//! repeated elements, inside a wrapper element when it is an element
//! itself, and `prefixItems` maps an element's child nodes in order;
//! `xml.namespace` must match the element's namespace when given. The
//! value built this way is then checked against the schema like a JSON
//! body. Elements the schema does not declare are kept, so
//! `additionalProperties` still applies.

use serde_json::{Map, Value};

use crate::doc::{self, Version};
use crate::params::{coerce, Types};

/// What node of the document a schema describes (OpenAPI 3.2 §4.26.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Node {
    Element,
    Attribute,
    /// Character data of the parent element (`text` and `cdata`).
    Text,
    /// No node: the schema's nodes are the parent's.
    None,
}

/// How one schema maps onto XML.
#[derive(Debug, Clone)]
pub(crate) struct XmlShape {
    pub name: Option<String>,
    pub namespace: Option<String>,
    pub node: Node,
    /// `null` is allowed: read from `xsi:nil`, or a missing attribute
    /// (OpenAPI 3.2 §4.26.5).
    pub nullable: bool,
    pub kind: Kind,
}

#[derive(Debug, Clone)]
pub(crate) enum Kind {
    Object(Vec<(String, XmlShape)>),
    Array {
        prefix: Vec<XmlShape>,
        items: Box<XmlShape>,
    },
    Primitive(Types),
    Any,
}

const MAX_DEPTH: usize = 24;

/// The XML Schema instance namespace, for `xsi:nil`.
const XSI: &str = "http://www.w3.org/2001/XMLSchema-instance";

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

/// The component name a `$ref` names, if it names one.
fn ref_name(s: &Value) -> Option<String> {
    s.get("$ref")
        .and_then(Value::as_str)
        .and_then(|r| r.rsplit('/').next())
        .map(|n| n.replace("~1", "/").replace("~0", "~"))
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
    let own_node = own_xml
        .and_then(|x| x.get("nodeType"))
        .and_then(Value::as_str)
        .filter(|_| version >= Version::V32);
    let is_ref = s.get("$ref").is_some() || s.get("$dynamicRef").is_some();
    // 3.2: a reference is no node of its own; what it names is the node,
    // named after the component (§4.26.2, §4.26.3).
    if version >= Version::V32 && is_ref && own_node.is_none() && depth <= MAX_DEPTH {
        let target = resolve(root, s);
        if !std::ptr::eq(target, s) {
            let n = ref_name(s);
            return shape(root, target, version, n.as_deref().or(name), depth + 1);
        }
    }
    let target = resolve(root, s);
    let xml = own_xml.or_else(|| target.get("xml"));
    let text = |k: &str| {
        xml.and_then(|x| x.get(k))
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    let flag = |k: &str| xml.and_then(|x| x.get(k)) == Some(&Value::Bool(true));
    let is_array = target.get("type").and_then(Value::as_str) == Some("array")
        || target.get("items").is_some()
        || target.get("prefixItems").is_some();
    let node_type = own_node.or_else(|| {
        xml.and_then(|x| x.get("nodeType"))
            .and_then(Value::as_str)
            .filter(|_| version >= Version::V32)
    });
    let node = match node_type {
        Some("attribute") => Node::Attribute,
        Some("text" | "cdata") => Node::Text,
        Some("none") => Node::None,
        Some(_) => Node::Element,
        None if flag("attribute") => Node::Attribute,
        None if is_array && !flag("wrapped") => Node::None,
        None => Node::Element,
    };
    let (types, object) = match crate::params::shape_of(root, target, version) {
        crate::params::Shape::Primitive(t) => (Some(t), t.has(Types::OBJ) && !t.has(Types::STR)),
        crate::params::Shape::Object(_) => (None, true),
        crate::params::Shape::Array(_) => (None, false),
    };
    let nullable = version >= Version::V32 && allows_null(target);
    let mut out = XmlShape {
        name: text("name")
            .or_else(|| name.map(str::to_owned))
            .or_else(|| ref_name(s)),
        namespace: text("namespace"),
        node,
        nullable,
        kind: Kind::Any,
    };
    if depth > MAX_DEPTH {
        return out;
    }
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
    out.kind = if is_array {
        let items = target.get("items").unwrap_or(&Value::Bool(true));
        // Items take the array's name unless they name themselves.
        let prefix = target
            .get("prefixItems")
            .and_then(Value::as_array)
            .filter(|_| version >= Version::V32)
            .map(|a| {
                a.iter()
                    .map(|p| shape(root, p, version, out.name.as_deref(), depth + 1))
                    .collect()
            })
            .unwrap_or_default();
        Kind::Array {
            prefix,
            items: Box::new(shape(root, items, version, out.name.as_deref(), depth + 1)),
        }
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

fn allows_null(s: &Value) -> bool {
    match s.get("type") {
        Some(Value::String(t)) => t == "null",
        Some(Value::Array(a)) => a.iter().any(|t| t == "null"),
        _ => false,
    }
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

/// The element's own character data, not its children's.
fn own_text(node: &roxmltree::Node<'_, '_>) -> String {
    node.children()
        .filter(|n| n.is_text())
        .filter_map(|n| n.text())
        .collect()
}

fn is_nil(node: &roxmltree::Node<'_, '_>) -> bool {
    node.attributes()
        .any(|a| a.name() == "nil" && a.namespace() == Some(XSI) && a.value().trim() == "true")
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

/// Whether the properties name the element `k`, counting those of
/// properties that are no node and so live in the same element.
fn declares(props: &[(String, XmlShape)], k: &str, depth: usize) -> bool {
    props.iter().any(|(p, s)| {
        *p == k
            || s.name.as_deref() == Some(k)
            || match (&s.node, &s.kind) {
                (Node::None, Kind::Object(inner)) if depth < MAX_DEPTH => {
                    declares(inner, k, depth + 1)
                }
                (Node::None, Kind::Array { items, .. }) => items.name.as_deref() == Some(k),
                _ => false,
            }
    })
}

fn primitive(t: &str, shape: &XmlShape) -> Value {
    match &shape.kind {
        Kind::Primitive(types) => coerce(t, *types),
        _ => Value::String(t.to_owned()),
    }
}

/// Converts an element along its mapping.
pub(crate) fn to_json(node: &roxmltree::Node<'_, '_>, shape: &XmlShape, depth: usize) -> Value {
    if depth > MAX_DEPTH {
        return untyped(node, depth);
    }
    if shape.nullable && is_nil(node) {
        return Value::Null;
    }
    match &shape.kind {
        Kind::Any => untyped(node, depth),
        Kind::Primitive(t) => coerce(&text(node), *t),
        Kind::Array { prefix, items } if !prefix.is_empty() => {
            // Child nodes in order: elements, and the text between them.
            let nodes: Vec<_> = node
                .children()
                .filter(|c| {
                    c.is_element() || (c.is_text() && !c.text().unwrap_or("").trim().is_empty())
                })
                .collect();
            let mut out = Vec::new();
            let mut it = nodes.iter().peekable();
            for p in prefix.iter().chain(std::iter::repeat(items.as_ref())) {
                let Some(c) = it.next() else {
                    break;
                };
                out.push(if p.node == Node::Text || c.is_text() {
                    primitive(c.text().unwrap_or("").trim(), p)
                } else {
                    to_json(c, p, depth + 1)
                });
            }
            Value::Array(out)
        }
        Kind::Array { items, .. } => {
            let name = items.name.clone().unwrap_or_default();
            Value::Array(
                node.children()
                    .filter(|c| {
                        c.is_element()
                            && (name.is_empty() || matches(c, &name, items.namespace.as_deref()))
                    })
                    .map(|c| to_json(&c, items, depth + 1))
                    .collect(),
            )
        }
        Kind::Object(props) => {
            let mut m = Map::new();
            let mut used: Vec<roxmltree::NodeId> = Vec::new();
            for (prop, ps) in props {
                let name = ps.name.clone().unwrap_or_else(|| prop.clone());
                match (ps.node, &ps.kind) {
                    (Node::Attribute, _) => {
                        let found = node.attributes().find(|a| {
                            a.name() == name
                                && ps
                                    .namespace
                                    .as_deref()
                                    .is_none_or(|ns| a.namespace() == Some(ns))
                        });
                        match found {
                            Some(v) => {
                                m.insert(prop.clone(), primitive(v.value(), ps));
                            }
                            // A missing attribute is null when null is allowed.
                            None if ps.nullable => {
                                m.insert(prop.clone(), Value::Null);
                            }
                            None => {}
                        }
                    }
                    (Node::Text, _) => {
                        m.insert(prop.clone(), primitive(&own_text(node), ps));
                    }
                    (Node::None, Kind::Array { items, .. }) => {
                        // Unwrapped: the items repeat directly in this element.
                        let item_name = items.name.clone().unwrap_or_else(|| name.clone());
                        let found: Vec<_> = node
                            .children()
                            .filter(|c| matches(c, &item_name, items.namespace.as_deref()))
                            .collect();
                        if !found.is_empty() {
                            used.extend(found.iter().map(|c| c.id()));
                            m.insert(
                                prop.clone(),
                                Value::Array(
                                    found.iter().map(|c| to_json(c, items, depth + 1)).collect(),
                                ),
                            );
                        }
                    }
                    (Node::None, _) => {
                        // No node: the property's own nodes are this element's.
                        m.insert(prop.clone(), to_json(node, ps, depth + 1));
                    }
                    (Node::Element, _) => {
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
                if declares(props, &k, 0) {
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

    fn convert32(components: Value, schema: Value, xml: &str) -> Value {
        let root = json!({"components": {"schemas": components}});
        let sh = shape(&root, &schema, Version::V32, None, 0);
        let d = roxmltree::Document::parse(xml).unwrap();
        to_json(&d.root_element(), &sh, 0)
    }

    // OpenAPI 3.2.0 §4.26.6.3: attribute by nodeType, namespaced element.
    #[test]
    fn oas32_s4_26_6_3_attribute_prefix_namespace() {
        let person = json!({"type": "object", "properties": {
            "id": {"type": "integer", "xml": {"nodeType": "attribute"}},
            "name": {"type": "string", "xml": {"namespace": "https://example.com/schema/sample", "prefix": "sample"}}}});
        let v = convert32(
            json!({"Person": person}),
            json!({"$ref": "#/components/schemas/Person"}),
            r#"<Person id="123"><sample:name xmlns:sample="https://example.com/schema/sample">example</sample:name></Person>"#,
        );
        assert_eq!(v, json!({"id": 123, "name": "example"}));
    }

    // §4.26.6.4: arrays are unwrapped by default and wrapped as elements.
    #[test]
    fn oas32_s4_26_6_4_arrays() {
        let doc = |animals: Value| json!({"type": "object", "xml": {"name": "document"}, "properties": {"animals": animals}});
        let unwrapped = convert32(
            json!({}),
            doc(
                json!({"type": "array", "xml": {"name": "aliens"}, "items": {"type": "string", "xml": {"name": "animal"}}}),
            ),
            "<document><animal>dog</animal><animal>cat</animal></document>",
        );
        assert_eq!(unwrapped, json!({"animals": ["dog", "cat"]}));
        let same_name = convert32(
            json!({}),
            doc(
                json!({"type": "array", "xml": {"nodeType": "element"}, "items": {"type": "string"}}),
            ),
            "<document><animals><animals>dog</animals><animals>cat</animals></animals></document>",
        );
        assert_eq!(same_name, json!({"animals": ["dog", "cat"]}));
        let renamed = convert32(
            json!({}),
            doc(
                json!({"type": "array", "xml": {"name": "aliens", "nodeType": "element"}, "items": {"type": "string", "xml": {"name": "animal"}}}),
            ),
            "<document><aliens><animal>dog</animal><animal>cat</animal></aliens></document>",
        );
        assert_eq!(renamed, json!({"animals": ["dog", "cat"]}));
    }

    // §4.26.6.5: elements with attributes and text (the example's closing
    // tags corrected).
    #[test]
    fn oas32_s4_26_6_5_attributes_and_text() {
        let schema = json!({"type": "array", "xml": {"nodeType": "element", "name": "animals"},
            "items": {"xml": {"name": "animal"}, "properties": {
                "kind": {"type": "string", "xml": {"nodeType": "attribute"}},
                "name": {"type": "string", "xml": {"nodeType": "text"}}}}});
        let v = convert32(
            json!({}),
            schema,
            r#"<animals><animal kind="Cat">Fluffy</animal><animal kind="Dog">Fido</animal></animals>"#,
        );
        assert_eq!(
            v,
            json!([{"kind": "Cat", "name": "Fluffy"}, {"kind": "Dog", "name": "Fido"}])
        );
    }

    // §4.26.6.6: a referenced element holding CDATA.
    #[test]
    fn oas32_s4_26_6_6_cdata() {
        let docs = json!({"type": "object", "properties": {
            "content": {"type": "string", "xml": {"nodeType": "cdata"}}}});
        let v = convert32(
            json!({"Documentation": docs}),
            json!({"$ref": "#/components/schemas/Documentation"}),
            "<Documentation><![CDATA[<html><body></body></html>]]></Documentation>",
        );
        assert_eq!(v, json!({"content": "<html><body></body></html>"}));
    }

    // §4.26.6.7: ordered elements and text, and xsi:nil (declared here; the
    // example leaves the prefix undeclared).
    #[test]
    fn oas32_s4_26_6_7_ordered() {
        let schema = json!({"xml": {"nodeType": "element", "name": "OneTwoThree"}, "type": "array",
            "prefixItems": [
                {"xml": {"name": "One"}, "type": "string"},
                {"xml": {"name": "Two"}, "type": "object", "properties": {
                    "unit": {"type": "string", "xml": {"nodeType": "attribute"}},
                    "value": {"type": "number", "xml": {"nodeType": "text"}}}},
                {"xml": {"name": "Three"}, "type": ["boolean", "null"]}]});
        let v = convert32(
            json!({}),
            schema,
            r#"<OneTwoThree xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><One>Some text</One><Two unit="cubits">42</Two><Three xsi:nil="true" /></OneTwoThree>"#,
        );
        assert_eq!(
            v,
            json!(["Some text", {"unit": "cubits", "value": 42}, null])
        );
        let report = json!({"xml": {"nodeType": "element", "name": "Report"}, "type": "array",
            "prefixItems": [
                {"xml": {"nodeType": "text"}, "type": "string"},
                {"xml": {"name": "data"}, "type": "number"},
                {"xml": {"nodeType": "text"}, "type": "string"}]});
        let v = convert32(
            json!({}),
            report,
            "<Report>Some preamble text.<data>42</data>Some postamble text.</Report>",
        );
        assert_eq!(
            v,
            json!(["Some preamble text.", 42, "Some postamble text."])
        );
    }

    // §4.26.5, §4.26.6.8: nulls from xsi:nil and missing attributes.
    #[test]
    fn oas32_s4_26_6_8_nulls() {
        let schema = json!({"xml": {"name": "product"}, "type": "object", "properties": {
            "count": {"type": ["number", "null"], "xml": {"nodeType": "attribute"}},
            "rating": {"type": "string", "xml": {"nodeType": "attribute"}},
            "description": {"type": "string"},
            "related": {"type": ["object", "null"]}}});
        let v = convert32(
            json!({}),
            schema.clone(),
            r#"<product xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><description>Thing</description><related xsi:nil="true" /></product>"#,
        );
        assert_eq!(
            v,
            json!({"count": null, "description": "Thing", "related": null})
        );
        let v = convert32(
            json!({}),
            schema,
            r#"<product count="42"><description>Thing</description><related></related></product>"#,
        );
        assert_eq!(
            v,
            json!({"count": 42, "description": "Thing", "related": {}})
        );
    }

    // §4.26.3: in 3.2 a referenced property's element is named after the
    // component; before 3.2, after the property.
    #[test]
    fn referenced_properties_are_named_by_version() {
        let root = json!({"components": {"schemas": {
            "Owner": {"type": "object", "properties": {"n": {"type": "string"}}},
            "Pet": {"type": "object", "properties": {"owner": {"$ref": "#/components/schemas/Owner"}}}}}});
        let s = json!({"$ref": "#/components/schemas/Pet"});
        let read = |version, xml: &str| {
            let sh = shape(&root, &s, version, None, 0);
            let d = roxmltree::Document::parse(xml).unwrap();
            to_json(&d.root_element(), &sh, 0)
        };
        assert_eq!(
            read(Version::V32, "<Pet><Owner><n>a</n></Owner></Pet>")["owner"],
            json!({"n": "a"})
        );
        assert_eq!(
            read(Version::V31, "<Pet><owner><n>a</n></owner></Pet>")["owner"],
            json!({"n": "a"})
        );
    }
}
