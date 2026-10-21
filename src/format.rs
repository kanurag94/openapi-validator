//! Format assertions (JSON Schema Validation 2020-12 §7.3, OpenAPI 3.0.4
//! §4.4 and 3.1.1 §4.2 "Data Types").
//!
//! Only the formats listed in [`ASSERTED`] are checked. Every other format,
//! including OpenAPI's `float`, `double`, `byte`, `binary` and `password`,
//! is an annotation and never fails a value.

use crate::number::Decimal;

/// Formats this library asserts.
pub const ASSERTED: &[&str] = &[
    // JSON Schema 2020-12 §7.3.
    "date-time",
    "date",
    "time",
    "duration",
    "email",
    "idn-email",
    "hostname",
    "idn-hostname",
    "ipv4",
    "ipv6",
    "uri",
    "uri-reference",
    "iri",
    "iri-reference",
    "uuid",
    "uri-template",
    "json-pointer",
    "relative-json-pointer",
    "regex",
    // OpenAPI data types (3.0.4 §4.4, 3.1.1 §4.2 and the Format Registry).
    "int32",
    "int64",
    "float",
    "double",
    "byte",
];

/// Formats that constrain numbers rather than strings.
pub fn is_numeric_format(name: &str) -> bool {
    matches!(name, "int32" | "int64" | "float" | "double")
}

/// Checks a string against a string format. Unknown formats pass.
pub fn check_str(format: &str, s: &str) -> bool {
    match format {
        "date" => is_date(s),
        "time" => is_time(s),
        "date-time" => is_date_time(s),
        "email" => is_email(s),
        "uuid" => is_uuid(s),
        "uri" => is_uri(s),
        "uri-reference" => is_uri_reference(s),
        "ipv4" => is_ipv4(s),
        "ipv6" => is_ipv6(s),
        "hostname" => is_hostname(s),
        "idn-hostname" => is_idn_hostname(s),
        "idn-email" => is_idn_email(s),
        "duration" => is_duration(s),
        "iri" => is_iri(s),
        "iri-reference" => is_iri_reference(s),
        "uri-template" => is_uri_template(s),
        "json-pointer" => is_json_pointer(s),
        "relative-json-pointer" => is_relative_json_pointer(s),
        "regex" => jsonschema_regex::is_valid_ecma_regex(s),
        "byte" => is_base64(s),
        _ => true,
    }
}

/// Checks a number against a numeric format: `int32` and `int64` need an
/// integer in the signed range; `float` and `double` need a value within the
/// IEEE 754 binary32 / binary64 finite range.
pub fn check_number(format: &str, n: &Decimal) -> bool {
    let within = |limit: &str| {
        let Some(max) = Decimal::parse(limit) else {
            return true;
        };
        let Some(min) = Decimal::parse(&format!("-{limit}")) else {
            return true;
        };
        *n >= min && *n <= max
    };
    match format {
        "int32" | "int64" => {
            let Some(v) = n.to_i128() else {
                return false;
            };
            if format == "int32" {
                i32::try_from(v).is_ok()
            } else {
                i64::try_from(v).is_ok()
            }
        }
        "float" => within("3.4028234663852886e38"),
        "double" => within("1.7976931348623157e308"),
        _ => true,
    }
}

fn digits(s: &str, n: usize) -> Option<u32> {
    if s.len() != n || !s.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400) => {
            29
        }
        2 => 28,
        _ => 0,
    }
}

/// RFC 3339 §5.6 `full-date`.
pub fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    if !s.is_ascii() || b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return false;
    }
    let (Some(y), Some(m), Some(d)) = (
        digits(&s[0..4], 4),
        digits(&s[5..7], 2),
        digits(&s[8..10], 2),
    ) else {
        return false;
    };
    (1..=12).contains(&m) && d >= 1 && d <= days_in_month(y, m)
}

/// Parsed RFC 3339 `full-time`: seconds since midnight (local) and the offset
/// in minutes.
struct Time {
    hour: u32,
    minute: u32,
    second: u32,
    offset_minutes: i32,
}

