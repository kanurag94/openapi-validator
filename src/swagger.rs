//! Swagger 2.0 documents read as OpenAPI 3.0.
//!
//! A Swagger 2.0 document is converted to the OpenAPI 3.0 objects that say
//! the same thing, and is then loaded like any 3.0 document:
//!
//! - `host`, `basePath` and `schemes` become `servers`;
//! - `definitions`, `parameters`, `responses` and `securityDefinitions`
//!   move under `components`, and references to them follow;
//! - a `body` parameter becomes the `requestBody`, and `formData`
//!   parameters become a form or multipart body, each with the operation's
//!   `consumes` (or the document's) as media types;
//! - responses get the `produces` media types for their schema;
//! - parameter and header types, items and `collectionFormat` become a
//!   `schema` with the matching `style` and `explode`;
//! - in schemas, a `discriminator` property name becomes a Discriminator
//!   Object, `type: file` a binary string, and `x-nullable` `nullable`;
//! - `basic` security becomes `http` basic, and OAuth `application` and
//!   `accessCode` flows become `clientCredentials` and `authorizationCode`.
//!
//! Without `consumes` or `produces` anywhere, media types are `*/*`, so
//! any body is accepted and parsed by its own type.

use serde_json::{json, Map, Value};

use crate::doc::{escape_token, LoadError, Warning};

const METHODS: [&str; 7] = ["get", "put", "post", "delete", "options", "head", "patch"];

/// Keys holding data, whose `$ref` members are not references.
const DATA_KEYS: &[&str] = &["example", "examples", "enum", "default", "x-example"];

/// Converts a Swagger 2.0 document. Other `swagger` versions are refused.
pub(crate) fn convert(root: Value) -> Result<(Value, Vec<Warning>), LoadError> {
    let version = match root.get("swagger") {
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    };
    if version != "2.0" {
        return Err(LoadError::UnsupportedVersion(version));
    }
    let mut c = Converter {
        root: &root,
        warnings: Vec::new(),
    };
    let out = c.document();
    let mut out = out;
    rewrite_refs(&mut out);
    Ok((out, c.warnings))
}

struct Converter<'a> {
    root: &'a Value,
    warnings: Vec<Warning>,
}

fn strings(v: Option<&Value>) -> Option<Vec<String>> {
    v.and_then(Value::as_array).map(|a| {
        a.iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    })
}

fn is_form(media: &str) -> bool {
    let m = media.to_ascii_lowercase();
    m.starts_with("application/x-www-form-urlencoded") || m.starts_with("multipart/form-data")
}

impl<'a> Converter<'a> {
    fn warn(&mut self, location: String, message: impl Into<String>) {
        self.warnings.push(Warning {
            location,
            message: message.into(),
        });
    }

