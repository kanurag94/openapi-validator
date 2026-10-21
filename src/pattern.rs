//! ECMAScript (ECMA-262) regular expressions translated to the `regex` crate.
//!
//! JSON Schema `pattern` and `patternProperties` use ECMA-262 syntax
//! (JSON Schema Validation 2020-12 §4.3, §6.3.3; OpenAPI 3.0.4 §4.8.24.1).
//! The `regex` crate matches in linear time, which a proxy needs, but its
//! syntax and some meanings differ. This module rewrites a pattern so the
//! `regex` crate gives the ECMA-262 answer:
//!
//! - `\d`, `\w`, `\b` have their ASCII meaning; `\s` is the ECMA-262
//!   WhiteSpace and LineTerminator set (§22.2.2.9);
//! - `.` does not match line terminators (§22.2.2.7, without the `s` flag);
//! - `\cX`, `\uXXXX` (including surrogate pairs), `\u{X}`, `\xHH`, `\0`,
//!   `\v` and identity escapes are translated;
//! - a `{` or `}` that does not form a quantifier is a literal;
//! - `[]` matches nothing and `[^]` matches any character.
//!
//! Features that need backtracking (lookahead, lookbehind, backreferences)
//! cannot run in linear time and are refused with an error that names the
//! pattern.

use std::fmt;

use regex::{Regex, RegexBuilder};

/// Compiled program size limit for one pattern.
const SIZE_LIMIT: usize = 1 << 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternError {
    pub pattern: String,
    pub reason: String,
}

impl fmt::Display for PatternError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "pattern {:?}: {}", self.pattern, self.reason)
    }
}

impl std::error::Error for PatternError {}

/// ECMA-262 WhiteSpace and LineTerminator code points, as class items.
const ECMA_SPACE: &str = r"\t\n\x0B\x0C\r \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}";
const DIGIT: &str = "0-9";
const WORD: &str = "A-Za-z0-9_";

/// Translates and compiles an ECMA-262 pattern.
pub fn compile(pattern: &str) -> Result<Regex, PatternError> {
    let translated = translate(pattern)?;
    build(&translated).map_err(|reason| PatternError {
        pattern: pattern.to_owned(),
        reason,
    })
}

/// Compiles an already translated pattern.
pub fn build(translated: &str) -> Result<Regex, String> {
    RegexBuilder::new(translated)
        .size_limit(SIZE_LIMIT)
        .dfa_size_limit(SIZE_LIMIT)
        .build()
        .map_err(|e| match e {
            regex::Error::CompiledTooBig(_) => "pattern is too large to compile".to_owned(),
            other => other
                .to_string()
                .lines()
                .last()
                .unwrap_or("invalid")
                .trim()
                .to_owned(),
        })
}

/// Rewrites an ECMA-262 pattern into equivalent `regex` crate syntax.
pub fn translate(pattern: &str) -> Result<String, PatternError> {
    Translator {
        src: pattern.chars().collect(),
        i: 0,
        out: String::with_capacity(pattern.len() + 8),
        pattern,
    }
    .run()
}

struct Translator<'a> {
    src: Vec<char>,
    i: usize,
    out: String,
    pattern: &'a str,
}

/// One atom inside a character class.
enum ClassAtom {
    Char(char),
    /// A set such as `\d` that cannot be a range endpoint.
    Set(String),
}

