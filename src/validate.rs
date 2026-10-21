//! Checks applied to a routed request or response.

use serde_json::Value;

use crate::http::Request;
use crate::json;
use crate::media::{best_match, MediaType};
use crate::model::{Body, MediaEntry};
use crate::params::{
    decode, header_index, header_value, path_value, query_value, readings, split_cookies,
    split_query, Found, In, Pair, Param, ParamValue,
};
use crate::path::Vars;
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

/// Header names never reported as unknown: OpenAPI ignores parameters with
/// these names (§4.8.12.2), and `Cookie` is checked through cookie
/// parameters.
const IMPLICIT_HEADERS: &[&str] = &["accept", "content-type", "authorization", "cookie"];

impl Spec {
    /// Checks one parameter value against its schema, accepting it when any
    /// reading of the text is valid.
    fn check_param_value(&self, p: &Param, v: Value, label: &str, report: &mut Report) {
        match &p.value {
            ParamValue::Schema { id, shape } => {
                let all = readings(v, shape);
                let mut first_err = None;
                for (i, r) in all.iter().enumerate() {
                    match self.store.is_valid(*id, r) {
                        Ok(true) => return,
                        Ok(false) if i == 0 => first_err = Some(r),
                        Ok(false) => {}
                        Err(_) => {
                            first_err = Some(r);
                            break;
                        }
                    }
                }
                if let Some(r) = first_err {
                    self.check_value(*id, r, label, ProblemKind::InvalidParameter, report);
                }
            }
            ParamValue::Content { .. } | ParamValue::Any => {}
        }
    }

    /// A `content` parameter's text: JSON media types are parsed, others are
    /// checked as a string. Without a schema the value is unconstrained.
    fn check_content_param(
        &self,
        media: &MediaType,
        id: Option<SchemaId>,
        text: &str,
        label: &str,
        report: &mut Report,
    ) {
        let Some(id) = id else {
            return;
        };
        let value = if media.is_json() {
            match json::parse(
                text.as_bytes(),
                json::Limits {
                    max_depth: self.options.max_body_depth,
                },
            ) {
                Ok(v) => v,
                Err(e) => {
                    report.push(
                        ProblemKind::InvalidParameter,
                        label,
                        format!("is not valid JSON: {e}"),
                    );
                    return;
                }
            }
        } else {
            Value::String(text.to_owned())
        };
        self.check_value(id, &value, label, ProblemKind::InvalidParameter, report);
    }