    fn document(&mut self) -> Value {
        let r = self.root;
        let mut out = Map::new();
        out.insert("openapi".into(), json!("3.0.3"));
        for k in ["info", "tags", "externalDocs", "security"] {
            if let Some(v) = r.get(k) {
                out.insert(k.into(), v.clone());
            }
        }
        for (k, v) in r.as_object().into_iter().flatten() {
            if k.starts_with("x-") {
                out.insert(k.clone(), v.clone());
            }
        }
        out.insert("servers".into(), self.servers());
        let consumes = strings(r.get("consumes"));
        let produces = strings(r.get("produces"));
        let mut paths = Map::new();
        for (p, item) in r
            .get("paths")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            if p.starts_with("x-") {
                paths.insert(p.clone(), item.clone());
                continue;
            }
            let ptr = format!("#/paths/{}", escape_token(p));
            let item = self.deref(item);
            paths.insert(
                p.clone(),
                self.path_item(item, &ptr, consumes.as_deref(), produces.as_deref()),
            );
        }
        out.insert("paths".into(), Value::Object(paths));
        out.insert("components".into(), self.components(produces.as_deref()));
        Value::Object(out)
    }

    /// Swagger 2.0 §5.2: the URL is `scheme://host/basePath`; without
    /// `host` it is the base path alone.
    fn servers(&self) -> Value {
        let r = self.root;
        let base = r.get("basePath").and_then(Value::as_str).unwrap_or("/");
        let base = if base.starts_with('/') {
            base.to_owned()
        } else {
            format!("/{base}")
        };
        let Some(host) = r.get("host").and_then(Value::as_str) else {
            return json!([{ "url": base }]);
        };
        let schemes = strings(r.get("schemes")).unwrap_or_else(|| vec!["https".into()]);
        let base = if base == "/" { String::new() } else { base };
        Value::Array(
            schemes
                .iter()
                .map(|s| json!({ "url": format!("{s}://{host}{base}") }))
                .collect(),
        )
    }

    /// Follows a local `$ref` (for path items and parameters).
    fn deref<'v>(&self, v: &'v Value) -> &'v Value
    where
        'a: 'v,
    {
        let mut cur = v;
        for _ in 0..32 {
            match cur
                .get("$ref")
                .and_then(Value::as_str)
                .and_then(|r| r.strip_prefix('#'))
                .and_then(|p| self.root.pointer(p))
            {
                Some(t) => cur = t,
                None => break,
            }
        }
        cur
    }

    fn path_item(
        &mut self,
        item: &Value,
        ptr: &str,
        consumes: Option<&[String]>,
        produces: Option<&[String]>,
    ) -> Value {
        let mut out = Map::new();
        for (k, v) in item.as_object().into_iter().flatten() {
            if k.starts_with("x-") {
                out.insert(k.clone(), v.clone());
            }
        }
        let shared: Vec<Value> = item
            .get("parameters")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        // Path-level parameters that stay parameters.
        let kept: Vec<Value> = shared
            .iter()
            .filter(|p| !matches!(self.location(p).as_deref(), Some("body" | "formData")))
            .cloned()
            .collect();
        let plain: Vec<Value> = kept
            .iter()
            .map(|p| self.parameter(p, &format!("{ptr}/parameters")))
            .collect();
        if !plain.is_empty() {
            out.insert("parameters".into(), Value::Array(plain));
        }
        for m in METHODS {
            let Some(op) = item.get(m).filter(|o| o.is_object()) else {
                continue;
            };
            let optr = format!("{ptr}/{m}");
            out.insert(
                m.into(),
                self.operation(op, &shared, &optr, consumes, produces),
            );
        }
        Value::Object(out)
    }

    /// A parameter's `in`, through `$ref`.
    fn location(&self, p: &Value) -> Option<String> {
        self.deref(p)
            .get("in")
            .and_then(Value::as_str)
            .map(str::to_owned)
    }

    fn operation(
        &mut self,
        op: &Value,
        shared: &[Value],
        ptr: &str,
        consumes: Option<&[String]>,
        produces: Option<&[String]>,
    ) -> Value {
        let mut out = Map::new();
        for k in [
            "tags",
            "summary",
            "description",
            "externalDocs",
            "operationId",
            "deprecated",
            "security",
        ] {
            if let Some(v) = op.get(k) {
                out.insert(k.into(), v.clone());
            }
        }
        for (k, v) in op.as_object().into_iter().flatten() {
            if k.starts_with("x-") {
                out.insert(k.clone(), v.clone());
            }
        }
        let consumes = strings(op.get("consumes")).or_else(|| consumes.map(<[String]>::to_vec));
        let produces = strings(op.get("produces")).or_else(|| produces.map(<[String]>::to_vec));
        let own: Vec<Value> = op
            .get("parameters")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        // Body and form parameters: the path item's, replaced by the
        // operation's with the same name and location.
        let key = |p: &Value| {
            let p = self.deref(p);
            (
                p.get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                p.get("in").and_then(Value::as_str).unwrap_or("").to_owned(),
            )
        };
        let own_keys: Vec<(String, String)> = own.iter().map(key).collect();
        let mut all: Vec<&Value> = shared
            .iter()
            .filter(|p| !own_keys.contains(&key(p)))
            .collect();
        all.extend(own.iter());
        let body: Vec<&Value> = all
            .iter()
            .copied()
            .filter(|p| self.location(p).as_deref() == Some("body"))
            .collect();
        let form: Vec<&Value> = all
            .iter()
            .copied()
            .filter(|p| self.location(p).as_deref() == Some("formData"))
            .collect();
        let kept: Vec<Value> = own
            .iter()
            .filter(|p| !matches!(self.location(p).as_deref(), Some("body" | "formData")))
            .cloned()
            .collect();
        let plain: Vec<Value> = kept
            .iter()
            .map(|p| self.parameter(p, &format!("{ptr}/parameters")))
            .collect();
        if !plain.is_empty() {
            out.insert("parameters".into(), Value::Array(plain));
        }
        let media = consumes.unwrap_or_default();
        if let Some(b) = body.first() {
            let b = self.deref(b);
            let types = if media.is_empty() {
                vec!["*/*".to_owned()]
            } else {
                media.clone()
            };
            let schema = b.get("schema").map(fix_schema).unwrap_or(json!({}));
            let content: Map<String, Value> = types
                .iter()
                .map(|t| (t.clone(), json!({ "schema": schema })))
                .collect();
            let mut rb = Map::new();
            if let Some(d) = b.get("description") {
                rb.insert("description".into(), d.clone());
            }
            rb.insert(
                "required".into(),
                json!(b.get("required") == Some(&json!(true))),
            );
            rb.insert("content".into(), Value::Object(content));
            out.insert("requestBody".into(), Value::Object(rb));
        } else if !form.is_empty() {
            out.insert("requestBody".into(), self.form_body(&form, &media));
        }
        let mut responses = Map::new();
        for (code, r) in op
            .get("responses")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            responses.insert(code.clone(), self.response(r, produces.as_deref()));
        }
        out.insert("responses".into(), Value::Object(responses));
        Value::Object(out)
    }

    /// `formData` parameters as one form or multipart body (Swagger 2.0
    /// §5.2.11: `file` needs multipart/form-data).
    fn form_body(&mut self, form: &[&Value], consumes: &[String]) -> Value {
        let mut props = Map::new();
        let mut required = Vec::new();
        let mut encoding = Map::new();
        let mut file = false;
        for p in form {
            let p = self.deref(p);
            let Some(name) = p.get("name").and_then(Value::as_str) else {
                continue;
            };
            file |= p.get("type").and_then(Value::as_str) == Some("file");
            props.insert(name.to_owned(), param_schema(p));
            if p.get("required") == Some(&json!(true)) {
                required.push(json!(name));
            }
            if p.get("type").and_then(Value::as_str) == Some("array") {
                let (style, explode) = match p
                    .get("collectionFormat")
                    .and_then(Value::as_str)
                    .unwrap_or("csv")
                {
                    "multi" => ("form", true),
                    "ssv" => ("spaceDelimited", false),
                    "pipes" => ("pipeDelimited", false),
                    _ => ("form", false),
                };
                encoding.insert(name.to_owned(), json!({"style": style, "explode": explode}));
            }
        }
        let mut schema = json!({ "type": "object", "properties": props });
        if !required.is_empty() {
            schema["required"] = Value::Array(required.clone());
        }
        let mut types: Vec<String> = consumes.iter().filter(|t| is_form(t)).cloned().collect();
        if types.is_empty() {
            types.push(if file {
                "multipart/form-data".into()
            } else {
                "application/x-www-form-urlencoded".into()
            });
        }
        let content: Map<String, Value> = types
            .iter()
            .map(|t| {
                let mut m = json!({ "schema": schema });
                if !encoding.is_empty() {
                    m["encoding"] = Value::Object(encoding.clone());
                }
                (t.clone(), m)
            })
            .collect();
        json!({ "required": !required.is_empty(), "content": content })
    }

    /// A non-body, non-form parameter, or a reference to one.
    fn parameter(&mut self, p: &Value, ptr: &str) -> Value {
        if p.get("$ref").is_some() {
            return p.clone();
        }
        let loc = p.get("in").and_then(Value::as_str).unwrap_or("");
        let mut out = Map::new();
        for k in ["name", "in", "description", "required", "allowEmptyValue"] {
            if let Some(v) = p.get(k) {
                out.insert(k.into(), v.clone());
            }
        }
        for (k, v) in p.as_object().into_iter().flatten() {
            if k.starts_with("x-") {
                out.insert(k.clone(), v.clone());
            }
        }
        out.insert("schema".into(), param_schema(p));
        if p.get("type").and_then(Value::as_str) == Some("array") {
            let format = p
                .get("collectionFormat")
                .and_then(Value::as_str)
                .unwrap_or("csv");
            let (style, explode) = match (loc, format) {
                ("query", "multi") => ("form", true),
                ("query", "ssv") => ("spaceDelimited", false),
                ("query", "pipes") => ("pipeDelimited", false),
                ("query", "csv") => ("form", false),
                (_, "csv") => ("simple", false),
                (l, f) => {
                    self.warn(
                        ptr.to_owned(),
                        format!("collectionFormat {f} has no OpenAPI 3.0 style for {l} parameters; read as csv"),
                    );
                    (if l == "query" { "form" } else { "simple" }, false)
                }
            };
            out.insert("style".into(), json!(style));
            out.insert("explode".into(), json!(explode));
        }
        Value::Object(out)
    }

    fn response(&mut self, r: &Value, produces: Option<&[String]>) -> Value {
        if r.get("$ref").is_some() {
            return r.clone();
        }
        let mut out = Map::new();
        out.insert(
            "description".into(),
            r.get("description").cloned().unwrap_or(json!("")),
        );
        if let Some(hs) = r.get("headers").and_then(Value::as_object) {
            let mut headers = Map::new();
            for (n, h) in hs {
                let mut ho = Map::new();
                if let Some(d) = h.get("description") {
                    ho.insert("description".into(), d.clone());
                }
                ho.insert("schema".into(), param_schema(h));
                headers.insert(n.clone(), Value::Object(ho));
            }
            out.insert("headers".into(), Value::Object(headers));
        }
        if let Some(s) = r.get("schema") {
            let types = match produces {
                Some(t) if !t.is_empty() => t.to_vec(),
                _ => vec!["*/*".to_owned()],
            };
            let schema = fix_schema(s);
            let content: Map<String, Value> = types
                .iter()
                .map(|t| (t.clone(), json!({ "schema": schema })))
                .collect();
            out.insert("content".into(), Value::Object(content));
        }
        for (k, v) in r.as_object().into_iter().flatten() {
            if k.starts_with("x-") {
                out.insert(k.clone(), v.clone());
            }
        }
        Value::Object(out)
    }

    fn components(&mut self, produces: Option<&[String]>) -> Value {
        let r = self.root;
        let mut out = Map::new();
        if let Some(defs) = r.get("definitions").and_then(Value::as_object) {
            let schemas: Map<String, Value> = defs
                .iter()
                .map(|(k, v)| (k.clone(), fix_schema(v)))
                .collect();
            out.insert("schemas".into(), Value::Object(schemas));
        }
        if let Some(ps) = r.get("parameters").and_then(Value::as_object) {
            let mut params = Map::new();
            for (k, p) in ps {
                // Body and form parameters become request bodies where used.
                if matches!(
                    p.get("in").and_then(Value::as_str),
                    Some("body" | "formData")
                ) {
                    continue;
                }
                let ptr = format!("#/parameters/{}", escape_token(k));
                params.insert(k.clone(), self.parameter(p, &ptr));
            }
            out.insert("parameters".into(), Value::Object(params));
        }
        if let Some(rs) = r.get("responses").and_then(Value::as_object) {
            let responses: Map<String, Value> = rs
                .iter()
                .map(|(k, v)| (k.clone(), self.response(v, produces)))
                .collect();
            out.insert("responses".into(), Value::Object(responses));
        }
        if let Some(ss) = r.get("securityDefinitions").and_then(Value::as_object) {
            let schemes: Map<String, Value> = ss
                .iter()
                .map(|(k, v)| (k.clone(), security_scheme(v)))
                .collect();
            out.insert("securitySchemes".into(), Value::Object(schemes));
        }
        Value::Object(out)
    }
}

