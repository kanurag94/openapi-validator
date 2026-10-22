//! ECMAScript (ECMA-262) regular expressions translated to the `regex` crate.
//!
//! JSON Schema `pattern` and `patternProperties` use ECMA-262 syntax
//! (JSON Schema Validation 2020-12 §4.3, §6.3.3; OpenAPI 3.0.4 §4.7.24.1).
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
//!
//! Two dialects are read. JSON Schema 2020-12 (OpenAPI 3.1 and 3.2) uses
//! the `u` flag's grammar, so an unknown escape is an error. OpenAPI 3.0's
//! JSON Schema (Wright draft 00) names ECMA-262 without flags, where Annex
//! B applies: `\A` is the letter A, `\c` without a letter is a backslash
//! and `c`, `\x`, `\u` and `\p` without their digits are letters, `\0` and
//! `\1`–`\9` past the group count are octal escapes (or the digits 8 and 9),
//! and `\k` is the letter k when the pattern has no named groups.

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
const DIGIT_RANGES: &[(u32, u32)] = &[(0x30, 0x39)];
const WORD_RANGES: &[(u32, u32)] = &[(0x30, 0x39), (0x41, 0x5A), (0x5F, 0x5F), (0x61, 0x7A)];
/// ECMA-262 WhiteSpace and LineTerminator code points (§12.2, §12.3).
const SPACE_RANGES: &[(u32, u32)] = &[
    (0x09, 0x0D),
    (0x20, 0x20),
    (0xA0, 0xA0),
    (0x1680, 0x1680),
    (0x2000, 0x200A),
    (0x2028, 0x2029),
    (0x202F, 0x202F),
    (0x205F, 0x205F),
    (0x3000, 0x3000),
    (0xFEFF, 0xFEFF),
];

/// Ranges of Unicode scalar values not in `ranges` (sorted, disjoint),
/// skipping the surrogate block, which no `char` can hold.
fn complement(ranges: &[(u32, u32)]) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    let mut next = 0u32;
    for &(a, b) in ranges {
        if a > next {
            out.push((next, a - 1));
        }
        next = b + 1;
    }
    if next <= 0x10FFFF {
        out.push((next, 0x10FFFF));
    }
    let mut scalar = Vec::new();
    for (a, b) in out {
        if b < 0xD800 || a > 0xDFFF {
            scalar.push((a, b));
        } else {
            if a < 0xD800 {
                scalar.push((a, 0xD7FF));
            }
            if b > 0xDFFF {
                scalar.push((0xE000, b));
            }
        }
    }
    scalar
}

fn class_items(ranges: &[(u32, u32)]) -> String {
    let mut s = String::new();
    for &(a, b) in ranges {
        if a == b {
            s.push_str(&format!("\\x{{{a:X}}}"));
        } else {
            s.push_str(&format!("\\x{{{a:X}}}-\\x{{{b:X}}}"));
        }
    }
    s
}

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
    translate_in(pattern, Dialect::Unicode)
}

/// Which ECMA-262 grammar a pattern is read with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    /// With the `u` flag (JSON Schema 2020-12).
    Unicode,
    /// Without flags, Annex B included (JSON Schema Wright draft 00).
    Legacy,
}

/// [`translate`] in a given dialect.
pub fn translate_in(pattern: &str, dialect: Dialect) -> Result<String, PatternError> {
    Translator::new(pattern, false, dialect).run()
}

/// Translates a pattern for the `regex` crate when it can run in linear
/// time, or else (lookaround, backreferences) for `fancy-regex`. The flag
/// says which engine the result is for.
pub fn translate_any(pattern: &str) -> Result<(String, bool), PatternError> {
    translate_any_in(pattern, Dialect::Unicode)
}

/// [`translate_any`] in a given dialect.
pub fn translate_any_in(pattern: &str, dialect: Dialect) -> Result<(String, bool), PatternError> {
    match translate_in(pattern, dialect) {
        Ok(t) => Ok((t, false)),
        Err(e) if e.reason.contains("backtracking") => Translator::new(pattern, true, dialect)
            .run()
            .map(|t| (t, true)),
        Err(e) => Err(e),
    }
}

/// Steps `fancy-regex` may take for one match before giving up.
pub const BACKTRACK_LIMIT: usize = 100_000;

/// A compiled pattern on whichever engine it needs.
#[derive(Debug)]
pub enum Matcher {
    Linear(Regex),
    Bounded(fancy_regex::Regex),
}

