//! Runtime validation of HTTP requests and responses against OpenAPI 3.0 and
//! 3.1 documents.

#![forbid(unsafe_code)]

mod doc;
pub mod format;
mod http;
pub mod json;
mod media;
mod model;
pub mod number;
mod options;
mod params;
mod path;
pub mod pattern;
mod report;
mod schema;
mod server;
mod spec;
mod validate;
pub mod yaml;

pub use doc::{LoadError, Version, Warning};
pub use http::{Request, Response};
pub use options::{
    Credential, CredentialLocation, Options, SecurityHook, ServerPaths, DEFAULT_IGNORED_HEADERS,
};
pub use report::{MatchedOperation, Problem, ProblemKind, Report};
pub use spec::Spec;
