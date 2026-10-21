//! Strict JSON parsing (RFC 8259) into `serde_json::Value`.
//!
//! The parser is iterative, so nesting depth costs heap, not stack, and is
//! capped by [`Limits::max_depth`]. It refuses input that different parsers
//! disagree about, because a gateway that reads a body one way while the
//! upstream service reads it another way can be bypassed (a parser
//! differential):
//!
//! - duplicate object member names (RFC 8259 §4 says names SHOULD be unique;
//!   parsers then differ on which value wins);
//! - invalid UTF-8 (RFC 8259 §8.1);
//! - `\u` escapes that leave a lone surrogate (RFC 8259 §8.2, §7);
//! - numbers beyond [`crate::number::MAX_EXPONENT`] or
//!   [`crate::number::MAX_DIGITS`], which cannot be compared exactly at a
//!   bounded cost.
//!
//! One leading UTF-8 byte order mark is accepted and ignored
//! (RFC 8259 §8.1 allows a parser to ignore it).

use std::fmt;

use serde_json::{Map, Number, Value};

use crate::number::Decimal;

/// Bounds applied while parsing.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Maximum nesting depth of arrays and objects.
    pub max_depth: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { max_depth: 128 }
    }
}

/// Why a JSON text was refused, with the byte offset where it was noticed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonError {
    pub offset: usize,
    pub kind: JsonErrorKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JsonErrorKind {
    InvalidUtf8,
    UnexpectedEnd,
    UnexpectedChar,
    TrailingData,
    InvalidEscape,
    LoneSurrogate,
    ControlCharInString,
    InvalidNumber,
    NumberOutOfRange,
    DuplicateKey(String),
    TooDeep(usize),
}

impl fmt::Display for JsonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let at = self.offset;
        match &self.kind {
            JsonErrorKind::InvalidUtf8 => write!(f, "invalid UTF-8 at byte {at}"),
            JsonErrorKind::UnexpectedEnd => write!(f, "unexpected end of JSON at byte {at}"),
            JsonErrorKind::UnexpectedChar => write!(f, "unexpected character at byte {at}"),
            JsonErrorKind::TrailingData => write!(f, "trailing data after JSON value at byte {at}"),
            JsonErrorKind::InvalidEscape => write!(f, "invalid escape sequence at byte {at}"),
            JsonErrorKind::LoneSurrogate => write!(f, "lone UTF-16 surrogate escape at byte {at}"),
            JsonErrorKind::ControlCharInString => {
                write!(f, "unescaped control character in string at byte {at}")
            }
            JsonErrorKind::InvalidNumber => write!(f, "invalid number at byte {at}"),
            JsonErrorKind::NumberOutOfRange => {
                write!(f, "number at byte {at} is too large to check exactly")
            }
            JsonErrorKind::DuplicateKey(k) => {
                write!(f, "duplicate object key {k:?} at byte {at}")
            }
            JsonErrorKind::TooDeep(d) => write!(f, "nesting deeper than {d} at byte {at}"),
        }
    }
}

impl std::error::Error for JsonError {}

/// Parses `input` as one JSON value.
pub fn parse(input: &[u8], limits: Limits) -> Result<Value, JsonError> {
    let input = input.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(input);
    let text = std::str::from_utf8(input).map_err(|e| JsonError {
        offset: e.valid_up_to(),
        kind: JsonErrorKind::InvalidUtf8,
    })?;
    Parser {
        s: text.as_bytes(),
        text,
        i: 0,
        limits,
    }
    .run()
}

enum Frame {
    Array(Vec<Value>),
    Object(Map<String, Value>, Option<(String, usize)>),
}

struct Parser<'a> {
    s: &'a [u8],
    text: &'a str,
    i: usize,
    limits: Limits,
}

