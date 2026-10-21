//! Structural checks of the document itself.
//!
//! Each finding is a MUST or SHALL of the specification that the document
//! breaks. They are load warnings by default (a flawed description should
//! not refuse every call), and load errors with `Options::strict_document`.

use std::collections::HashMap;

use serde_json::Value;

use crate::doc::{self, Version, Warning};

/// Styles each parameter location allows (OpenAPI 3.1.1 §4.8.12.3).
fn style_allowed(location: &str, style: &str) -> bool {
    matches!(
        (location, style),
        ("path", "matrix" | "label" | "simple")
            | (
                "query",
                "form" | "spaceDelimited" | "pipeDelimited" | "deepObject"
            )
            | ("header", "simple")
            | ("cookie", "form")
    )
}

struct Lint<'a> {
    root: &'a Value,
    version: Version,
    out: Vec<Warning>,
}

impl<'a> Lint<'a> {
    fn warn(&mut self, location: &str, message: impl Into<String>) {
        self.out.push(Warning {
            location: location.to_owned(),
            message: message.into(),
        });
    }

    fn deref(&self, v: &'a Value, ptr: &str) -> Option<(&'a Value, String)> {
        doc::deref(self.root, v, ptr).ok()
    }

    fn parameter(&mut self, v: &'a Value, ptr: &str, header_object: bool) {
        let Some((p, ptr)) = self.deref(v, ptr) else {
            return;
        };
        if header_object {
            for k in ["name", "in"] {
                if p.get(k).is_some() {
                    self.warn(
                        &ptr,
                        format!("a Header Object MUST NOT have `{k}` (§4.8.21)"),
                    );
                }
            }
            for k in ["allowEmptyValue", "allowReserved"] {
                if p.get(k).is_some() {
                    self.warn(
                        &ptr,
                        format!("`{k}` MUST NOT be used in a Header Object (§4.8.21)"),
                    );
                }
            }
            if let Some(s) = p.get("style").and_then(Value::as_str) {
                if s != "simple" {
                    self.warn(
                        &ptr,
                        format!("a Header Object's style MUST be simple, not {s:?} (§4.8.21)"),
                    );
                }
            }
        } else {
            let location = p.get("in").and_then(Value::as_str).unwrap_or("");
            if let Some(s) = p.get("style").and_then(Value::as_str) {
                if !style_allowed(location, s) {
                    self.warn(
                        &ptr,
                        format!("style {s:?} is not defined for {location} parameters (§4.8.12.3)"),
                    );
                }
            }
            if p.get("allowEmptyValue").is_some() && location != "query" {
                self.warn(
                    &ptr,
                    "allowEmptyValue is valid only for query parameters (§4.8.12.2.1)",
                );
            }
        }
    }

    fn media(&mut self, content: &'a Value, ptr: &str, request: bool) {
        let Some(map) = content.as_object() else {
            return;
        };
        for (key, m) in map {
            let mptr = doc::join(ptr, key);
            let Some(enc) = m.get("encoding").and_then(Value::as_object) else {
                continue;
            };
            let k = key.to_ascii_lowercase();
            let form =
                k.starts_with("application/x-www-form-urlencoded") || k.starts_with("multipart/");
            if !request || !form {
                self.warn(
                    &mptr,
                    "encoding SHALL only apply to request bodies of multipart or form media types (§4.8.14.1)",
                );
                continue;
            }
            let props = m
                .get("schema")
                .map(|s| self.property_names(s, 0))
                .unwrap_or_default();
            for name in enc.keys() {
                if !props.is_empty() && !props.contains(name) {
                    self.warn(
                        &doc::join(&doc::join(&mptr, "encoding"), name),
                        "an encoding key MUST exist in the schema as a property (§4.8.14.1)",
                    );
                }
            }
            for (name, e) in enc {
                if let Some(hs) = e.get("headers").and_then(Value::as_object) {
                    for (h, hv) in hs {
                        let hp = doc::join(
                            &doc::join(&doc::join(&doc::join(&mptr, "encoding"), name), "headers"),
                            h,
                        );
                        self.parameter(hv, &hp, true);
                    }
                }
            }
        }
    }

    fn property_names(&self, s: &Value, depth: usize) -> Vec<String> {
        if depth > 16 {
            return Vec::new();
        }
        let s = match s
            .get("$ref")
            .and_then(Value::as_str)
            .and_then(|r| doc::lookup_ref(self.root, r))
        {
            Some(t) => t,
            None => s,
        };
        let mut out: Vec<String> = s
            .get("properties")
            .and_then(Value::as_object)
            .map(|p| p.keys().cloned().collect())
            .unwrap_or_default();
        for key in ["allOf", "anyOf", "oneOf"] {
            for m in s.get(key).and_then(Value::as_array).into_iter().flatten() {
                out.extend(self.property_names(m, depth + 1));
            }
        }
        out
    }

