//! Structural checks of the document itself.
//!
//! Each finding is a MUST or SHALL of the specification that the document
//! breaks. They are load warnings by default (a flawed description should
//! not refuse every call), and load errors with `Options::strict_document`.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::doc::{self, Version, Warning};

/// Styles each parameter location allows (OpenAPI 3.1.1 §4.8.12.3; 3.2
/// adds `cookie`, §4.12.3).
fn style_allowed(location: &str, style: &str, version: Version) -> bool {
    matches!(
        (location, style),
        ("path", "matrix" | "label" | "simple")
            | (
                "query",
                "form" | "spaceDelimited" | "pipeDelimited" | "deepObject"
            )
            | ("header", "simple")
            | ("cookie", "form")
    ) || (version >= Version::V32 && (location, style) == ("cookie", "cookie"))
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
            // 3.2 (§4.21) keeps only the allowEmptyValue ban.
            let banned: &[&str] = if self.version >= Version::V32 {
                &["allowEmptyValue"]
            } else {
                &["allowEmptyValue", "allowReserved"]
            };
            for k in banned {
                if p.get(*k).is_some() {
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
            if location == "querystring" && self.version >= Version::V32 {
                if p.get("content").is_none() {
                    self.warn(
                        &ptr,
                        "an in: querystring parameter MUST use `content` (§4.12.1)",
                    );
                }
                for k in ["schema", "style", "explode", "allowReserved"] {
                    if p.get(k).is_some() {
                        self.warn(
                            &ptr,
                            format!("`{k}` MUST NOT be used with in: querystring (§4.12.2.2)"),
                        );
                    }
                }
                return;
            }
            if let Some(s) = p.get("style").and_then(Value::as_str) {
                if !style_allowed(location, s, self.version) {
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
            let k = key.to_ascii_lowercase();
            let (m, mptr) = if self.version >= Version::V32 {
                match self.deref(m, &mptr) {
                    Some(x) => x,
                    None => continue,
                }
            } else {
                (m, mptr)
            };
            if self.version >= Version::V32 {
                self.positional(&k, m, &mptr);
            }
            let Some(enc) = m.get("encoding").and_then(Value::as_object) else {
                continue;
            };
            let form =
                k.starts_with("application/x-www-form-urlencoded") || k.starts_with("multipart/");
            // 3.2 no longer limits `encoding` to request bodies (§4.14.1).
            let request = request || self.version >= Version::V32;
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
            // 3.2.1 §4.14.5.1 asks only that the key exist in the data, and
            // ignores entries without a property.
            let check_keys = self.version < Version::V32;
            for name in enc.keys().filter(|_| check_keys) {
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

    /// OpenAPI 3.2 §4.14.1, §4.14.5.2: positional encodings.
    fn positional(&mut self, key: &str, m: &Value, ptr: &str) {
        let positional = m.get("prefixEncoding").is_some() || m.get("itemEncoding").is_some();
        if !positional {
            return;
        }
        if m.get("encoding").is_some() {
            self.warn(
                ptr,
                "encoding MUST NOT be present with prefixEncoding or itemEncoding (§4.14.1)",
            );
        }
        if !key.starts_with("multipart/") {
            self.warn(
                ptr,
                "prefixEncoding and itemEncoding SHALL only apply to multipart media types (§4.14.1)",
            );
        }
        // 3.2.0 required itemSchema or an array schema here; 3.2.1
        // (§4.14.5.2) ignores entries without an item instead.
    }

    /// Whether `prop` is required by `s`: by `required` here or in an
    /// `allOf` member, or by every `oneOf`/`anyOf` branch.
    fn requires(&self, s: &Value, prop: &str, depth: usize) -> bool {
        self.requires_in(s, prop, depth, &mut HashMap::new())
    }

    /// `memo` holds each schema's answer; one still being worked out (a
    /// cycle) counts as not requiring, so the walk is linear.
    fn requires_in(
        &self,
        s: &Value,
        prop: &str,
        depth: usize,
        memo: &mut HashMap<usize, bool>,
    ) -> bool {
        if depth > 16 {
            return true;
        }
        let s = s
            .get("$ref")
            .and_then(Value::as_str)
            .and_then(|r| doc::lookup_ref(self.root, r))
            .unwrap_or(s);
        let key = s as *const Value as usize;
        if let Some(&known) = memo.get(&key) {
            return known;
        }
        memo.insert(key, false);
        let here = s
            .get("required")
            .and_then(Value::as_array)
            .is_some_and(|r| r.iter().any(|x| x == prop));
        let all_of = s
            .get("allOf")
            .and_then(Value::as_array)
            .is_some_and(|a| a.iter().any(|m| self.requires_in(m, prop, depth + 1, memo)));
        let branches = ["oneOf", "anyOf"].iter().any(|k| {
            s.get(*k).and_then(Value::as_array).is_some_and(|a| {
                !a.is_empty() && a.iter().all(|m| self.requires_in(m, prop, depth + 1, memo))
            })
        });
        let answer = here || all_of || branches;
        memo.insert(key, answer);
        answer
    }

    /// OpenAPI 3.2 §4.26.1: `attribute` and `wrapped` MUST NOT be used
    /// with `nodeType`. Walks the schema's subschemas.
    fn xml_objects(&mut self, s: &Value, ptr: &str, depth: usize) {
        if depth > 32 {
            return;
        }
        match s {
            Value::Object(m) => {
                if let Some(x) = m.get("xml").filter(|x| x.get("nodeType").is_some()) {
                    for k in ["attribute", "wrapped"] {
                        if x.get(k).is_some() {
                            self.warn(
                                &doc::join(ptr, "xml"),
                                format!("`{k}` MUST NOT be present with `nodeType` (§4.26.1)"),
                            );
                        }
                    }
                }
                for (k, v) in m {
                    if !matches!(
                        k.as_str(),
                        "enum" | "const" | "default" | "example" | "examples" | "xml"
                    ) {
                        self.xml_objects(v, &doc::join(ptr, k), depth + 1);
                    }
                }
            }
            Value::Array(a) => {
                for (i, v) in a.iter().enumerate() {
                    self.xml_objects(v, &doc::join(ptr, &i.to_string()), depth + 1);
                }
            }
            _ => {}
        }
    }

    /// 3.1 and 3.2 Schema Objects are JSON Schema 2020-12 schemas (§4.8.24):
    /// each is checked against the 2020-12 meta-schema, unless its `$schema`
    /// names another dialect.
    /// Every Schema Object position: component schemas, and each `schema`
    /// and `itemSchema` value outside schemas.
    fn schema_locations(&self) -> Vec<(&'a Value, String)> {
        let mut found: Vec<(&'a Value, String)> = Vec::new();
        for (name, s) in self
            .root
            .pointer("/components/schemas")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            found.push((s, doc::join("#/components/schemas", name)));
        }
        // Pointers are built only for the schemas found.
        fn walk<'v>(v: &'v Value, path: &mut Vec<String>, found: &mut Vec<(&'v Value, String)>) {
            let at = |path: &[String], last: &str| {
                let mut p = String::from("#");
                for t in path.iter().map(String::as_str).chain(std::iter::once(last)) {
                    p.push('/');
                    p.push_str(&doc::escape_token(t));
                }
                p
            };
            match v {
                Value::Object(m) => {
                    for (k, c) in m {
                        if matches!(k.as_str(), "schema" | "itemSchema") {
                            found.push((c, at(path, k)));
                        } else if !(k.starts_with("x-")
                            || matches!(k.as_str(), "example" | "examples" | "default")
                            || (path.len() == 1 && path[0] == "components" && k == "schemas"))
                        {
                            path.push(k.clone());
                            walk(c, path, found);
                            path.pop();
                        }
                    }
                }
                Value::Array(a) => {
                    for (i, c) in a.iter().enumerate() {
                        path.push(i.to_string());
                        walk(c, path, found);
                        path.pop();
                    }
                }
                _ => {}
            }
        }
        walk(self.root, &mut Vec::new(), &mut found);
        found
    }

    fn schemas_2020(&mut self) {
        let meta = jsonschema::draft202012::meta::validator();
        for (s, ptr) in self.schema_locations() {
            let dialect = s.get("$schema").and_then(Value::as_str);
            if dialect.is_some_and(|d| {
                !d.contains("json-schema.org/draft/2020-12")
                    && !d.contains("spec.openapis.org/oas/")
            }) {
                continue;
            }
            if meta.is_valid(s) {
                continue;
            }
            if let Err(e) = meta.validate(s) {
                let at = e.instance_path().to_string();
                self.warn(
                    &format!("{ptr}{at}"),
                    format!("not a valid JSON Schema 2020-12 schema: {} (§4.8.24)", e),
                );
            }
        }
    }

    /// Tag names are unique (§4.8.1); in 3.2 a `parent` names an existing
    /// tag and parents form no cycle (3.2 §4.22.1).
    fn tags(&mut self, tags: Option<&Value>) {
        let Some(tags) = tags.and_then(Value::as_array) else {
            return;
        };
        let names: Vec<Option<&str>> = tags
            .iter()
            .map(|t| t.get("name").and_then(Value::as_str))
            .collect();
        for (i, n) in names.iter().enumerate() {
            if n.is_some() && names[..i].contains(n) {
                self.warn(
                    &format!("#/tags/{i}"),
                    format!(
                        "tag {:?} is listed twice; tag names MUST be unique (§4.8.1)",
                        n.unwrap_or("")
                    ),
                );
            }
        }
        if self.version < Version::V32 {
            return;
        }
        let parent_of = |n: &str| {
            tags.iter()
                .find(|t| t.get("name").and_then(Value::as_str) == Some(n))
                .and_then(|t| t.get("parent"))
                .and_then(Value::as_str)
        };
        for (i, t) in tags.iter().enumerate() {
            let Some(parent) = t.get("parent").and_then(Value::as_str) else {
                continue;
            };
            let ptr = format!("#/tags/{i}/parent");
            if !names.contains(&Some(parent)) {
                self.warn(
                    &ptr,
                    format!("parent tag {parent:?} MUST exist (3.2 §4.22.1)"),
                );
                continue;
            }
            // Follow parents; returning to this tag is a cycle.
            let own = names[i];
            let mut cur = Some(parent);
            for _ in 0..=tags.len() {
                match cur {
                    Some(c) if Some(c) == own => {
                        self.warn(&ptr, "parent tags MUST NOT form a cycle (3.2 §4.22.1)");
                        break;
                    }
                    Some(c) => cur = parent_of(c),
                    None => break,
                }
            }
        }
    }

    /// OpenAPI 3.2 §4.25.1: an optional discriminating property needs a
    /// `defaultMapping`.
    fn discriminator(&mut self, s: &Value, ptr: &str) {
        let Some(d) = s.get("discriminator") else {
            return;
        };
        let Some(prop) = d.get("propertyName").and_then(Value::as_str) else {
            return;
        };
        if d.get("defaultMapping").is_none() && !self.requires(s, prop, 0) {
            self.warn(
                &doc::join(ptr, "discriminator"),
                format!("{prop:?} is optional, so the discriminator MUST have a defaultMapping (§4.25.1)"),
            );
        }
    }

    fn property_names(&self, s: &Value, depth: usize) -> Vec<String> {
        let mut out = Vec::new();
        self.property_names_in(s, depth, &mut HashSet::new(), &mut out);
        out
    }

    /// Each schema's names are collected once, so the walk is linear.
    fn property_names_in(
        &self,
        s: &Value,
        depth: usize,
        seen: &mut HashSet<usize>,
        out: &mut Vec<String>,
    ) {
        if depth > 16 {
            return;
        }
        let s = match s
            .get("$ref")
            .and_then(Value::as_str)
            .and_then(|r| doc::lookup_ref(self.root, r))
        {
            Some(t) => t,
            None => s,
        };
        if !seen.insert(s as *const Value as usize) {
            return;
        }
        if let Some(p) = s.get("properties").and_then(Value::as_object) {
            out.extend(p.keys().cloned());
        }
        for key in ["allOf", "anyOf", "oneOf"] {
            for m in s.get(key).and_then(Value::as_array).into_iter().flatten() {
                self.property_names_in(m, depth + 1, seen, out);
            }
        }
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
        let (mut querystrings, mut queries) = (0, 0);
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
                    match p.get("in").and_then(Value::as_str) {
                        Some("querystring") => querystrings += 1,
                        Some("query") => queries += 1,
                        _ => {}
                    }
                    if p.get("in").and_then(Value::as_str) == Some("path") {
                        if let Some(n) = p.get("name").and_then(Value::as_str) {
                            path_params.push((n.to_owned(), pptr.clone()));
                        }
                    }
                }
            }
        }
        if self.version >= Version::V32 {
            if querystrings > 1 {
                self.warn(
                    op_ptr,
                    "an in: querystring parameter MUST NOT appear more than once (§4.12.1)",
                );
            }
            if querystrings > 0 && queries > 0 {
                self.warn(
                    op_ptr,
                    "in: query parameters MUST NOT appear with an in: querystring parameter (§4.12.1)",
                );
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
                Some("oauth2") => self.flows(s.get("flows"), &doc::join(&ptr, "flows")),
                Some("openIdConnect") if need("openIdConnectUrl") => {
                    self.warn(&ptr, "an openIdConnect scheme REQUIRES `openIdConnectUrl`")
                }
                Some("mutualTLS") if self.version == Version::V30 => {
                    self.warn(&ptr, "mutualTLS is not a scheme type in OpenAPI 3.0")
                }
                Some("http" | "openIdConnect" | "mutualTLS") => {}
                other => self.warn(
                    &ptr,
                    format!("unknown security scheme type {:?}", other.unwrap_or("")),
                ),
            }
        }
    }

    /// Required fields of each OAuth Flow Object (§4.8.29; 3.2 adds the
    /// device authorization flow, §4.28, §4.29).
    fn flows(&mut self, flows: Option<&Value>, ptr: &str) {
        let Some(flows) = flows.and_then(Value::as_object) else {
            return;
        };
        for (name, f) in flows {
            let need: &[&str] = match name.as_str() {
                "implicit" => &["authorizationUrl", "scopes"],
                "password" | "clientCredentials" => &["tokenUrl", "scopes"],
                "authorizationCode" => &["authorizationUrl", "tokenUrl", "scopes"],
                "deviceAuthorization" if self.version >= Version::V32 => {
                    &["deviceAuthorizationUrl", "tokenUrl", "scopes"]
                }
                _ => continue,
            };
            for k in need {
                if f.get(*k).is_none() {
                    self.warn(
                        &doc::join(ptr, name),
                        format!("the {name} flow REQUIRES `{k}`"),
                    );
                }
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
            if s.get("url")
                .and_then(Value::as_str)
                .is_some_and(|u| u.contains(['?', '#']))
            {
                self.warn(
                    &doc::join(&doc::join(ptr, &i.to_string()), "url"),
                    "a server URL MUST NOT have a query or fragment (3.1.2 §4.8.5.1)",
                );
            }
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
        // 3.0.4 §4.7.24: the Schema Object takes these keywords and
        // extensions; others are not applied.
        for k in m.keys() {
            if !k.starts_with("x-") && !crate::schema::OAS30_KEYWORDS.contains(&k.as_str()) {
                self.warn(
                    &doc::join(ptr, k),
                    format!("{k:?} is not an OpenAPI 3.0 Schema Object keyword, so it is not applied (3.0.4 §4.7.24)"),
                );
            }
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
    l.tags(root.get("tags"));
    if version >= Version::V31 {
        l.schemas_2020();
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
        let mut seen: Vec<&str> = Vec::new();
        for name in key
            .split('{')
            .skip(1)
            .filter_map(|s| s.split_once('}').map(|(n, _)| n))
        {
            if seen.contains(&name) {
                l.warn(
                    &iptr,
                    format!("template expression {{{name}}} MUST NOT appear more than once in a path (3.2 §4.8.2)"),
                );
            }
            seen.push(name);
        }
        let Some((item, iptr)) = l.deref(item, &iptr) else {
            continue;
        };
        l.servers(item.get("servers"), &doc::join(&iptr, "servers"));
        if version >= Version::V32 {
            for k in item
                .get("additionalOperations")
                .and_then(Value::as_object)
                .into_iter()
                .flat_map(|m| m.keys())
            {
                let mut fixed = crate::spec::METHODS.iter().chain(&["query"]);
                if fixed.any(|m| m.to_ascii_uppercase() == *k) {
                    l.warn(
                        &doc::join(&doc::join(&iptr, "additionalOperations"), k),
                        format!("additionalOperations MUST NOT contain {k}, which a fixed field describes (§4.9.1)"),
                    );
                }
            }
        }
        for it in crate::spec::item_operations(item, &iptr, version) {
            l.servers(it.op.get("servers"), &doc::join(&it.ptr, "servers"));
            l.operation(key, item, &iptr, it.op, &it.ptr, &mut ids);
        }
    }
    if version >= Version::V32 {
        for (name, s) in root
            .pointer("/components/schemas")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            let ptr = doc::join("#/components/schemas", name);
            l.discriminator(s, &ptr);
            l.xml_objects(s, &ptr, 0);
        }
    }
    if version == Version::V30 {
        for (s, ptr) in l.schema_locations() {
            l.schema30(s, &ptr, 0);
        }
    }
    l.out
}