fn parse_time(s: &str) -> Option<Time> {
    // partial-time = time-hour ":" time-minute ":" time-second [time-secfrac]
    let b = s.as_bytes();
    if !s.is_ascii() || b.len() < 9 || b[2] != b':' || b[5] != b':' {
        return None;
    }
    let hour = digits(&s[0..2], 2)?;
    let minute = digits(&s[3..5], 2)?;
    let second = digits(&s[6..8], 2)?;
    let mut rest = &s[8..];
    if let Some(frac) = rest.strip_prefix('.') {
        let n = frac.bytes().take_while(u8::is_ascii_digit).count();
        if n == 0 {
            return None;
        }
        rest = &frac[n..];
    }
    // time-offset = "Z" / time-numoffset
    let offset_minutes = match rest {
        "Z" | "z" => 0,
        _ => {
            let rb = rest.as_bytes();
            if rb.len() != 6 || !matches!(rb[0], b'+' | b'-') || rb[3] != b':' {
                return None;
            }
            let oh = digits(&rest[1..3], 2)?;
            let om = digits(&rest[4..6], 2)?;
            if oh > 23 || om > 59 {
                return None;
            }
            let v = (oh * 60 + om) as i32;
            if rb[0] == b'-' {
                -v
            } else {
                v
            }
        }
    };
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let t = Time {
        hour,
        minute,
        second,
        offset_minutes,
    };
    if t.second == 60 {
        // RFC 3339 §5.7: a leap second is only valid at 23:59:60 UTC.
        let utc = (t.hour as i32 * 60 + t.minute as i32 - t.offset_minutes).rem_euclid(24 * 60);
        if utc != 23 * 60 + 59 {
            return None;
        }
    }
    Some(t)
}

/// RFC 3339 §5.6 `full-time`.
pub fn is_time(s: &str) -> bool {
    parse_time(s).is_some()
}

/// RFC 3339 §5.6 `date-time`. `T` and `Z` may be lower case (§5.6 note).
pub fn is_date_time(s: &str) -> bool {
    if s.len() < 11 || !s.is_char_boundary(10) {
        return false;
    }
    let (d, rest) = s.split_at(10);
    let Some(t) = rest.strip_prefix(['T', 't']) else {
        return false;
    };
    is_date(d) && is_time(t)
}

/// RFC 4122 §3 string representation.
pub fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// RFC 3339 Appendix A ISO 8601 duration, such as `P1Y2M3DT4H5M6S` or `P2W`.
pub fn is_duration(s: &str) -> bool {
    let Some(body) = s.strip_prefix('P') else {
        return false;
    };
    // Splits "12X..." into (12, 'X', rest).
    fn unit(s: &str) -> Option<(char, &str)> {
        let n = s.bytes().take_while(u8::is_ascii_digit).count();
        if n == 0 {
            return None;
        }
        let c = s[n..].chars().next()?;
        Some((c, &s[n + c.len_utf8()..]))
    }
    let (date, time) = match body.split_once('T') {
        Some((d, t)) => (d, Some(t)),
        None => (body, None),
    };
    if date.is_empty() && time.is_none() {
        return false;
    }
    // dur-week stands alone.
    if let Some((c, rest)) = unit(date) {
        if c == 'W' {
            return rest.is_empty() && time.is_none();
        }
    }
    let mut rest = date;
    let mut order = ['Y', 'M', 'D'].iter().peekable();
    let mut last = None;
    while !rest.is_empty() {
        let Some((c, r)) = unit(rest) else {
            return false;
        };
        // Units appear in order, and no unit may be skipped in the middle
        // (dur-year = Y [M [D]], dur-month = M [D], dur-day = D).
        loop {
            match order.next() {
                Some(u) if *u == c => break,
                Some(_) if last.is_some() => return false,
                Some(_) => continue,
                None => return false,
            }
        }
        last = Some(c);
        rest = r;
    }
    if let Some(t) = time {
        if t.is_empty() {
            return false;
        }
        let mut rest = t;
        let mut order = ['H', 'M', 'S'].iter();
        let mut started = false;
        while !rest.is_empty() {
            let Some((c, r)) = unit(rest) else {
                return false;
            };
            loop {
                match order.next() {
                    Some(u) if *u == c => break,
                    Some(_) if started => return false,
                    Some(_) => continue,
                    None => return false,
                }
            }
            started = true;
            rest = r;
        }
    }
    true
}

/// RFC 6901 §3 JSON Pointer.
pub fn is_json_pointer(s: &str) -> bool {
    if s.is_empty() {
        return true;
    }
    if !s.starts_with('/') {
        return false;
    }
    let b = s.as_bytes();
    b.iter()
        .enumerate()
        .all(|(i, &c)| c != b'~' || matches!(b.get(i + 1), Some(b'0' | b'1')))
}

