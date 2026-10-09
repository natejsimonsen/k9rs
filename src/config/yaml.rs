//! YAML helpers matching k9s (gopkg.in/yaml.v3) behavior:
//! - an emitter that writes files the way `data.SaveYAML` does (2 space
//!   indent, indented sequences, double quotes for strings that would not
//!   round trip as strings, Go float formatting, yaml.v3 map key order);
//! - lenient deserializers: YAML 1.1 booleans (`yes`/`on`/...) for typed bool
//!   fields and `null` treated as the zero value, like yaml.v3 does.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt::Write as _;

use anyhow::Result;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_yaml::Value;

/// Serialize `v` to YAML text exactly like yaml.v3 with `SetIndent(2)`.
pub fn to_string<T: Serialize>(v: &T) -> Result<String> {
    let value = serde_yaml::to_value(v)?;
    let mut out = String::new();
    match &value {
        Value::Mapping(m) if !m.is_empty() => emit_mapping(&mut out, m, 0),
        Value::Sequence(s) if !s.is_empty() => emit_sequence(&mut out, s, 0),
        other => {
            out.push_str(&scalar(other, false));
            out.push('\n');
        }
    }
    Ok(out)
}

fn pad(out: &mut String, n: usize) {
    for _ in 0..n {
        out.push(' ');
    }
}

fn emit_mapping(out: &mut String, m: &serde_yaml::Mapping, indent: usize) {
    for (k, v) in m {
        pad(out, indent);
        out.push_str(&scalar(k, true));
        out.push(':');
        emit_value_after_key(out, v, indent);
    }
}

fn emit_value_after_key(out: &mut String, v: &Value, indent: usize) {
    match v {
        Value::Mapping(m) if !m.is_empty() => {
            out.push('\n');
            emit_mapping(out, m, indent + 2);
        }
        Value::Sequence(s) if !s.is_empty() => {
            out.push('\n');
            emit_sequence(out, s, indent + 2);
        }
        Value::Tagged(t) => emit_value_after_key(out, &t.value, indent),
        Value::String(s) if s.contains('\n') => {
            out.push(' ');
            emit_literal(out, s, indent + 2);
        }
        other => {
            out.push(' ');
            out.push_str(&scalar(other, false));
            out.push('\n');
        }
    }
}

fn emit_sequence(out: &mut String, s: &[Value], indent: usize) {
    for item in s {
        pad(out, indent);
        out.push('-');
        emit_seq_item(out, item, indent);
    }
}

fn emit_seq_item(out: &mut String, item: &Value, indent: usize) {
    match item {
        Value::Mapping(m) if !m.is_empty() => {
            out.push(' ');
            // First entry shares the dash line, the others align with it.
            let mut first = true;
            for (k, v) in m {
                if !first {
                    pad(out, indent + 2);
                }
                first = false;
                out.push_str(&scalar(k, true));
                out.push(':');
                emit_value_after_key(out, v, indent + 2);
            }
        }
        Value::Sequence(s) if !s.is_empty() => {
            out.push(' ');
            let mut first = true;
            for sub in s {
                if !first {
                    pad(out, indent + 2);
                }
                first = false;
                out.push('-');
                emit_seq_item(out, sub, indent + 2);
            }
        }
        Value::Tagged(t) => emit_seq_item(out, &t.value, indent),
        Value::String(s) if s.contains('\n') => {
            out.push(' ');
            emit_literal(out, s, indent + 2);
        }
        other => {
            out.push(' ');
            out.push_str(&scalar(other, false));
            out.push('\n');
        }
    }
}

fn emit_literal(out: &mut String, s: &str, indent: usize) {
    out.push('|');
    if s.starts_with(' ') {
        out.push('2');
    }
    let trailing = s.len() - s.trim_end_matches('\n').len();
    match trailing {
        0 => out.push('-'),
        1 => {}
        _ => out.push('+'),
    }
    out.push('\n');
    let body = if trailing >= 1 { &s[..s.len() - 1] } else { s };
    for line in body.split('\n') {
        if !line.is_empty() {
            pad(out, indent);
            out.push_str(line);
        }
        out.push('\n');
    }
}