impl Matcher {
    /// `None` when the bounded engine gave up (step limit reached).
    pub fn is_match(&self, s: &str) -> Option<bool> {
        match self {
            Matcher::Linear(r) => Some(r.is_match(s)),
            Matcher::Bounded(r) => r.is_match(s).ok(),
        }
    }
}

/// Compiles a translated pattern for the engine `translate_any` chose.
pub fn build_any(translated: &str, backtracking: bool) -> Result<Matcher, String> {
    if !backtracking {
        return build(translated).map(Matcher::Linear);
    }
    fancy_regex::RegexBuilder::new(translated)
        .backtrack_limit(BACKTRACK_LIMIT)
        .delegate_size_limit(SIZE_LIMIT)
        .delegate_dfa_size_limit(SIZE_LIMIT)
        .build()
        .map(Matcher::Bounded)
        .map_err(|e| e.to_string())
}

struct Translator<'a> {
    src: Vec<char>,
    i: usize,
    out: String,
    pattern: &'a str,
    /// Emit `fancy-regex` syntax: lookaround and backreferences are kept.
    fancy: bool,
    /// Annex B reading (no `u` flag).
    legacy: bool,
    /// Capturing groups in the pattern, and whether any is named.
    groups: usize,
    named: bool,
}

/// Counts capturing groups, skipping escapes and classes.
fn count_groups(src: &[char]) -> (usize, bool) {
    let (mut n, mut named, mut i, mut in_class) = (0, false, 0, false);
    while i < src.len() {
        match src[i] {
            '\\' => i += 1,
            '[' if !in_class => in_class = true,
            ']' if in_class => in_class = false,
            '(' if !in_class => match (src.get(i + 1), src.get(i + 2)) {
                (Some('?'), Some('<')) if !matches!(src.get(i + 3), Some('=' | '!')) => {
                    n += 1;
                    named = true;
                }
                (Some('?'), _) => {}
                _ => n += 1,
            },
            _ => {}
        }
        i += 1;
    }
    (n, named)
}

const ASCII_WORD: &str = "[A-Za-z0-9_]";

