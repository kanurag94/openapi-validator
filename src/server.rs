//! Server Objects (OpenAPI 3.1.1 §4.8.5, §4.8.6).
//!
//! Only the path part of a server URL matters for matching: the gateway has
//! already chosen the host. Variables are substituted from their `enum`
//! values, or their `default` when there is no `enum`, so a server
//! `/{version}` with `enum: [v1, v2]` yields two base paths.

use serde_json::Value;

use crate::doc::Warning;
use crate::path::decode_segment;

/// Upper bound on the base paths one Server Object may expand to.
const MAX_EXPANSIONS: usize = 64;

/// A server base path as decoded segments; empty for `/`.
pub type BasePath = Vec<String>;

/// Expands the base paths of a `servers` array. A missing or empty array is
/// the single server `/` (OpenAPI 3.1.1 §4.8.1.1, `servers`).
pub fn base_paths(
    servers: Option<&Value>,
    location: &str,
    warnings: &mut Vec<Warning>,
) -> Vec<BasePath> {
    let list = servers.and_then(Value::as_array).filter(|a| !a.is_empty());
    let Some(list) = list else {
        return vec![Vec::new()];
    };
    let mut out: Vec<BasePath> = Vec::new();
    for (i, s) in list.iter().enumerate() {
        let Some(url) = s.get("url").and_then(Value::as_str) else {
            warnings.push(Warning {
                location: format!("{location}/{i}"),
                message: "server has no url; treated as /".into(),
            });
            push_unique(&mut out, Vec::new());
            continue;
        };
        for expanded in expand(
            url,
            s.get("variables"),
            &format!("{location}/{i}"),
            warnings,
        ) {
            push_unique(&mut out, url_path(&expanded));
        }
    }
    out
}

fn push_unique(out: &mut Vec<BasePath>, p: BasePath) {
    if !out.contains(&p) {
        out.push(p);
    }
}

/// Substitutes `{name}` expressions with every allowed value.
fn expand(
    url: &str,
    vars: Option<&Value>,
    location: &str,
    warnings: &mut Vec<Warning>,
) -> Vec<String> {
    let mut results = vec![String::new()];
    let mut rest = url;
    while let Some(open) = rest.find('{') {
        let Some(close) = rest[open..].find('}').map(|c| open + c) else {
            break;
        };
        let lit = &rest[..open];
        let name = &rest[open + 1..close];
        let var = vars.and_then(|v| v.get(name));
        let mut values: Vec<String> = var
            .and_then(|v| v.get("enum"))
            .and_then(Value::as_array)
            .map(|e| {
                e.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        if values.is_empty() {
            if let Some(d) = var.and_then(|v| v.get("default")).and_then(Value::as_str) {
                values.push(d.to_owned());
            }
        }
        if values.is_empty() {
            warnings.push(Warning {
                location: location.to_owned(),
                message: format!("server variable {name:?} has no default; substituted as empty"),
            });
            values.push(String::new());
        }
        let mut next = Vec::new();
        'outer: for r in &results {
            for v in &values {
                if next.len() >= MAX_EXPANSIONS {
                    warnings.push(Warning {
                        location: location.to_owned(),
                        message: format!("server expands to more than {MAX_EXPANSIONS} URLs; the rest are ignored"),
                    });
                    break 'outer;
                }
                next.push(format!("{r}{lit}{v}"));
            }
        }
        results = next;
        rest = &rest[close + 1..];
    }
    for r in &mut results {
        r.push_str(rest);
    }
    results
}

/// The decoded path segments of a URL, without query or fragment.
fn url_path(url: &str) -> BasePath {
    let no_frag = url.split(['?', '#']).next().unwrap_or("");
    let path = if let Some(p) = no_frag.find("://") {
        let after = &no_frag[p + 3..];
        after.find('/').map_or("", |s| &after[s..])
    } else if let Some(after) = no_frag.strip_prefix("//") {
        after.find('/').map_or("", |s| &after[s..])
    } else {
        no_frag
    };
    path.split('/')
        .filter(|s| !s.is_empty())
        .map(|s| {
            decode_segment(s)
                .map(|d| d.text)
                .unwrap_or_else(|_| s.to_owned())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn paths(v: Value) -> Vec<BasePath> {
        base_paths(Some(&v), "#/servers", &mut Vec::new())
    }

    #[test]
    fn oas311_s4_8_5_url_paths() {
        assert_eq!(
            paths(json!([{"url": "https://api.example.com/v1/"}])),
            vec![vec!["v1".to_owned()]]
        );
        assert_eq!(
            paths(json!([{"url": "/api/v2"}])),
            vec![vec!["api".to_owned(), "v2".to_owned()]]
        );
        assert_eq!(paths(json!([{"url": "api"}])), vec![vec!["api".to_owned()]]);
        assert_eq!(
            paths(json!([{"url": "https://example.com"}])),
            vec![Vec::<String>::new()]
        );
        assert_eq!(
            paths(json!([{"url": "//cdn.example.com/x?y#z"}])),
            vec![vec!["x".to_owned()]]
        );
        assert_eq!(
            base_paths(None, "#/servers", &mut Vec::new()),
            vec![Vec::<String>::new()]
        );
    }

    #[test]
    fn oas311_s4_8_6_variables_use_enum_or_default() {
        let v = paths(json!([{
            "url": "https://{host}/{version}",
            "variables": {
                "host": {"default": "example.com"},
                "version": {"default": "v1", "enum": ["v1", "v2"]}
            }
        }]));
        assert_eq!(v, vec![vec!["v1".to_owned()], vec!["v2".to_owned()]]);
        let d = paths(json!([{"url": "/{base}", "variables": {"base": {"default": "api"}}}]));
        assert_eq!(d, vec![vec!["api".to_owned()]]);
    }

    #[test]
    fn expansion_is_bounded() {
        let e: Vec<String> = (0..20).map(|i| format!("v{i}")).collect();
        let mut w = Vec::new();
        let v = base_paths(
            Some(
                &json!([{"url": "/{a}/{b}", "variables": {"a": {"default": "v0", "enum": e}, "b": {"default": "v0", "enum": e}}}]),
            ),
            "#/servers",
            &mut w,
        );
        assert!(v.len() <= MAX_EXPANSIONS);
        assert!(!w.is_empty());
    }
}
