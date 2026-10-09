//! Kubernetes resource quantity parsing (apimachinery resource.Quantity).
//!
//! Supports the three k8s formats: decimal SI suffixes (`n u m "" k M G T P E`),
//! binary SI suffixes (`Ki Mi Gi Ti Pi Ei`) and decimal exponents (`1e3`, `5E-2`).
//! `milli_value` and `value` round up like Go's `Quantity.MilliValue/Value`.

use anyhow::{anyhow, Result};

/// A parsed quantity: `mantissa / 10^frac_digits * 10^exp10 * 2^exp2`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Quantity {
    negative: bool,
    mantissa: u128,
    frac_digits: u32,
    exp10: i32,
    exp2: u32,
}

impl Quantity {
    /// Parse a quantity string (resource.ParseQuantity).
    pub(crate) fn parse(s: &str) -> Result<Quantity> {
        let s = s.trim();
        if s.is_empty() {
            return Err(anyhow!("quantities must match the regular expression"));
        }
        let bytes = s.as_bytes();
        let mut i = 0;
        let mut negative = false;
        if bytes[0] == b'+' || bytes[0] == b'-' {
            negative = bytes[0] == b'-';
            i = 1;
        }
        let num_start = i;
        while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
            i += 1;
        }
        let number = &s[num_start..i];
        let suffix = &s[i..];
        if number.is_empty() || number == "." || number.matches('.').count() > 1 {
            return Err(anyhow!("unable to parse quantity's suffix"));
        }
        let (int_part, frac_part) = number.split_once('.').unwrap_or((number, ""));
        let digits = format!("{int_part}{frac_part}");
        let digits = digits.trim_start_matches('0');
        let mantissa: u128 = if digits.is_empty() {
            0
        } else {
            digits
                .parse()
                .map_err(|_| anyhow!("quantity too large: {s}"))?
        };
        let frac_digits = frac_part.len() as u32;
        let (exp10, exp2) =
            parse_suffix(suffix).ok_or_else(|| anyhow!("unable to parse quantity's suffix"))?;
        Ok(Quantity {
            negative,
            mantissa,
            frac_digits,
            exp10,
            exp2,
        })
    }

    /// Value scaled by 10^scale, rounded up (towards +inf), saturating.
    fn scaled(&self, scale: i32) -> i64 {
        if self.mantissa == 0 {
            return 0;
        }
        // value = mantissa * 2^exp2 * 10^(exp10 + scale - frac_digits)
        let mut num: u128 = self.mantissa;
        let mut den: u128 = 1;
        let saturated = if self.negative { i64::MIN } else { i64::MAX };
        for _ in 0..self.exp2 {
            num = match num.checked_mul(2) {
                Some(v) => v,
                None => return saturated,
            };
        }
        let e = self.exp10 + scale - self.frac_digits as i32;
        if e >= 0 {
            for _ in 0..e {
                num = match num.checked_mul(10) {
                    Some(v) => v,
                    None => return saturated,
                };
            }
        } else {
            for _ in 0..(-e) {
                den = match den.checked_mul(10) {
                    Some(v) => v,
                    // Denominator is astronomically large: value rounds to 1 or 0.
                    None => return if self.negative { 0 } else { 1 },
                };
            }
        }
        let q = num / den;
        let r = num % den;
        if self.negative {
            // ceil of a negative number truncates towards zero.
            if q > i64::MAX as u128 {
                return i64::MIN;
            }
            -(q as i64)
        } else {
            let v = if r > 0 { q + 1 } else { q };
            if v > i64::MAX as u128 {
                return i64::MAX;
            }
            v as i64
        }
    }

    /// Quantity.MilliValue.
    pub(crate) fn milli_value(&self) -> i64 {
        self.scaled(3)
    }

    /// Quantity.Value.
    pub(crate) fn value(&self) -> i64 {
        self.scaled(0)
    }
}

/// Returns (base-10 exponent, base-2 exponent) for a suffix.
fn parse_suffix(suffix: &str) -> Option<(i32, u32)> {
    let r = match suffix {
        "" => (0, 0),
        "n" => (-9, 0),
        "u" => (-6, 0),
        "m" => (-3, 0),
        "k" => (3, 0),
        "M" => (6, 0),
        "G" => (9, 0),
        "T" => (12, 0),
        "P" => (15, 0),
        "E" => (18, 0),
        "Ki" => (0, 10),
        "Mi" => (0, 20),
        "Gi" => (0, 30),
        "Ti" => (0, 40),
        "Pi" => (0, 50),
        "Ei" => (0, 60),
        _ => {
            let rest = suffix
                .strip_prefix('e')
                .or_else(|| suffix.strip_prefix('E'))?;
            let digits = rest.strip_prefix(['+', '-']).unwrap_or(rest);
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            (rest.parse::<i32>().ok()?, 0)
        }
    };
    Some(r)
}