    pub(crate) fn check_params(
        &self,
        params: &[Param],
        vars: &Vars<'_>,
        req: &Request<'_>,
        extra: &Declared,
        report: &mut Report,
    ) {
        let plus = self.options.query_plus_as_space;
        let (pairs, bad) = split_query(req.query, plus);
        for b in bad {
            report.push(
                ProblemKind::MalformedQuery,
                "query",
                format!(
                    "the pair {:?} has an invalid percent-escape",
                    truncate(b, 60)
                ),
            );
        }
        let headers = header_index(req.headers);
        let cookie_pairs: Vec<Pair<'_>> =
            split_cookies(headers.get("cookie").into_iter().flatten().copied())
                .into_iter()
                .map(|(n, raw)| Pair {
                    name: decode(n, false).unwrap_or_else(|_| n.to_owned()),
                    raw,
                })
                .collect();
        let mut used_query: Vec<String> = Vec::new();
        let mut used_cookies: Vec<String> = Vec::new();

        for p in params {
            let label = p.label();
            match (&p.value, p.location) {
                (ParamValue::Content { media, id }, loc) => {
                    let texts: Vec<String> = match loc {
                        In::Path => vars
                            .iter()
                            .filter(|(n, _)| *n == p.name)
                            .map(|(_, raw)| {
                                decode(raw, false).unwrap_or_else(|_| (*raw).to_owned())
                            })
                            .collect(),
                        In::Query | In::Cookie => {
                            let src = if loc == In::Query {
                                &pairs
                            } else {
                                &cookie_pairs
                            };
                            let used = if loc == In::Query {
                                &mut used_query
                            } else {
                                &mut used_cookies
                            };
                            let mut out = Vec::new();
                            for q in src.iter().filter(|q| q.name == p.name) {
                                used.push(q.name.clone());
                                match decode(q.raw, plus && loc == In::Query) {
                                    Ok(t) => out.push(t),
                                    Err(()) => report.push(
                                        ProblemKind::MalformedQuery,
                                        label.as_str(),
                                        "has an invalid percent-escape",
                                    ),
                                }
                            }
                            out
                        }
                        In::Header => headers
                            .get(&p.name.to_ascii_lowercase())
                            .map(|l| l.iter().map(|s| (*s).to_owned()).collect())
                            .unwrap_or_default(),
                    };
                    if texts.is_empty() {
                        if p.required {
                            report.push(
                                ProblemKind::MissingParameter,
                                label.as_str(),
                                "is required",
                            );
                        }
                        continue;
                    }
                    for t in texts {
                        self.check_content_param(media, *id, &t, &label, report);
                    }
                }
                (ParamValue::Any, loc) => {
                    let present = match loc {
                        In::Path => vars.iter().any(|(n, _)| *n == p.name),
                        In::Query => {
                            let found = pairs.iter().any(|q| q.name == p.name);
                            if found {
                                used_query.push(p.name.clone());
                            }
                            found
                        }
                        In::Cookie => {
                            let found = cookie_pairs.iter().any(|q| q.name == p.name);
                            if found {
                                used_cookies.push(p.name.clone());
                            }
                            found
                        }
                        In::Header => headers.contains_key(&p.name.to_ascii_lowercase()),
                    };
                    if !present && p.required {
                        report.push(ProblemKind::MissingParameter, label.as_str(), "is required");
                    }
                }
                (ParamValue::Schema { shape, .. }, In::Path) => {
                    let Some((_, raw)) = vars.iter().find(|(n, _)| *n == p.name) else {
                        continue;
                    };
                    match path_value(p, raw, shape) {
                        Ok(v) => self.check_param_value(p, v, &label, report),
                        Err(m) => report.push(ProblemKind::InvalidParameter, label.as_str(), m),
                    }
                }
                (ParamValue::Schema { shape, .. }, In::Header) => {
                    let lines = headers.get(&p.name.to_ascii_lowercase());
                    match lines {
                        None => {
                            if p.required {
                                report.push(
                                    ProblemKind::MissingParameter,
                                    label.as_str(),
                                    "is required",
                                );
                            }
                        }
                        // Every line of a repeated header is checked.
                        Some(lines) => {
                            for line in lines {
                                self.check_param_value(
                                    p,
                                    header_value(line, shape, p.explode),
                                    &label,
                                    report,
                                );
                            }
                        }
                    }
                }
                (ParamValue::Schema { shape, .. }, loc) => {
                    let (src, used, plus) = if loc == In::Query {
                        (&pairs, &mut used_query, plus)
                    } else {
                        (&cookie_pairs, &mut used_cookies, false)
                    };
                    match query_value(p, src, shape, plus, used) {
                        Found::Absent => {
                            if p.required {
                                report.push(
                                    ProblemKind::MissingParameter,
                                    label.as_str(),
                                    "is required",
                                );
                            }
                        }
                        Found::Malformed(m) => {
                            report.push(ProblemKind::MalformedQuery, label.as_str(), m)
                        }
                        Found::Values(vs) => {
                            for v in vs {
                                self.check_param_value(p, v, &label, report);
                            }
                        }
                    }
                }
            }
        }

        if !self.options.allow_unknown_query {
            for q in &pairs {
                if !used_query.contains(&q.name) && !extra.query.contains(&q.name) {
                    report.push(
                        ProblemKind::UnknownParameter,
                        format!("query.{}", truncate(&q.name, 100)),
                        "is not a declared query parameter",
                    );
                }
            }
        }
        if !self.options.allow_unknown_cookies {
            for q in &cookie_pairs {
                if !used_cookies.contains(&q.name) && !extra.cookies.contains(&q.name) {
                    report.push(
                        ProblemKind::UnknownParameter,
                        format!("cookie.{}", truncate(&q.name, 100)),
                        "is not a declared cookie",
                    );
                }
            }
        }
        if !self.options.allow_unknown_headers {
            let mut names: Vec<&String> = headers.keys().collect();
            names.sort();
            for n in names {
                let declared = params
                    .iter()
                    .any(|p| p.location == In::Header && p.name.eq_ignore_ascii_case(n))
                    || IMPLICIT_HEADERS.contains(&n.as_str())
                    || extra.headers.iter().any(|h| h.eq_ignore_ascii_case(n))
                    || self
                        .options
                        .ignored_headers
                        .iter()
                        .any(|h| h.eq_ignore_ascii_case(n));
                if !declared {
                    report.push(
                        ProblemKind::UnknownParameter,
                        format!("header.{}", truncate(n, 100)),
                        "is not a declared header",
                    );
                }
            }
        }
    }
}

/// Names declared outside Parameter Objects (security schemes).
#[derive(Debug, Default)]
pub(crate) struct Declared {
    pub query: Vec<String>,
    pub headers: Vec<String>,
    pub cookies: Vec<String>,
}
