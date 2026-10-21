//! Checks applied to a routed request or response.

use serde_json::Value;

use crate::http::Request;
use crate::json;
use crate::media::{best_match, MediaType};
use crate::model::{Body, MediaEntry};
use crate::report::{ProblemKind, Report};
use crate::schema::{Outcome, SchemaId};
use crate::spec::{truncate, Spec};

/// Header values by case-insensitive name (RFC 9110 §5.1), in arrival order,
/// with surrounding whitespace removed (RFC 9110 §5.5).
pub(crate) fn header_values<'a>(
    headers: &'a [(&'a str, &'a str)],
    name: &'a str,
) -> impl Iterator<Item = &'a str> + 'a {
    headers
        .iter()
        .filter(move |(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.trim_matches([' ', '\t']))
}

impl Spec {
    /// Checks a value against a schema and records what is wrong with it.
    pub(crate) fn check_value(
        &self,
        id: SchemaId,
        value: &Value,
        location: &str,
        kind: ProblemKind,
        report: &mut Report,
    ) {
        let room = self
            .options
            .max_problems
            .saturating_sub(report.problems().len())
            .max(1);
        match self.store.check(id, value, room) {
            Outcome::Valid => {}
            Outcome::Invalid(found, more) => {
                for (path, msg) in found {
                    report.push(kind, format!("{location}{path}"), msg);
                }
                report.add_omitted(more);
            }
            Outcome::SchemaError(e) => report.push(
                ProblemKind::SchemaError,
                location,
                format!("the schema could not be compiled: {}", truncate(&e, 200)),
            ),
        }
    }

    /// The `content` entry for a message's `Content-Type`, or a problem.
    pub(crate) fn select_media<'b>(
        &self,
        content: &'b [(MediaType, MediaEntry)],
        content_type: Option<&str>,
        report: &mut Report,
    ) -> Option<(MediaType, &'b MediaEntry)> {
        let accepted = || {
            content
                .iter()
                .map(|(_, e)| e.key.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let actual = match content_type {
            Some(ct) => match MediaType::parse(ct) {
                Some(mt) => mt,
                None => {
                    report.push(
                        ProblemKind::UnsupportedMediaType,
                        "header.content-type",
                        format!("{:?} is not a valid media type", truncate(ct, 100)),
                    );
                    return None;
                }
            },
            // RFC 9110 §8.3: without Content-Type the recipient may assume a
            // type. With a single declared type, that one is assumed.
            None => match content {
                [(mt, e)] if mt.ty != "*" && mt.sub != "*" => return Some((mt.clone(), e)),
                _ => {
                    report.push(
                        ProblemKind::UnsupportedMediaType,
                        "header.content-type",
                        format!("the body has no Content-Type; accepted: {}", accepted()),
                    );
                    return None;
                }
            },
        };
        match best_match(content, &actual) {
            Some((_, e)) => Some((actual, e)),
            None => {
                report.push(
                    ProblemKind::UnsupportedMediaType,
                    "header.content-type",
                    format!(
                        "{}/{} is not accepted; accepted: {}",
                        actual.ty,
                        actual.sub,
                        accepted()
                    ),
                );
                None
            }
        }
    }

    /// Parses a body by its media type and checks it against the schema.
    pub(crate) fn check_payload(
        &self,
        actual: &MediaType,
        entry: &MediaEntry,
        bytes: &[u8],
        location: &str,
        report: &mut Report,
    ) {
        if actual.is_json() {
            let limits = json::Limits {
                max_depth: self.options.max_body_depth,
            };
            match json::parse(bytes, limits) {
                Ok(v) => {
                    if let Some(id) = entry.schema {
                        self.check_value(id, &v, location, ProblemKind::SchemaViolation, report);
                    }
                }
                Err(e) => report.push(
                    ProblemKind::MalformedBody,
                    location,
                    format!("the body is not valid JSON: {e}"),
                ),
            }
        }
    }

    pub(crate) fn check_request_body(
        &self,
        body: Option<&Body>,
        req: &Request<'_>,
        report: &mut Report,
    ) {
        if !self.options.validate_body {
            return;
        }
        let Some(body) = body else {
            return;
        };
        if req.body.is_empty() {
            if body.required {
                report.push(
                    ProblemKind::MissingBody,
                    "body",
                    "a request body is required",
                );
            }
            return;
        }
        if req.body.len() > self.options.max_body_bytes {
            report.push(
                ProblemKind::BodyTooLarge,
                "body",
                format!(
                    "the body is larger than {} bytes",
                    self.options.max_body_bytes
                ),
            );
            return;
        }
        let ct = header_values(req.headers, "content-type").next();
        if let Some((actual, entry)) = self.select_media(&body.content, ct, report) {
            self.check_payload(&actual, entry, req.body, "body", report);
        }
    }
}