/// Relative JSON Pointer (draft-bhutton-relative-json-pointer-00 §3): a
/// non-negative integer, an optional index adjustment, then `#` or a JSON
/// Pointer.
pub fn is_relative_json_pointer(s: &str) -> bool {
    let n = s.bytes().take_while(u8::is_ascii_digit).count();
    if n == 0 || (n > 1 && s.starts_with('0')) {
        return false;
    }
    let mut rest = &s[n..];
    if let Some(r) = rest.strip_prefix(['+', '-']) {
        let m = r.bytes().take_while(u8::is_ascii_digit).count();
        if m == 0 || (m > 1 && r.starts_with('0')) || r.starts_with('0') {
            return false;
        }
        rest = &r[m..];
    }
    rest == "#" || is_json_pointer(rest)
}

/// RFC 4648 §4 base64 (OpenAPI `byte`). Padding may be omitted.
pub fn is_base64(s: &str) -> bool {
    let body = s.trim_end_matches('=');
    let pad = s.len() - body.len();
    if pad > 2
        || !body
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'+' || c == b'/')
    {
        return false;
    }
    if pad > 0 {
        s.len().is_multiple_of(4)
    } else {
        body.len() % 4 != 1
    }
}

/// RFC 6570 §2 URI Template syntax.
pub fn is_uri_template(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'{' => {
                let Some(end) = s[i..].find('}').map(|e| i + e) else {
                    return false;
                };
                let mut expr = &s[i + 1..end];
                if let Some(c) = expr.chars().next() {
                    if "+#./;?&=,!@|".contains(c) {
                        expr = &expr[1..];
                    }
                }
                if expr.is_empty() {
                    return false;
                }
                for spec in expr.split(',') {
                    let (name, modifier) = match spec.find([':', '*']) {
                        Some(p) => (&spec[..p], Some(&spec[p..])),
                        None => (spec, None),
                    };
                    if !is_template_varname(name) {
                        return false;
                    }
                    match modifier {
                        None | Some("*") => {}
                        Some(m) => {
                            let Some(len) = m.strip_prefix(':') else {
                                return false;
                            };
                            let ok = !len.is_empty()
                                && len.len() <= 4
                                && len.bytes().all(|c| c.is_ascii_digit())
                                && !len.starts_with('0');
                            if !ok {
                                return false;
                            }
                        }
                    }
                }
                i = end + 1;
            }
            b'%' => {
                if !(b.get(i + 1).is_some_and(u8::is_ascii_hexdigit)
                    && b.get(i + 2).is_some_and(u8::is_ascii_hexdigit))
                {
                    return false;
                }
                i += 3;
            }
            c if c < 0x21 || b"\"<>\\^`|}".contains(&c) || c == 0x7F => return false,
            _ => {
                // Multi-byte UTF-8 (ucschar / iprivate) is allowed.
                i += 1;
            }
        }
    }
    true
}

fn is_template_varname(name: &str) -> bool {
    if name.is_empty() || name.starts_with('.') || name.ends_with('.') || name.contains("..") {
        return false;
    }
    let b = name.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => {
                if !(b.get(i + 1).is_some_and(u8::is_ascii_hexdigit)
                    && b.get(i + 2).is_some_and(u8::is_ascii_hexdigit))
                {
                    return false;
                }
                i += 3;
            }
            c if c.is_ascii_alphanumeric() || c == b'_' || c == b'.' => i += 1,
            _ => return false,
        }
    }
    true
}

/// Dotted-quad IPv4 (RFC 2673 §3.2 `dotted-quad`): four decimal octets with
/// no leading zeros.
pub fn is_ipv4(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    parts.len() == 4
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.len() <= 3
                && p.bytes().all(|c| c.is_ascii_digit())
                && (p.len() == 1 || !p.starts_with('0'))
                && p.parse::<u16>().is_ok_and(|v| v <= 255)
        })
}

/// RFC 4291 §2.2 text representation.
pub fn is_ipv6(s: &str) -> bool {
    s.is_ascii() && s.parse::<std::net::Ipv6Addr>().is_ok()
}