/// CPU quantity in millicores; unparsable values count as 0.
pub(crate) fn cpu_millis(s: &str) -> i64 {
    Quantity::parse(s).map(|q| q.milli_value()).unwrap_or(0)
}

/// Memory quantity in bytes; unparsable values count as 0.
pub(crate) fn mem_bytes(s: &str) -> i64 {
    Quantity::parse(s).map(|q| q.value()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn milli(s: &str) -> i64 {
        Quantity::parse(s).unwrap().milli_value()
    }
    fn val(s: &str) -> i64 {
        Quantity::parse(s).unwrap().value()
    }

    #[test]
    fn cpu_formats() {
        assert_eq!(milli("1"), 1000);
        assert_eq!(milli("2"), 2000);
        assert_eq!(milli("0.5"), 500);
        assert_eq!(milli(".5"), 500);
        assert_eq!(milli("250m"), 250);
        assert_eq!(milli("1500m"), 1500);
        assert_eq!(milli("100u"), 1); // rounds up
        assert_eq!(milli("1000000n"), 1);
        assert_eq!(milli("12345678n"), 13);
        assert_eq!(milli("999999u"), 1000);
        assert_eq!(milli("0"), 0);
        assert_eq!(milli("0m"), 0);
        assert_eq!(milli("1k"), 1_000_000);
        assert_eq!(milli("1.25"), 1250);
        assert_eq!(milli("1e3"), 1_000_000);
        assert_eq!(milli("1e-3"), 1);
        assert_eq!(milli("5E-1"), 500);
    }

    #[test]
    fn memory_formats() {
        assert_eq!(val("128974848"), 128_974_848);
        assert_eq!(val("129e6"), 129_000_000);
        assert_eq!(val("129M"), 129_000_000);
        assert_eq!(val("123Mi"), 123 * 1024 * 1024);
        assert_eq!(val("1Ki"), 1024);
        assert_eq!(val("1Gi"), 1 << 30);
        assert_eq!(val("1.5Gi"), 1_610_612_736);
        assert_eq!(val("2Ti"), 2 << 40);
        assert_eq!(val("1Pi"), 1 << 50);
        assert_eq!(val("1Ei"), 1 << 60);
        assert_eq!(val("1k"), 1000);
        assert_eq!(val("1G"), 1_000_000_000);
        assert_eq!(val("1T"), 1_000_000_000_000);
        assert_eq!(val("1P"), 1_000_000_000_000_000);
        assert_eq!(val("1E"), 1_000_000_000_000_000_000);
        assert_eq!(val("1e9"), 1_000_000_000);
        assert_eq!(val("1E+2"), 100);
        assert_eq!(val("500m"), 1); // rounds up
        assert_eq!(val("1500m"), 2);
        assert_eq!(val("8010812Ki"), 8_010_812 * 1024);
    }

    #[test]
    fn signs_and_rounding() {
        assert_eq!(val("-1"), -1);
        assert_eq!(val("-1500m"), -1);
        assert_eq!(milli("+2"), 2000);
        assert_eq!(val("007"), 7);
        assert_eq!(val("0.000"), 0);
    }

    #[test]
    fn saturation() {
        assert_eq!(val("100E"), i64::MAX);
        assert_eq!(milli("10E"), i64::MAX);
        assert_eq!(val("-100E"), i64::MIN);
        assert_eq!(val("1e-400"), 1);
    }

    #[test]
    fn invalid() {
        for s in [
            "", "abc", "1x", "1.2.3", "Mi", ".", "1e", "1e+", "1ee3", "1Kb", "--1",
        ] {
            assert!(Quantity::parse(s).is_err(), "{s} should fail");
        }
        assert_eq!(cpu_millis("bogus"), 0);
        assert_eq!(mem_bytes("bogus"), 0);
        assert_eq!(cpu_millis("100m"), 100);
        assert_eq!(mem_bytes("1Mi"), 1 << 20);
    }
}