/// The schema of a non-body parameter, header or items object (Swagger
/// 2.0 §5.2.11, §5.2.12): the validation keywords move under `schema`.
fn param_schema(p: &Value) -> Value {
    const KEYS: &[&str] = &[
        "type",
        "format",
        "default",
        "maximum",
        "exclusiveMaximum",
        "minimum",
        "exclusiveMinimum",
        "maxLength",
        "minLength",
        "pattern",
        "maxItems",
        "minItems",
        "uniqueItems",
        "enum",
        "multipleOf",
    ];
    let mut out = Map::new();
    for k in KEYS {
        if let Some(v) = p.get(*k) {
            out.insert((*k).into(), v.clone());
        }
    }
    if let Some(items) = p.get("items") {
        out.insert("items".into(), param_schema(items));
    }
    if p.get("type").and_then(Value::as_str) == Some("file") {
        out.insert("type".into(), json!("string"));
        out.insert("format".into(), json!("binary"));
    }
    if p.get("x-nullable") == Some(&json!(true)) {
        out.insert("nullable".into(), json!(true));
    }
    Value::Object(out)
}

/// A Swagger 2.0 Schema Object as an OpenAPI 3.0 one.
fn fix_schema(s: &Value) -> Value {
    let mut s = s.clone();
    let mut stack = vec![&mut s];
    while let Some(v) = stack.pop() {
        let Value::Object(m) = v else {
            continue;
        };
        if let Some(Value::String(d)) = m.get("discriminator") {
            let d = d.clone();
            m.insert("discriminator".into(), json!({ "propertyName": d }));
        }
        if m.get("type").and_then(Value::as_str) == Some("file") {
            m.insert("type".into(), json!("string"));
            m.insert("format".into(), json!("binary"));
        }
        if m.remove("x-nullable") == Some(json!(true)) {
            m.insert("nullable".into(), json!(true));
        }
        for (k, c) in m.iter_mut() {
            match k.as_str() {
                "properties" => {
                    if let Value::Object(ps) = c {
                        stack.extend(ps.values_mut());
                    }
                }
                "allOf" => {
                    if let Value::Array(a) = c {
                        stack.extend(a.iter_mut());
                    }
                }
                "items" | "additionalProperties" => stack.push(c),
                _ => {}
            }
        }
    }
    s
}

