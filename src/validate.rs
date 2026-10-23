//! Checks applied to a routed request or response.

use serde_json::Value;

use crate::body;
use crate::doc::Version;
use crate::http::{Request, Response};
use crate::json;
use crate::media::{best_match, MediaType};
use crate::model::{Body, MediaEntry, Resp};
use crate::params::{
    coerce, decode, header_index, header_value, path_value, query_value, readings, split_cookies,
    split_query, Found, In, Pair, Param, ParamValue, Shape, Style, Types,
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

    /// The `content` entry for a message's `Content-Type`. A body without a
    /// Content-Type, or of a type `content` does not describe, has no schema
    /// to check: §4.8.13.1 and §4.8.17.1 list the media types described and
    /// make no other type invalid, and RFC 9110 §8.3 leaves an unlabelled
    /// body's type to the recipient. Strict mode refuses both, assuming a
    /// single declared type for an unlabelled body.
    pub(crate) fn select_media<'b>(
        &self,
        content: &'b [(MediaType, MediaEntry)],
        content_type: Option<&str>,
        report: &mut Report,
    ) -> Option<(MediaType, &'b MediaEntry)> {
        let strict = self.options.strict_mode;
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
            None if !strict => return None,
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
        let found = best_match(content, &actual);
        if found.is_none() && strict {
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
        }
        found.map(|(_, e)| (actual, e))
    }

    /// Checks the items of a sequential body: `itemSchema` each item, and
    /// `schema` the array of them.
    fn check_items(
        &self,
        entry: &MediaEntry,
        items: Vec<Value>,
        location: &str,
        report: &mut Report,
    ) {
        if let Some(id) = entry.item_schema {
            for (i, item) in items.iter().enumerate() {
                self.check_value(
                    id,
                    item,
                    &format!("{location}/{i}"),
                    ProblemKind::SchemaViolation,
                    report,
                );
            }
        }
        if let Some(id) = entry.schema {
            let all = Value::Array(items);
            self.check_value(id, &all, location, ProblemKind::SchemaViolation, report);
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
        let depth = self.store.depth_limit(self.options.max_body_depth);
        // OpenAPI 3.2 §4.14.3.1: a sequential body is an array of its items;
        // `schema` checks the array and `itemSchema` each item.
        if let Some(seq) = actual.sequence().filter(|_| self.version >= Version::V32) {
            match body::sequence(bytes, actual, seq, depth) {
                Ok(items) => self.check_items(entry, items, location, report),
                Err(m) => report.push(ProblemKind::MalformedBody, location, m),
            }
            return;
        }
        // 3.2.1 §4.14.5.1: form-data whose schema is an array keeps part
        // order, one {name: value} item per part.
        let named = actual.is_multipart_form() && matches!(entry.shape, Shape::Array(_));
        if self.version >= Version::V32
            && actual.ty == "multipart"
            && (!actual.is_multipart_form()
                || named
                || !entry.prefix_encoding.is_empty()
                || entry.item_encoding.is_some())
        {
            match self.positional(
                bytes,
                actual,
                &entry.prefix_encoding,
                entry.item_encoding.as_deref(),
                &entry.parts,
                location,
                0,
                named,
                report,
            ) {
                Ok(items) => self.check_items(entry, items, location, report),
                Err(m) => report.push(ProblemKind::MalformedBody, location, m),
            }
            return;
        }
        let parsed: Result<Option<Value>, String> = if actual.is_json() {
            json::parse(bytes, json::Limits { max_depth: depth })
                .map(Some)
                .map_err(|e| format!("the body is not valid JSON: {e}"))
        } else if actual.is_form() {
            match std::str::from_utf8(bytes) {
                Ok(t) => body::form(t, &entry.props, &entry.encodings, depth).map(Some),
                Err(_) => Err("the body is not valid UTF-8".into()),
            }
        } else if actual.is_multipart_form() {
            let styled = self.multipart_styles;
            body::multipart(bytes, actual, &entry.props, &entry.encodings, depth, styled).map(
                |(v, parts)| {
                    self.check_parts(&entry.encodings, &parts, location, report);
                    Some(v)
                },
            )
        } else if actual.is_xml() {
            body::xml(bytes, actual, depth, entry.xml.as_ref())
        } else if actual.ty == "text" {
            body::text(bytes, actual).map(|t| Some(coerce_text(t, &entry.shape)))
        } else {
            // Other media types (binary, images, ...) are not parsed.
            Ok(None)
        };
        match parsed {
            Ok(Some(v)) => {
                if let Some(id) = entry.schema {
                    // JSON carries its own types; only values read from text
                    // get a second, all-strings reading.
                    if actual.is_json() {
                        self.check_value(id, &v, location, ProblemKind::SchemaViolation, report);
                        return;
                    }
                    let all = readings(v, &entry.shape);
                    if all.len() > 1
                        && all[1..]
                            .iter()
                            .any(|r| self.store.is_valid(id, r) == Ok(true))
                    {
                        return;
                    }
                    self.check_value(id, &all[0], location, ProblemKind::SchemaViolation, report);
                }
            }
            Ok(None) => {}
            Err(m) => report.push(ProblemKind::MalformedBody, location, m),
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
            if let Some(bytes) = self.decoded(req.headers, req.body, report) {
                self.check_payload(&actual, entry, &bytes, "body", report);
            }
        }
    }
}