impl Parser<'_> {
    fn err<T>(&self, kind: JsonErrorKind) -> Result<T, JsonError> {
        Err(JsonError {
            offset: self.i,
            kind,
        })
    }

    fn ws(&mut self) {
        while let Some(b' ' | b'\t' | b'\n' | b'\r') = self.s.get(self.i) {
            self.i += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn expect_byte(&mut self, c: u8) -> Result<(), JsonError> {
        match self.peek() {
            Some(x) if x == c => {
                self.i += 1;
                Ok(())
            }
            Some(_) => self.err(JsonErrorKind::UnexpectedChar),
            None => self.err(JsonErrorKind::UnexpectedEnd),
        }
    }

    fn run(mut self) -> Result<Value, JsonError> {
        let mut stack: Vec<Frame> = Vec::new();
        'value: loop {
            // Parse one value, or open a container.
            self.ws();
            let mut value = match self.peek() {
                None => return self.err(JsonErrorKind::UnexpectedEnd),
                Some(b'[') => {
                    self.i += 1;
                    if stack.len() >= self.limits.max_depth {
                        return self.err(JsonErrorKind::TooDeep(self.limits.max_depth));
                    }
                    self.ws();
                    if self.peek() == Some(b']') {
                        self.i += 1;
                        Value::Array(Vec::new())
                    } else {
                        stack.push(Frame::Array(Vec::new()));
                        continue 'value;
                    }
                }
                Some(b'{') => {
                    self.i += 1;
                    if stack.len() >= self.limits.max_depth {
                        return self.err(JsonErrorKind::TooDeep(self.limits.max_depth));
                    }
                    self.ws();
                    if self.peek() == Some(b'}') {
                        self.i += 1;
                        Value::Object(Map::new())
                    } else {
                        let key = self.key()?;
                        stack.push(Frame::Object(Map::new(), Some(key)));
                        continue 'value;
                    }
                }
                Some(b'"') => Value::String(self.string()?),
                Some(b't') => self.literal("true", Value::Bool(true))?,
                Some(b'f') => self.literal("false", Value::Bool(false))?,
                Some(b'n') => self.literal("null", Value::Null)?,
                Some(b'-' | b'0'..=b'9') => Value::Number(self.number()?),
                Some(_) => return self.err(JsonErrorKind::UnexpectedChar),
            };
            // Attach the value to its container, closing containers as needed.
            loop {
                let Some(top) = stack.last_mut() else {
                    self.ws();
                    if self.i != self.s.len() {
                        return self.err(JsonErrorKind::TrailingData);
                    }
                    return Ok(value);
                };
                self.ws();
                match top {
                    Frame::Array(items) => {
                        items.push(value);
                        match self.peek() {
                            Some(b',') => {
                                self.i += 1;
                                continue 'value;
                            }
                            Some(b']') => {
                                self.i += 1;
                                let Some(Frame::Array(items)) = stack.pop() else {
                                    unreachable_frame();
                                    return self.err(JsonErrorKind::UnexpectedChar);
                                };
                                value = Value::Array(items);
                            }
                            None => return self.err(JsonErrorKind::UnexpectedEnd),
                            Some(_) => return self.err(JsonErrorKind::UnexpectedChar),
                        }
                    }
                    Frame::Object(map, pending) => {
                        if let Some((k, at)) = pending.take() {
                            if map.contains_key(&k) {
                                return Err(JsonError {
                                    offset: at,
                                    kind: JsonErrorKind::DuplicateKey(k),
                                });
                            }
                            map.insert(k, value);
                        }
                        match self.peek() {
                            Some(b',') => {
                                self.i += 1;
                                self.ws();
                                let key = self.key()?;
                                *pending = Some(key);
                                continue 'value;
                            }
                            Some(b'}') => {
                                self.i += 1;
                                let Some(Frame::Object(map, _)) = stack.pop() else {
                                    unreachable_frame();
                                    return self.err(JsonErrorKind::UnexpectedChar);
                                };
                                value = Value::Object(map);
                            }
                            None => return self.err(JsonErrorKind::UnexpectedEnd),
                            Some(_) => return self.err(JsonErrorKind::UnexpectedChar),
                        }
                    }
                }
            }
        }
    }

    /// Reads `"name" :` and returns the name with its offset.
    fn key(&mut self) -> Result<(String, usize), JsonError> {
        let at = self.i;
        if self.peek() != Some(b'"') {
            return match self.peek() {
                None => self.err(JsonErrorKind::UnexpectedEnd),
                Some(_) => self.err(JsonErrorKind::UnexpectedChar),
            };
        }
        let k = self.string()?;
        self.ws();
        self.expect_byte(b':')?;
        Ok((k, at))
    }

    fn literal(&mut self, word: &str, v: Value) -> Result<Value, JsonError> {
        if self.s[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            Ok(v)
        } else {
            self.err(JsonErrorKind::UnexpectedChar)
        }
    }

    fn number(&mut self) -> Result<Number, JsonError> {
        let start = self.i;
        while let Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') = self.peek() {
            self.i += 1;
        }
        let lit = &self.text[start..self.i];
        if Decimal::parse(lit).is_none() {
            self.i = start;
            // Distinguish malformed literals from well-formed but huge ones.
            return if is_number_syntax(lit) {
                self.err(JsonErrorKind::NumberOutOfRange)
            } else {
                self.err(JsonErrorKind::InvalidNumber)
            };
        }
        serde_json::from_str::<Number>(lit).or_else(|_| {
            self.i = start;
            self.err(JsonErrorKind::InvalidNumber)
        })
    }

    fn hex4(&mut self) -> Result<u32, JsonError> {
        let Some(h) = self.s.get(self.i..self.i + 4) else {
            return self.err(JsonErrorKind::InvalidEscape);
        };
        let mut v = 0u32;
        for &c in h {
            let d = match c {
                b'0'..=b'9' => c - b'0',
                b'a'..=b'f' => c - b'a' + 10,
                b'A'..=b'F' => c - b'A' + 10,
                _ => return self.err(JsonErrorKind::InvalidEscape),
            };
            v = v * 16 + u32::from(d);
        }
        self.i += 4;
        Ok(v)
    }

    fn string(&mut self) -> Result<String, JsonError> {
        self.i += 1; // opening quote
        let mut out = String::new();
        loop {
            let run_start = self.i;
            while let Some(c) = self.peek() {
                if c == b'"' || c == b'\\' || c < 0x20 {
                    break;
                }
                self.i += 1;
            }
            out.push_str(&self.text[run_start..self.i]);
            match self.peek() {
                None => return self.err(JsonErrorKind::UnexpectedEnd),
                Some(b'"') => {
                    self.i += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.i += 1;
                    let Some(e) = self.peek() else {
                        return self.err(JsonErrorKind::UnexpectedEnd);
                    };
                    self.i += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let esc_at = self.i - 2;
                            let hi = self.hex4()?;
                            let cp = if (0xD800..0xDC00).contains(&hi) {
                                if self.s.get(self.i..self.i + 2) != Some(b"\\u") {
                                    self.i = esc_at;
                                    return self.err(JsonErrorKind::LoneSurrogate);
                                }
                                self.i += 2;
                                let lo = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&lo) {
                                    self.i = esc_at;
                                    return self.err(JsonErrorKind::LoneSurrogate);
                                }
                                0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                            } else if (0xDC00..0xE000).contains(&hi) {
                                self.i = esc_at;
                                return self.err(JsonErrorKind::LoneSurrogate);
                            } else {
                                hi
                            };
                            match char::from_u32(cp) {
                                Some(c) => out.push(c),
                                None => return self.err(JsonErrorKind::InvalidEscape),
                            }
                        }
                        _ => {
                            self.i -= 2;
                            return self.err(JsonErrorKind::InvalidEscape);
                        }
                    }
                }
                Some(_) => return self.err(JsonErrorKind::ControlCharInString),
            }
        }
    }
}