/// Render a scalar the way yaml.v3 would.
fn scalar(v: &Value, _key: bool) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.to_string()
            } else if let Some(u) = n.as_u64() {
                u.to_string()
            } else {
                go_float(n.as_f64().unwrap_or(0.0))
            }
        }
        Value::String(s) => string_scalar(s),
        Value::Sequence(_) => "[]".into(),
        Value::Mapping(_) => "{}".into(),
        Value::Tagged(t) => scalar(&t.value, _key),
    }
}

/// Go `strconv.FormatFloat(f, 'g', -1, 64)` with yaml.v3 inf/nan names.
pub fn go_float(f: f64) -> String {
    if f.is_nan() {
        return ".nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 {
            ".inf".into()
        } else {
            "-.inf".into()
        };
    }
    if f == 0.0 {
        return if f.is_sign_negative() {
            "-0".into()
        } else {
            "0".into()
        };
    }
    // Shortest round-trip digits via Rust's `{:e}` formatting.
    let e = format!("{:e}", f);
    let (mant, exp) = e.split_once('e').unwrap_or((&e, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let neg = mant.starts_with('-');
    let digits: String = mant.chars().filter(|c| c.is_ascii_digit()).collect();
    let nd = digits.len() as i32;
    let sign = if neg { "-" } else { "" };
    // Go: %e when exp < -4 || exp >= eprec; eprec is 6 for shortest output.
    let eprec = 6;
    if exp < -4 || exp >= eprec {
        let mut m = String::new();
        m.push_str(&digits[..1]);
        if nd > 1 {
            m.push('.');
            m.push_str(&digits[1..]);
        }
        let es = if exp < 0 { '-' } else { '+' };
        return format!("{sign}{m}e{es}{:02}", exp.abs());
    }
    let dp = exp + 1; // digits before the decimal point
    let mut s = String::from(sign);
    if dp <= 0 {
        s.push_str("0.");
        for _ in 0..(-dp) {
            s.push('0');
        }
        s.push_str(&digits);
    } else if dp >= nd {
        s.push_str(&digits);
        for _ in 0..(dp - nd) {
            s.push('0');
        }
    } else {
        s.push_str(&digits[..dp as usize]);
        s.push('.');
        s.push_str(&digits[dp as usize..]);
    }
    s
}

fn string_scalar(s: &str) -> String {
    if !resolves_as_str(s) || is_base60_float(s) || is_old_bool(s) {
        return double_quoted(s);
    }
    match analyze(s) {
        Style::Plain => s.to_string(),
        Style::Single => format!("'{}'", s.replace('\'', "''")),
        Style::Double => double_quoted(s),
    }
}

enum Style {
    Plain,
    Single,
    Double,
}

fn is_printable(c: char) -> bool {
    matches!(c, '\n' | '\u{20}'..='\u{7e}' | '\u{85}' | '\u{a0}'..='\u{d7ff}' | '\u{e000}'..='\u{fffd}' | '\u{10000}'..='\u{10ffff}')
        && c != '\u{feff}'
}

/// libyaml yaml_emitter_analyze_scalar, block context.
fn analyze(s: &str) -> Style {
    if s.is_empty() {
        return Style::Double;
    }
    if s.starts_with("---") || s.starts_with("...") {
        return Style::Single;
    }
    let chars: Vec<char> = s.chars().collect();
    let mut block_indicators = false;
    let mut special = false;
    let mut space_break = false;
    let mut previous_space = false;
    let mut preceded_by_ws = true;
    let n = chars.len();
    for (i, &c) in chars.iter().enumerate() {
        let followed_by_ws = i + 1 >= n || matches!(chars[i + 1], ' ' | '\t' | '\n' | '\r');
        if i == 0 {
            match c {
                '#' | ',' | '[' | ']' | '{' | '}' | '&' | '*' | '!' | '|' | '>' | '\'' | '"'
                | '%' | '@' | '`' => block_indicators = true,
                '?' | ':' | '-' if followed_by_ws => block_indicators = true,
                _ => {}
            }
        } else {
            match c {
                ':' if followed_by_ws => block_indicators = true,
                '#' if preceded_by_ws => block_indicators = true,
                _ => {}
            }
        }
        if !is_printable(c) {
            special = true;
        }
        if c == ' ' {
            previous_space = true;
        } else if c == '\n' {
            if previous_space {
                space_break = true;
            }
            previous_space = false;
        } else {
            previous_space = false;
        }
        preceded_by_ws = matches!(c, ' ' | '\t' | '\n' | '\r');
    }
    let leading_space = chars[0] == ' ';
    let trailing_space = chars[n - 1] == ' ';
    if special {
        return Style::Double;
    }
    if leading_space || trailing_space || block_indicators || space_break {
        if space_break {
            return Style::Double;
        }
        return Style::Single;
    }
    Style::Plain
}

fn double_quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\0' => out.push_str("\\0"),
            '\u{7}' => out.push_str("\\a"),
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{b}' => out.push_str("\\v"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            '\u{1b}' => out.push_str("\\e"),
            '\u{85}' => out.push_str("\\N"),
            '\u{a0}' => out.push_str("\\_"),
            '\u{2028}' => out.push_str("\\L"),
            '\u{2029}' => out.push_str("\\P"),
            c if !is_printable(c) => {
                let v = c as u32;
                if v <= 0xff {
                    let _ = write!(out, "\\x{v:02X}");
                } else if v <= 0xffff {
                    let _ = write!(out, "\\u{v:04X}");
                } else {
                    let _ = write!(out, "\\U{v:08X}");
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// yaml.v3 resolve(): would this plain scalar be read back as a string?
fn resolves_as_str(s: &str) -> bool {
    if matches!(
        s,
        "" | "~"
            | "null"
            | "Null"
            | "NULL"
            | "true"
            | "True"
            | "TRUE"
            | "false"
            | "False"
            | "FALSE"
            | ".inf"
            | ".Inf"
            | ".INF"
            | "+.inf"
            | "+.Inf"
            | "+.INF"
            | "-.inf"
            | "-.Inf"
            | "-.INF"
            | ".nan"
            | ".NaN"
            | ".NAN"
            | "<<"
    ) {
        return false;
    }
    let first = s.chars().next().unwrap_or(' ');
    if first.is_ascii_digit() || first == '+' || first == '-' || first == '.' {
        if is_timestamp(s) {
            return false;
        }
        let plain = s.replace('_', "");
        if go_parse_int(&plain) {
            return false;
        }
        if yaml_style_float(&plain) {
            return false;
        }
        let p = plain.strip_prefix('-').unwrap_or(&plain);
        let p = p.strip_prefix('+').unwrap_or(p);
        if let Some(b) = p.strip_prefix("0b") {
            if !b.is_empty() && b.chars().all(|c| c == '0' || c == '1') {
                return false;
            }
        }
        if let Some(o) = p.strip_prefix("0o") {
            if !o.is_empty() && o.chars().all(|c| ('0'..='7').contains(&c)) {
                return false;
            }
        }
    }
    true
}

/// strconv.ParseInt(s, 0, 64) / ParseUint success.
fn go_parse_int(s: &str) -> bool {
    let body = s.strip_prefix(['+', '-']).unwrap_or(s);
    if body.is_empty() {
        return false;
    }
    let (digits, radix) =
        if let Some(h) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
            (h, 16)
        } else if let Some(b) = body.strip_prefix("0b").or_else(|| body.strip_prefix("0B")) {
            (b, 2)
        } else if let Some(o) = body.strip_prefix("0o").or_else(|| body.strip_prefix("0O")) {
            (o, 8)
        } else if body.len() > 1 && body.starts_with('0') {
            (&body[1..], 8)
        } else {
            (body, 10)
        };
    !digits.is_empty() && u64::from_str_radix(digits, radix).is_ok()
}

fn yaml_style_float(s: &str) -> bool {
    // ^[-+]?(\.[0-9]+|[0-9]+(\.[0-9]*)?)([eE][-+]?[0-9]+)?$
    let b = s.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let start = i;
    if i < b.len() && b[i] == b'.' {
        i += 1;
        let d = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == d {
            return false;
        }
    } else {
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == start {
            return false;
        }
        if i < b.len() && b[i] == b'.' {
            i += 1;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
        }
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            i += 1;
        }
        let d = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == d {
            return false;
        }
    }
    i == b.len()
}

fn is_timestamp(s: &str) -> bool {
    static RX: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let rx = RX.get_or_init(|| {
        regex::Regex::new(
            r"^[0-9]{4}-[0-9]{1,2}-[0-9]{1,2}(([Tt]|[ \t]+)[0-9]{1,2}:[0-9]{1,2}:[0-9]{1,2}(\.[0-9]*)?(([ \t]*)(Z|[-+][0-9]{1,2}(:[0-9]{2})?))?)?$",
        )
        .expect("timestamp regex")
    });
    rx.is_match(s)
}

fn is_base60_float(s: &str) -> bool {
    static RX: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let rx = RX.get_or_init(|| {
        regex::Regex::new(r"^[-+]?[0-9][0-9_]*(?::[0-5]?[0-9])+(?:\.[0-9_]*)?$")
            .expect("base60 regex")
    });
    let first = s.chars().next().unwrap_or(' ');
    (first == '+' || first == '-' || first.is_ascii_digit()) && s.contains(':') && rx.is_match(s)
}

fn is_old_bool(s: &str) -> bool {
    matches!(
        s,
        "y" | "Y"
            | "yes"
            | "Yes"
            | "YES"
            | "on"
            | "On"
            | "ON"
            | "n"
            | "N"
            | "no"
            | "No"
            | "NO"
            | "off"
            | "Off"
            | "OFF"
    )
}

/// yaml.v3 map key ordering (keyList.Less) for string keys.
pub fn key_less(a: &str, b: &str) -> Ordering {
    let ar: Vec<char> = a.chars().collect();
    let br: Vec<char> = b.chars().collect();
    let mut digits = false;
    let mut i = 0;
    while i < ar.len() && i < br.len() {
        if ar[i] == br[i] {
            digits = ar[i].is_numeric();
            i += 1;
            continue;
        }
        let al = ar[i].is_alphabetic();
        let bl = br[i].is_alphabetic();
        if al && bl {
            return ar[i].cmp(&br[i]);
        }
        if al || bl {
            let a_first = if digits { al } else { bl };
            return if a_first {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        }
        let (mut an, mut bn): (i64, i64) = (0, 0);
        if ar[i] == '0' || br[i] == '0' {
            let mut j = i as isize - 1;
            while j >= 0 && ar[j as usize].is_numeric() {
                if ar[j as usize] != '0' {
                    an = 1;
                    bn = 1;
                    break;
                }
                j -= 1;
            }
        }
        let mut ai = i;
        while ai < ar.len() && ar[ai].is_ascii_digit() {
            an = an.wrapping_mul(10).wrapping_add(ar[ai] as i64 - '0' as i64);
            ai += 1;
        }
        let mut bi = i;
        while bi < br.len() && br[bi].is_ascii_digit() {
            bn = bn.wrapping_mul(10).wrapping_add(br[bi] as i64 - '0' as i64);
            bi += 1;
        }
        if an != bn {
            return an.cmp(&bn);
        }
        if ai != bi {
            return ai.cmp(&bi);
        }
        return ar[i].cmp(&br[i]);
    }
    ar.len().cmp(&br.len())
}

/// Serialize a string-keyed map in yaml.v3 key order.
pub fn sorted_map<S, V>(m: &BTreeMap<String, V>, s: S) -> std::result::Result<S::Ok, S::Error>
where
    S: Serializer,
    V: Serialize,
{
    use serde::ser::SerializeMap;
    let mut keys: Vec<&String> = m.keys().collect();
    keys.sort_by(|a, b| key_less(a, b));
    let mut map = s.serialize_map(Some(keys.len()))?;
    for k in keys {
        map.serialize_entry(k, &m[k])?;
    }
    map.end()
}

/// Serialize an f32 through its shortest decimal form (Go FormatFloat 32 bits).
pub fn f32_go<S: Serializer>(f: &f32, s: S) -> std::result::Result<S::Ok, S::Error> {
    let v: f64 = format!("{f}").parse().unwrap_or(*f as f64);
    s.serialize_f64(v)
}

/// Deserialize a typed bool like yaml.v3 (YAML 1.1 forms accepted, null = false).
pub fn de_bool<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<bool, D::Error> {
    let v = Option::<Value>::deserialize(d)?;
    match v {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(b),
        Some(Value::String(s)) => match s.as_str() {
            "y" | "Y" | "yes" | "Yes" | "YES" | "on" | "On" | "ON" => Ok(true),
            "n" | "N" | "no" | "No" | "NO" | "off" | "Off" | "OFF" => Ok(false),
            _ => Err(serde::de::Error::custom(format!(
                "cannot unmarshal !!str `{s}` into bool"
            ))),
        },
        Some(other) => Err(serde::de::Error::custom(format!(
            "cannot unmarshal {} into bool",
            kind(&other)
        ))),
    }
}

/// Optional typed bool (Go `*bool`).
pub fn de_opt_bool<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<Option<bool>, D::Error> {
    let v = Option::<Value>::deserialize(d)?;
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(v) => de_bool(v).map(Some).map_err(serde::de::Error::custom),
    }
}

/// `null` decodes to the zero value (yaml.v3 leaves the field untouched).
pub fn de_null<'de, D, T>(d: D) -> std::result::Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// A string field that also takes numbers/bools (yaml.v3 uses the scalar
/// text when decoding into a string).
pub fn de_str<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<String, D::Error> {
    let v = Option::<Value>::deserialize(d)?;
    match v {
        None | Some(Value::Null) => Ok(String::new()),
        Some(v) => scalar_text(&v).ok_or_else(|| {
            serde::de::Error::custom(format!("cannot unmarshal {} into string", kind(&v)))
        }),
    }
}

/// A list of strings that also takes numbers/bools.
pub fn de_str_vec<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<Vec<String>, D::Error> {
    let v = Option::<Vec<Value>>::deserialize(d)?;
    v.unwrap_or_default()
        .iter()
        .map(|v| match v {
            Value::Null => Ok(String::new()),
            v => scalar_text(v).ok_or_else(|| {
                serde::de::Error::custom(format!("cannot unmarshal {} into string", kind(v)))
            }),
        })
        .collect()
}

fn scalar_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::Tagged(t) => scalar_text(&t.value),
        _ => None,
    }
}

fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "!!null",
        Value::Bool(_) => "!!bool",
        Value::Number(n) if n.is_f64() => "!!float",
        Value::Number(_) => "!!int",
        Value::String(_) => "!!str",
        Value::Sequence(_) => "!!seq",
        Value::Mapping(_) => "!!map",
        Value::Tagged(_) => "!!tagged",
    }
}

/// Parse YAML text into a generic value (empty documents are `Null`).
pub fn parse_value(bb: &str) -> Result<Value> {
    if bb.trim().is_empty() {
        return Ok(Value::Null);
    }
    let mut v: Value = serde_yaml::from_str(bb)?;
    v.apply_merge()?;
    Ok(v)
}

/// Deep merge `over` into `base`: mappings merge key by key, everything else
/// replaces (yaml.v3 decoding into an already populated struct).
pub fn merge_value(base: &mut Value, over: Value) {
    match (base, over) {
        (Value::Mapping(b), Value::Mapping(o)) => {
            for (k, v) in o {
                if v.is_null() {
                    continue;
                }
                match b.get_mut(&k) {
                    Some(bv) => merge_value(bv, v),
                    None => {
                        b.insert(k, v);
                    }
                }
            }
        }
        (b, o) => {
            if !o.is_null() {
                *b = o;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;

    #[test]
    fn go_floats() {
        for (f, e) in [
            (2.0, "2"),
            (2.5, "2.5"),
            (100.0, "100"),
            (0.1, "0.1"),
            (1e6, "1e+06"),
            (123456.0, "123456"),
            (1234567.0, "1.234567e+06"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (-3.25, "-3.25"),
        ] {
            assert_eq!(go_float(f), e, "{f}");
        }
    }

    #[test]
    fn quoting() {
        for (s, e) in [
            ("", "\"\""),
            ("po", "po"),
            ("2m0s", "2m0s"),
            ("true", "\"true\""),
            ("yes", "\"yes\""),
            ("10", "\"10\""),
            ("1.5", "\"1.5\""),
            ("0755", "\"0755\""),
            ("null", "\"null\""),
            ("~", "\"~\""),
            ("-", "'-'"),
            ("- a", "'- a'"),
            ("a: b", "'a: b'"),
            ("a:b", "a:b"),
            ("*", "'*'"),
            ("a #b", "'a #b'"),
            ("a#b", "a#b"),
            (" a", "' a'"),
            ("a ", "'a '"),
            ("1:20", "\"1:20\""),
            ("2001-12-14", "\"2001-12-14\""),
            (
                "arn:aws:eks:eu-central-1:xxx:cluster/fred-blee",
                "arn:aws:eks:eu-central-1:xxx:cluster/fred-blee",
            ),
            ("v1/pods default", "v1/pods default"),
            ("a\tb", "\"a\\tb\""),
            ("#ff0000", "'#ff0000'"),
        ] {
            assert_eq!(string_scalar(s), e, "{s:?}");
        }
    }

    #[derive(Serialize)]
    struct Doc {
        a: String,
        #[serde(serialize_with = "sorted_map")]
        m: BTreeMap<String, String>,
        e: BTreeMap<String, String>,
        l: Vec<String>,
        n: Vec<String>,
        o: Option<String>,
        body: String,
        objs: Vec<BTreeMap<String, i32>>,
    }

    #[test]
    fn emitter() {
        let d = Doc {
            a: "".into(),
            m: [("b10", "x"), ("b9", "y"), ("a", "z")]
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            e: BTreeMap::new(),
            l: vec!["default".into(), "kube-system".into()],
            n: vec![],
            o: None,
            body: "{\"fred\": \"blee\"}\nline2".into(),
            objs: vec![[("x".to_string(), 1), ("y".to_string(), 2)]
                .into_iter()
                .collect()],
        };
        let s = to_string(&d).unwrap();
        assert_eq!(
            s,
            "a: \"\"\nm:\n  a: z\n  b9: \"y\"\n  b10: x\ne: {}\nl:\n  - default\n  - kube-system\n\"n\": []\no: null\nbody: |-\n  {\"fred\": \"blee\"}\n  line2\nobjs:\n  - x: 1\n    \"y\": 2\n"
        );
    }

    #[test]
    fn key_order() {
        let mut v = vec!["b10", "b9", "a", "B", "1", "a1", "a01"];
        v.sort_by(|a, b| key_less(a, b));
        assert_eq!(v, vec!["1", "B", "a", "a1", "a01", "b9", "b10"]);
    }

    #[derive(Deserialize, Default)]
    #[serde(default)]
    struct Lenient {
        #[serde(deserialize_with = "de_bool")]
        b: bool,
        #[serde(deserialize_with = "de_opt_bool")]
        o: Option<bool>,
        #[serde(deserialize_with = "de_null")]
        v: Vec<String>,
    }

    #[test]
    fn lenient() {
        let l: Lenient = serde_yaml::from_str("b: yes\no: off\nv:\n").unwrap();
        assert!(l.b);
        assert_eq!(l.o, Some(false));
        assert!(l.v.is_empty());
        let l: Lenient = serde_yaml::from_str("b: true\n").unwrap();
        assert!(l.b);
        assert!(l.o.is_none());
        assert!(serde_yaml::from_str::<Lenient>("b: blee\n").is_err());
    }

    #[test]
    fn merge() {
        let mut a = parse_value("k9s:\n  body:\n    fgColor: red\n    bgColor: black\n").unwrap();
        let b = parse_value("k9s:\n  body:\n    fgColor: blue\n").unwrap();
        merge_value(&mut a, b);
        assert_eq!(
            a,
            parse_value("k9s:\n  body:\n    fgColor: blue\n    bgColor: black\n").unwrap()
        );
    }
}
