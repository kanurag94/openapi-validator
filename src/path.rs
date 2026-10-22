//! Path templates (OpenAPI 3.1.1 §4.8.8, §4.8.2.2 "Path Templating").
//!
//! A request path is split on `/` first and each segment is percent-decoded
//! on its own, so `%2F` stays inside its segment and never creates a new
//! one (RFC 3986 §2.2, §3.3). A template variable matches one or more
//! characters within a single segment.
//!
//! When several templates match, a concrete path beats a templated one
//! (OpenAPI 3.1.1 §4.8.8.2) and, among templates, the one with the most
//! literal characters wins, so `/f/{id}.json` beats `/f/{id}`. Remaining ties
//! go to the template whose first differing segment is literal.

use std::collections::HashMap;

/// One part of a template segment.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    Lit(String),
    Var(String),
}

#[derive(Debug, Clone)]
struct Segment {
    parts: Vec<Part>,
}

impl Segment {
    fn literal(&self) -> Option<&str> {
        match self.parts.as_slice() {
            [Part::Lit(s)] => Some(s),
            [] => Some(""),
            _ => None,
        }
    }

    fn literal_len(&self) -> usize {
        self.parts
            .iter()
            .map(|p| match p {
                Part::Lit(s) => s.chars().count(),
                Part::Var(_) => 0,
            })
            .sum()
    }
}

/// A parsed path template.
#[derive(Debug, Clone)]
pub struct Template {
    pub text: String,
    segments: Vec<Segment>,
    literal_chars: usize,
}

impl Template {
    /// The variable names, in order.
    pub fn var_names(&self) -> Vec<String> {
        self.segments
            .iter()
            .flat_map(|s| s.parts.iter())
            .filter_map(|p| match p {
                Part::Var(n) => Some(n.clone()),
                Part::Lit(_) => None,
            })
            .collect()
    }

    /// Parses a template such as `/users/{id}/files/{name}.json`.
    pub fn parse(text: &str) -> Result<Template, String> {
        let body = text.strip_prefix('/').ok_or("a path must start with /")?;
        let mut segments = Vec::new();
        for raw in body.split('/') {
            let mut parts = Vec::new();
            let mut rest = raw;
            while !rest.is_empty() {
                if let Some(open) = rest.find('{') {
                    if open > 0 {
                        parts.push(Part::Lit(decode_lossy(&rest[..open])));
                    }
                    let after = &rest[open + 1..];
                    let close = after.find('}').ok_or("unclosed { in path template")?;
                    let name = &after[..close];
                    if name.is_empty() || name.contains('{') {
                        return Err("empty or nested template expression".into());
                    }
                    parts.push(Part::Var(name.to_owned()));
                    rest = &after[close + 1..];
                } else {
                    if rest.contains('}') {
                        return Err("unmatched } in path template".into());
                    }
                    parts.push(Part::Lit(decode_lossy(rest)));
                    rest = "";
                }
            }
            segments.push(Segment { parts });
        }
        let literal_chars = segments.iter().map(Segment::literal_len).sum();
        Ok(Template {
            text: text.to_owned(),
            segments,
            literal_chars,
        })
    }

    fn rank(&self) -> (usize, Vec<bool>) {
        (
            self.literal_chars,
            self.segments
                .iter()
                .map(|s| s.literal().is_some())
                .collect(),
        )
    }
}

/// Lenient percent-decoding: malformed escapes are kept as written.
fn decode_lossy(s: &str) -> String {
    match decode_segment(s) {
        Ok(d) => d.text,
        Err(_) => s.to_owned(),
    }
}

/// A decoded request path segment with a map back to its raw text.
#[derive(Debug, Clone)]
pub struct RawSegment<'a> {
    pub raw: &'a str,
    pub text: String,
    /// For each byte of `text`, the byte offset in `raw` where it came from;
    /// one extra entry holds `raw.len()`.
    map: Vec<usize>,
}

impl<'a> RawSegment<'a> {
    fn raw_slice(&self, start: usize, end: usize) -> &'a str {
        let s = self.map.get(start).copied().unwrap_or(self.raw.len());
        let e = self.map.get(end).copied().unwrap_or(self.raw.len());
        self.raw.get(s..e).unwrap_or("")
    }
}

/// Error for a malformed percent-escape or non-UTF-8 result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeError;

pub fn decode_segment(raw: &str) -> Result<RawSegment<'_>, DecodeError> {
    let b = raw.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut map = Vec::with_capacity(b.len() + 1);
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = b.get(i + 1..i + 3).ok_or(DecodeError)?;
            let v = std::str::from_utf8(hex)
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
                .filter(|_| hex.iter().all(u8::is_ascii_hexdigit))
                .ok_or(DecodeError)?;
            out.push(v);
            map.push(i);
            i += 3;
        } else {
            out.push(b[i]);
            map.push(i);
            i += 1;
        }
    }
    map.push(b.len());
    let text = String::from_utf8(out).map_err(|_| DecodeError)?;
    Ok(RawSegment { raw, text, map })
}