fn is_number_syntax(lit: &str) -> bool {
    // Same grammar as `Decimal::parse`, without the range limits.
    let t = lit.strip_prefix('-').unwrap_or(lit);
    let (mant, exp) = match t.find(['e', 'E']) {
        Some(p) => (&t[..p], Some(&t[p + 1..])),
        None => (t, None),
    };
    let (int, frac) = match mant.find('.') {
        Some(p) => (&mant[..p], Some(&mant[p + 1..])),
        None => (mant, None),
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit());
    let int_ok = int == "0" || (digits(int) && !int.starts_with('0'));
    let frac_ok = frac.is_none_or(digits);
    let exp_ok = exp.is_none_or(|e| digits(e.strip_prefix(['+', '-']).unwrap_or(e)));
    int_ok && frac_ok && exp_ok
}

#[cold]
fn unreachable_frame() {
    debug_assert!(false, "frame kind mismatch");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<Value, JsonErrorKind> {
        parse(s.as_bytes(), Limits::default()).map_err(|e| e.kind)
    }

    #[test]
    fn parses_rfc8259_values() {
        assert_eq!(
            p(r#" {"a": [1, -2.5e3, true, false, null, "x"]} "#).unwrap()["a"][1].to_string(),
            "-2.5e+3"
        );
        assert_eq!(p("[]").unwrap(), Value::Array(vec![]));
        assert_eq!(p("{}").unwrap(), Value::Object(Map::new()));
        assert_eq!(
            p(r#""\u00e9\ud83d\ude00\n""#).unwrap(),
            Value::String("é😀\n".into())
        );
    }

    #[test]
    fn numbers_keep_their_literal() {
        assert_eq!(p("1.0").unwrap().to_string(), "1.0");
        assert_eq!(
            p("9007199254740993").unwrap().to_string(),
            "9007199254740993"
        );
        assert_eq!(
            p("[1e400]").unwrap()[0]
                .as_number()
                .map(|n| n.to_string().contains('e')),
            Some(true)
        );
    }

    #[test]
    fn rfc8259_s4_duplicate_names_are_refused() {
        assert_eq!(
            p(r#"{"a":1,"a":2}"#),
            Err(JsonErrorKind::DuplicateKey("a".into()))
        );
        assert_eq!(
            p(r#"{"a":1,"\u0061":2}"#),
            Err(JsonErrorKind::DuplicateKey("a".into()))
        );
        assert!(p(r#"{"a":{"a":1}}"#).is_ok());
    }

    #[test]
    fn rfc8259_s8_1_invalid_utf8_is_refused_and_bom_ignored() {
        assert_eq!(
            parse(b"\"\xff\"", Limits::default()).map_err(|e| e.kind),
            Err(JsonErrorKind::InvalidUtf8)
        );
        assert_eq!(
            parse(b"\xEF\xBB\xBF{\"a\":1}", Limits::default()).unwrap()["a"],
            1
        );
        assert!(
            parse(b"\xEF\xBB\xBF\xEF\xBB\xBF{}", Limits::default()).is_err(),
            "only one BOM"
        );
    }

    #[test]
    fn rfc8259_s8_2_lone_surrogates_are_refused() {
        assert_eq!(p(r#""\ud800""#), Err(JsonErrorKind::LoneSurrogate));
        assert_eq!(p(r#""\udc00""#), Err(JsonErrorKind::LoneSurrogate));
        assert_eq!(p(r#""\ud800\u0041""#), Err(JsonErrorKind::LoneSurrogate));
        assert_eq!(p(r#""\ud800x""#), Err(JsonErrorKind::LoneSurrogate));
    }

    #[test]
    fn malformed_input_is_refused() {
        for bad in [
            "", "[", "[1,]", "{\"a\"}", "{\"a\":}", "01", "1.", "tru", "\"\t\"", "\"\\x\"", "[1]x",
            "{1:2}", "nul",
        ] {
            assert!(p(bad).is_err(), "{bad:?}");
        }
        assert_eq!(p("1e99999"), Err(JsonErrorKind::NumberOutOfRange));
        assert_eq!(p("1.2.3"), Err(JsonErrorKind::InvalidNumber));
    }

    #[test]
    fn depth_is_bounded_without_recursion() {
        let deep = "[".repeat(1_000_000);
        assert_eq!(p(&deep), Err(JsonErrorKind::TooDeep(128)));
        let ok = format!("{}{}", "[".repeat(128), "]".repeat(128));
        assert!(p(&ok).is_ok());
        let over = format!("{}{}", "[".repeat(129), "]".repeat(129));
        assert_eq!(p(&over), Err(JsonErrorKind::TooDeep(128)));
    }
}
