//! Media types and media ranges (RFC 9110 §8.3.1, §12.5.1; OpenAPI 3.1.1
//! §4.8.14 "content" keys).

/// A parsed `type/subtype; param=value` (type, subtype and parameter names
/// lower-cased).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaType {
    pub ty: String,
    pub sub: String,
    pub params: Vec<(String, String)>,
}

impl MediaType {
    pub fn parse(s: &str) -> Option<MediaType> {
        let mut parts = s.split(';');
        let essence = parts.next()?.trim();
        let (ty, sub) = essence.split_once('/')?;
        let token = |t: &str| {
            !t.is_empty()
                && t.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&c))
        };
        if !token(ty) || !token(sub) {
            return None;
        }
        let mut params = Vec::new();
        for p in parts {
            let Some((k, v)) = p.split_once('=') else {
                continue;
            };
            let v = v.trim();
            let v = v
                .strip_prefix('"')
                .and_then(|x| x.strip_suffix('"'))
                .unwrap_or(v);
            params.push((k.trim().to_ascii_lowercase(), v.to_owned()));
        }
        Some(MediaType {
            ty: ty.to_ascii_lowercase(),
            sub: sub.to_ascii_lowercase(),
            params,
        })
    }

    pub fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// JSON, including structured-syntax `+json` types (RFC 6839 §3.1).
    pub fn is_json(&self) -> bool {
        (self.ty == "application" && self.sub == "json") || self.sub.ends_with("+json")
    }

    pub fn is_xml(&self) -> bool {
        ((self.ty == "application" || self.ty == "text") && self.sub == "xml")
            || self.sub.ends_with("+xml")
    }

    pub fn is_form(&self) -> bool {
        self.ty == "application" && self.sub == "x-www-form-urlencoded"
    }

    /// A sequential media type (OpenAPI 3.2 §4.14.3.1): a repeating
    /// structure read as an array of items.
    pub fn sequence(&self) -> Option<Sequence> {
        match (self.ty.as_str(), self.sub.as_str()) {
            ("application", "jsonl" | "x-ndjson" | "x-jsonlines" | "jsonlines") => {
                Some(Sequence::JsonLines)
            }
            ("application", "json-seq") => Some(Sequence::JsonSeq),
            (_, sub) if sub.ends_with("+json-seq") => Some(Sequence::JsonSeq),
            ("text", "event-stream") => Some(Sequence::EventStream),
            _ => None,
        }
    }

    pub fn is_multipart_form(&self) -> bool {
        self.ty == "multipart" && self.sub == "form-data"
    }

    /// How specifically `self`, used as a range, matches `actual`: 3 for the
    /// same type with matching parameters, 2 for the same type, 1 for
    /// `type/*`, 0 for `*/*`, `None` when it does not match.
    pub fn match_rank(&self, actual: &MediaType) -> Option<u8> {
        if self.ty == "*" && self.sub == "*" {
            return Some(0);
        }
        if self.ty != actual.ty {
            return None;
        }
        if self.sub == "*" {
            return Some(1);
        }
        if self.sub != actual.sub {
            return None;
        }
        let params_match = self.params.iter().all(|(k, v)| {
            actual.param(k).is_some_and(|a| {
                if k == "charset" {
                    a.eq_ignore_ascii_case(v)
                } else {
                    a == v
                }
            })
        });
        // Parameters that differ still match the type, only less specifically
        // than an exact match: `charset` is advisory for most types.
        Some(if !self.params.is_empty() && params_match {
            3
        } else {
            2
        })
    }
}

/// How a sequential media type separates its items.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sequence {
    /// JSON Lines and NDJSON: one JSON value per line.
    JsonLines,
    /// JSON Text Sequences (RFC 7464): records led by RS (0x1E).
    JsonSeq,
    /// Server-Sent Events (WHATWG HTML §9.2.6).
    EventStream,
}

/// Picks the best `content` key for an actual media type.
pub fn best_match<'a, T>(
    entries: &'a [(MediaType, T)],
    actual: &MediaType,
) -> Option<&'a (MediaType, T)> {
    entries
        .iter()
        .filter_map(|e| e.0.match_rank(actual).map(|r| (r, e)))
        .max_by_key(|(r, _)| *r)
        .map(|(_, e)| e)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mt(s: &str) -> MediaType {
        MediaType::parse(s).unwrap()
    }

    #[test]
    fn rfc9110_s8_3_1_parsing_is_case_insensitive() {
        let m = mt("Application/JSON; Charset=\"UTF-8\"");
        assert_eq!((m.ty.as_str(), m.sub.as_str()), ("application", "json"));
        assert_eq!(m.param("charset"), Some("UTF-8"));
        assert!(MediaType::parse("json").is_none());
        assert!(MediaType::parse("a b/c").is_none());
    }

    #[test]
    fn oas311_s4_8_14_media_ranges_most_specific_wins() {
        let entries = vec![
            (mt("*/*"), 0),
            (mt("application/*"), 1),
            (mt("application/json"), 2),
        ];
        assert_eq!(
            best_match(&entries, &mt("application/json; charset=utf-8"))
                .unwrap()
                .1,
            2
        );
        assert_eq!(best_match(&entries, &mt("application/xml")).unwrap().1, 1);
        assert_eq!(best_match(&entries, &mt("text/plain")).unwrap().1, 0);
        // Matching parameters rank above none; `charset` ignores case, other
        // parameters do not (RFC 9110 §8.3.1).
        let rank = |range: &str, actual: &str| mt(range).match_rank(&mt(actual));
        assert_eq!(
            rank("text/plain; charset=utf-8", "text/plain; charset=UTF-8"),
            Some(3)
        );
        assert_eq!(rank("text/plain; charset=utf-8", "text/plain"), Some(2));
        assert_eq!(rank("text/plain", "text/plain; charset=utf-8"), Some(2));
        assert_eq!(rank("text/plain; v=A", "text/plain; v=A"), Some(3));
        assert_eq!(rank("text/plain; v=A", "text/plain; v=a"), Some(2));
        assert_eq!(rank("text/plain; v=A", "text/html; v=A"), None);
        let only = vec![(mt("application/json"), 0)];
        assert!(best_match(&only, &mt("text/plain")).is_none());
    }

    #[test]
    fn structured_syntax_suffixes() {
        assert!(mt("application/problem+json").is_json());
        assert!(mt("application/vnd.api+json").is_json());
        assert!(!mt("application/jsonx").is_json());
    }
}