    fn responses(&mut self, v: &'a Value, ptr: &str) {
        let Some(map) = v.as_object() else {
            return;
        };
        if !map.keys().any(|k| !k.starts_with("x-")) {
            self.warn(
                ptr,
                "a Responses Object MUST contain at least one response code (§4.8.16)",
            );
        }
        for (code, r) in map {
            if code.starts_with("x-") {
                continue;
            }
            let rptr = doc::join(ptr, code);
            let Some((r, rptr)) = self.deref(r, &rptr) else {
                continue;
            };
            if let Some(hs) = r.get("headers").and_then(Value::as_object) {
                for (h, hv) in hs {
                    self.parameter(hv, &doc::join(&doc::join(&rptr, "headers"), h), true);
                }
            }
            if let Some(c) = r.get("content") {
                self.media(c, &doc::join(&rptr, "content"), false);
            }
        }
    }

    fn operation(
        &mut self,
        template: &str,
        item: &'a Value,
        item_ptr: &str,
        op: &'a Value,
        op_ptr: &str,
        ids: &mut HashMap<String, String>,
    ) {
        if let Some(id) = op.get("operationId").and_then(Value::as_str) {
            if let Some(first) = ids.insert(id.to_owned(), op_ptr.to_owned()) {
                self.warn(
                    op_ptr,
                    format!(
                        "operationId {id:?} is also used at {first}; it MUST be unique (§4.8.10.1)"
                    ),
                );
            }
        }
        let mut path_params = Vec::new();
        for (c, cptr) in [(item, item_ptr), (op, op_ptr)] {
            for (i, p) in c
                .get("parameters")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .enumerate()
            {
                let pptr = doc::join(&doc::join(cptr, "parameters"), &i.to_string());
                self.parameter(p, &pptr, false);
                if let Some((p, _)) = self.deref(p, &pptr) {
                    if p.get("in").and_then(Value::as_str) == Some("path") {
                        if let Some(n) = p.get("name").and_then(Value::as_str) {
                            path_params.push((n.to_owned(), pptr.clone()));
                        }
                    }
                }
            }
        }
        let vars: Vec<&str> = template
            .split('{')
            .skip(1)
            .filter_map(|s| s.split_once('}').map(|(n, _)| n))
            .collect();
        for v in &vars {
            if !path_params.iter().any(|(n, _)| n == v) {
                self.warn(
                    op_ptr,
                    format!("template expression {{{v}}} has no path parameter (§3.5)"),
                );
            }
        }
        for (n, p) in &path_params {
            if !vars.contains(&n.as_str()) {
                self.warn(
                    p,
                    format!(
                        "path parameter {n:?} does not appear in the path template (§4.8.12.2.1)"
                    ),
                );
            }
        }
        if let Some(rb) = op.get("requestBody") {
            if let Some((rb, rbptr)) = self.deref(rb, &doc::join(op_ptr, "requestBody")) {
                if let Some(c) = rb.get("content") {
                    self.media(c, &doc::join(&rbptr, "content"), true);
                }
            }
        }
        if let Some(r) = op.get("responses") {
            self.responses(r, &doc::join(op_ptr, "responses"));
        }
        if let Some(sec) = op.get("security") {
            self.security(sec, &doc::join(op_ptr, "security"));
        }
    }

    fn security(&mut self, v: &Value, ptr: &str) {
        if self.version != Version::V30 {
            return;
        }
        let schemes = self.root.pointer("/components/securitySchemes");
        for (i, req) in v.as_array().into_iter().flatten().enumerate() {
            for (name, scopes) in req.as_object().into_iter().flatten() {
                let kind = schemes
                    .and_then(|s| s.get(name))
                    .and_then(|s| s.get("type"))
                    .and_then(Value::as_str);
                let oauth = matches!(kind, Some("oauth2" | "openIdConnect"));
                if !oauth && scopes.as_array().is_some_and(|a| !a.is_empty()) {
                    self.warn(
                        &doc::join(ptr, &i.to_string()),
                        format!("scopes for the non-OAuth scheme {name:?} MUST be empty in OpenAPI 3.0 (§4.7.30.1)"),
                    );
                }
            }
        }
    }

    fn schemes(&mut self) {
        let Some(map) = self
            .root
            .pointer("/components/securitySchemes")
            .and_then(Value::as_object)
        else {
            return;
        };
        for (name, s) in map {
            let ptr = doc::join("#/components/securitySchemes", name);
            let Some((s, ptr)) = self.deref(s, &ptr) else {
                continue;
            };
            let need = |k: &str| s.get(k).is_none();
            match s.get("type").and_then(Value::as_str) {
                Some("apiKey") => {
                    if need("name")
                        || !matches!(
                            s.get("in").and_then(Value::as_str),
                            Some("query" | "header" | "cookie")
                        )
                    {
                        self.warn(
                            &ptr,
                            "an apiKey scheme REQUIRES `name` and `in` (query, header or cookie)",
                        );
                    }
                }
                Some("http") if need("scheme") => {
                    self.warn(&ptr, "an http scheme REQUIRES `scheme`")
                }
                Some("oauth2") if need("flows") => {
                    self.warn(&ptr, "an oauth2 scheme REQUIRES `flows`")
                }
                Some("openIdConnect") if need("openIdConnectUrl") => {
                    self.warn(&ptr, "an openIdConnect scheme REQUIRES `openIdConnectUrl`")
                }
                Some("mutualTLS") if self.version == Version::V30 => {
                    self.warn(&ptr, "mutualTLS is not a scheme type in OpenAPI 3.0")
                }
                Some("http" | "oauth2" | "openIdConnect" | "mutualTLS") => {}
                other => self.warn(
                    &ptr,
                    format!("unknown security scheme type {:?}", other.unwrap_or("")),
                ),
            }
        }
    }