/// RFC 1123 §2.1 host name.
pub fn is_hostname(s: &str) -> bool {
    if s.is_empty() || s.len() > 253 {
        return false;
    }
    s.split('.').all(|label| {
        let b = label.as_bytes();
        !b.is_empty()
            && b.len() <= 63
            && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'-')
            && b[0] != b'-'
            && b[b.len() - 1] != b'-'
            // RFC 5891 §4.2.3.1: "--" in positions 3 and 4 is reserved for
            // A-labels ("xn--"), which must decode to valid U-labels.
            && if b.len() >= 4 && label[..4].eq_ignore_ascii_case("xn--") {
                is_a_label(label)
            } else {
                !(b.len() >= 4 && &b[2..4] == b"--")
            }
    })
}

/// RFC 5321 §4.1.2 `Mailbox`: a dot-atom or quoted-string local part, and a
/// domain name or address literal.
pub fn is_email(s: &str) -> bool {
    let Some(at) = s.rfind('@') else {
        return false;
    };
    let (local, domain) = (&s[..at], &s[at + 1..]);
    if local.is_empty() || local.len() > 64 || domain.is_empty() {
        return false;
    }
    let atext = |c: u8| c.is_ascii_alphanumeric() || b"!#$%&'*+-/=?^_`{|}~".contains(&c);
    let local_ok = if local.starts_with('"') {
        local.len() >= 2 && local.ends_with('"') && {
            let inner = &local.as_bytes()[1..local.len() - 1];
            let mut i = 0;
            let mut ok = true;
            while i < inner.len() {
                match inner[i] {
                    b'\\' => {
                        ok &= inner.get(i + 1).is_some_and(|c| (32..=126).contains(c));
                        i += 2;
                    }
                    b'"' => {
                        ok = false;
                        i += 1;
                    }
                    c => {
                        ok &= (32..=126).contains(&c);
                        i += 1;
                    }
                }
            }
            ok
        }
    } else {
        local
            .split('.')
            .all(|a| !a.is_empty() && a.bytes().all(atext))
    };
    if !local_ok {
        return false;
    }
    if let Some(lit) = domain.strip_prefix('[').and_then(|d| d.strip_suffix(']')) {
        // RFC 5321 §4.1.3: the "IPv6:" tag is case-insensitive.
        return match lit.get(..5).filter(|t| t.eq_ignore_ascii_case("IPv6:")) {
            Some(_) => is_ipv6(&lit[5..]),
            None => is_ipv4(lit),
        };
    }
    is_hostname(domain)
}

// RFC 3986 §2 character classes.
fn unreserved(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'-' | b'.' | b'_' | b'~')
}

fn sub_delim(c: u8) -> bool {
    matches!(
        c,
        b'!' | b'$' | b'&' | b'\'' | b'(' | b')' | b'*' | b'+' | b',' | b';' | b'='
    )
}

/// Every byte is allowed by `extra` or is unreserved, a sub-delimiter or a
/// well-formed percent-encoding.
fn chars_ok(s: &[u8], extra: &[u8]) -> bool {
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        if c == b'%' {
            if !(s.get(i + 1).is_some_and(u8::is_ascii_hexdigit)
                && s.get(i + 2).is_some_and(u8::is_ascii_hexdigit))
            {
                return false;
            }
            i += 3;
            continue;
        }
        if !(unreserved(c) || sub_delim(c) || extra.contains(&c)) {
            return false;
        }
        i += 1;
    }
    true
}

/// RFC 3986 §3.2 `authority`.
fn authority_ok(a: &str) -> bool {
    let (userinfo, hostport) = match a.rfind('@') {
        Some(p) => (Some(&a[..p]), &a[p + 1..]),
        None => (None, a),
    };
    if userinfo.is_some_and(|u| !chars_ok(u.as_bytes(), b":")) {
        return false;
    }
    let (host, port) = if let Some(rest) = hostport.strip_prefix('[') {
        let Some(end) = rest.find(']') else {
            return false;
        };
        let lit = &rest[..end];
        let after = &rest[end + 1..];
        let lit_ok = if lit.starts_with(['v', 'V']) {
            // IPvFuture = "v" 1*HEXDIG "." 1*( unreserved / sub-delims / ":" )
            lit[1..].split_once('.').is_some_and(|(h, r)| {
                !h.is_empty()
                    && h.bytes().all(|c| c.is_ascii_hexdigit())
                    && !r.is_empty()
                    && r.bytes()
                        .all(|c| unreserved(c) || sub_delim(c) || c == b':')
            })
        } else {
            is_ipv6(lit)
        };
        if !lit_ok {
            return false;
        }
        match after {
            "" => ("", None),
            p if p.starts_with(':') => ("", Some(&p[1..])),
            _ => return false,
        }
    } else {
        match hostport.rfind(':') {
            Some(p) => (&hostport[..p], Some(&hostport[p + 1..])),
            None => (hostport, None),
        }
    };
    if port.is_some_and(|p| !p.bytes().all(|c| c.is_ascii_digit())) {
        return false;
    }
    chars_ok(host.as_bytes(), b"")
}