/// Splits a raw path into decoded segments (after the leading `/`).
pub fn split_path(raw: &str) -> Result<Vec<RawSegment<'_>>, DecodeError> {
    let body = raw.strip_prefix('/').unwrap_or(raw);
    body.split('/').map(decode_segment).collect()
}

/// Matches one segment's parts against decoded text. Returns `(start, end)`
/// byte ranges for each variable. Variables take at least one character;
/// a variable followed by a literal takes the shortest run that lets the
/// literal follow, and a final literal must end the segment.
fn match_parts(parts: &[Part], text: &str) -> Option<Vec<(usize, usize)>> {
    let mut pos = 0;
    let mut caps = Vec::new();
    let mut i = 0;
    while i < parts.len() {
        match &parts[i] {
            Part::Lit(l) => {
                if !text[pos..].starts_with(l.as_str()) {
                    return None;
                }
                pos += l.len();
            }
            Part::Var(_) => {
                let first = text[pos..].chars().next()?;
                let min_end = pos + first.len_utf8();
                match parts.get(i + 1) {
                    None => {
                        caps.push((pos, text.len()));
                        pos = text.len();
                    }
                    Some(Part::Var(_)) => {
                        caps.push((pos, min_end));
                        pos = min_end;
                    }
                    Some(Part::Lit(l)) => {
                        let at = if i + 2 == parts.len() {
                            // Final literal: it must end the segment.
                            let end = text.len().checked_sub(l.len())?;
                            if end < min_end || !text.is_char_boundary(end) || !text[end..].eq(l) {
                                return None;
                            }
                            end
                        } else {
                            min_end + text.get(min_end..)?.find(l.as_str())?
                        };
                        caps.push((pos, at));
                        pos = at;
                    }
                }
            }
        }
        i += 1;
    }
    (pos == text.len()).then_some(caps)
}

#[derive(Debug)]
struct Edge {
    parts: Vec<Part>,
    child: usize,
}

#[derive(Debug, Default)]
struct Node {
    literal: HashMap<String, usize>,
    patterned: Vec<Edge>,
    terminal: Option<usize>,
}

/// Template variables and their raw (still percent-encoded) values.
pub type Vars<'a> = Vec<(String, &'a str)>;

/// A successful match: the template and its variables' raw (still
/// percent-encoded) values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match<'a> {
    pub template: usize,
    pub vars: Vars<'a>,
}

/// All templates of a document, indexed for matching.
#[derive(Debug, Default)]
pub struct PathIndex {
    templates: Vec<Template>,
    nodes: Vec<Node>,
}

impl PathIndex {
    /// Adds a template; the returned id is its position. A template with the
    /// same shape as an earlier one is refused with the earlier one's text.
    /// Adds a template. A template that differs from one already added only
    /// in its variable names is the same path (OpenAPI 3.1.1 §4.8.8.2):
    /// `Err` gives that template's id.
    pub fn insert(&mut self, t: Template) -> Result<usize, usize> {
        if self.nodes.is_empty() {
            self.nodes.push(Node::default());
        }
        let mut node = 0;
        for seg in &t.segments {
            let next = match seg.literal() {
                Some(lit) => self.nodes[node].literal.get(lit).copied(),
                None => self.nodes[node]
                    .patterned
                    .iter()
                    .find(|e| same_shape(&e.parts, &seg.parts))
                    .map(|e| e.child),
            };
            node = match next {
                Some(c) => c,
                None => {
                    self.nodes.push(Node::default());
                    let c = self.nodes.len() - 1;
                    match seg.literal() {
                        Some(lit) => {
                            self.nodes[node].literal.insert(lit.to_owned(), c);
                        }
                        None => self.nodes[node].patterned.push(Edge {
                            parts: seg.parts.clone(),
                            child: c,
                        }),
                    }
                    c
                }
            };
        }
        if let Some(other) = self.nodes[node].terminal {
            return Err(other);
        }
        let id = self.templates.len();
        self.nodes[node].terminal = Some(id);
        self.templates.push(t);
        Ok(id)
    }

    /// Specificity of a template; larger is more specific.
    pub fn rank_of(&self, id: usize) -> (usize, Vec<bool>) {
        self.templates[id].rank()
    }

    pub fn template(&self, id: usize) -> &Template {
        &self.templates[id]
    }

    /// All templates matching the segments, best first.
    pub fn find<'a>(&self, segs: &[RawSegment<'a>]) -> Vec<Match<'a>> {
        let mut found: Vec<Match<'a>> = Vec::new();
        if self.nodes.is_empty() {
            return found;
        }
        // Depth-first over the trie. A trie node is reached by one path only,
        // so the work is bounded by the trie size.
        let mut stack: Vec<(usize, usize, Vars<'a>)> = vec![(0, 0, Vec::new())];
        while let Some((node, depth, vars)) = stack.pop() {
            if depth == segs.len() {
                if let Some(t) = self.nodes[node].terminal {
                    found.push(Match { template: t, vars });
                }
                continue;
            }
            let seg = &segs[depth];
            if let Some(&c) = self.nodes[node].literal.get(&seg.text) {
                stack.push((c, depth + 1, vars.clone()));
            }
            for edge in &self.nodes[node].patterned {
                let parts = &edge.parts;
                let Some(caps) = match_parts(parts, &seg.text) else {
                    continue;
                };
                let child = edge.child;
                let mut v = vars.clone();
                let names = parts.iter().filter_map(|p| match p {
                    Part::Var(n) => Some(n),
                    Part::Lit(_) => None,
                });
                for (name, (s, e)) in names.zip(caps) {
                    v.push((name.clone(), seg.raw_slice(s, e)));
                }
                stack.push((child, depth + 1, v));
            }
        }
        found.sort_by(|a, b| {
            let (ra, rb) = (
                self.templates[a.template].rank(),
                self.templates[b.template].rank(),
            );
            rb.cmp(&ra).then(a.template.cmp(&b.template))
        });
        found
    }
}

