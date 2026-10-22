//! Exact decimal arithmetic on JSON number literals.
//!
//! Numbers are never rounded through `f64`. A literal such as `-12.50e3` is
//! held as a sign, a digit string without leading or trailing zeros, and a
//! power-of-ten exponent, so comparisons, integer checks and `multipleOf` are
//! exact for any value the parser accepts (RFC 8259 §6, JSON Schema
//! Validation 2020-12 §6.2).

use std::cmp::Ordering;

use num_bigint::BigUint;
use num_traits::{ToPrimitive, Zero};

/// Largest decimal exponent magnitude, and largest digit count, the parser
/// accepts. Larger literals are refused rather than approximated.
pub const MAX_EXPONENT: i64 = 4096;
pub const MAX_DIGITS: usize = 4096;

/// An exact decimal value: `(-1)^negative × digits × 10^exp`.
///
/// `digits` has no leading or trailing zeros; zero is the empty string with
/// `negative == false`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decimal {
    negative: bool,
    digits: String,
    exp: i64,
}

impl Decimal {
    /// Parses a JSON number literal (RFC 8259 §6 grammar). Returns `None` for
    /// anything else, or for literals beyond [`MAX_EXPONENT`]/[`MAX_DIGITS`].
    pub fn parse(lit: &str) -> Option<Decimal> {
        let b = lit.as_bytes();
        let mut i = 0;
        let negative = b.first() == Some(&b'-');
        if negative {
            i += 1;
        }
        let int_start = i;
        match b.get(i) {
            Some(b'0') => i += 1,
            Some(c) if c.is_ascii_digit() => {
                while b.get(i).is_some_and(u8::is_ascii_digit) {
                    i += 1;
                }
            }
            _ => return None,
        }
        let int_part = &lit[int_start..i];
        let mut frac_part = "";
        if b.get(i) == Some(&b'.') {
            i += 1;
            let s = i;
            while b.get(i).is_some_and(u8::is_ascii_digit) {
                i += 1;
            }
            if i == s {
                return None;
            }
            frac_part = &lit[s..i];
        }
        let mut exp: i64 = 0;
        if matches!(b.get(i), Some(b'e' | b'E')) {
            i += 1;
            let mut eneg = false;
            if matches!(b.get(i), Some(b'+' | b'-')) {
                eneg = b[i] == b'-';
                i += 1;
            }
            let s = i;
            while let Some(c) = b.get(i).filter(|c| c.is_ascii_digit()) {
                exp = exp.saturating_mul(10).saturating_add(i64::from(c - b'0'));
                if exp > MAX_EXPONENT * 2 {
                    return None;
                }
                i += 1;
            }
            if i == s {
                return None;
            }
            if eneg {
                exp = -exp;
            }
        }
        if i != b.len() {
            return None;
        }
        if int_part.len() + frac_part.len() > MAX_DIGITS {
            return None;
        }
        let mut digits = String::with_capacity(int_part.len() + frac_part.len());
        digits.push_str(int_part);
        digits.push_str(frac_part);
        exp -= frac_part.len() as i64;
        Some(Self::normalize(negative, digits, exp)).filter(|d| {
            d.digits.is_empty() || (d.magnitude() <= MAX_EXPONENT + 1 && d.exp >= -2 * MAX_EXPONENT)
        })
    }

    fn normalize(negative: bool, digits: String, mut exp: i64) -> Decimal {
        let trimmed = digits.trim_start_matches('0');
        let without_trailing = trimmed.trim_end_matches('0');
        exp += (trimmed.len() - without_trailing.len()) as i64;
        if without_trailing.is_empty() {
            return Decimal {
                negative: false,
                digits: String::new(),
                exp: 0,
            };
        }
        Decimal {
            negative,
            digits: without_trailing.to_owned(),
            exp,
        }
    }

    /// Position of the most significant digit: the value lies in
    /// `[10^(m-1), 10^m)`.
    fn magnitude(&self) -> i64 {
        self.digits.len() as i64 + self.exp
    }

    pub fn is_zero(&self) -> bool {
        self.digits.is_empty()
    }

    pub fn is_negative(&self) -> bool {
        self.negative
    }

    /// True when the value is a mathematical integer (`1.0`, `1e2`, `15e-1`
    /// is not). This is the JSON Schema 2020-12 meaning (§6.1.1 of the core
    /// specification, "integer").
    pub fn is_integer(&self) -> bool {
        self.exp >= 0
    }