impl Translator<'_> {
    fn fail<T>(&self, reason: impl Into<String>) -> Result<T, PatternError> {
        Err(PatternError {
            pattern: self.pattern.to_owned(),
            reason: reason.into(),
        })
    }

    fn peek(&self) -> Option<char> {
        self.src.get(self.i).copied()
    }

    fn peek_at(&self, k: usize) -> Option<char> {
        self.src.get(self.i + k).copied()
    }

    fn run(mut self) -> Result<String, PatternError> {
        let mut depth = 0usize;
        while let Some(c) = self.peek() {
            self.i += 1;
            match c {
                '\\' => {
                    let atom = self.escape(false)?;
                    match atom {
                        ClassAtom::Char(c) => push_literal(&mut self.out, c),
                        ClassAtom::Set(s) => self.out.push_str(&s),
                    }
                }
                '.' => self.out.push_str(r"[^\n\r\x{2028}\x{2029}]"),
                '[' => self.class()?,
                '(' => {
                    depth += 1;
                    if self.peek() == Some('?') {
                        match (self.peek_at(1), self.peek_at(2)) {
                            (Some(':'), _) => {
                                self.i += 2;
                                self.out.push_str("(?:");
                            }
                            (Some('=' | '!'), _) => {
                                return self.fail("lookahead needs backtracking")
                            }
                            (Some('<'), Some('=' | '!')) => {
                                return self.fail("lookbehind needs backtracking")
                            }
                            (Some('<'), _) => {
                                self.i += 2;
                                let mut name = String::new();
                                while let Some(n) = self.peek() {
                                    self.i += 1;
                                    if n == '>' {
                                        break;
                                    }
                                    name.push(n);
                                }
                                if name.is_empty()
                                    || !name.chars().all(|n| n.is_alphanumeric() || n == '_')
                                {
                                    return self.fail("invalid group name");
                                }
                                self.out.push_str("(?P<");
                                self.out.push_str(&name);
                                self.out.push('>');
                            }
                            _ => return self.fail("unsupported group syntax"),
                        }
                    } else {
                        self.out.push('(');
                    }
                }
                ')' => {
                    if depth == 0 {
                        return self.fail("unbalanced parenthesis");
                    }
                    depth -= 1;
                    self.out.push(')');
                }
                '{' => {
                    if let Some(q) = self.quantifier_body() {
                        self.out.push('{');
                        self.out.push_str(&q);
                        self.out.push('}');
                    } else {
                        self.out.push_str(r"\{");
                    }
                }
                '}' => self.out.push_str(r"\}"),
                ']' => self.out.push_str(r"\]"),
                '^' | '$' | '|' | '*' | '+' | '?' => self.out.push(c),
                c => push_literal(&mut self.out, c),
            }
        }
        if depth != 0 {
            return self.fail("unbalanced parenthesis");
        }
        Ok(self.out)
    }

    /// After `{`: returns `n`, `n,` or `n,m` and consumes through `}` when the
    /// brace forms a quantifier.
    fn quantifier_body(&mut self) -> Option<String> {
        let mut j = self.i;
        let mut body = String::new();
        let mut seen_digit = false;
        let mut seen_comma = false;
        while let Some(&c) = self.src.get(j) {
            j += 1;
            match c {
                '0'..='9' => {
                    seen_digit = true;
                    body.push(c);
                }
                ',' if seen_digit && !seen_comma => {
                    seen_comma = true;
                    body.push(c);
                }
                '}' if seen_digit => {
                    self.i = j;
                    return Some(body);
                }
                _ => return None,
            }
        }
        None
    }

    /// After a backslash. `in_class` changes `\b` (backspace in a class) and
    /// the handling of `\B`.
    fn escape(&mut self, in_class: bool) -> Result<ClassAtom, PatternError> {
        let Some(c) = self.peek() else {
            return self.fail("pattern ends with a backslash");
        };
        self.i += 1;
        Ok(match c {
            'd' => ClassAtom::Set(format!("[{DIGIT}]")),
            'D' => ClassAtom::Set(format!("[^{DIGIT}]")),
            'w' => ClassAtom::Set(format!("[{WORD}]")),
            'W' => ClassAtom::Set(format!("[^{WORD}]")),
            's' => ClassAtom::Set(format!("[{ECMA_SPACE}]")),
            'S' => ClassAtom::Set(format!("[^{ECMA_SPACE}]")),
            'b' if in_class => ClassAtom::Char('\u{8}'),
            'b' => ClassAtom::Set(r"(?-u:\b)".into()),
            'B' if !in_class => ClassAtom::Set(r"(?-u:\B)".into()),
            't' => ClassAtom::Char('\t'),
            'n' => ClassAtom::Char('\n'),
            'v' => ClassAtom::Char('\u{B}'),
            'f' => ClassAtom::Char('\u{C}'),
            'r' => ClassAtom::Char('\r'),
            '0' if !self.peek().is_some_and(|d| d.is_ascii_digit()) => ClassAtom::Char('\0'),
            '1'..='9' => return self.fail("backreferences need backtracking"),
            'k' if self.peek() == Some('<') => {
                return self.fail("named backreferences need backtracking")
            }
            'c' => match self.peek() {
                Some(l) if l.is_ascii_alphabetic() => {
                    self.i += 1;
                    ClassAtom::Char(char::from((l as u8) % 32))
                }
                _ => return self.fail(r"\c must be followed by an ASCII letter"),
            },
            'x' => {
                let h = self
                    .hex_digits(2)
                    .ok_or_else(|| self.err(r"\x needs two hex digits"))?;
                ClassAtom::Char(char::from_u32(h).ok_or_else(|| self.err("invalid code point"))?)
            }
            'u' => {
                if self.peek() == Some('{') {
                    self.i += 1;
                    let mut v: u32 = 0;
                    let mut n = 0;
                    while let Some(d) = self.peek().and_then(|d| d.to_digit(16)) {
                        self.i += 1;
                        n += 1;
                        v = v.saturating_mul(16).saturating_add(d);
                    }
                    if n == 0 || self.peek() != Some('}') {
                        return self.fail(r"invalid \u{...} escape");
                    }
                    self.i += 1;
                    ClassAtom::Char(
                        char::from_u32(v).ok_or_else(|| self.err("invalid code point"))?,
                    )
                } else {
                    let hi = self
                        .hex_digits(4)
                        .ok_or_else(|| self.err(r"\u needs four hex digits"))?;
                    let cp = if (0xD800..0xDC00).contains(&hi) {
                        if self.peek() == Some('\\') && self.peek_at(1) == Some('u') {
                            let save = self.i;
                            self.i += 2;
                            match self.hex_digits(4) {
                                Some(lo) if (0xDC00..0xE000).contains(&lo) => {
                                    0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                                }
                                _ => {
                                    self.i = save;
                                    return self.fail("lone surrogate escape");
                                }
                            }
                        } else {
                            return self.fail("lone surrogate escape");
                        }
                    } else {
                        hi
                    };
                    ClassAtom::Char(
                        char::from_u32(cp).ok_or_else(|| self.err("lone surrogate escape"))?,
                    )
                }
            }
            'p' | 'P' => {
                if self.peek() != Some('{') {
                    return self.fail(r"\p needs a {property}");
                }
                let start = self.i;
                while let Some(x) = self.peek() {
                    self.i += 1;
                    if x == '}' {
                        let body: String = self.src[start..self.i].iter().collect();
                        return Ok(ClassAtom::Set(format!("\\{c}{body}")));
                    }
                }
                return self.fail(r"unterminated \p{...}");
            }
            c if c.is_ascii_alphanumeric() => {
                return self.fail(format!("unknown escape \\{c}"));
            }
            c => ClassAtom::Char(c),
        })
    }

    fn err(&self, reason: &str) -> PatternError {
        PatternError {
            pattern: self.pattern.to_owned(),
            reason: reason.to_owned(),
        }
    }

    fn hex_digits(&mut self, n: usize) -> Option<u32> {
        let mut v = 0;
        for k in 0..n {
            v = v * 16 + self.peek_at(k)?.to_digit(16)?;
        }
        self.i += n;
        Some(v)
    }

    fn class_atom(&mut self) -> Result<Option<ClassAtom>, PatternError> {
        match self.peek() {
            None => self.fail("unterminated character class"),
            Some(']') => Ok(None),
            Some('\\') => {
                self.i += 1;
                if self.peek() == Some('-') {
                    self.i += 1;
                    return Ok(Some(ClassAtom::Char('-')));
                }
                self.escape(true).map(Some)
            }
            Some(c) => {
                self.i += 1;
                Ok(Some(ClassAtom::Char(c)))
            }
        }
    }

    /// After `[`.
    fn class(&mut self) -> Result<(), PatternError> {
        let negated = self.peek() == Some('^');
        if negated {
            self.i += 1;
        }
        let mut items = String::new();
        let mut any = false;
        loop {
            let Some(first) = self.class_atom()? else {
                self.i += 1; // ']'
                break;
            };
            any = true;
            // A range needs `-` followed by an atom other than `]`.
            let is_range = self.peek() == Some('-') && self.peek_at(1).is_some_and(|c| c != ']');
            if is_range {
                self.i += 1;
                let Some(second) = self.class_atom()? else {
                    return self.fail("unterminated character class");
                };
                match (first, second) {
                    (ClassAtom::Char(a), ClassAtom::Char(b)) => {
                        if a > b {
                            return self.fail("character class range out of order");
                        }
                        push_class_char(&mut items, a);
                        items.push('-');
                        push_class_char(&mut items, b);
                    }
                    (a, b) => {
                        // `[\d-z]`: ECMA-262 Annex B reads the dash literally.
                        push_class_atom(&mut items, a);
                        push_class_char(&mut items, '-');
                        push_class_atom(&mut items, b);
                    }
                }
            } else {
                push_class_atom(&mut items, first);
            }
        }
        match (any, negated) {
            (false, false) => self.out.push_str(r"[^\x{0}-\x{10FFFF}]"),
            (false, true) => self.out.push_str(r"[\x{0}-\x{10FFFF}]"),
            (true, false) => {
                self.out.push('[');
                self.out.push_str(&items);
                self.out.push(']');
            }
            (true, true) => {
                self.out.push_str("[^");
                self.out.push_str(&items);
                self.out.push(']');
            }
        }
        Ok(())
    }
}

