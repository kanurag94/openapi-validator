//! Runtime validation of HTTP requests and responses against OpenAPI 3.0 and
//! 3.1 documents.

#![forbid(unsafe_code)]

pub mod format;
pub mod json;
pub mod number;
pub mod pattern;
pub mod yaml;