    fn cmp_abs(&self, other: &Decimal) -> Ordering {
        match (self.is_zero(), other.is_zero()) {
            (true, true) => return Ordering::Equal,
            (true, false) => return Ordering::Less,
            (false, true) => return Ordering::Greater,
            _ => {}
        }
        match self.magnitude().cmp(&other.magnitude()) {
            Ordering::Equal => {}
            o => return o,
        }
        // Same magnitude: compare digit strings left-aligned.
        let (a, b) = (self.digits.as_bytes(), other.digits.as_bytes());
        for i in 0..a.len().max(b.len()) {
            let x = a.get(i).copied().unwrap_or(b'0');
            let y = b.get(i).copied().unwrap_or(b'0');
            match x.cmp(&y) {
                Ordering::Equal => {}
                o => return o,
            }
        }
        Ordering::Equal
    }

    /// True when `self / divisor` is an integer. A zero or negative divisor
    /// never divides (JSON Schema Validation §6.2.1 requires `> 0`).
    pub fn is_multiple_of(&self, divisor: &Decimal) -> bool {
        if divisor.is_zero() || divisor.negative {
            return false;
        }
        if self.is_zero() {
            return true;
        }
        // self = a·10^ea, divisor = b·10^eb, a and b without factors of ten
        // at the end. a·10^(ea-eb) / b must be an integer.
        let k = self.exp - divisor.exp;
        if k < 0 {
            // a has no trailing zero, so a·10^k with k < 0 is an integer only
            // if 10^-k divides a, which cannot happen.
            return false;
        }
        // Fast path: b below 2^64 keeps every product below 2^128.
        if divisor.digits.len() <= 19 && self.digits.len() <= 38 {
            if let (Ok(a), Ok(b)) = (self.digits.parse::<u128>(), divisor.digits.parse::<u128>()) {
                return (a % b * pow10_mod(k as u128, b)).is_multiple_of(b);
            }
        }
        let (Some(a), Some(b)) = (digits_to_big(&self.digits), digits_to_big(&divisor.digits))
        else {
            return false;
        };
        let Ok(k) = u32::try_from(k) else {
            return false;
        };
        let ten = BigUint::from(10u32);
        let r = (&a % &b) * ten.modpow(&BigUint::from(k), &b) % &b;
        r.is_zero()
    }

    /// The value as `i128` when it is an integer that fits.
    pub fn to_i128(&self) -> Option<i128> {
        if !self.is_integer() {
            return None;
        }
        if self.is_zero() {
            return Some(0);
        }
        if self.magnitude() > 39 {
            return None;
        }
        let mut s = self.digits.clone();
        s.extend(std::iter::repeat_n('0', usize::try_from(self.exp).ok()?));
        let v: i128 = s.parse().ok()?;
        Some(if self.negative { -v } else { v })
    }

    /// Canonical text: shortest exact representation without exponent when
    /// the value is an integer of modest size, otherwise `digits e exp`.
    pub fn to_canonical(&self) -> String {
        if self.is_zero() {
            return "0".into();
        }
        let sign = if self.negative { "-" } else { "" };
        if self.exp >= 0 && self.exp <= 32 {
            let zeros = "0".repeat(self.exp as usize);
            return format!("{sign}{}{zeros}", self.digits);
        }
        format!("{sign}{}e{}", self.digits, self.exp)
    }

    pub fn as_f64_lossy(&self) -> f64 {
        self.to_canonical().parse().unwrap_or(f64::NAN)
    }
}

/// 10^k mod m for m < 2^64, by squaring (products stay below 2^128).
fn pow10_mod(mut k: u128, m: u128) -> u128 {
    let (mut base, mut out) = (10 % m, 1 % m);
    while k > 0 {
        if k & 1 == 1 {
            out = out * base % m;
        }
        base = base * base % m;
        k >>= 1;
    }
    out
}

fn digits_to_big(d: &str) -> Option<BigUint> {
    BigUint::parse_bytes(d.as_bytes(), 10)
}

impl Ord for Decimal {
    fn cmp(&self, other: &Self) -> Ordering {
        let neg_a = self.negative && !self.is_zero();
        let neg_b = other.negative && !other.is_zero();
        match (neg_a, neg_b) {
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
            (false, false) => self.cmp_abs(other),
            (true, true) => other.cmp_abs(self),
        }
    }
}

impl PartialOrd for Decimal {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// True when the literal is written as a JSON integer: digits only, with no
/// fraction or exponent part. OpenAPI 3.0 uses JSON Schema Wright draft 00,
/// whose `integer` type is the JSON integer production, so `1.0` and `1e2`
/// are not integers there.
pub fn is_lexical_integer(lit: &str) -> bool {
    let s = lit.strip_prefix('-').unwrap_or(lit);
    !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit())
}