    fn servers(&mut self, v: Option<&Value>, ptr: &str) {
        for (i, s) in v
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            for (name, var) in s
                .get("variables")
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
            {
                let vptr = doc::join(
                    &doc::join(&doc::join(ptr, &i.to_string()), "variables"),
                    name,
                );
                let default = var.get("default");
                if let Some(e) = var.get("enum").and_then(Value::as_array) {
                    if e.is_empty() {
                        self.warn(
                            &vptr,
                            "a server variable `enum` SHOULD NOT be empty (§4.8.6.1)",
                        );
                    } else if default.is_some_and(|d| !e.contains(d)) {
                        self.warn(&vptr, "a server variable `default` MUST be one of its `enum` values (§4.8.6.1)");
                    }
                }
            }
        }
    }

    /// OpenAPI 3.0 Schema Object rules (3.0.4 §4.7.24.1).
    fn schema30(&mut self, s: &Value, ptr: &str, depth: usize) {
        if depth > 64 {
            return;
        }
        let Some(m) = s.as_object() else {
            return;
        };
        if m.contains_key("$ref") {
            return;
        }
        match m.get("type") {
            Some(Value::String(t)) if t == "array" && !m.contains_key("items") => self.warn(
                ptr,
                "`items` MUST be present when type is array (3.0.4 §4.7.24.1)",
            ),
            Some(Value::Array(_)) => self.warn(
                ptr,
                "`type` MUST be a string in OpenAPI 3.0 (3.0.4 §4.7.24.1)",
            ),
            _ => {}
        }
        if m.get("readOnly") == Some(&Value::Bool(true))
            && m.get("writeOnly") == Some(&Value::Bool(true))
        {
            self.warn(
                ptr,
                "a property MUST NOT be both readOnly and writeOnly (3.0.4 §4.7.24.2)",
            );
        }
        if let Some(d) = m
            .get("discriminator")
            .and_then(|d| d.get("propertyName"))
            .and_then(Value::as_str)
        {
            let required = m
                .get("required")
                .and_then(Value::as_array)
                .is_some_and(|r| r.iter().any(|x| x == d));
            if !required && !m.contains_key("oneOf") && !m.contains_key("anyOf") {
                self.warn(
                    ptr,
                    format!("the discriminator property {d:?} MUST be required (3.0.4 §4.7.25)"),
                );
            }
        }
        for key in ["properties"] {
            for (k, c) in m.get(key).and_then(Value::as_object).into_iter().flatten() {
                self.schema30(c, &doc::join(&doc::join(ptr, key), k), depth + 1);
            }
        }
        for key in ["items", "not", "additionalProperties"] {
            if let Some(c) = m.get(key) {
                self.schema30(c, &doc::join(ptr, key), depth + 1);
            }
        }
        for key in ["allOf", "anyOf", "oneOf"] {
            for (i, c) in m
                .get(key)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .enumerate()
            {
                self.schema30(
                    c,
                    &doc::join(&doc::join(ptr, key), &i.to_string()),
                    depth + 1,
                );
            }
        }
    }
}

/// Every structural finding for a document.
pub(crate) fn check(root: &Value, version: Version) -> Vec<Warning> {
    let mut l = Lint {
        root,
        version,
        out: Vec::new(),
    };
    l.servers(root.get("servers"), "#/servers");
    l.schemes();
    if let Some(sec) = root.get("security") {
        l.security(sec, "#/security");
    }
    let mut ids = HashMap::new();
    for (key, item) in root
        .get("paths")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        if !key.starts_with('/') {
            continue;
        }
        let iptr = doc::join("#/paths", key);
        let Some((item, iptr)) = l.deref(item, &iptr) else {
            continue;
        };
        l.servers(item.get("servers"), &doc::join(&iptr, "servers"));
        for it in crate::spec::item_operations(item, &iptr, version) {
            l.servers(it.op.get("servers"), &doc::join(&it.ptr, "servers"));
            l.operation(key, item, &iptr, it.op, &it.ptr, &mut ids);
        }
    }
    if version == Version::V30 {
        for (name, s) in root
            .pointer("/components/schemas")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            l.schema30(s, &doc::join("#/components/schemas", name), 0);
        }
    }
    l.out
}