/// RFC 3986 §3.3 path, §3.4 query and §3.5 fragment characters.
fn path_query_fragment_ok(rest: &str) -> bool {
    let (before_frag, frag) = match rest.find('#') {
        Some(p) => (&rest[..p], Some(&rest[p + 1..])),
        None => (rest, None),
    };
    let (path, query) = match before_frag.find('?') {
        Some(p) => (&before_frag[..p], Some(&before_frag[p + 1..])),
        None => (before_frag, None),
    };
    chars_ok(path.as_bytes(), b":@/")
        && query.is_none_or(|q| chars_ok(q.as_bytes(), b":@/?"))
        && frag.is_none_or(|f| chars_ok(f.as_bytes(), b":@/?"))
}

/// `hier-part` or `relative-part` after the scheme (if any).
fn hier_ok(rest: &str, relative: bool) -> bool {
    if let Some(after) = rest.strip_prefix("//") {
        let end = after.find(['/', '?', '#']).unwrap_or(after.len());
        return authority_ok(&after[..end]) && path_query_fragment_ok(&after[end..]);
    }
    if relative {
        // path-noscheme: the first segment must not contain ':'.
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        if rest[..end].contains(':') {
            return false;
        }
    }
    path_query_fragment_ok(rest)
}

fn scheme_end(s: &str) -> Option<usize> {
    let p = s.find(':')?;
    let scheme = &s.as_bytes()[..p];
    let ok = !scheme.is_empty()
        && scheme[0].is_ascii_alphabetic()
        && scheme
            .iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'-' | b'.'));
    ok.then_some(p)
}

/// RFC 3986 §3 `URI` (absolute, with a scheme).
pub fn is_uri(s: &str) -> bool {
    if !s.is_ascii() {
        return false;
    }
    match scheme_end(s) {
        Some(p) => hier_ok(&s[p + 1..], false),
        None => false,
    }
}

/// RFC 3986 §4.1 `URI-reference`.
pub fn is_uri_reference(s: &str) -> bool {
    if !s.is_ascii() {
        return false;
    }
    if is_uri(s) {
        return true;
    }
    hier_ok(s, true)
}

// ------------------------------------------------------------------ IRIs

/// RFC 3987 §2.2 `ucschar` (and `iprivate`, allowed in queries).
fn iri_char(c: char) -> bool {
    let c = c as u32;
    let plane_ok = |c: u32| (c & 0xFFFF) < 0xFFFE;
    matches!(c, 0xA0..=0xD7FF | 0xF900..=0xFDCF | 0xFDF0..=0xFFEF)
        || (0x10000..=0xEFFFD).contains(&c) && plane_ok(c) && !(0xE0000..=0xE0FFF).contains(&c)
        || matches!(c, 0xE000..=0xF8FF)
        || (0xF0000..=0x10FFFD).contains(&c) && plane_ok(c)
}

/// Maps an IRI onto a URI of the same shape: each allowed non-ASCII
/// character becomes an unreserved ASCII letter.
fn iri_as_uri(s: &str) -> Option<String> {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii() {
            out.push(c);
        } else if iri_char(c) {
            out.push('a');
        } else {
            return None;
        }
    }
    Some(out)
}

/// RFC 3987 §2.2 `IRI`.
pub fn is_iri(s: &str) -> bool {
    iri_as_uri(s).is_some_and(|u| is_uri(&u))
}

/// RFC 3987 §2.2 `IRI-reference`.
pub fn is_iri_reference(s: &str) -> bool {
    iri_as_uri(s).is_some_and(|u| is_uri_reference(&u))
}

// ------------------------------------------------------------ IDN labels

/// Code points RFC 5892 §2.6 lists as DISALLOWED exceptions.
fn idna_disallowed(c: char) -> bool {
    matches!(
        c as u32,
        0x0640 | 0x07FA | 0x302E | 0x302F | 0x3031..=0x3035 | 0x303B
    )
}