fn same_shape(a: &[Part], b: &[Part]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| match (x, y) {
            (Part::Lit(p), Part::Lit(q)) => p == q,
            (Part::Var(_), Part::Var(_)) => true,
            _ => false,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(ts: &[&str]) -> PathIndex {
        let mut ix = PathIndex::default();
        for t in ts {
            ix.insert(Template::parse(t).unwrap()).unwrap();
        }
        ix
    }

    fn best<'a>(ix: &'a PathIndex, path: &'a str) -> Option<(&'a str, Vec<(String, &'a str)>)> {
        let segs = split_path(path).ok()?;
        let m = ix.find(&segs).into_iter().next()?;
        Some((ix.template(m.template).text.as_str(), m.vars))
    }

    #[test]
    fn oas311_s4_8_8_2_concrete_beats_template() {
        let ix = index(&["/pets/{id}", "/pets/mine"]);
        assert_eq!(best(&ix, "/pets/mine").unwrap().0, "/pets/mine");
        assert_eq!(best(&ix, "/pets/7").unwrap().0, "/pets/{id}");
    }

    #[test]
    fn most_literal_characters_wins() {
        let ix = index(&["/f/{id}", "/f/{id}.json"]);
        let (t, vars) = best(&ix, "/f/abc.json").unwrap();
        assert_eq!(t, "/f/{id}.json");
        assert_eq!(vars, vec![("id".to_owned(), "abc")]);
        assert_eq!(best(&ix, "/f/abc.xml").unwrap().0, "/f/{id}");
    }

    #[test]
    fn rfc3986_s2_2_encoded_slash_stays_in_segment() {
        let ix = index(&["/files/{name}", "/files/{dir}/{name}"]);
        let (t, vars) = best(&ix, "/files/a%2Fb").unwrap();
        assert_eq!(t, "/files/{name}");
        assert_eq!(
            vars,
            vec![("name".to_owned(), "a%2Fb")],
            "raw value kept for style parsing"
        );
        assert_eq!(best(&ix, "/files/a/b").unwrap().0, "/files/{dir}/{name}");
    }

    #[test]
    fn literal_segments_compare_decoded() {
        let ix = index(&["/caf\u{e9}/x"]);
        let _ = ix;
        let ix = index(&["/café/x", "/a b"]);
        assert_eq!(best(&ix, "/caf%C3%A9/x").unwrap().0, "/café/x");
        assert_eq!(best(&ix, "/a%20b").unwrap().0, "/a b");
    }

    #[test]
    fn several_variables_in_one_segment() {
        let ix = index(&["/r/{a}-{b}.{ext}"]);
        let (_, vars) = best(&ix, "/r/x-y-z.tar.gz").unwrap();
        assert_eq!(
            vars,
            vec![
                ("a".to_owned(), "x"),
                ("b".to_owned(), "y-z"),
                ("ext".to_owned(), "tar.gz")
            ],
            "each variable takes the shortest run"
        );
        assert!(
            best(&ix, "/r/-y.gz").is_none(),
            "a variable needs one character"
        );
    }

    #[test]
    fn variables_need_a_character_and_segments_must_align() {
        let ix = index(&["/pets/{id}"]);
        assert!(best(&ix, "/pets/").is_none());
        assert!(best(&ix, "/pets").is_none());
        assert!(best(&ix, "/pets/1/2").is_none());
    }

    #[test]
    fn oas311_s4_8_8_2_same_shape_is_the_same_path() {
        let mut ix = PathIndex::default();
        let first = ix
            .insert(Template::parse("/pets/{petId}").unwrap())
            .unwrap();
        let e = ix
            .insert(Template::parse("/pets/{name}").unwrap())
            .unwrap_err();
        assert_eq!(e, first);
        assert_eq!(ix.template(e).text, "/pets/{petId}");
        assert!(ix
            .insert(Template::parse("/pets/{id}.json").unwrap())
            .is_ok());
    }

    #[test]
    fn template_syntax_errors() {
        for t in ["pets", "/pets/{id", "/pets/id}", "/pets/{}", "/a/{b{c}}"] {
            assert!(Template::parse(t).is_err(), "{t}");
        }
    }

    #[test]
    fn malformed_escapes_are_reported() {
        assert!(split_path("/a%zz").is_err());
        assert!(split_path("/a%").is_err());
        assert!(split_path("/a%FF").is_err(), "not UTF-8");
    }
}