fn security_scheme(s: &Value) -> Value {
    match s.get("type").and_then(Value::as_str) {
        Some("basic") => {
            let mut out = json!({"type": "http", "scheme": "basic"});
            if let Some(d) = s.get("description") {
                out["description"] = d.clone();
            }
            out
        }
        Some("oauth2") => {
            let flow = match s.get("flow").and_then(Value::as_str) {
                Some("application") => "clientCredentials",
                Some("accessCode") => "authorizationCode",
                Some(f) => f,
                None => "implicit",
            };
            let mut f = Map::new();
            for k in ["authorizationUrl", "tokenUrl"] {
                if let Some(v) = s.get(k) {
                    f.insert(k.into(), v.clone());
                }
            }
            f.insert(
                "scopes".into(),
                s.get("scopes").cloned().unwrap_or(json!({})),
            );
            let mut out = json!({"type": "oauth2", "flows": { flow: f }});
            if let Some(d) = s.get("description") {
                out["description"] = d.clone();
            }
            out
        }
        _ => s.clone(),
    }
}

/// `#/definitions/`, `#/parameters/` and `#/responses/` references follow
/// their objects under `components`.
fn rewrite_refs(v: &mut Value) {
    let mut stack = vec![v];
    while let Some(v) = stack.pop() {
        match v {
            Value::Object(m) => {
                if let Some(Value::String(r)) = m.get_mut("$ref") {
                    for (from, to) in [
                        ("#/definitions/", "#/components/schemas/"),
                        ("#/parameters/", "#/components/parameters/"),
                        ("#/responses/", "#/components/responses/"),
                    ] {
                        if let Some(rest) = r.strip_prefix(from) {
                            *r = format!("{to}{rest}");
                            break;
                        }
                    }
                }
                for (k, c) in m.iter_mut() {
                    if !DATA_KEYS.contains(&k.as_str()) {
                        stack.push(c);
                    }
                }
            }
            Value::Array(a) => stack.extend(a.iter_mut()),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swagger_2_0_converts_to_openapi_3_0() {
        let doc = json!({
            "swagger": "2.0",
            "info": {"title": "t", "version": "1"},
            "host": "api.example.com",
            "basePath": "/v1",
            "schemes": ["https"],
            "consumes": ["application/json"],
            "produces": ["application/json"],
            "paths": {"/pets/{id}": {
                "parameters": [{"name": "id", "in": "path", "required": true, "type": "integer"}],
                "put": {
                    "parameters": [
                        {"name": "tags", "in": "query", "type": "array", "items": {"type": "string"}, "collectionFormat": "multi"},
                        {"name": "pet", "in": "body", "required": true, "schema": {"$ref": "#/definitions/Pet"}}
                    ],
                    "responses": {"200": {"description": "ok", "schema": {"$ref": "#/definitions/Pet"},
                        "headers": {"X-Rate": {"type": "integer"}}}}
                }}},
            "definitions": {"Pet": {"type": "object", "discriminator": "kind", "x-nullable": true,
                "properties": {"kind": {"type": "string"}, "photo": {"type": "file"}}}},
            "securityDefinitions": {"b": {"type": "basic"},
                "o": {"type": "oauth2", "flow": "accessCode", "authorizationUrl": "https://a/a", "tokenUrl": "https://a/t", "scopes": {}}}
        });
        let (out, warnings) = convert(doc).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(out["openapi"], "3.0.3");
        assert_eq!(out["servers"][0]["url"], "https://api.example.com/v1");
        let put = &out["paths"]["/pets/{id}"]["put"];
        assert_eq!(
            out["paths"]["/pets/{id}"]["parameters"][0]["schema"],
            json!({"type": "integer"})
        );
        assert_eq!(put["parameters"][0]["style"], "form");
        assert_eq!(put["parameters"][0]["explode"], true);
        assert_eq!(put["requestBody"]["required"], true);
        assert_eq!(
            put["requestBody"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/Pet"
        );
        assert_eq!(
            put["responses"]["200"]["headers"]["X-Rate"]["schema"],
            json!({"type": "integer"})
        );
        let pet = &out["components"]["schemas"]["Pet"];
        assert_eq!(pet["discriminator"], json!({"propertyName": "kind"}));
        assert_eq!(pet["nullable"], true);
        assert_eq!(
            pet["properties"]["photo"],
            json!({"type": "string", "format": "binary"})
        );
        assert_eq!(
            out["components"]["securitySchemes"]["b"],
            json!({"type": "http", "scheme": "basic"})
        );
        assert!(
            out["components"]["securitySchemes"]["o"]["flows"]["authorizationCode"].is_object()
        );
    }

    #[test]
    fn form_data_becomes_a_form_body() {
        let doc = json!({
            "swagger": "2.0",
            "info": {"title": "t", "version": "1"},
            "paths": {"/up": {"post": {
                "parameters": [
                    {"name": "file", "in": "formData", "type": "file", "required": true},
                    {"name": "tags", "in": "formData", "type": "array", "items": {"type": "string"}}
                ],
                "responses": {"204": {"description": "ok"}}}}}
        });
        let (out, _) = convert(doc).unwrap();
        assert_eq!(out["servers"][0]["url"], "/");
        let rb = &out["paths"]["/up"]["post"]["requestBody"];
        assert_eq!(rb["required"], true);
        let m = &rb["content"]["multipart/form-data"];
        assert_eq!(m["schema"]["required"], json!(["file"]));
        assert_eq!(
            m["encoding"]["tags"],
            json!({"style": "form", "explode": false})
        );
    }

    #[test]
    fn other_swagger_versions_are_refused() {
        assert!(matches!(
            convert(json!({"swagger": "1.2"})),
            Err(LoadError::UnsupportedVersion(_))
        ));
    }
}
