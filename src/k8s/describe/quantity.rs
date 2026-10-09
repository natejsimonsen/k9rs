//! Port of apimachinery's `resource.Quantity` parsing and canonical
//! `String()` formatting, plus the bits of arithmetic describers use.

use std::cmp::Ordering;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Format {
    /// Go's zero value (`Quantity{}`), formats as DecimalExponent.
    Unset,
    DecimalExponent,
    BinarySI,
    DecimalSI,
}

/// value = `m * 10^e`, always rounded to nano precision (`e >= -9`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Quantity {
    m: i128,
    e: i32,
    pub(crate) format: Format,
}

impl Default for Quantity {
    fn default() -> Self {
        Quantity {
            m: 0,
            e: 0,
            format: Format::Unset,
        }
    }
}

const DEC_SUFFIXES: [(&str, i32); 10] = [
    ("n", -9),
    ("u", -6),
    ("m", -3),
    ("", 0),
    ("k", 3),
    ("M", 6),
    ("G", 9),
    ("T", 12),
    ("P", 15),
    ("E", 18),
];

const BIN_SUFFIXES: [(&str, u32); 6] = [
    ("Ki", 10),
    ("Mi", 20),
    ("Gi", 30),
    ("Ti", 40),
    ("Pi", 50),
    ("Ei", 60),
];

fn pow10(n: u32) -> Option<i128> {
    10i128.checked_pow(n)
}

impl Quantity {
    pub(crate) fn new(value: i64, format: Format) -> Quantity {
        Quantity {
            m: value as i128,
            e: 0,
            format,
        }
    }

    /// resource.ParseQuantity.
    pub(crate) fn parse(raw: &str) -> Option<Quantity> {
        let s = raw.trim();
        if s.is_empty() {
            return None;
        }
        if s == "0" {
            return Some(Quantity::new(0, Format::DecimalSI));
        }
        let b = s.as_bytes();
        let mut pos = 0;
        let mut positive = true;
        match b[0] {
            b'-' => {
                positive = false;
                pos += 1;
            }
            b'+' => pos += 1,
            _ => {}
        }
        let num_start = pos;
        while pos < b.len() && b[pos].is_ascii_digit() {
            pos += 1;
        }
        let num = &s[num_start..pos];
        let mut denom = "";
        if pos < b.len() && b[pos] == b'.' {
            pos += 1;
            let d_start = pos;
            while pos < b.len() && b[pos].is_ascii_digit() {
                pos += 1;
            }
            denom = &s[d_start..pos];
        }
        let suffix = &s[pos..];
        if !valid_suffix_shape(suffix) {
            return None;
        }
        let (base, exponent, format) = interpret_suffix(suffix)?;
        let digits = format!("{num}{denom}");
        let digits = if digits.is_empty() {
            "0".to_string()
        } else {
            digits
        };
        let mut m: i128 = digits.parse().ok()?;
        let mut e: i32 = -(denom.len() as i32);
        if base == 10 {
            e = e.checked_add(exponent)?;
        } else {
            m = m.checked_mul(1i128.checked_shl(exponent as u32)?)?;
        }
        if !positive {
            m = -m;
        }
        let mut q = Quantity { m, e, format };
        q.round_to_nano()?;
        if q.format == Format::BinarySI && q.m != 0 && q.cmp_int(1) == Ordering::Less && q.m > 0 {
            q.format = Format::DecimalSI;
        }
        Some(q)
    }

    fn round_to_nano(&mut self) -> Option<()> {
        if self.e < -9 {
            let div = pow10((-9 - self.e) as u32)?;
            let neg = self.m < 0;
            let a = self.m.abs();
            let mut r = a / div;
            if a % div != 0 {
                r += 1;
            }
            self.m = if neg { -r } else { r };
            self.e = -9;
        }
        Some(())
    }

    pub(crate) fn is_zero(&self) -> bool {
        self.m == 0
    }

