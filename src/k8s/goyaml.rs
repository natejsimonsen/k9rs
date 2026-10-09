//! YAML output byte-compatible with k9s (kubectl YAMLPrinter ->
//! sigs.k8s.io/yaml JSONToYAML -> go.yaml.in/yaml/v2 Marshal).
//!
//! A port of the parts of yaml.v2's encoder and libyaml emitter that a JSON
//! document reaches: sorted block mappings, indentless block sequences,
//! `{}`/`[]` for empty collections, and scalar styles chosen like yaml.v2
//! (plain, else single quoted, else double quoted; literal blocks for
//! multi-line strings), folded at 80 columns.

use serde_json::Value;

const BEST_INDENT: i64 = 2;
const BEST_WIDTH: usize = 80;

/// Marshal a JSON value like k9s' YAML view.
pub fn to_yaml(v: &Value) -> String {
    let mut e = Emitter {
        out: String::new(),
        column: 0,
        indent: -1,
        indents: Vec::new(),
        whitespace: true,
        indention: true,
        flow_level: 0,
    };
    e.node(v, Ctx::default());
    e.write_indent();
    e.out
}

#[derive(Clone, Copy, Default)]
struct Ctx {
    mapping: bool,
    simple_key: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Style {
    Plain,
    Single,
    Double,
    Literal,
}

struct Emitter {
    out: String,
    column: usize,
    indent: i64,
    indents: Vec<i64>,
    whitespace: bool,
    indention: bool,
    flow_level: usize,
}

impl Emitter {
    fn put(&mut self, c: char) {
        self.out.push(c);
        self.column += 1;
    }

    fn put_break(&mut self) {
        self.out.push('\n');
        self.column = 0;
    }

    fn write_all(&mut self, s: &str) {
        for c in s.chars() {
            self.put(c);
        }
    }

    fn increase_indent(&mut self, flow: bool, indentless: bool) {
        self.indents.push(self.indent);
        if self.indent < 0 {
            self.indent = if flow { BEST_INDENT } else { 0 };
        } else if !indentless {
            self.indent += BEST_INDENT;
        }
    }

    fn pop_indent(&mut self) {
        self.indent = self.indents.pop().unwrap_or(-1);
    }

    fn write_indent(&mut self) {
        let indent = self.indent.max(0) as usize;
        if !self.indention || self.column > indent || (self.column == indent && !self.whitespace) {
            self.put_break();
        }
        while self.column < indent {
            self.put(' ');
        }
        self.whitespace = true;
        self.indention = true;
    }

    fn write_indicator(&mut self, s: &str, need_ws: bool, is_ws: bool, is_indention: bool) {
        if need_ws && !self.whitespace {
            self.put(' ');
        }
        self.write_all(s);
        self.whitespace = is_ws;
        self.indention = self.indention && is_indention;
    }

    fn node(&mut self, v: &Value, ctx: Ctx) {
        match v {
            Value::Object(m) if m.is_empty() => self.empty_flow("{", "}"),
            Value::Array(a) if a.is_empty() => self.empty_flow("[", "]"),
            Value::Object(m) => {
                let mut keys: Vec<&String> = m.keys().collect();
                keys.sort_by(|a, b| key_cmp(a, b));
                self.increase_indent(false, false);
                for k in keys {
                    self.write_indent();
                    let simple = !k.chars().any(is_break) && k.len() <= 128;
                    if simple {
                        self.scalar(
                            k,
                            string_style(k),
                            Ctx {
                                mapping: true,
                                simple_key: true,
                            },
                        );
                        self.write_indicator(":", false, false, false);
                    } else {
                        self.write_indicator("?", true, false, true);
                        self.scalar(
                            k,
                            string_style(k),
                            Ctx {
                                mapping: true,
                                simple_key: false,
                            },
                        );
                        self.write_indent();
                        self.write_indicator(":", true, false, true);
                    }
                    self.node(
                        &m[k.as_str()],
                        Ctx {
                            mapping: true,
                            simple_key: false,
                        },
                    );
                }
                self.pop_indent();
            }
            Value::Array(a) => {
                self.increase_indent(false, ctx.mapping && !self.indention);
                for item in a {
                    self.write_indent();
                    self.write_indicator("-", true, false, true);
                    self.node(item, Ctx::default());
                }
                self.pop_indent();
            }
            Value::String(s) => self.scalar(s, string_style(s), ctx),
            Value::Bool(b) => self.scalar(if *b { "true" } else { "false" }, Style::Plain, ctx),
            Value::Null => self.scalar("null", Style::Plain, ctx),
            Value::Number(n) => {
                let s = match (n.as_i64(), n.as_u64(), n.as_f64()) {
                    (Some(i), _, _) => i.to_string(),
                    (_, Some(u), _) => u.to_string(),
                    (_, _, Some(f)) => go_float(f),
                    _ => n.to_string(),
                };
                self.scalar(&s, Style::Plain, ctx)
            }
        }
    }

