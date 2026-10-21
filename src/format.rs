//! Format assertions (JSON Schema Validation 2020-12 §7.3, OpenAPI 3.0.4
//! §4.4 and 3.1.1 §4.2 "Data Types").
//!
//! Only the formats listed in [`ASSERTED`] are checked. Every other format,
//! including OpenAPI's `float`, `double`, `byte`, `binary` and `password`,
//! is an annotation and never fails a value.

use crate::number::Decimal;

/// Formats this library asserts.
pub const ASSERTED: &[&str] = &[
    "date",
    "time",
    "date-time",
    "email",
    "uuid",
    "uri",
    "uri-reference",
    "ipv4",
    "ipv6",
    "hostname",
    "int32",
    "int64",
];

/// Formats that constrain numbers rather than strings.
pub fn is_numeric_format(name: &str) -> bool {
    matches!(name, "int32" | "int64")
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
        _ => true,
    }
}

/// Checks a number literal against `int32` or `int64`. The value must be an
/// integer (by the document's meaning of integer, decided by the caller) in
/// the signed range.
pub fn check_int_range(format: &str, n: &Decimal) -> bool {
    let Some(v) = n.to_i128() else {
        return false;
    };
    match format {
        "int32" => i32::try_from(v).is_ok(),
        "int64" => i64::try_from(v).is_ok(),
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
            // A-labels ("xn--").
            && !(b.len() >= 4 && &b[2..4] == b"--" && !label[..2].eq_ignore_ascii_case("xn"))
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
    fn int_ranges() {
        let d = |s: &str| Decimal::parse(s).unwrap();
        assert!(check_int_range("int32", &d("2147483647")));
        assert!(!check_int_range("int32", &d("2147483648")));
        assert!(check_int_range("int64", &d("-9223372036854775808")));
        assert!(!check_int_range("int64", &d("9223372036854775808")));
        assert!(!check_int_range("int32", &d("1.5")));
        assert!(check_int_range("int64", &d("1e3")));
    }
}
