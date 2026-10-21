//! Validation options.

use std::fmt;
use std::sync::Arc;

use crate::http::Request;

/// Where a credential was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialLocation {
    Query,
    Header,
    Cookie,
}

/// A credential found for a security scheme, handed to
/// [`Options::security_hook`].
#[derive(Debug)]
pub struct Credential<'a> {
    /// The scheme's name under `components.securitySchemes`.
    pub scheme_name: &'a str,
    /// The scheme's `type`: `apiKey`, `http`, `oauth2`, `openIdConnect` or
    /// `mutualTLS`.
    pub scheme_type: &'a str,
    pub location: CredentialLocation,
    /// The credential text: the API key, or what follows the auth-scheme
    /// token in `Authorization`.
    pub value: &'a str,
    /// Scopes the requirement lists for this scheme.
    pub scopes: &'a [String],
    pub request: &'a Request<'a>,
}

/// A caller-supplied check run on every credential that is present.
/// Returning `Err(reason)` makes the scheme unsatisfied.
pub type SecurityHook = Arc<dyn Fn(&Credential<'_>) -> Result<(), String> + Send + Sync>;

/// How server URL paths (OpenAPI 3.1.1 §4.8.5) combine with path templates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ServerPaths {
    /// Match the request path with or without a server's path in front.
    #[default]
    Either,
    /// The request path must start with one of the operation's server paths
    /// (a server path of `/` always matches).
    Required,
    /// Ignore server paths; match the request path as is.
    Ignored,
}

/// When `format` is asserted (JSON Schema 2020-12 §7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FormatAssertion {
    /// OpenAPI 3.0: asserted (3.0 lets tools validate formats).
    /// OpenAPI 3.1: annotation only, as the format-annotation vocabulary
    /// requires (§7.2.1).
    #[default]
    ByVersion,
    Always,
    Never,
}

/// What the Discriminator Object does (OpenAPI 3.1.1 §4.8.25).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiscriminatorMode {
    /// A hint only: it "MUST NOT change the validation outcome". It is
    /// used to point problem reports at the named branch.
    #[default]
    Annotation,
    /// The property's value must be a mapped name, and the payload is
    /// checked against that schema alone.
    Enforce,
}

/// What `type: integer` accepts in OpenAPI 3.0 documents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IntegerSemantics {
    /// Any number with a zero fraction, so `1.0` is an integer (OpenAPI
    /// 3.0.4 §4.4, and always in 3.1).
    #[default]
    Mathematical,
    /// OpenAPI 3.0.3 §4.4: a JSON number without a fraction or exponent.
    LexicalIn30,
}

/// Whether readOnly / writeOnly properties drop out of `required`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReadWriteRequired {
    /// OpenAPI 3.0 (§4.7.24.1): `required` applies to responses only for
    /// readOnly and to requests only for writeOnly. OpenAPI 3.1
    /// (§4.8.24.3.2): they are annotations and `required` always applies.
    #[default]
    ByVersion,
    /// Use the 3.0 rule for 3.1 documents too.
    RelaxAlways,
}

/// Header names that are always treated as declared when
/// `allow_unknown_headers` is false: transport headers (RFC 9110, RFC 9112).
pub const DEFAULT_IGNORED_HEADERS: &[&str] = &[
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "user-agent",
];

#[derive(Clone)]
pub struct Options {
    /// Check request and response bodies. Default true.
    pub validate_body: bool,
    /// Allow query parameters the operation does not declare. Default true.
    pub allow_unknown_query: bool,
    /// Allow headers the operation does not declare. Default true.
    pub allow_unknown_headers: bool,
    /// Allow cookies the operation does not declare. Default true.
    pub allow_unknown_cookies: bool,
    /// Header names (case-insensitive) always counted as declared, in
    /// addition to `Accept`, `Content-Type`, `Authorization` and the security
    /// schemes' header names.
    pub ignored_headers: Vec<String>,
    /// Problems kept in a report. Default 20.
    pub max_problems: usize,
    /// Bytes of locations and messages kept in a report. Default 8 KiB.
    pub max_report_bytes: usize,
    /// Larger bodies are reported as `BodyTooLarge` and not parsed.
    /// Default 16 MiB.
    pub max_body_bytes: usize,
    /// Maximum nesting depth of a JSON body. Default 128.
    pub max_body_depth: usize,
    /// Check that security credentials are present. Default true.
    pub check_security: bool,
    /// Called for each credential that is present.
    pub security_hook: Option<SecurityHook>,
    pub server_paths: ServerPaths,
    /// When formats in [`crate::format::ASSERTED`] are checked.
    pub formats: FormatAssertion,
    pub discriminator: DiscriminatorMode,
    pub integers: IntegerSemantics,
    pub read_write_required: ReadWriteRequired,
    /// Decode `+` as a space in query strings, as HTML forms and the WHATWG
    /// URL standard do. Default true.
    pub query_plus_as_space: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            validate_body: true,
            allow_unknown_query: true,
            allow_unknown_headers: true,
            allow_unknown_cookies: true,
            ignored_headers: DEFAULT_IGNORED_HEADERS
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            max_problems: 20,
            max_report_bytes: 8 * 1024,
            max_body_bytes: 16 * 1024 * 1024,
            max_body_depth: 128,
            check_security: true,
            security_hook: None,
            server_paths: ServerPaths::default(),
            formats: FormatAssertion::default(),
            discriminator: DiscriminatorMode::default(),
            integers: IntegerSemantics::default(),
            read_write_required: ReadWriteRequired::default(),
            query_plus_as_space: true,
        }
    }
}

impl Options {
    /// The defaults with the gateway-oriented choices of this library's
    /// original design switched on: every listed format asserted, the
    /// discriminator enforced, OpenAPI 3.0 integers read lexically, and
    /// readOnly/writeOnly relaxing `required` in 3.1 as in 3.0. Each goes
    /// beyond what the specifications require.
    pub fn gateway_profile() -> Options {
        Options {
            formats: FormatAssertion::Always,
            discriminator: DiscriminatorMode::Enforce,
            integers: IntegerSemantics::LexicalIn30,
            read_write_required: ReadWriteRequired::RelaxAlways,
            ..Options::default()
        }
    }
}

impl fmt::Debug for Options {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Options")
            .field("validate_body", &self.validate_body)
            .field("allow_unknown_query", &self.allow_unknown_query)
            .field("allow_unknown_headers", &self.allow_unknown_headers)
            .field("allow_unknown_cookies", &self.allow_unknown_cookies)
            .field("ignored_headers", &self.ignored_headers)
            .field("max_problems", &self.max_problems)
            .field("max_report_bytes", &self.max_report_bytes)
            .field("max_body_bytes", &self.max_body_bytes)
            .field("max_body_depth", &self.max_body_depth)
            .field("check_security", &self.check_security)
            .field(
                "security_hook",
                &self.security_hook.as_ref().map(|_| "<fn>"),
            )
            .field("server_paths", &self.server_paths)
            .field("formats", &self.formats)
            .field("discriminator", &self.discriminator)
            .field("integers", &self.integers)
            .field("read_write_required", &self.read_write_required)
            .field("query_plus_as_space", &self.query_plus_as_space)
            .finish()
    }
}