    fn empty_flow(&mut self, open: &str, close: &str) {
        self.write_indicator(open, true, true, false);
        self.increase_indent(true, false);
        self.flow_level += 1;
        self.flow_level -= 1;
        self.pop_indent();
        self.write_indicator(close, false, false, false);
    }

    fn scalar(&mut self, value: &str, requested: Style, ctx: Ctx) {
        let a = analyze(value);
        let mut style = requested;
        if ctx.simple_key && a.multiline {
            style = Style::Double;
        }
        if style == Style::Plain {
            let allowed = if self.flow_level > 0 {
                a.flow_plain
            } else {
                a.block_plain
            };
            if !allowed || (value.is_empty() && (self.flow_level > 0 || ctx.simple_key)) {
                style = Style::Single;
            }
        }
        if style == Style::Single && !a.single_quoted {
            style = Style::Double;
        }
        if style == Style::Literal && (!a.block || self.flow_level > 0 || ctx.simple_key) {
            style = Style::Double;
        }
        self.increase_indent(true, false);
        let breaks = !ctx.simple_key;
        match style {
            Style::Plain => self.write_plain(value, breaks),
            Style::Single => self.write_single(value, breaks),
            Style::Double => self.write_double(value, breaks),
            Style::Literal => self.write_literal(value),
        }
        self.pop_indent();
    }

    fn write_plain(&mut self, value: &str, allow_breaks: bool) {
        if !self.whitespace {
            self.put(' ');
        }
        let cs: Vec<char> = value.chars().collect();
        let mut spaces = false;
        for (i, &c) in cs.iter().enumerate() {
            if c == ' ' {
                if allow_breaks
                    && !spaces
                    && self.column > BEST_WIDTH
                    && cs.get(i + 1) != Some(&' ')
                {
                    self.write_indent();
                } else {
                    self.put(c);
                }
                spaces = true;
            } else {
                self.put(c);
                self.indention = false;
                spaces = false;
            }
        }
        self.whitespace = false;
        self.indention = false;
    }

    fn write_single(&mut self, value: &str, allow_breaks: bool) {
        self.write_indicator("'", true, false, false);
        let cs: Vec<char> = value.chars().collect();
        let mut spaces = false;
        let mut breaks = false;
        for (i, &c) in cs.iter().enumerate() {
            if c == ' ' {
                if allow_breaks
                    && !spaces
                    && self.column > BEST_WIDTH
                    && i > 0
                    && i + 1 < cs.len()
                    && cs.get(i + 1) != Some(&' ')
                {
                    self.write_indent();
                } else {
                    self.put(c);
                }
                spaces = true;
            } else if is_break(c) {
                if !breaks && c == '\n' {
                    self.put_break();
                }
                self.write_break(c);
                self.indention = true;
                breaks = true;
            } else {
                if breaks {
                    self.write_indent();
                }
                if c == '\'' {
                    self.put('\'');
                }
                self.put(c);
                self.indention = false;
                spaces = false;
                breaks = false;
            }
        }
        self.write_indicator("'", false, false, false);
        self.whitespace = false;
        self.indention = false;
    }