fn push_class_atom(out: &mut String, a: ClassAtom) {
    match a {
        ClassAtom::Char(c) => push_class_char(out, c),
        ClassAtom::Set(s) => out.push_str(&s),
    }
}

/// Characters that are always safe unescaped, in or out of a class.
fn plain(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || c == '_'
        || c == ' '
        || (!c.is_ascii() && !c.is_control() && !c.is_whitespace())
}

fn push_class_char(out: &mut String, c: char) {
    push_literal(out, c);
}

fn push_literal(out: &mut String, c: char) {
    if plain(c) {
        out.push(c);
    } else {
        out.push_str(&format!("\\x{{{:X}}}", c as u32));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(p: &str, s: &str) -> bool {
        compile(p).unwrap_or_else(|e| panic!("{e}")).is_match(s)
    }

    #[test]
    fn ecma262_s22_2_2_9_class_escapes_are_ascii() {
        assert!(m(r"^\d+$", "123"));
        assert!(!m(r"^\d$", "٣"), "Arabic-Indic digit is not \\d");
        assert!(m(r"^\w+$", "a_Z9"));
        assert!(!m(r"^\w$", "é"));
        assert!(m(r"^\s$", "\u{FEFF}"));
        assert!(m(r"^\s$", "\u{2028}"));
        assert!(!m(r"^\s$", "\u{85}"), "NEL is not ECMA whitespace");
        assert!(m(r"^[\d\s]+$", "1 2"));
        assert!(m(r"^[^\d]$", "a"));
        assert!(m(r"^[\D]$", "a"));
    }

    #[test]
    fn ecma262_escapes() {
        assert!(m(r"^\u0041$", "A"));
        assert!(m(r"^\uD83D\uDE00$", "😀"));
        assert!(m(r"^\u{1F600}$", "😀"));
        assert!(m(r"^\x41$", "A"));
        assert!(m(r"^\cJ$", "\n"));
        assert!(m(r"^\ca$", "\u{1}"));
        assert!(m(r"^\v$", "\u{B}"));
        assert!(m(r"^\0$", "\0"));
        assert!(m(r"^\/\.\-$", "/.-"));
        assert!(m(r"^[\b]$", "\u{8}"));
        assert!(m(r"\bfoo\b", "a foo b"));
        assert!(!m(r"\bfoo\b", "afoob"));
    }

    #[test]
    fn dot_excludes_line_terminators() {
        assert!(!m("^.$", "\n"));
        assert!(!m("^.$", "\u{2028}"));
        assert!(m("^.$", "😀"));
    }

    #[test]
    fn braces_and_classes() {
        assert!(m("^a{2}$", "aa"));
        assert!(m("^a{2,}$", "aaa"));
        assert!(m("^a{1,2}$", "a"));
        assert!(m("^{foo}$", "{foo}"));
        assert!(m("^a{$", "a{"));
        assert!(m("^a{,2}$", "a{,2}"));
        assert!(m("^[a-c-]+$", "a-c"));
        assert!(m("^[-a]$", "-"));
        assert!(m(r"^[\d-z]+$", "1-z"));
        assert!(m("^[[]$", "["));
        assert!(m("^[&&]+$", "&&"));
        assert!(m("^[~~]$", "~"));
        assert!(!m("^[]$", "a"));
        assert!(m("^[^]$", "\n"));
        assert!(m("^]$", "]"));
        assert!(m("(?<year>[0-9]{4})", "2024"));
        assert!(m(r"^\p{Lu}$", "Á"));
    }

    #[test]
    fn backtracking_features_are_refused_by_name() {
        for p in [
            r"(?=a)",
            r"(?!a)",
            r"(?<=a)b",
            r"(?<!a)b",
            r"(a)\1",
            r"(?<n>a)\k<n>",
        ] {
            let e = compile(p).unwrap_err();
            assert_eq!(e.pattern, p);
            assert!(e.reason.contains("backtracking"), "{e}");
        }
    }

    #[test]
    fn invalid_patterns_are_refused() {
        for p in [
            "(",
            ")",
            "[a",
            r"\",
            r"\q",
            "[z-a]",
            r"\uD800",
            "a{99999999}",
        ] {
            assert!(compile(p).is_err(), "{p}");
        }
    }

    #[test]
    fn pathological_patterns_run_in_linear_time() {
        let re = compile("^(a+)+$").unwrap();
        let s = format!("{}!", "a".repeat(100_000));
        let t = std::time::Instant::now();
        assert!(!re.is_match(&s));
        assert!(t.elapsed() < std::time::Duration::from_secs(1));
    }
}