    /// Align two quantities to a common exponent.
    fn aligned(a: &Quantity, b: &Quantity) -> Option<(i128, i128, i32)> {
        let e = a.e.min(b.e);
        let am = a.m.checked_mul(pow10((a.e - e) as u32)?)?;
        let bm = b.m.checked_mul(pow10((b.e - e) as u32)?)?;
        Some((am, bm, e))
    }

    fn as_f64(&self) -> f64 {
        self.m as f64 * 10f64.powi(self.e)
    }

    pub(crate) fn cmp(&self, other: &Quantity) -> Ordering {
        match Quantity::aligned(self, other) {
            Some((a, b, _)) => a.cmp(&b),
            None => self
                .as_f64()
                .partial_cmp(&other.as_f64())
                .unwrap_or(Ordering::Equal),
        }
    }

    fn cmp_int(&self, v: i64) -> Ordering {
        self.cmp(&Quantity::new(v, Format::DecimalSI))
    }

    /// q.Add(y): sum keeps q's format unless q is zero.
    pub(crate) fn add(&mut self, y: &Quantity) {
        if self.is_zero() {
            self.format = y.format;
        }
        if let Some((a, b, e)) = Quantity::aligned(self, y) {
            if let Some(m) = a.checked_add(b) {
                self.m = m;
                self.e = e;
                return;
            }
        }
        // Overflow: degrade to float precision (never hit by real quantities).
        let v = self.as_f64() + y.as_f64();
        self.m = v as i128;
        self.e = 0;
    }

    /// ceil(q / 10^scale) as Go's ScaledValue.
    fn scaled_value(&self, scale: i32) -> i64 {
        let shift = self.e - scale;
        if shift >= 0 {
            return pow10(shift as u32)
                .and_then(|p| self.m.checked_mul(p))
                .map(|v| v.clamp(i64::MIN as i128, i64::MAX as i128) as i64)
                .unwrap_or(i64::MAX);
        }
        let Some(div) = pow10((-shift) as u32) else {
            return 0;
        };
        let neg = self.m < 0;
        let a = self.m.abs();
        let mut r = a / div;
        if a % div != 0 {
            r += 1;
        }
        let r = if neg { -r } else { r };
        r.clamp(i64::MIN as i128, i64::MAX as i128) as i64
    }

    pub(crate) fn value(&self) -> i64 {
        self.scaled_value(0)
    }

    pub(crate) fn milli_value(&self) -> i64 {
        self.scaled_value(-3)
    }

    /// Quantity.String(): the canonical representation.
    pub(crate) fn go_string(&self) -> String {
        if self.is_zero() {
            return "0".to_string();
        }
        let (mut m, mut e) = (self.m, self.e);
        while m % 10 == 0 {
            m /= 10;
            e += 1;
        }
        let mut format = self.format;
        match format {
            Format::DecimalExponent | Format::DecimalSI => {}
            Format::BinarySI => {
                // Small values, and values that are not whole numbers, print as DecimalSI.
                let small = self.cmp_int(-1024) == Ordering::Greater
                    && self.cmp_int(1024) == Ordering::Less;
                if small || e < 0 {
                    format = Format::DecimalSI;
                }
            }
            Format::Unset => format = Format::DecimalExponent,
        }
        if format == Format::BinarySI {
            if let Some(mut n) = pow10(e as u32).and_then(|p| m.checked_mul(p)) {
                let mut k = 0;
                while n % 1024 == 0 {
                    n /= 1024;
                    k += 1;
                }
                let suffix = BIN_SUFFIXES
                    .iter()
                    .find(|(_, x)| *x == k * 10)
                    .map(|(s, _)| *s)
                    .unwrap_or("");
                return format!("{n}{suffix}");
            }
            format = Format::DecimalSI;
        }
        while e % 3 != 0 {
            m *= 10;
            e -= 1;
        }
        let suffix = if format == Format::DecimalExponent {
            if e == 0 {
                String::new()
            } else {
                format!("e{e}")
            }
        } else {
            DEC_SUFFIXES
                .iter()
                .find(|(_, x)| *x == e)
                .map(|(s, _)| s.to_string())
                .unwrap_or_default()
        };
        format!("{m}{suffix}")
    }
}