    fn write_double(&mut self, value: &str, allow_breaks: bool) {
        self.write_indicator("\"", true, false, false);
        let cs: Vec<char> = value.chars().collect();
        let mut spaces = false;
        for (i, &c) in cs.iter().enumerate() {
            if !is_printable(c) || c == '\u{feff}' || is_break(c) || c == '"' || c == '\\' {
                self.put('\\');
                match c {
                    '\0' => self.put('0'),
                    '\u{7}' => self.put('a'),
                    '\u{8}' => self.put('b'),
                    '\t' => self.put('t'),
                    '\n' => self.put('n'),
                    '\u{b}' => self.put('v'),
                    '\u{c}' => self.put('f'),
                    '\r' => self.put('r'),
                    '\u{1b}' => self.put('e'),
                    '"' => self.put('"'),
                    '\\' => self.put('\\'),
                    '\u{85}' => self.put('N'),
                    '\u{a0}' => self.put('_'),
                    '\u{2028}' => self.put('L'),
                    '\u{2029}' => self.put('P'),
                    _ => {
                        let v = c as u32;
                        let s = if v <= 0xFF {
                            format!("x{v:02X}")
                        } else if v <= 0xFFFF {
                            format!("u{v:04X}")
                        } else {
                            format!("U{v:08X}")
                        };
                        self.write_all(&s);
                    }
                }
                spaces = false;
            } else if c == ' ' {
                if allow_breaks && !spaces && self.column > BEST_WIDTH && i > 0 && i + 1 < cs.len()
                {
                    self.write_indent();
                    if cs.get(i + 1) == Some(&' ') {
                        self.put('\\');
                    }
                } else {
                    self.put(c);
                }
                spaces = true;
            } else {
                self.put(c);
                spaces = false;
            }
        }
        self.write_indicator("\"", false, false, false);
        self.whitespace = false;
        self.indention = false;
    }

    fn write_break(&mut self, c: char) {
        if c == '\n' {
            self.put_break();
        } else {
            self.out.push(c);
            self.column = 0;
        }
    }

