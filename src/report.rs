//! Validation results.

use std::fmt;

/// What kind of problem was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ProblemKind {
    /// No path in the document matches the request path.
    PathNotFound,
    /// The path matches but declares no operation for the method.
    MethodNotAllowed,
    /// The request path is not a valid percent-encoded path.
    MalformedPath,
    /// The query string is malformed (for example a bad percent-escape).
    MalformedQuery,
    /// A required parameter or header is absent.
    MissingParameter,
    /// A parameter or header value does not fit its description.
    InvalidParameter,
    /// A query parameter, header or cookie the operation does not declare.
    UnknownParameter,
    /// No security requirement of the operation is met.
    MissingCredentials,
    /// The request requires a body and none was sent.
    MissingBody,
    /// The body could not be parsed as its media type.
    MalformedBody,
    /// The body is larger than `Options::max_body_bytes`.
    BodyTooLarge,
    /// The body's media type is not one the operation accepts.
    UnsupportedMediaType,
    /// A value in the body does not fit its schema.
    SchemaViolation,
    /// A message carries content where none is allowed (a response to
    /// HEAD, a 204 or a 304).
    UnexpectedBody,
    /// The response status code is not declared for the operation.
    UndeclaredStatus,
    /// A response was given for a request that matches no operation.
    UndeclaredOperation,
    /// A schema in the document could not be compiled.
    SchemaError,
    /// Validation stopped because of an internal limit or fault.
    Internal,
}

impl ProblemKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ProblemKind::PathNotFound => "path_not_found",
            ProblemKind::MethodNotAllowed => "method_not_allowed",
            ProblemKind::MalformedPath => "malformed_path",
            ProblemKind::MalformedQuery => "malformed_query",
            ProblemKind::MissingParameter => "missing_parameter",
            ProblemKind::InvalidParameter => "invalid_parameter",
            ProblemKind::UnknownParameter => "unknown_parameter",
            ProblemKind::MissingCredentials => "missing_credentials",
            ProblemKind::MissingBody => "missing_body",
            ProblemKind::MalformedBody => "malformed_body",
            ProblemKind::BodyTooLarge => "body_too_large",
            ProblemKind::UnsupportedMediaType => "unsupported_media_type",
            ProblemKind::SchemaViolation => "schema_violation",
            ProblemKind::UnexpectedBody => "unexpected_body",
            ProblemKind::UndeclaredStatus => "undeclared_status",
            ProblemKind::UndeclaredOperation => "undeclared_operation",
            ProblemKind::SchemaError => "schema_error",
            ProblemKind::Internal => "internal",
        }
    }
}

impl fmt::Display for ProblemKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One problem: a kind, where it is (`query.limit`, `header.x-tag`,
/// `body/items/3`), and a one-sentence message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub kind: ProblemKind,
    pub location: String,
    pub message: String,
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {} ({})", self.location, self.message, self.kind)
    }
}

/// The operation a request was matched to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchedOperation {
    /// Path template as written in the document, such as `/pets/{id}`; for
    /// a webhook its name, and for a callback its expression.
    pub path: String,
    /// The field the operation is under: a lower-case method such as
    /// `get`, or an OpenAPI 3.2 `additionalOperations` key such as `LINK`.
    pub method: String,
    pub operation_id: Option<String>,
}

/// Every problem found, bounded by `Options::max_problems` and
/// `Options::max_report_bytes`. Problems past either bound are counted in
/// [`Report::omitted`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    problems: Vec<Problem>,
    omitted: usize,
    bytes: usize,
    max_problems: usize,
    max_bytes: usize,
    operation: Option<MatchedOperation>,
}

impl Report {
    pub(crate) fn new(max_problems: usize, max_bytes: usize) -> Report {
        Report {
            max_problems,
            max_bytes,
            ..Report::default()
        }
    }

    /// True when no problem was found.
    pub fn is_valid(&self) -> bool {
        self.problems.is_empty() && self.omitted == 0
    }

    /// The problems kept in the report, in the order found.
    pub fn problems(&self) -> &[Problem] {
        &self.problems
    }

    /// Problems found but left out because of the report bounds.
    pub fn omitted(&self) -> usize {
        self.omitted
    }

    /// Total problems found.
    pub fn total(&self) -> usize {
        self.problems.len() + self.omitted
    }

    pub fn operation(&self) -> Option<&MatchedOperation> {
        self.operation.as_ref()
    }

    pub(crate) fn set_operation(&mut self, op: MatchedOperation) {
        self.operation = Some(op);
    }

    pub(crate) fn push(
        &mut self,
        kind: ProblemKind,
        location: impl Into<String>,
        message: impl Into<String>,
    ) {
        let location = location.into();
        let message = message.into();
        let size = location.len() + message.len() + kind.as_str().len() + 8;
        if self.problems.len() >= self.max_problems || self.bytes + size > self.max_bytes {
            self.omitted += 1;
            return;
        }
        self.bytes += size;
        self.problems.push(Problem {
            kind,
            location,
            message,
        });
    }

    /// Counts problems found but not kept.
    pub(crate) fn add_omitted(&mut self, n: usize) {
        self.omitted += n;
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_valid() {
            return f.write_str("valid");
        }
        for (i, p) in self.problems.iter().enumerate() {
            if i > 0 {
                f.write_str("\n")?;
            }
            write!(f, "{p}")?;
        }
        if self.omitted > 0 {
            if !self.problems.is_empty() {
                f.write_str("\n")?;
            }
            write!(f, "and {} more", self.omitted)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_by_count() {
        let mut r = Report::new(2, 8192);
        for i in 0..5 {
            r.push(
                ProblemKind::InvalidParameter,
                format!("query.p{i}"),
                "must be an integer",
            );
        }
        assert_eq!(r.problems().len(), 2);
        assert_eq!(r.omitted(), 3);
        assert!(r.to_string().ends_with("and 3 more"));
    }

    #[test]
    fn bounded_by_bytes() {
        let mut r = Report::new(100, 100);
        r.push(ProblemKind::SchemaViolation, "body", "x".repeat(40));
        r.push(ProblemKind::SchemaViolation, "body", "y".repeat(40));
        assert_eq!(r.problems().len(), 1);
        assert_eq!(r.omitted(), 1);
        assert!(!r.is_valid());
    }
}