/// RFC 5892 §2.1 LetterDigits (letters, marks, decimal digits), plus the
/// hyphen and the code points allowed by contextual rules (§2.7, §2.8).
fn letter_digit(c: char) -> bool {
    c == '-'
        || (c.is_alphanumeric() && !matches!(c as u32, 0x00B2 | 0x00B3 | 0x00B9 | 0x00BC..=0x00BE))
        || unicode_normalization::char::is_combining_mark(c)
        || matches!(c as u32, 0x200C | 0x200D | 0x00B7 | 0x0375 | 0x05F3 | 0x05F4 | 0x30FB)
        // §2.6 exceptions that are PVALID whatever their category.
        || matches!(c as u32, 0x00DF | 0x03C2 | 0x06FD | 0x06FE | 0x0F0B | 0x3007)
}

fn is_greek(c: char) -> bool {
    matches!(c as u32, 0x0370..=0x03FF | 0x1F00..=0x1FFF)
}

fn is_hebrew(c: char) -> bool {
    matches!(c as u32, 0x0590..=0x05FF | 0xFB1D..=0xFB4F)
}

fn is_hiragana_katakana_han(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x309F | 0x30A0..=0x30FA | 0x30FC..=0x30FF | 0x31F0..=0x31FF | 0xFF66..=0xFF9F
        | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x2A6DF | 0x2F800..=0x2FA1F)
}

/// RFC 5892 Appendix A CONTEXTO rules (A.3–A.9). CONTEXTJ rules (A.1,
/// A.2) and the Bidi rule are left to UTS 46 processing.
fn contexto_ok(label: &[char]) -> bool {
    let arabic_indic = label.iter().any(|c| matches!(*c as u32, 0x0660..=0x0669));
    let extended = label.iter().any(|c| matches!(*c as u32, 0x06F0..=0x06F9));
    if arabic_indic && extended {
        return false;
    }
    for (i, &c) in label.iter().enumerate() {
        let before = i.checked_sub(1).and_then(|j| label.get(j)).copied();
        let after = label.get(i + 1).copied();
        let ok = match c as u32 {
            0x00B7 => before == Some('l') && after == Some('l'),
            0x0375 => after.is_some_and(is_greek),
            0x05F3 | 0x05F4 => before.is_some_and(is_hebrew),
            0x30FB => label.iter().any(|&x| x != c && is_hiragana_katakana_han(x)),
            _ => true,
        };
        if !ok {
            return false;
        }
    }
    true
}

/// An IDNA2008 U-label (RFC 5890 §2.3.2.1): valid under UTS 46 strict
/// processing without being changed by it, and meeting the CONTEXTO rules.
fn is_u_label(label: &str) -> bool {
    let chars: Vec<char> = label.chars().collect();
    if chars.is_empty() || chars.iter().any(|&c| idna_disallowed(c)) || !contexto_ok(&chars) {
        return false;
    }
    let Ok(ascii) = idna::domain_to_ascii_strict(label) else {
        return false;
    };
    if ascii.len() > 63 {
        return false;
    }
    // IDNA2008 does not map: the label must already be in its final form.
    let (unicode, ok) = idna::domain_to_unicode(&ascii);
    ok.is_ok() && unicode == label
}

/// An A-label (`xn--` + Punycode) whose decoded form is a valid U-label.
fn is_a_label(label: &str) -> bool {
    let Some(code) = label
        .get(4..)
        .filter(|_| label[..4].eq_ignore_ascii_case("xn--"))
    else {
        return false;
    };
    let Some(decoded) = idna::punycode::decode_to_string(code) else {
        return false;
    };
    !decoded.is_ascii()
        && decoded.chars().all(letter_digit)
        && is_u_label(&decoded)
        && idna::punycode::encode_str(&decoded).is_some_and(|re| re.eq_ignore_ascii_case(code))
}

fn ldh_label_ok(label: &str) -> bool {
    let b = label.as_bytes();
    !b.is_empty()
        && b.len() <= 63
        && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'-')
        && b[0] != b'-'
        && b[b.len() - 1] != b'-'
}