/// Fields defined as a single value, which a sender MUST NOT repeat
/// (RFC 9110 §5.3; §8.3 Content-Type, §11.6.2 Authorization).
const SINGLE_VALUE_HEADERS: &[&str] = &["content-type", "authorization"];

/// Reports single-value header fields sent on more than one line.
pub(crate) fn check_single_headers(headers: &[(&str, &str)], report: &mut Report) {
    for name in SINGLE_VALUE_HEADERS {
        let n = headers
            .iter()
            .filter(|(h, _)| h.eq_ignore_ascii_case(name))
            .count();
        if n > 1 {
            report.push(
                ProblemKind::InvalidParameter,
                format!("header.{name}"),
                format!("is sent on {n} lines but takes a single value"),
            );
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
            ParamValue::Content { .. } | ParamValue::QueryString { .. } | ParamValue::Any => {}
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
                    max_depth: self.store.depth_limit(self.options.max_body_depth),
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
        // An `in: querystring` parameter takes the whole query string, which
        // then need not be name-value pairs.
        let whole_query = params.iter().any(|p| p.location == In::QueryString);
        let (pairs, bad) = split_query(req.query, plus);
        for b in bad.into_iter().filter(|_| !whole_query) {
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
        // `style: cookie` removes no percent-encoding (OpenAPI 3.2 Appendix
        // D): its pairs are escaped so that decoding gives them back as sent.
        let escaped: Vec<(String, String)> = if params.iter().any(|p| p.style == Style::Cookie) {
            split_cookies(headers.get("cookie").into_iter().flatten().copied())
                .into_iter()
                .map(|(n, raw)| (n.to_owned(), raw.replace('%', "%25")))
                .collect()
        } else {
            Vec::new()
        };
        let literal_cookies: Vec<Pair<'_>> = escaped
            .iter()
            .map(|(n, raw)| Pair {
                name: n.clone(),
                raw,
            })
            .collect();
        let mut used_query: Vec<String> = Vec::new();
        let mut used_cookies: Vec<String> = Vec::new();

        for p in params {
            let label = p.label();
            match (&p.value, p.location) {
                (ParamValue::QueryString { media, entry }, _) => {
                    if req.query.is_empty() {
                        if p.required {
                            report.push(
                                ProblemKind::MissingParameter,
                                "querystring",
                                "is required",
                            );
                        }
                        continue;
                    }
                    // Form content is already URL-ready; any other media
                    // type was percent-encoded into the URL (Appendix E).
                    let text = if media.is_form() {
                        req.query.to_owned()
                    } else {
                        match decode(req.query, false) {
                            Ok(t) => t,
                            Err(()) => {
                                report.push(
                                    ProblemKind::MalformedQuery,
                                    "querystring",
                                    "has an invalid percent-escape",
                                );
                                continue;
                            }
                        }
                    };
                    // Problems are a parameter's, whatever the media type.
                    let mut scratch = self.new_report();
                    self.check_payload(media, entry, text.as_bytes(), "querystring", &mut scratch);
                    for q in scratch.problems() {
                        let kind = match q.kind {
                            ProblemKind::SchemaViolation | ProblemKind::MalformedBody => {
                                ProblemKind::InvalidParameter
                            }
                            k => k,
                        };
                        report.push(kind, q.location.as_str(), q.message.as_str());
                    }
                    report.add_omitted(scratch.omitted());
                }
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
                            .map(|l| vec![l.join(", ")])
                            .unwrap_or_default(),
                        In::QueryString => Vec::new(),
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
                        In::QueryString => !req.query.is_empty(),
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
                        // RFC 9110 §5.3: field lines with one name are one
                        // value, their contents joined with commas.
                        Some(lines) => {
                            let joined = lines.join(", ");
                            self.check_param_value(
                                p,
                                header_value(&joined, shape, p.explode),
                                &label,
                                report,
                            );
                        }
                    }
                }
                (ParamValue::Schema { shape, .. }, loc) => {
                    let (src, used, plus) = if loc == In::Query {
                        (&pairs, &mut used_query, plus)
                    } else if p.style == Style::Cookie {
                        (&literal_cookies, &mut used_cookies, false)
                    } else {
                        (&cookie_pairs, &mut used_cookies, false)
                    };
                    // Names declared elsewhere in this place, which an
                    // exploded map does not take.
                    let declared: Vec<&str> = params
                        .iter()
                        .filter(|o| o.location == loc && !std::ptr::eq(*o, p))
                        .flat_map(|o| {
                            // An exploded object's pairs are its property names.
                            let props: Vec<&str> = match &o.value {
                                ParamValue::Schema {
                                    shape: Shape::Object(ps),
                                    ..
                                } if o.explode && o.style != Style::DeepObject => {
                                    ps.iter().map(|(n, _)| n.as_str()).collect()
                                }
                                _ => Vec::new(),
                            };
                            std::iter::once(o.name.as_str()).chain(props)
                        })
                        .chain(
                            if loc == In::Query {
                                &extra.query
                            } else {
                                &extra.cookies
                            }
                            .iter()
                            .map(String::as_str),
                        )
                        .collect();
                    match query_value(p, src, shape, plus, used, &declared) {
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

        if !self.options.allow_unknown_query && !whole_query {
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
            for (q, raw) in cookie_pairs.iter().zip(split_cookies(
                headers.get("cookie").into_iter().flatten().copied(),
            )) {
                let used =
                    used_cookies.contains(&q.name) || used_cookies.iter().any(|u| u == raw.0);
                if !used && !extra.cookies.contains(&q.name) {
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

/// A text body read as the schema's type allows (so `text/plain` "5" can
/// meet an integer schema).
fn coerce_text(t: String, shape: &Shape) -> Value {
    match shape {
        Shape::Primitive(types) if *types != Types::ANY => coerce(&t, *types),
        _ => Value::String(t),
    }
}

impl Spec {
    /// `Set-Cookie` takes one value per line and its values may hold
    /// commas (RFC 9110 §5.3), so lines are never split. A `schema`
    /// object has one property per line (`name=rest`) and an array one
    /// item per line; `content` sees the lines joined with LF (OpenAPI 3.2
    /// §4.21.3).
    fn check_set_cookie(&self, p: &Param, lines: &[&str], label: &str, report: &mut Report) {
        match &p.value {
            ParamValue::Schema { shape, .. } => match shape {
                Shape::Object(props) => {
                    let mut m = serde_json::Map::new();
                    for line in lines {
                        let (name, rest) = line.split_once('=').unwrap_or((line, ""));
                        let name = name.trim();
                        let t = props
                            .iter()
                            .find(|(n, _)| n == name)
                            .map_or(Types::STR, |(_, t)| *t);
                        m.insert(name.to_owned(), coerce(rest, t));
                    }
                    self.check_param_value(p, Value::Object(m), label, report);
                }
                Shape::Array(t) => {
                    let items = lines.iter().map(|l| coerce(l, *t)).collect();
                    self.check_param_value(p, Value::Array(items), label, report);
                }
                Shape::Primitive(t) => {
                    for line in lines {
                        self.check_param_value(p, coerce(line, *t), label, report);
                    }
                }
            },
            ParamValue::Content { media, id } => {
                self.check_content_param(media, *id, &lines.join("\n"), label, report)
            }
            ParamValue::QueryString { .. } | ParamValue::Any => {}
        }
    }

    /// Checks a response's headers and body against the Response Object.
    pub(crate) fn check_response(
        &self,
        resp_obj: &Resp,
        req: &Request<'_>,
        head: bool,
        resp: &Response<'_>,
        report: &mut Report,
    ) {
        let headers = header_index(resp.headers);
        if resp
            .headers
            .iter()
            .filter(|(h, _)| h.eq_ignore_ascii_case("content-type"))
            .count()
            > 1
        {
            report.push(
                ProblemKind::InvalidParameter,
                "header.content-type",
                "is sent on several lines but takes a single value",
            );
        }
        for p in &resp_obj.headers {
            let label = p.label();
            let Some(lines) = headers.get(&p.name.to_ascii_lowercase()) else {
                if p.required {
                    report.push(
                        ProblemKind::MissingParameter,
                        label.as_str(),
                        "is a required response header",
                    );
                }
                continue;
            };
            if p.name.eq_ignore_ascii_case("set-cookie") {
                self.check_set_cookie(p, lines, &label, report);
                continue;
            }
            // RFC 9110 §5.3: the lines are one value, joined with commas.
            let line = lines.join(", ");
            match &p.value {
                // Header values are literal text in simple style; they are
                // never re-read as JSON.
                ParamValue::Schema { shape, .. } => {
                    self.check_param_value(p, header_value(&line, shape, p.explode), &label, report)
                }
                ParamValue::Content { media, id } => {
                    self.check_content_param(media, *id, &line, &label, report)
                }
                ParamValue::QueryString { .. } | ParamValue::Any => {}
            }
        }
        if !self.options.validate_body {
            return;
        }
        // RFC 9110 §9.3.2, §15.3.5, §15.4.5: no content in these responses.
        let head = head || req.method == "HEAD";
        if !resp.body.is_empty() && (head || resp.status == 204 || resp.status == 304) {
            let why = if head {
                "a response to HEAD".to_owned()
            } else {
                format!("a {} response", resp.status)
            };
            report.push(
                ProblemKind::UnexpectedBody,
                "body",
                format!("{why} must not carry content"),
            );
            return;
        }
        if head || resp_obj.content.is_empty() {
            return;
        }
        if resp.body.is_empty() {
            // RFC 9110 §15.3.5, §15.4.5: 204 and 304 never carry content.
            if resp.status != 204 && resp.status != 304 {
                report.push(
                    ProblemKind::MissingBody,
                    "body",
                    format!(
                        "the response declares content for status {} but the body is empty",
                        resp.status
                    ),
                );
            }
            return;
        }
        if resp.body.len() > self.options.max_body_bytes {
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
        let ct = header_values(resp.headers, "content-type").next();
        if let Some((actual, entry)) = self.select_media(&resp_obj.content, ct, report) {
            if let Some(bytes) = self.decoded(resp.headers, resp.body, report) {
                self.check_payload(&actual, entry, &bytes, "body", report);
            }
        }
    }
}

impl Spec {
    /// Encoding Object rules for multipart parts (OpenAPI 3.1.1 §4.8.15.1):
    /// the part's media type must be one `contentType` allows, and its
    /// declared headers must be present (when required) and valid.
    pub(crate) fn check_parts(
        &self,
        encodings: &[body::Encoding],
        parts: &[body::PartInfo],
        location: &str,
        report: &mut Report,
    ) {
        for enc in encodings {
            for part in parts.iter().filter(|p| p.name == enc.name) {
                let at = format!("{location}/{}", crate::doc::escape_token(&part.name));
                self.check_part(enc, part.content_type.as_ref(), &part.headers, &at, report);
            }
        }
    }

    /// Checks one part's type and headers against its Encoding Object.
    fn check_part(
        &self,
        enc: &body::Encoding,
        content_type: Option<&MediaType>,
        headers: &[(String, String)],
        at: &str,
        report: &mut Report,
    ) {
        if let (Some(ct), false) = (content_type, enc.allowed.is_empty()) {
            if !enc.allowed.iter().any(|r| r.match_rank(ct).is_some()) {
                let allowed: Vec<String> = enc
                    .allowed
                    .iter()
                    .map(|m| format!("{}/{}", m.ty, m.sub))
                    .collect();
                report.push(
                    ProblemKind::UnsupportedMediaType,
                    at,
                    format!(
                        "part type {}/{} is not allowed; allowed: {}",
                        ct.ty,
                        ct.sub,
                        allowed.join(", ")
                    ),
                );
            }
        }
        let hs: Vec<(&str, &str)> = headers
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let index = header_index(&hs);
        for p in &enc.headers {
            let label = format!("{at}:header.{}", p.name.to_ascii_lowercase());
            match index.get(&p.name.to_ascii_lowercase()) {
                None if p.required => report.push(
                    ProblemKind::MissingParameter,
                    label,
                    "is a required part header",
                ),
                None => {}
                Some(lines) => {
                    for line in lines {
                        match &p.value {
                            ParamValue::Schema { shape, .. } => self.check_param_value(
                                p,
                                header_value(line, shape, p.explode),
                                &label,
                                report,
                            ),
                            ParamValue::Content { media, id } => {
                                self.check_content_param(media, *id, line, &label, report)
                            }
                            ParamValue::QueryString { .. } | ParamValue::Any => {}
                        }
                    }
                }
            }
        }
    }

    /// A multipart body read by position (OpenAPI 3.2 §4.14.5.2): one item
    /// per part, in order, each parsed by its type and checked against
    /// `prefixEncoding`, then `itemEncoding`. A nested multipart part is
    /// read the same way with the Encoding Object's own positional fields.
    #[allow(clippy::too_many_arguments)]
    fn positional(
        &self,
        bytes: &[u8],
        media: &MediaType,
        prefix: &[body::Encoding],
        item: Option<&body::Encoding>,
        shapes: &crate::model::PartShapes,
        location: &str,
        nest: usize,
        named: bool,
        report: &mut Report,
    ) -> Result<Vec<Value>, String> {
        let depth = self.store.depth_limit(self.options.max_body_depth);
        let parts = body::parts(bytes, body::boundary(media)?)?;
        let mut out = Vec::with_capacity(parts.len());
        for (i, part) in parts.iter().enumerate() {
            let at = format!("{location}/{i}");
            let enc = prefix.get(i).or(item);
            if let Some(enc) = enc {
                self.check_part(enc, part.content_type.as_ref(), &part.headers, &at, report);
            }
            // RFC 2046 §5.1: a part without Content-Type is text/plain; the
            // description's default comes first.
            let text_plain = MediaType::parse("text/plain").expect("a valid media type");
            // Named items describe the {name: value} pair, not the part.
            let shape = shapes.at(i).filter(|_| !named);
            let ct = part
                .content_type
                .as_ref()
                .or_else(|| enc.and_then(|e| e.content_type.as_ref()))
                .or_else(|| shape.map(|s| &s.default))
                .unwrap_or(&text_plain);
            let value = if ct.is_json() {
                json::parse(part.data, json::Limits { max_depth: depth })
                    .map_err(|e| format!("part {i} is not valid JSON: {e}"))?
            } else if ct.ty == "multipart" && nest < 4 {
                let (np, ni) = enc.map_or((&[][..], None), |e| (&e.prefix[..], e.item.as_deref()));
                let empty = crate::model::PartShapes::default();
                Value::Array(self.positional(
                    part.data,
                    ct,
                    np,
                    ni,
                    shape.map_or(&empty, |s| &s.nested),
                    &at,
                    nest + 1,
                    false,
                    report,
                )?)
            } else if ct.is_xml() {
                body::xml(part.data, ct, depth, shape.map(|s| &s.xml))
                    .map_err(|e| format!("part {i}: {e}"))?
                    .unwrap_or(Value::Null)
            } else if ct.ty == "text" {
                let t = String::from_utf8_lossy(part.data).into_owned();
                match shape {
                    Some(s) => coerce_text(t, &s.shape),
                    None => Value::String(t),
                }
            } else {
                Value::String(String::from_utf8_lossy(part.data).into_owned())
            };
            out.push(match (&part.name, named) {
                (Some(n), true) => serde_json::json!({ n.as_str(): value }),
                _ => value,
            });
        }
        Ok(out)
    }
}

impl Spec {
    /// The body with its `Content-Encoding` undone, or `None` after
    /// reporting why it cannot be checked.
    pub(crate) fn decoded<'b>(
        &self,
        headers: &[(&str, &str)],
        body: &'b [u8],
        report: &mut Report,
    ) -> Option<std::borrow::Cow<'b, [u8]>> {
        let codings: Vec<&str> = header_values(headers, "content-encoding").collect();
        if codings.is_empty() {
            return Some(std::borrow::Cow::Borrowed(body));
        }
        match body::decode_content(body, &codings.join(","), self.options.max_body_bytes) {
            Ok(b) => Some(std::borrow::Cow::Owned(b)),
            Err(m) => {
                let kind = if m.contains("not supported") {
                    ProblemKind::UnsupportedMediaType
                } else if m.contains("larger than") {
                    ProblemKind::BodyTooLarge
                } else {
                    ProblemKind::MalformedBody
                };
                report.push(kind, "header.content-encoding", m);
                None
            }
        }
    }
}
