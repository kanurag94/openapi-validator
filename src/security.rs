//! Security Requirement and Security Scheme Objects (OpenAPI 3.1.1
//! §4.8.27, §4.8.30).
//!
//! Only presence is checked: a credential of the right kind is in the right
//! place and is not empty. Schemes within one requirement must all be met
//! (AND); one met requirement is enough (OR); an empty requirement `{}`
//! makes security optional (§4.8.30.1). `Options::security_hook` can check
//! more.

use serde_json::Value;

use crate::doc::{self, LoadError};
use crate::http::Request;
use crate::model::Builder;
use crate::options::{Credential, CredentialLocation, Options};
use crate::params::{decode, split_cookies, split_query};
use crate::validate::{header_values, Declared};

#[derive(Debug, Clone)]
pub(crate) enum Kind {
    ApiKey {
        location: CredentialLocation,
        name: String,
    },
    /// `scheme` lower-cased, such as `basic` or `bearer` (RFC 9110 §11.1:
    /// auth-scheme tokens are case-insensitive).
    Http { scheme: String },
    /// oauth2 and openIdConnect: a bearer token (RFC 6750 §2.1).
    Bearer,
    /// mutualTLS and unknown types: not visible in the HTTP message.
    Unverifiable,
}

#[derive(Debug, Clone)]
pub(crate) struct Scheme {
    pub name: String,
    pub type_name: String,
    pub kind: Kind,
}

/// One requirement: every listed scheme, with its scopes.
pub(crate) type Requirement = Vec<(usize, Vec<String>)>;

pub(crate) struct Schemes {
    pub list: Vec<Scheme>,
}

impl Builder<'_> {
    pub(crate) fn schemes(&mut self) -> Result<Schemes, LoadError> {
        let mut list = Vec::new();
        let Some(map) = self
            .root
            .pointer("/components/securitySchemes")
            .and_then(Value::as_object)
        else {
            return Ok(Schemes { list });
        };
        for (name, v) in map {
            let ptr = doc::join("#/components/securitySchemes", name);
            let (s, sptr) = doc::deref(self.root, v, &ptr)?;
            let type_name = s
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let kind = match type_name.as_str() {
                "apiKey" => {
                    let location = match s.get("in").and_then(Value::as_str) {
                        Some("query") => Some(CredentialLocation::Query),
                        Some("header") => Some(CredentialLocation::Header),
                        Some("cookie") => Some(CredentialLocation::Cookie),
                        _ => None,
                    };
                    match (location, s.get("name").and_then(Value::as_str)) {
                        (Some(location), Some(n)) => Kind::ApiKey {
                            location,
                            name: n.to_owned(),
                        },
                        _ => {
                            self.warn(
                                &sptr,
                                "apiKey scheme without a valid `in` and `name`; not checked",
                            );
                            Kind::Unverifiable
                        }
                    }
                }
                "http" => match s.get("scheme").and_then(Value::as_str) {
                    Some(sc) => Kind::Http {
                        scheme: sc.to_ascii_lowercase(),
                    },
                    None => {
                        self.warn(&sptr, "http scheme without `scheme`; not checked");
                        Kind::Unverifiable
                    }
                },
                "oauth2" | "openIdConnect" => Kind::Bearer,
                "mutualTLS" => Kind::Unverifiable,
                other => {
                    self.warn(
                        &sptr,
                        format!("unknown security scheme type {other:?}; not checked"),
                    );
                    Kind::Unverifiable
                }
            };
            list.push(Scheme {
                name: name.clone(),
                type_name,
                kind,
            });
        }
        Ok(Schemes { list })
    }

    /// A `security` array; `None` when the field is absent.
    pub(crate) fn requirements(
        &mut self,
        v: Option<&Value>,
        ptr: &str,
        schemes: &Schemes,
    ) -> Result<Option<Vec<Requirement>>, LoadError> {
        let Some(v) = v else {
            return Ok(None);
        };
        let Some(list) = v.as_array() else {
            self.warn(ptr, "security is not an array; ignored");
            return Ok(None);
        };
        let mut out = Vec::new();
        for (i, req) in list.iter().enumerate() {
            let rptr = doc::join(ptr, &i.to_string());
            let Some(m) = req.as_object() else {
                continue;
            };
            let mut r = Vec::new();
            for (name, scopes) in m {
                let Some(idx) = schemes.list.iter().position(|s| s.name == *name) else {
                    return Err(LoadError::UnknownSecurityScheme {
                        name: name.clone(),
                        location: rptr,
                    });
                };
                let scopes = scopes
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
                r.push((idx, scopes));
            }
            out.push(r);
        }
        Ok(Some(out))
    }
}