/// RFC 5890 §2.3.2.3 `idn-hostname`, processed as UTS 46 (non-transitional,
/// strict: STD3 rules, hyphen and Bidi checks, DNS length): the name must
/// convert to ASCII, and every A-label it yields must decode to a label
/// that also meets the RFC 5892 CONTEXTO rules.
pub fn is_idn_hostname(s: &str) -> bool {
    if s.is_empty() || s.ends_with(['.', '\u{3002}', '\u{FF0E}', '\u{FF61}']) {
        return false;
    }
    let Ok(ascii) = idna::domain_to_ascii_strict(s) else {
        return false;
    };
    if ascii.is_empty() || ascii.len() > 253 {
        return false;
    }
    ascii.split('.').all(|label| {
        if !ldh_label_ok(label) {
            return false;
        }
        if label.len() >= 4 && label[..4].eq_ignore_ascii_case("xn--") {
            let Some(decoded) = idna::punycode::decode_to_string(&label[4..]) else {
                return false;
            };
            let chars: Vec<char> = decoded.chars().collect();
            !chars
                .iter()
                .any(|&c| idna_disallowed(c) || !letter_digit(c))
                && contexto_ok(&chars)
        } else {
            true
        }
    })
}

/// RFC 6531 §3.3 internationalised `Mailbox`: UTF-8 is allowed in the
/// local part, and the domain is an IDN host name or an address literal.
pub fn is_idn_email(s: &str) -> bool {
    let Some(at) = s.rfind('@') else {
        return false;
    };
    let (local, domain) = (&s[..at], &s[at + 1..]);
    if local.is_empty() || domain.is_empty() {
        return false;
    }
    // RFC 6531 §3.3: atext also admits any UTF8-non-ascii character.
    let atext =
        |c: char| !c.is_ascii() || c.is_ascii_alphanumeric() || "!#$%&'*+-/=?^_`{|}~".contains(c);
    let local_ok = if local.starts_with('"') {
        local.len() >= 2 && local.ends_with('"')
    } else {
        local
            .split('.')
            .all(|a| !a.is_empty() && a.chars().all(atext))
    };
    if !local_ok {
        return false;
    }
    if domain.starts_with('[') {
        return is_email(&format!("a@{domain}"));
    }
    is_idn_hostname(domain)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_s5_6_dates() {
        assert!(is_date("2024-02-29"));
        assert!(!is_date("2023-02-29"));
        assert!(!is_date("1900-02-29"));
        assert!(is_date("2000-02-29"));
        assert!(!is_date("2024-13-01"));
        assert!(!is_date("2024-04-31"));
        assert!(!is_date("2024-1-01"));
        assert!(!is_date("２０２４-01-01"));
        assert!(!is_date("2é24-01-01"), "multibyte at a slice boundary");
        assert!(!is_time("0é:00:00Z"));
    }

    #[test]
    fn rfc3339_s5_6_times_and_leap_seconds() {
        assert!(is_time("08:30:06Z"));
        assert!(is_time("08:30:06.283185+05:30"));
        assert!(!is_time("08:30:06"), "offset is required");
        assert!(!is_time("24:00:00Z"));
        assert!(is_time("23:59:60Z"));
        assert!(!is_time("22:59:60Z"));
        assert!(is_time("15:59:60-08:00"));
        assert!(!is_time("08:30:06.Z"));
        assert!(!is_time("08:30:06+24:00"));
        assert!(is_date_time("1985-04-12T23:20:50.52Z"));
        assert!(is_date_time("1985-04-12t23:20:50z"));
        assert!(!is_date_time("1985-04-12 23:20:50Z"));
        assert!(!is_date_time("1985-04-12T23:20:50"));
    }

    #[test]
    fn rfc3986_uris() {
        assert!(is_uri("https://example.com/a/b?c=d#e"));
        assert!(is_uri("urn:isbn:0451450523"));
        assert!(is_uri("mailto:a@b.c"));
        assert!(is_uri("http://[::1]:8080/"));
        assert!(is_uri("http://a/%20b"));
        assert!(!is_uri("http://a/b c"), "space is not a URI character");
        assert!(!is_uri("/relative"));
        assert!(!is_uri("http://a/%zz"));
        assert!(!is_uri("http://ex ample.com"));
        assert!(!is_uri("http://例え.jp"));
        assert!(is_uri_reference("/relative/path?q#f"));
        assert!(is_uri_reference(""));
        assert!(is_uri_reference("#frag"));
        assert!(is_uri_reference("../a"));
        assert!(!is_uri_reference("a b"));
        assert!(!is_uri_reference("a:b:c d"));
        assert!(!is_uri_reference("\\\\WINDOWS\\share"));
    }

    #[test]
    fn ip_addresses() {
        assert!(is_ipv4("192.168.0.1"));
        assert!(!is_ipv4("192.168.0.01"));
        assert!(!is_ipv4("256.0.0.1"));
        assert!(!is_ipv4("1.2.3"));
        assert!(!is_ipv4("١.2.3.4"));
        assert!(is_ipv6("::1"));
        assert!(is_ipv6("2001:db8::ff00:42:8329"));
        assert!(is_ipv6("::ffff:192.0.2.1"));
        assert!(!is_ipv6("12345::"));
        assert!(!is_ipv6("fe80::1%eth0"));
    }

    #[test]
    fn hostnames_emails_uuids() {
        assert!(is_hostname("www.example.com"));
        assert!(is_hostname("xn--4gbwdl.xn--wgbh1c"));
        assert!(!is_hostname("-a.com"));
        assert!(!is_hostname("a..b"));
        assert!(!is_hostname("ab--cd.com"));
        assert!(!is_hostname(&"a".repeat(64)));
        assert!(is_email("joe.bloggs@example.com"));
        assert!(is_email("\"joe bloggs\"@example.com"));
        assert!(is_email("joe@[127.0.0.1]"));
        assert!(is_email("joe@[IPv6:::1]"));
        assert!(is_email("joe@[ipv6:::1]"));
        assert!(!is_email("joe..bloggs@example.com"));
        assert!(!is_email("@example.com"));
        assert!(!is_email("joe@"));
        assert!(!is_email("joe"));
        assert!(is_uuid("2EB8AA08-AA98-11EA-B4AA-73B441D16380"));
        assert!(!is_uuid("2eb8aa08-aa98-11ea-b4aa-73b441d1638"));
        assert!(!is_uuid("2eb8aa08aa9811eab4aa73b441d16380"));
    }

    #[test]
    fn json_schema_formats() {
        assert!(is_duration("P1Y2M3DT4H5M6S"));
        assert!(is_duration("P2W"));
        assert!(is_duration("PT1M"));
        assert!(!is_duration("P"));
        assert!(!is_duration("PT"));
        assert!(!is_duration("P1D2Y"), "units out of order");
        assert!(!is_duration("P1Y2D"), "month skipped");
        assert!(!is_duration("P2W1D"));
        assert!(is_json_pointer(""));
        assert!(is_json_pointer("/a~0b/c~1d/"));
        assert!(!is_json_pointer("a"));
        assert!(!is_json_pointer("/a~2"));
        assert!(is_relative_json_pointer("0#"));
        assert!(is_relative_json_pointer("1/a"));
        assert!(!is_relative_json_pointer("01/a"));
        assert!(is_uri_template("https://x/{user}{?q,lang:2}{/path*}"));
        assert!(!is_uri_template("{"));
        assert!(!is_uri_template("{a b}"));
        assert!(is_base64("aGVsbG8="));
        assert!(is_base64("aGVsbG8"));
        assert!(!is_base64("aGVsbG8=="));
        assert!(!is_base64("a-b_"));
        assert!(is_iri("https://例え.jp/ドキュメント"));
        assert!(!is_iri("/relative"));
        assert!(is_iri_reference("ドキュメント#節"));
    }

    #[test]
    fn idn_formats() {
        assert!(is_idn_hostname("実例.テスト"));
        assert!(is_idn_hostname("xn--hxajbheg2az3al.xn--jxalpdlp"));
        assert!(!is_idn_hostname("xn--7a"), "U+00A1 is disallowed");
        assert!(
            !is_idn_hostname("l\u{00B7}x"),
            "middle dot needs l on both sides"
        );
        assert!(is_idn_hostname("l\u{00B7}l"));
        assert!(is_hostname("xn--hxajbheg2az3al"));
        assert!(!is_hostname("xn--7a"));
        assert!(is_idn_email("用户@例子.广告"));
        assert!(!is_idn_email("用户@"));
    }

    #[test]
    fn number_formats() {
        let d = |s: &str| Decimal::parse(s).unwrap();
        assert!(check_number("float", &d("3.4e38")));
        assert!(!check_number("float", &d("3.5e38")));
        assert!(check_number("double", &d("-1.7e308")));
        assert!(!check_number("double", &d("1.8e308")));
    }

    #[test]
    fn int_ranges() {
        let d = |s: &str| Decimal::parse(s).unwrap();
        assert!(check_number("int32", &d("2147483647")));
        assert!(!check_number("int32", &d("2147483648")));
        assert!(check_number("int64", &d("-9223372036854775808")));
        assert!(!check_number("int64", &d("9223372036854775808")));
        assert!(!check_number("int32", &d("1.5")));
        assert!(check_number("int64", &d("1e3")));
    }
}