/// Lossless `u64` view for counts written as `2`, `2.0` or `2e0`.
pub fn as_count(lit: &str) -> Option<Result<u64, ()>> {
    let d = Decimal::parse(lit)?;
    if !d.is_integer() {
        return None;
    }
    if d.is_negative() {
        return Some(Err(()));
    }
    Some(d.to_i128().and_then(|v| v.to_u64()).ok_or(()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Decimal {
        Decimal::parse(s).unwrap_or_else(|| panic!("parse {s}"))
    }

    #[test]
    fn rfc8259_s6_grammar() {
        for ok in [
            "0",
            "-0",
            "1",
            "-1.5",
            "1e2",
            "1E+2",
            "1.0e-2",
            "123456789012345678901234567890",
        ] {
            assert!(Decimal::parse(ok).is_some(), "{ok}");
        }
        for bad in [
            "", "-", "01", "1.", ".5", "+1", "1e", "1e+", "0x10", "NaN", "1 ", "--1",
        ] {
            assert!(Decimal::parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn huge_exponents_are_refused_not_approximated() {
        assert!(Decimal::parse("1e4096").is_some());
        assert!(Decimal::parse("1e5000").is_none());
        assert!(Decimal::parse("1e1000000000").is_none());
        assert!(Decimal::parse("1e-99999999999999999999").is_none());
        assert!(Decimal::parse("0e8000").is_some_and(|z| z.is_zero()));
    }

    #[test]
    fn ordering_is_exact_past_2_pow_53() {
        assert!(d("9007199254740993") > d("9007199254740992"));
        assert!(d("-9007199254740993") < d("-9007199254740992"));
        assert_eq!(d("1e2").cmp(&d("100.000")), Ordering::Equal);
        assert_eq!(d("-0").cmp(&d("0")), Ordering::Equal);
        assert!(d("0.1") > d("0.09999999999999999999999"));
        assert!(d("1e-400") > d("0"));
        assert!(d("-1e-400") < d("0"));
        assert!(d("1e309") > d("1.7976931348623157e308"));
    }

    #[test]
    fn integer_meaning_by_version() {
        // JSON Schema 2020-12 (OpenAPI 3.1): mathematical integers.
        assert!(d("1.0").is_integer());
        assert!(d("1e2").is_integer());
        assert!(!d("15e-1").is_integer());
        assert!(d("1.5e1").is_integer());
        // OpenAPI 3.0: the JSON integer production only.
        assert!(is_lexical_integer("-12"));
        assert!(!is_lexical_integer("1.0"));
        assert!(!is_lexical_integer("1e2"));
    }

    #[test]
    fn multiple_of_is_exact() {
        // Both the 128-bit path and the big-integer path.
        assert!(d("12345678901234567890123456789012345678").is_multiple_of(&d("2")));
        assert!(!d("12345678901234567890123456789012345679").is_multiple_of(&d("2")));
        assert!(d("1e50").is_multiple_of(&d("12500000000000000000000")));
        assert!(d("0.0000000000000000000000000000000000000004").is_multiple_of(&d("4e-40")));
        assert!(!d("9.999").is_multiple_of(&d("0.01")));
        assert!(d("19.99").is_multiple_of(&d("0.01")));
        assert!(d("0.9").is_multiple_of(&d("0.3")));
        assert!(!d("0.91").is_multiple_of(&d("0.3")));
        assert!(d("1e308").is_multiple_of(&d("0.1")));
        assert!(!d("1e100").is_multiple_of(&d("7")));
        assert!(d("7e100").is_multiple_of(&d("7")));
        assert!(!d("1.5").is_multiple_of(&d("1")));
        assert!(d("0").is_multiple_of(&d("3.3")));
        assert!(!d("3").is_multiple_of(&d("0")));
        assert!(d("9007199254740993").is_multiple_of(&d("3")));
        assert!(!d("9007199254740994").is_multiple_of(&d("3")));
    }

    #[test]
    fn i128_and_counts() {
        assert_eq!(d("9223372036854775807").to_i128(), Some(i64::MAX as i128));
        assert_eq!(
            d("-9.2233720368547758080e18").to_i128(),
            Some(i64::MIN as i128)
        );
        assert_eq!(as_count("2.0"), Some(Ok(2)));
        assert_eq!(as_count("2e0"), Some(Ok(2)));
        assert_eq!(as_count("-1"), Some(Err(())));
        assert_eq!(as_count("1.79e308"), Some(Err(())));
        assert_eq!(as_count("1.5"), None);
    }
}