    fn write_literal(&mut self, value: &str) {
        self.write_indicator("|", true, false, false);
        // Block scalar hints.
        let first = value.chars().next();
        if first.is_some_and(|c| c == ' ' || is_break(c)) {
            self.write_indicator(&BEST_INDENT.to_string(), false, false, false);
        }
        let cs: Vec<char> = value.chars().collect();
        let chomp = match cs.len() {
            0 => Some("-"),
            n if !is_break(cs[n - 1]) => Some("-"),
            1 => Some("+"),
            n if is_break(cs[n - 2]) => Some("+"),
            _ => None,
        };
        if let Some(h) = chomp {
            self.write_indicator(h, false, false, false);
        }
        self.put_break();
        self.indention = true;
        self.whitespace = true;
        let mut breaks = true;
        for &c in &cs {
            if is_break(c) {
                self.write_break(c);
                self.indention = true;
                breaks = true;
            } else {
                if breaks {
                    self.write_indent();
                }
                self.put(c);
                self.indention = false;
                breaks = false;
            }
        }
    }
}

fn is_break(c: char) -> bool {
    matches!(c, '\r' | '\n' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

/// libyaml is_printable (UTF-8 lead byte checks, so nothing past U+FFFF).
fn is_printable(c: char) -> bool {
    let v = c as u32;
    v == 0x0A
        || (0x20..=0x7E).contains(&v)
        || (0xA0..=0xD7FF).contains(&v)
        || ((0xE000..=0xFFFD).contains(&v) && v != 0xFEFF)
}

struct Analysis {
    multiline: bool,
    flow_plain: bool,
    block_plain: bool,
    single_quoted: bool,
    block: bool,
}

/// yaml_emitter_analyze_scalar.
fn analyze(value: &str) -> Analysis {
    if value.is_empty() {
        return Analysis {
            multiline: false,
            flow_plain: false,
            block_plain: true,
            single_quoted: true,
            block: false,
        };
    }
    let cs: Vec<char> = value.chars().collect();
    let (mut block_ind, mut flow_ind) = (false, false);
    if value.starts_with("---") || value.starts_with("...") {
        block_ind = true;
        flow_ind = true;
    }
    let (mut line_breaks, mut special) = (false, false);
    let (mut leading_space, mut leading_break, mut trailing_space, mut trailing_break) =
        (false, false, false, false);
    let (mut break_space, mut space_break) = (false, false);
    let (mut prev_space, mut prev_break) = (false, false);
    let mut preceded_by_ws = true;
    let n = cs.len();
    for (i, &c) in cs.iter().enumerate() {
        let followed_by_ws = i + 1 >= n || cs[i + 1] == ' ' || cs[i + 1] == '\t';
        if i == 0 {
            match c {
                '#' | ',' | '[' | ']' | '{' | '}' | '&' | '*' | '!' | '|' | '>' | '\'' | '"'
                | '%' | '@' | '`' => {
                    flow_ind = true;
                    block_ind = true;
                }
                '?' | ':' => {
                    flow_ind = true;
                    if followed_by_ws {
                        block_ind = true;
                    }
                }
                '-' if followed_by_ws => {
                    flow_ind = true;
                    block_ind = true;
                }
                _ => {}
            }
        } else {
            match c {
                ',' | '?' | '[' | ']' | '{' | '}' => flow_ind = true,
                ':' => {
                    flow_ind = true;
                    if followed_by_ws {
                        block_ind = true;
                    }
                }
                '#' if preceded_by_ws => {
                    flow_ind = true;
                    block_ind = true;
                }
                _ => {}
            }
        }
        if !is_printable(c) {
            special = true;
        }
        if c == ' ' {
            if i == 0 {
                leading_space = true;
            }
            if i + 1 == n {
                trailing_space = true;
            }
            if prev_break {
                break_space = true;
            }
            prev_space = true;
            prev_break = false;
        } else if is_break(c) {
            line_breaks = true;
            if i == 0 {
                leading_break = true;
            }
            if i + 1 == n {
                trailing_break = true;
            }
            if prev_space {
                space_break = true;
            }
            prev_space = false;
            prev_break = true;
        } else {
            prev_space = false;
            prev_break = false;
        }
        preceded_by_ws = c == ' ' || c == '\t' || is_break(c);
    }
    let mut a = Analysis {
        multiline: line_breaks,
        flow_plain: true,
        block_plain: true,
        single_quoted: true,
        block: true,
    };
    if leading_space || leading_break || trailing_space || trailing_break {
        a.flow_plain = false;
        a.block_plain = false;
    }
    if trailing_space {
        a.block = false;
    }
    if break_space {
        a.flow_plain = false;
        a.block_plain = false;
        a.single_quoted = false;
    }
    if space_break || special {
        a.flow_plain = false;
        a.block_plain = false;
        a.single_quoted = false;
        a.block = false;
    }
    if line_breaks {
        a.flow_plain = false;
        a.block_plain = false;
    }
    if flow_ind {
        a.flow_plain = false;
    }
    if block_ind {
        a.block_plain = false;
    }
    a
}

/// encoder.stringv: literal for multi-line text, plain when the text would
/// read back as a string, else double quoted.
fn string_style(s: &str) -> Style {
    if s.contains('\n') {
        Style::Literal
    } else if resolves_to_str(s) && !is_base60_float(s) {
        Style::Plain
    } else {
        Style::Double
    }
}

/// resolve("", s) == !!str.
fn resolves_to_str(s: &str) -> bool {
    const MAP: &[&str] = &[
        "y", "Y", "yes", "Yes", "YES", "true", "True", "TRUE", "on", "On", "ON", "n", "N", "no",
        "No", "NO", "false", "False", "FALSE", "off", "Off", "OFF", "", "~", "null", "Null",
        "NULL", ".nan", ".NaN", ".NAN", ".inf", ".Inf", ".INF", "+.inf", "+.Inf", "+.INF", "-.inf",
        "-.Inf", "-.INF", "<<",
    ];
    let Some(first) = s.bytes().next() else {
        return false;
    };
    match first {
        b'y' | b'Y' | b'n' | b'N' | b't' | b'T' | b'f' | b'F' | b'o' | b'O' | b'~' => {
            !MAP.contains(&s)
        }
        b'.' => !MAP.contains(&s) && go_parse_float(s).is_none(),
        b'+' | b'-' | b'0'..=b'9' => {
            if MAP.contains(&s) || is_timestamp(s) {
                return false;
            }
            let plain = s.replace('_', "");
            if go_parse_int(&plain).is_some() {
                return false;
            }
            if yaml_style_float(&plain) && go_parse_float(&plain).is_some() {
                return false;
            }
            if let Some(b) = plain.strip_prefix("0b") {
                if u64::from_str_radix(b, 2).is_ok() {
                    return false;
                }
            } else if let Some(b) = plain.strip_prefix("-0b") {
                if i64::from_str_radix(&format!("-{b}"), 2).is_ok() {
                    return false;
                }
            }
            true
        }
        _ => true,
    }
}

/// strconv.ParseInt/ParseUint with base 0 (0x, 0o, 0b and leading 0 octal).
fn go_parse_int(s: &str) -> Option<i128> {
    let (neg, body) = match s.as_bytes().first()? {
        b'-' => (true, &s[1..]),
        b'+' => (false, &s[1..]),
        _ => (false, s),
    };
    if body.is_empty() {
        return None;
    }
    let lower = body.to_ascii_lowercase();
    let (radix, digits) = if let Some(d) = lower.strip_prefix("0x") {
        (16, d.to_string())
    } else if let Some(d) = lower.strip_prefix("0o") {
        (8, d.to_string())
    } else if let Some(d) = lower.strip_prefix("0b") {
        (2, d.to_string())
    } else if lower.len() > 1 && lower.starts_with('0') {
        (8, lower[1..].to_string())
    } else {
        (10, lower.clone())
    };
    if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
        return None;
    }
    let v = u128::from_str_radix(&digits, radix).ok()?;
    let v = v as i128;
    let v = if neg { -v } else { v };
    // Fits an int64 or (positive) uint64.
    if v >= i64::MIN as i128 && v <= u64::MAX as i128 {
        Some(v)
    } else {
        None
    }
}

/// strconv.ParseFloat for the syntaxes the resolver reaches.
fn go_parse_float(s: &str) -> Option<f64> {
    let t = s.strip_prefix('+').unwrap_or(s);
    let lower = t.to_ascii_lowercase();
    let body = lower.strip_prefix('-').unwrap_or(&lower);
    if matches!(body, "inf" | "infinity" | "nan") {
        return Some(0.0);
    }
    if body.starts_with("0x") {
        return None;
    }
    if body.is_empty()
        || !body
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | '+' | '-'))
    {
        return None;
    }
    t.parse::<f64>().ok()
}

