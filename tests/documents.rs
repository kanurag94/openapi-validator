//! Descriptions spread over several documents (OpenAPI 3.1.1 §4.3,
//! §4.8.23; RFC 3986 §5.2). The caller supplies every document; nothing
//! is fetched.

use openapi_validator::{LoadError, Options, Request, Spec};

const ENTRY: &str = r#"
openapi: 3.1.0
info: {title: pets, version: "1"}
paths:
  /pets/{id}:
    $ref: paths/pets.yaml#/item
"#;

const PATHS: &str = r#"
item:
  get:
    parameters:
      - $ref: ../components/parameters.yaml#/id
    responses:
      "200":
        description: ok
  put:
    parameters:
      - $ref: ../components/parameters.yaml#/id
    requestBody:
      required: true
      content:
        application/json:
          schema:
            $ref: ../schemas/pet.json
    responses:
      "204":
        description: done
"#;

const PARAMETERS: &str = r#"
id:
  name: id
  in: path
  required: true
  schema: {type: integer, minimum: 1}
"#;

const PET: &str = r#"{
  "type": "object",
  "required": ["name", "owner"],
  "properties": {
    "name": {"type": "string", "minLength": 1},
    "owner": {"$ref": "person.json#/$defs/person"}
  }
}"#;

const PERSON: &str = r#"{"$defs": {"person": {"type": "object", "required": ["email"],
  "properties": {"email": {"type": "string", "maxLength": 20}}}}}"#;

fn spec() -> Spec {
    Spec::load_documents(
        "https://api.example.com/spec/openapi.yaml",
        ENTRY.as_bytes(),
        &[
            ("paths/pets.yaml", PATHS.as_bytes()),
            ("components/parameters.yaml", PARAMETERS.as_bytes()),
            (
                "https://api.example.com/spec/schemas/pet.json",
                PET.as_bytes(),
            ),
            ("schemas/person.json", PERSON.as_bytes()),
        ],
        Options::default(),
    )
    .unwrap_or_else(|e| panic!("{e}"))
}

fn put(s: &Spec, path: &str, body: &str) -> openapi_validator::Report {
    s.validate_request(&Request {
        method: "PUT",
        path,
        headers: &[("Content-Type", "application/json")],
        body: body.as_bytes(),
        ..Request::default()
    })
}

#[test]
fn references_resolve_across_documents() {
    let s = spec();
    let get = |p| {
        s.validate_request(&Request {
            method: "GET",
            path: p,
            ..Request::default()
        })
    };
    assert!(get("/pets/3").is_valid());
    assert_eq!(get("/pets/0").problems()[0].location, "path.id");
    assert!(put(
        &s,
        "/pets/3",
        r#"{"name": "rex", "owner": {"email": "a@b.c"}}"#
    )
    .is_valid());
    let r = put(&s, "/pets/3", r#"{"name": "rex", "owner": {}}"#);
    assert!(
        r.problems()
            .iter()
            .any(|p| p.location == "body/owner/email"),
        "{r}"
    );
}

#[test]
fn a_document_not_supplied_is_a_load_error() {
    let e = Spec::load_documents(
        "https://api.example.com/spec/openapi.yaml",
        ENTRY.as_bytes(),
        &[("paths/pets.yaml", PATHS.as_bytes())],
        Options::default(),
    )
    .err()
    .expect("missing documents refused");
    assert!(matches!(e, LoadError::ExternalRef { .. }), "{e}");
    assert!(e.to_string().contains("parameters.yaml"), "{e}");
}

#[test]
fn a_single_document_still_refuses_external_references() {
    let e = Spec::load(ENTRY.as_bytes(), Options::default())
        .err()
        .expect("refused");
    assert!(matches!(e, LoadError::ExternalRef { .. }), "{e}");
}