impl<'a> Translator<'a> {
    fn new(pattern: &'a str, fancy: bool, dialect: Dialect) -> Translator<'a> {
        let src: Vec<char> = pattern.chars().collect();
        let (groups, named) = count_groups(&src);
        Translator {
            src,
            i: 0,
            out: String::with_capacity(pattern.len() + 8),
            pattern,
            fancy,
            legacy: dialect == Dialect::Legacy,
            groups,
            named,
        }
    }

    /// Annex B B.1.2 LegacyOctalEscapeSequence: up to three octal digits,
    /// the first already read, at most 0o377.
    fn octal(&mut self, first: char) -> ClassAtom {
        let mut v = first.to_digit(8).unwrap_or(0);
        for _ in 0..2 {
            match self.peek().and_then(|d| d.to_digit(8)) {
                Some(d) if v * 8 + d <= 0o377 => {
                    self.i += 1;
                    v = v * 8 + d;
                }
                _ => break,
            }
        }
        ClassAtom::Char(char::from_u32(v).unwrap_or('\0'))
    }
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
                            (Some(c @ ('=' | '!')), _) => {
                                if !self.fancy {
                                    return self.fail("lookahead needs backtracking");
                                }
                                self.i += 2;
                                self.out.push_str(if c == '=' { "(?=" } else { "(?!" });
                            }
                            (Some('<'), Some(c @ ('=' | '!'))) => {
                                if !self.fancy {
                                    return self.fail("lookbehind needs backtracking");
                                }
                                self.i += 3;
                                self.out.push_str(if c == '=' { "(?<=" } else { "(?<!" });
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
            'd' | 'D' | 'w' | 'W' | 's' | 'S' => {
                let ranges = match c.to_ascii_lowercase() {
                    'd' => DIGIT_RANGES,
                    'w' => WORD_RANGES,
                    _ => SPACE_RANGES,
                };
                let negated = c.is_ascii_uppercase();
                // Inside a class the items are written inline: a nested
                // `[...]` would be read as a literal `[` by ECMA-262 tooling
                // that sees the translated text again.
                let items = if negated {
                    complement(ranges)
                } else {
                    ranges.to_vec()
                };
                let body = class_items(&items);
                ClassAtom::Set(if in_class {
                    body
                } else if negated {
                    format!("[^{}]", class_items(ranges))
                } else {
                    format!("[{body}]")
                })
            }
            'b' if in_class => ClassAtom::Char('\u{8}'),
            // ASCII word boundaries; fancy-regex cannot switch Unicode
            // mode inline, so it gets the equivalent lookarounds.
            'b' if self.fancy => ClassAtom::Set(format!(
                "(?:(?<={ASCII_WORD})(?!{ASCII_WORD})|(?<!{ASCII_WORD})(?={ASCII_WORD}))"
            )),
            'B' if self.fancy && !in_class => ClassAtom::Set(format!(
                "(?:(?<={ASCII_WORD})(?={ASCII_WORD})|(?<!{ASCII_WORD})(?!{ASCII_WORD}))"
            )),
            'b' => ClassAtom::Set(r"(?-u:\b)".into()),
            'B' if !in_class => ClassAtom::Set(r"(?-u:\B)".into()),
            't' => ClassAtom::Char('\t'),
            'n' => ClassAtom::Char('\n'),
            'v' => ClassAtom::Char('\u{B}'),
            'f' => ClassAtom::Char('\u{C}'),
            'r' => ClassAtom::Char('\r'),
            '0' if !self.peek().is_some_and(|d| d.is_ascii_digit()) => ClassAtom::Char('\0'),
            '0'..='7' if self.legacy && (c == '0' || in_class || self.backref_beyond(c)) => {
                self.octal(c)
            }
            '8' | '9' if self.legacy && (in_class || self.backref_beyond(c)) => ClassAtom::Char(c),
            '1'..='9' if !in_class => {
                if !self.fancy {
                    return self.fail("backreferences need backtracking");
                }
                let mut n = String::from(c);
                while let Some(d) = self.peek().filter(char::is_ascii_digit) {
                    self.i += 1;
                    n.push(d);
                }
                ClassAtom::Set(format!("\\{n}"))
            }
            'k' if self.legacy && !self.named => ClassAtom::Char('k'),
            'k' if self.peek() == Some('<') => {
                if !self.fancy {
                    return self.fail("named backreferences need backtracking");
                }
                let start = self.i;
                while let Some(x) = self.peek() {
                    self.i += 1;
                    if x == '>' {
                        let body: String = self.src[start..self.i].iter().collect();
                        return Ok(ClassAtom::Set(format!("\\k{body}")));
                    }
                }
                return self.fail("unterminated named backreference");
            }
            'c' => match self.peek() {
                Some(l) if l.is_ascii_alphabetic() => {
                    self.i += 1;
                    ClassAtom::Char(char::from((l as u8) % 32))
                }
                // Annex B ClassControlLetter.
                Some(l) if self.legacy && in_class && (l.is_ascii_digit() || l == '_') => {
                    self.i += 1;
                    ClassAtom::Char(char::from((l as u8) % 32))
                }
                // Annex B: a backslash, and `c` read again as itself.
                _ if self.legacy => {
                    self.i -= 1;
                    ClassAtom::Char('\\')
                }
                _ => return self.fail(r"\c must be followed by an ASCII letter"),
            },
            'x' if self.legacy && self.peek_hex(2).is_none() => ClassAtom::Char('x'),
            'u' if self.legacy && self.peek_hex(4).is_none() => ClassAtom::Char('u'),
            'p' | 'P' if self.legacy => ClassAtom::Char(c),
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
            // Annex B IdentityEscape: any other character is itself.
            c if self.legacy => ClassAtom::Char(c),
            c if c.is_ascii_alphanumeric() => {
                return self.fail(format!("unknown escape \\{c}"));
            }
            c => ClassAtom::Char(c),
        })
    }

    /// `\N` names more groups than the pattern has (it starts at `first`,
    /// already read).
    fn backref_beyond(&self, first: char) -> bool {
        let mut n = first.to_digit(10).unwrap_or(0) as usize;
        let mut j = self.i;
        while let Some(d) = self.src.get(j).and_then(|d| d.to_digit(10)) {
            n = n.saturating_mul(10).saturating_add(d as usize);
            j += 1;
        }
        n > self.groups
    }

    /// The next `n` characters, when all are hex digits.
    fn peek_hex(&self, n: usize) -> Option<u32> {
        let mut v = 0u32;
        for k in 0..n {
            v = v * 16 + self.src.get(self.i + k)?.to_digit(16)?;
        }
        Some(v)
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

    // ECMA-262 Annex B (B.1.2): without the `u` flag, as OpenAPI 3.0 reads
    // patterns.
    #[test]
    fn ecma262_annex_b_in_the_legacy_dialect() {
        let m = |p: &str, s: &str| {
            build(&translate_in(p, Dialect::Legacy).unwrap_or_else(|e| panic!("{p}: {e}")))
                .unwrap()
                .is_match(s)
        };
        assert!(m(r"^\A$", "A"), "identity escape");
        assert!(
            m(r"^\c$", "\\c"),
            "\\c without a letter is a backslash and c"
        );
        assert!(m(r"^[\c1]$", "\u{11}"), "class control letter");
        assert!(m(r"^\x4$", "x4"), "\\x without two hex digits");
        assert!(m(r"^\u{2}$", "uu"), "\\u{{2}} is u twice");
        assert!(m(r"^\p{L}$", "p{L}"), "\\p is p");
        assert!(m(r"^\101$", "A"), "octal escape past the group count");
        assert!(m(r"^\8$", "8"));
        assert!(m(r"^\k$", "k"), "\\k without named groups");
        assert!(m(r"^\0101$", "\u{8}1"), "\\010 is octal, then 1");
        // The Unicode dialect keeps refusing them.
        for p in [r"\A", r"\c", r"\x4", r"\8", r"\k"] {
            assert!(translate_in(p, Dialect::Unicode).is_err(), "{p}");
        }
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
    fn backtracking_features_run_bounded_when_allowed() {
        let run = |p: &str, s: &str| {
            let (t, fancy) = translate_any(p).unwrap();
            assert!(fancy, "{p}");
            build_any(&t, fancy).unwrap().is_match(s)
        };
        assert_eq!(run(r"^(?=.*\d)\w+$", "ab1"), Some(true));
        assert_eq!(run(r"^(?=.*\d)\w+$", "abc"), Some(false));
        assert_eq!(run(r"(?<!x)y", "xy"), Some(false));
        assert_eq!(run(r"^(\w)\1$", "aa"), Some(true));
        assert_eq!(run(r"^(?<c>\w)\k<c>$", "ab"), Some(false));
        assert_eq!(
            run(r"\bfoo\b(?=!)", "a foo!"),
            Some(true),
            "ASCII word boundary"
        );
        assert_eq!(
            run(r"\bfoo\b(?=!)", "éfoo!"),
            Some(true),
            "é is not an ASCII word char"
        );
        // A step-limited engine gives up instead of running for ever.
        let long = format!("{}c", "a".repeat(60));
        assert!(run(r"^(?=a)(a|a)*\1b$", &long) != Some(true));
        // Linear patterns stay on the linear engine.
        assert_eq!(translate_any(r"^\d+$").map(|(_, f)| f), Ok(false));
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

    /// `jsonschema` translates ECMA-262 patterns again before compiling. Our
    /// output must already be a fixed point of that translation, or its
    /// meaning would change (a nested class `[[a-z]]` reads as a literal
    /// `[` in ECMA-262).
    #[test]
    fn translation_is_a_fixed_point_of_jsonschema_regex() {
        let pats = [
            r"^[a-z]+$",
            r"^\d{3}$",
            r"^[\w.-]+$",
            r"^\S+$",
            r"^[^\s]+$",
            r"^.+$",
            r"^(?:a|b)$",
            r"\bx\b",
            r"^[A-Za-z0-9 ]*$",
            r"^\p{L}+$",
            r"^[^/]+$",
            r"^[a-zA-Z0-9_\-]{1,50}$",
            r"^\w+@\w+\.\w+$",
            r"^[\u0020-\u007e]*$",
            r"^[^<>]*$",
            r"^(\+|-)?\d+$",
            r"^\$[a-z]+$",
            r"^[\x20-\x7E]+$",
            r"^a{2,}$",
            r"^[\s\S]*$",
            r"^[^\n]*$",
            r"^[A-Z]{2}\-[0-9]+$",
            r"^[\D\W]$",
            r"^[^\d\s]+$",
            r"^[&&~~]$",
            r"[]",
            r"[^]",
            r"(?<y>\d{4})-(?<m>\d{2})",
            r"^\cJ\u{1F600}\0$",
        ];
        for p in pats {
            let t = translate(p).unwrap();
            let again = jsonschema_regex::to_rust_regex(&t).map(|c| c.into_owned());
            assert_eq!(again.as_deref(), Ok(t.as_str()), "{p}");
        }
    }

    #[test]
    fn negated_escapes_inside_classes() {
        assert!(m(r"^[\D]$", "a"));
        assert!(!m(r"^[\D]$", "5"));
        assert!(m(r"^[\W5]$", "5"));
        assert!(m(r"^[\W]$", "😀"));
        assert!(!m(r"^[^\d\s]$", " "));
        assert!(m(r"^[\w.-]+$", "a.b-c"));
        assert!(!m(r"^[\w.-]+$", "a[b"), "no stray literal [");
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