/// `^[-+]?(\.[0-9]+|[0-9]+(\.[0-9]*)?)([eE][-+]?[0-9]+)?$`
fn yaml_style_float(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
        i += 1;
    }
    let digits = |i: &mut usize| {
        let st = *i;
        while *i < b.len() && b[*i].is_ascii_digit() {
            *i += 1;
        }
        *i - st
    };
    if i < b.len() && b[i] == b'.' {
        i += 1;
        if digits(&mut i) == 0 {
            return false;
        }
    } else {
        if digits(&mut i) == 0 {
            return false;
        }
        if i < b.len() && b[i] == b'.' {
            i += 1;
            digits(&mut i);
        }
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
            i += 1;
        }
        if digits(&mut i) == 0 {
            return false;
        }
    }
    i == b.len()
}

/// `^[-+]?[0-9][0-9_]*(?::[0-5]?[0-9])+(?:\.[0-9_]*)?$`
fn is_base60_float(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
        i += 1;
    }
    if i >= b.len() || !b[i].is_ascii_digit() {
        return false;
    }
    i += 1;
    while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'_') {
        i += 1;
    }
    let mut groups = 0;
    while i < b.len() && b[i] == b':' {
        i += 1;
        let st = i;
        while i < b.len() && b[i].is_ascii_digit() && i - st < 2 {
            i += 1;
        }
        let g = &b[st..i];
        let ok = match g.len() {
            1 => true,
            2 => g[0] <= b'5',
            _ => false,
        };
        if !ok {
            return false;
        }
        groups += 1;
    }
    if groups == 0 {
        return false;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'_') {
            i += 1;
        }
    }
    i == b.len()
}