/// The suffix must be `[eEinumkKMGTP]*[+-]?[0-9]*` (parseQuantityString).
fn valid_suffix_shape(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && b"eEinumkKMGTP".contains(&b[i]) {
        i += 1;
    }
    if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
        i += 1;
    }
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    i == b.len()
}

fn interpret_suffix(s: &str) -> Option<(u32, i32, Format)> {
    if let Some((_, e)) = DEC_SUFFIXES.iter().find(|(x, _)| *x == s) {
        return Some((10, *e, Format::DecimalSI));
    }
    if let Some((_, e)) = BIN_SUFFIXES.iter().find(|(x, _)| *x == s) {
        return Some((2, *e as i32, Format::BinarySI));
    }
    if s.len() > 1 && (s.starts_with('e') || s.starts_with('E')) {
        let e: i32 = s[1..].parse().ok()?;
        return Some((10, e, Format::DecimalExponent));
    }
    None
}

/// Canonical form of a raw quantity string; unparsable input is kept as is.
pub(crate) fn canonical(raw: &str) -> String {
    match Quantity::parse(raw) {
        Some(q) => q.go_string(),
        None => raw.to_string(),
    }
}

/// Parse or zero (Go's zero `Quantity{}` for missing/invalid values).
pub(crate) fn parse_or_zero(raw: &str) -> Quantity {
    Quantity::parse(raw).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_forms() {
        let cases = [
            ("100m", "100m"),
            ("1000m", "1"),
            ("0.5", "500m"),
            ("1.5", "1500m"),
            ("1", "1"),
            ("1024Mi", "1Gi"),
            ("1536Mi", "1536Mi"),
            ("128Mi", "128Mi"),
            ("1Gi", "1Gi"),
            ("1000Ki", "1000Ki"),
            ("1024", "1024"),
            ("1e3", "1e3"),
            ("1000", "1k"),
            ("2000k", "2M"),
            ("12k", "12k"),
            ("1500", "1500"),
            ("0", "0"),
            ("0Mi", "0"),
            ("10G", "10G"),
            ("500Ki", "500Ki"),
            ("1.5Gi", "1536Mi"),
            ("0.5Ki", "512"),
            ("100Ki", "100Ki"),
            ("3Ki", "3Ki"),
            ("1025", "1025"),
            ("110", "110"),
            ("8053040Ki", "8053040Ki"),
            ("16247736Ki", "16247736Ki"),
            ("0.0000000001", "1n"),
            ("-100m", "-100m"),
            ("1.1e-3", "1100e-6"),
            ("5e8", "500e6"),
        ];
        for (input, want) in cases {
            assert_eq!(canonical(input), want, "input {input}");
        }
    }

    #[test]
    fn invalid_kept() {
        assert_eq!(canonical("abc"), "abc");
        assert_eq!(canonical("1Qi"), "1Qi");
    }

    #[test]
    fn arithmetic() {
        let mut a = parse_or_zero("100m");
        a.add(&parse_or_zero("1"));
        assert_eq!(a.go_string(), "1100m");
        assert_eq!(a.milli_value(), 1100);
        assert_eq!(parse_or_zero("1Gi").value(), 1 << 30);
        assert_eq!(parse_or_zero("1500m").value(), 2);
        let mut z = Quantity::default();
        z.add(&parse_or_zero("64Mi"));
        assert_eq!(z.go_string(), "64Mi");
        assert_eq!(
            parse_or_zero("1").cmp(&parse_or_zero("1000m")),
            Ordering::Equal
        );
    }
}