/// The credential text after an auth-scheme token, when `line` starts with
/// that whole token followed by whitespace (RFC 9110 §11.4).
fn after_scheme<'a>(line: &'a str, scheme: &str) -> Option<&'a str> {
    let (token, rest) = line.split_once([' ', '\t'])?;
    if !token.eq_ignore_ascii_case(scheme) {
        return None;
    }
    let cred = rest.trim_matches([' ', '\t']);
    (!cred.is_empty()).then_some(cred)
}

/// Names that security schemes put in the request, for the
/// unknown-parameter checks.
pub(crate) fn declared(schemes: &Schemes, reqs: Option<&[Requirement]>) -> Declared {
    let mut d = Declared::default();
    for r in reqs.unwrap_or_default() {
        for (idx, _) in r {
            if let Some(Kind::ApiKey { location, name }) = schemes.list.get(*idx).map(|s| &s.kind) {
                let list = match location {
                    CredentialLocation::Query => &mut d.query,
                    CredentialLocation::Header => &mut d.headers,
                    CredentialLocation::Cookie => &mut d.cookies,
                };
                if !list.contains(name) {
                    list.push(name.clone());
                }
            }
        }
    }
    d
}

/// Why a scheme is not met, or `None` when it is.
fn unmet(scheme: &Scheme, scopes: &[String], req: &Request<'_>, opts: &Options) -> Option<String> {
    let found: Option<(CredentialLocation, String)> = match &scheme.kind {
        Kind::Unverifiable => return None,
        Kind::ApiKey { location, name } => {
            let v = match location {
                CredentialLocation::Query => {
                    let (pairs, _) = split_query(req.query, opts.query_plus_as_space);
                    pairs
                        .iter()
                        .filter(|p| p.name == *name)
                        .filter_map(|p| decode(p.raw, opts.query_plus_as_space).ok())
                        .find(|v| !v.is_empty())
                }
                CredentialLocation::Header => header_values(req.headers, name)
                    .find(|v| !v.is_empty())
                    .map(str::to_owned),
                CredentialLocation::Cookie => split_cookies(header_values(req.headers, "cookie"))
                    .into_iter()
                    .filter(|(n, _)| n == name)
                    .map(|(_, v)| v)
                    .find(|v| !v.is_empty())
                    .map(str::to_owned),
            };
            v.map(|v| (*location, v))
        }
        Kind::Http { scheme: s } => header_values(req.headers, "authorization")
            .find_map(|line| after_scheme(line, s))
            .map(|c| (CredentialLocation::Header, c.to_owned())),
        Kind::Bearer => header_values(req.headers, "authorization")
            .find_map(|line| after_scheme(line, "bearer"))
            .map(|c| (CredentialLocation::Header, c.to_owned())),
    };
    let Some((location, value)) = found else {
        return Some(match &scheme.kind {
            Kind::ApiKey { location, name } => {
                let place = match location {
                    CredentialLocation::Query => "query parameter",
                    CredentialLocation::Header => "header",
                    CredentialLocation::Cookie => "cookie",
                };
                format!("{} needs a non-empty {place} {name}", scheme.name)
            }
            Kind::Http { scheme: s } => format!(
                "{} needs an Authorization header with scheme {s}",
                scheme.name
            ),
            _ => format!(
                "{} needs an Authorization header with a bearer token",
                scheme.name
            ),
        });
    };
    if let Some(hook) = &opts.security_hook {
        let c = Credential {
            scheme_name: &scheme.name,
            scheme_type: &scheme.type_name,
            location,
            value: &value,
            scopes,
            request: req,
        };
        if let Err(reason) = hook(&c) {
            return Some(format!("{}: {reason}", scheme.name));
        }
    }
    None
}

/// `None` when some requirement is met, else a one-sentence explanation.
pub(crate) fn check(
    schemes: &Schemes,
    reqs: &[Requirement],
    req: &Request<'_>,
    opts: &Options,
) -> Option<String> {
    if reqs.is_empty() {
        return None;
    }
    let mut reasons = Vec::new();
    for r in reqs {
        let mut missing = Vec::new();
        for (idx, scopes) in r {
            if let Some(s) = schemes.list.get(*idx) {
                if let Some(why) = unmet(s, scopes, req, opts) {
                    missing.push(why);
                }
            }
        }
        if missing.is_empty() {
            return None;
        }
        reasons.push(missing.join(" and "));
    }
    Some(format!(
        "no security requirement is met: {}",
        reasons.join("; or ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc9110_s11_4_whole_token_case_insensitive() {
        assert_eq!(after_scheme("Bearer abc", "bearer"), Some("abc"));
        assert_eq!(after_scheme("bearer   abc ", "bearer"), Some("abc"));
        assert_eq!(after_scheme("BearerX abc", "bearer"), None);
        assert_eq!(after_scheme("Bearer", "bearer"), None);
        assert_eq!(after_scheme("Bearer   ", "bearer"), None);
        assert_eq!(after_scheme("Basic dXNlcjpw", "basic"), Some("dXNlcjpw"));
    }
}