/// parseTimestamp: `YYYY-` then one of yaml.v2's time layouts.
fn is_timestamp(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() < 5 || !b[..4].iter().all(u8::is_ascii_digit) || b[4] != b'-' {
        return false;
    }
    let rest = &s[5..];
    // Date: M-D with 1-2 digit fields.
    let num = |t: &str, max: usize| -> Option<usize> {
        let n = t.bytes().take_while(u8::is_ascii_digit).count();
        (1..=max).contains(&n).then_some(n)
    };
    let Some(m) = num(rest, 2) else {
        return false;
    };
    let mut r = &rest[m..];
    let Some(r2) = r.strip_prefix('-') else {
        return false;
    };
    r = r2;
    let Some(d) = num(r, 2) else {
        return false;
    };
    let month: u32 = rest[..m].parse().unwrap_or(0);
    let day: u32 = r[..d].parse().unwrap_or(0);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return false;
    }
    r = &r[d..];
    if r.is_empty() {
        return true;
    }
    let (sep, zoned) = match r.as_bytes()[0] {
        b'T' | b't' => (1, true),
        b' ' => (1, false),
        _ => return false,
    };
    r = &r[sep..];
    // h:m:s with 1-2 digit fields.
    let mut parts = Vec::new();
    for k in 0..3 {
        let Some(n) = num(r, 2) else {
            return false;
        };
        parts.push(r[..n].parse::<u32>().unwrap_or(99));
        r = &r[n..];
        if k < 2 {
            let Some(r2) = r.strip_prefix(':') else {
                return false;
            };
            r = r2;
        }
    }
    if parts[0] > 23 || parts[1] > 59 || parts[2] > 59 {
        return false;
    }
    if let Some(r2) = r.strip_prefix('.') {
        let n = r2.bytes().take_while(u8::is_ascii_digit).count();
        if n == 0 || n > 9 {
            return false;
        }
        r = &r2[n..];
    }
    if !zoned {
        return r.is_empty();
    }
    if r == "Z" {
        return true;
    }
    let b = r.as_bytes();
    b.len() == 6
        && (b[0] == b'+' || b[0] == b'-')
        && b[1].is_ascii_digit()
        && b[2].is_ascii_digit()
        && b[3] == b':'
        && b[4].is_ascii_digit()
        && b[5].is_ascii_digit()
}

