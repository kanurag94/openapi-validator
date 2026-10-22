//! A synthetic OpenAPI document for build-cost and latency measurements:
//! `paths` path templates, each with a GET (query, path and header
//! parameters, a response through a `$ref` chain) and a POST (a request body
//! through a `$ref` chain), and component schemas chained `depth` deep.

#![allow(dead_code)]

use std::fmt::Write;

pub fn document(paths: usize, chain: usize) -> String {
    let mut s = String::with_capacity(paths * 900);
    s.push_str(r#"{"openapi": "3.1.0", "info": {"title": "synthetic", "version": "1"}, "servers": [{"url": "https://api.example.com/v1"}], "paths": {"#);
    for i in 0..paths {
        if i > 0 {
            s.push(',');
        }
        let model = i % 200;
        write!(
            s,
            r##""/resource{i}/{{id}}/items": {{
  "parameters": [{{"name": "id", "in": "path", "required": true, "schema": {{"type": "string", "pattern": "^[a-z0-9-]{{1,36}}$"}}}}],
  "get": {{"operationId": "list{i}",
    "parameters": [
      {{"name": "limit", "in": "query", "schema": {{"type": "integer", "minimum": 1, "maximum": 100}}}},
      {{"name": "tags", "in": "query", "schema": {{"type": "array", "items": {{"type": "string"}}, "maxItems": 10}}}},
      {{"name": "X-Request-Id", "in": "header", "schema": {{"type": "string", "format": "uuid"}}}}],
    "responses": {{"200": {{"description": "ok", "content": {{"application/json": {{"schema": {{"type": "array", "items": {{"$ref": "#/components/schemas/Model{model}_0"}}}}}}}}}},
                   "default": {{"description": "error", "content": {{"application/json": {{"schema": {{"$ref": "#/components/schemas/Error"}}}}}}}}}}}},
  "post": {{"operationId": "create{i}",
    "requestBody": {{"required": true, "content": {{"application/json": {{"schema": {{"$ref": "#/components/schemas/Model{model}_0"}}}}}}}},
    "responses": {{"201": {{"description": "created"}}}}}}}}"##
        )
        .unwrap();
    }
    s.push_str(r#"}, "components": {"schemas": {"Error": {"type": "object", "required": ["code"], "properties": {"code": {"type": "integer"}, "message": {"type": "string"}}}"#);
    for m in 0..200 {
        for d in 0..chain {
            let next = if d + 1 < chain {
                format!(
                    r##", "child": {{"$ref": "#/components/schemas/Model{m}_{}"}}"##,
                    d + 1
                )
            } else {
                String::new()
            };
            write!(
                s,
                r##","Model{m}_{d}": {{"type": "object", "required": ["id", "name"], "properties": {{
  "id": {{"type": "string", "readOnly": true, "format": "uuid"}},
  "name": {{"type": "string", "minLength": 1, "maxLength": 200}},
  "count": {{"type": "integer", "format": "int64", "minimum": 0}},
  "price": {{"type": "number", "multipleOf": 0.01}},
  "status": {{"type": "string", "enum": ["active", "inactive", "pending"]}},
  "created": {{"type": "string", "format": "date-time"}},
  "labels": {{"type": "object", "additionalProperties": {{"type": "string"}}}}{next}}}}}"##
            )
            .unwrap();
        }
    }
    s.push_str("}}}");
    s
}

/// A valid POST body for `Model*_0` with `depth` nested children. `id` is
/// readOnly but required, and in OpenAPI 3.1 `required` still applies to
/// requests, so every level sends it.
pub fn body(depth: usize) -> String {
    let mut b = String::from(
        r#"{"id": "2eb8aa08-aa98-11ea-b4aa-73b441d16380", "name": "leaf", "count": 1, "price": 9.99, "status": "active"}"#,
    );
    for _ in 0..depth {
        b = format!(
            r#"{{"id": "2eb8aa08-aa98-11ea-b4aa-73b441d16380", "name": "node", "count": 2, "price": 0.5, "status": "pending", "created": "2024-01-01T00:00:00Z", "labels": {{"a": "b"}}, "child": {b}}}"#
        );
    }
    b
}
