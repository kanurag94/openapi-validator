//! Runtime validation of HTTP requests and responses against OpenAPI 3.0 and
//! 3.1 documents, built to run on every request in a proxy.
//!
//! ```
//! use openapi_validator::{Options, Request, Response, Spec};
//!
//! let doc = r#"
//! openapi: 3.1.0
//! info: {title: pets, version: "1"}
//! paths:
//!   /pets/{id}:
//!     get:
//!       parameters:
//!         - {name: id, in: path, required: true, schema: {type: integer}}
//!       responses:
//!         "200":
//!           description: ok
//!           content:
//!             application/json:
//!               schema: {type: object, required: [name]}
//! "#;
//! // Load once; share the `Spec` across threads.
//! let spec = Spec::load(doc.as_bytes(), Options::default()).unwrap();
//!
//! let req = Request { method: "GET", path: "/pets/seven", ..Request::default() };
//! let report = spec.validate_request(&req);
//! assert!(!report.is_valid());
//! assert_eq!(report.problems()[0].location, "path.id");
//!
//! let req = Request { method: "GET", path: "/pets/7", ..Request::default() };
//! let resp = Response {
//!     status: 200,
//!     headers: &[("Content-Type", "application/json")],
//!     body: br#"{"name": "rex"}"#,
//! };
//! assert!(spec.validate_response(&req, &resp).is_valid());
//! ```
//!
//! Behaviour where the specifications leave a choice is listed in
//! `SPEC_DECISIONS.md`.

#![forbid(unsafe_code)]

mod body;
mod bundle;
mod doc;
pub mod format;
mod http;
pub mod json;
mod lint;
mod media;
mod model;
pub mod number;
mod options;
mod params;
mod path;
pub mod pattern;
mod report;
mod schema;
mod security;
mod server;
mod spec;
mod validate;
mod xml;
pub mod yaml;

pub use doc::{LoadError, Version, Warning};
pub use http::{Request, Response};
pub use options::{
    Credential, CredentialLocation, DiscriminatorMode, FormatAssertion, IntegerSemantics, Options,
    PatternEngine, ReadWriteRequired, SecurityHook, ServerPaths, DEFAULT_IGNORED_HEADERS,
};
pub use report::{MatchedOperation, Problem, ProblemKind, Report};
pub use spec::Spec;