/// strconv.FormatFloat(f, 'g', -1, 64) with yaml.v2's inf/nan names.
fn go_float(f: f64) -> String {
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
    // Shortest round-trip digits and decimal exponent.
    let e = format!("{f:e}");
    let (mant, exp) = e.split_once('e').unwrap_or((&e, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let neg = mant.starts_with('-');
    let digits: String = mant.chars().filter(|c| c.is_ascii_digit()).collect();
    let sign = if neg { "-" } else { "" };
    // 'g' with shortest precision switches to %e at exponent 6.
    if !(-4..6).contains(&exp) {
        let rest = &digits[1..];
        let m = if rest.is_empty() {
            digits[..1].to_string()
        } else {
            format!("{}.{rest}", &digits[..1])
        };
        let es = if exp < 0 { '-' } else { '+' };
        return format!("{sign}{m}e{es}{:02}", exp.abs());
    }
    let point = exp + 1;
    if point <= 0 {
        format!("{sign}0.{}{digits}", "0".repeat((-point) as usize))
    } else if point as usize >= digits.len() {
        format!(
            "{sign}{digits}{}",
            "0".repeat(point as usize - digits.len())
        )
    } else {
        let (a, b) = digits.split_at(point as usize);
        format!("{sign}{a}.{b}")
    }
}

/// yaml.v2 keyList.Less for string keys: letters before non-letters,
/// digit runs compared as numbers.
fn key_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    if key_less(a, b) {
        Ordering::Less
    } else if key_less(b, a) {
        Ordering::Greater
    } else {
        Ordering::Equal
    }
}

fn key_less(a: &str, b: &str) -> bool {
    let ar: Vec<char> = a.chars().collect();
    let br: Vec<char> = b.chars().collect();
    let mut i = 0;
    while i < ar.len() && i < br.len() {
        if ar[i] == br[i] {
            i += 1;
            continue;
        }
        let al = ar[i].is_alphabetic();
        let bl = br[i].is_alphabetic();
        if al && bl {
            return ar[i] < br[i];
        }
        if al || bl {
            return bl;
        }
        let (mut an, mut bn): (i64, i64) = (0, 0);
        if ar[i] == '0' || br[i] == '0' {
            let mut j = i as i64 - 1;
            while j >= 0 && ar[j as usize].is_ascii_digit() {
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
            return an < bn;
        }
        if ai != bi {
            return ai < bi;
        }
        return ar[i] < br[i];
    }
    ar.len() < br.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn k9s_pod_yaml() {
        let v = json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {
                "annotations": {"scaleops.sh/admission": "true", "note": "a: b", "n": "0755", "e": ""},
                "creationTimestamp": "2026-10-08T22:08:04Z",
                "labels": {"app": "web"},
                "name": "web-0"
            },
            "spec": {
                "containers": [{"args": ["--v=2", "-"], "name": "c", "ports": [{"containerPort": 80}]}],
                "nodeSelector": {},
                "tolerations": []
            },
            "status": {"phase": "Running", "ratio": 0.5, "big": 1e21}
        });
        let want = "\
apiVersion: v1
kind: Pod
metadata:
  annotations:
    e: \"\"
    \"n\": \"0755\"
    note: 'a: b'
    scaleops.sh/admission: \"true\"
  creationTimestamp: \"2026-10-08T22:08:04Z\"
  labels:
    app: web
  name: web-0
spec:
  containers:
  - args:
    - --v=2
    - '-'
    name: c
    ports:
    - containerPort: 80
  nodeSelector: {}
  tolerations: []
status:
  big: 1e+21
  phase: Running
  ratio: 0.5
";
        assert_eq!(to_yaml(&v), want);
    }

    #[test]
    fn literal_and_quoted_multiline() {
        let v = json!({"a": "x\ny\n", "b": "x\ny", "c": "x\n\n", "d": " lead\nz", "e": "tab\there", "f": "trail \nz"});
        let want = "\
a: |
  x
  y
b: |-
  x
  y
c: |+
  x

d: |2-
   lead
  z
e: \"tab\\there\"
f: \"trail \\nz\"
";
        assert_eq!(to_yaml(&v), want);
    }

    #[test]
    fn long_strings_fold_at_80() {
        let words = vec!["word"; 30].join(" ");
        let v = json!({"metadata": {"description": words}});
        let out = to_yaml(&v);
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[1].starts_with("  description: word word"));
        assert!(lines[1].len() > 80 && lines[1].len() < 90, "{}", lines[1]);
        assert!(lines[2].starts_with("    word"), "{out}");
    }

    #[test]
    fn scalar_resolution() {
        for s in [
            "true",
            "yes",
            "Off",
            "~",
            "null",
            "12",
            "0x1F",
            "1e3",
            ".5",
            "1_000",
            "2026-10-08",
            "1:20",
            "-0b11",
        ] {
            assert_eq!(string_style(s), Style::Double, "{s}");
        }
        for s in [
            "web",
            "v1.25.16",
            "10.20.0.1",
            "1.2.3",
            "2026-13-01",
            "0x",
            "-",
            "1e",
        ] {
            assert_eq!(string_style(s), Style::Plain, "{s}");
        }
    }

    #[test]
    fn keys_sort_like_yaml_v2() {
        let mut k = vec!["b", "a10", "a2", "A", "_x", "a"];
        k.sort_by(|a, b| key_cmp(a, b));
        // A non-letter sorts before a letter at the first difference.
        assert_eq!(k, vec!["_x", "A", "a", "a2", "a10", "b"]);
    }

    #[test]
    fn floats_format_like_go() {
        assert_eq!(go_float(0.5), "0.5");
        assert_eq!(go_float(1e21), "1e+21");
        assert_eq!(go_float(123456.0), "123456");
        assert_eq!(go_float(1234567.0), "1.234567e+06");
        assert_eq!(go_float(0.0001), "0.0001");
        assert_eq!(go_float(0.00001), "1e-05");
        assert_eq!(go_float(-2.5), "-2.5");
    }
}
