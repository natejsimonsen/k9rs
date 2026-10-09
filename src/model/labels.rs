//! Kubernetes label selector parsing (apimachinery `labels.Parse`), used by the
//! `-l` filter and by label arguments of `:` commands.
//!
//! The lexer/parser mirrors the upstream grammar, error texts for syntax
//! errors and the canonical `String()` form (requirements sorted by key,
//! set values sorted).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// Selector operator (apimachinery `selection.Operator`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operator {
    Equals,
    DoubleEquals,
    NotEquals,
    In,
    NotIn,
    Exists,
    DoesNotExist,
    GreaterThan,
    LessThan,
}

/// A single selector requirement such as `app=fred` or `env in (a,b)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Requirement {
    pub key: String,
    pub op: Operator,
    /// Sorted, de-duplicated values.
    pub values: Vec<String>,
}

impl Requirement {
    /// Does the requirement hold for `labels` (labels.Requirement.Matches)?
    pub fn matches(&self, labels: &BTreeMap<String, String>) -> bool {
        let val = labels.get(&self.key);
        match self.op {
            Operator::In | Operator::Equals | Operator::DoubleEquals => {
                val.is_some_and(|v| self.values.iter().any(|x| x == v))
            }
            Operator::NotIn | Operator::NotEquals => {
                val.is_none_or(|v| !self.values.iter().any(|x| x == v))
            }
            Operator::Exists => val.is_some(),
            Operator::DoesNotExist => val.is_none(),
            Operator::GreaterThan | Operator::LessThan => {
                let Some(lv) = val.and_then(|v| v.parse::<i64>().ok()) else {
                    return false;
                };
                let Some(rv) = self.values.first().and_then(|v| v.parse::<i64>().ok()) else {
                    return false;
                };
                if self.op == Operator::GreaterThan {
                    lv > rv
                } else {
                    lv < rv
                }
            }
        }
    }
}

impl fmt::Display for Requirement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.op == Operator::DoesNotExist {
            f.write_str("!")?;
        }
        f.write_str(&self.key)?;
        let op = match self.op {
            Operator::Equals => "=",
            Operator::DoubleEquals => "==",
            Operator::NotEquals => "!=",
            Operator::In => " in ",
            Operator::NotIn => " notin ",
            Operator::GreaterThan => ">",
            Operator::LessThan => "<",
            Operator::Exists | Operator::DoesNotExist => return Ok(()),
        };
        f.write_str(op)?;
        let set = matches!(self.op, Operator::In | Operator::NotIn);
        if set {
            f.write_str("(")?;
        }
        f.write_str(&self.values.join(","))?;
        if set {
            f.write_str(")")?;
        }
        Ok(())
    }
}

/// A parsed label selector. An empty selector matches everything.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selector(pub Vec<Requirement>);

impl Selector {
    /// True when the selector has no requirements (labels.Everything).
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    /// Does the selector match `labels`?
    pub fn matches(&self, labels: &BTreeMap<String, String>) -> bool {
        self.0.iter().all(|r| r.matches(labels))
    }
}

impl fmt::Display for Selector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, r) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(",")?;
            }
            write!(f, "{r}")?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tok {
    Error,
    End,
    ClosedPar,
    Comma,
    DoesNotExist,
    DoubleEquals,
    Equals,
    GreaterThan,
    Identifier,
    In,
    LessThan,
    NotEquals,
    NotIn,
    OpenPar,
}

fn string_to_token(s: &str) -> Option<Tok> {
    Some(match s {
        ")" => Tok::ClosedPar,
        "," => Tok::Comma,
        "!" => Tok::DoesNotExist,
        "==" => Tok::DoubleEquals,
        "=" => Tok::Equals,
        ">" => Tok::GreaterThan,
        "in" => Tok::In,
        "<" => Tok::LessThan,
        "!=" => Tok::NotEquals,
        "notin" => Tok::NotIn,
        "(" => Tok::OpenPar,
        _ => return None,
    })
}

fn is_whitespace(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\r' | b'\n')
}

fn is_special(c: u8) -> bool {
    matches!(c, b'=' | b'!' | b'(' | b')' | b',' | b'>' | b'<')
}

struct Lexer<'a> {
    s: &'a [u8],
    pos: usize,
}

impl Lexer<'_> {
    fn read(&mut self) -> u8 {
        if self.pos < self.s.len() {
            let c = self.s[self.pos];
            self.pos += 1;
            c
        } else {
            self.pos += 1;
            0
        }
    }
    fn unread(&mut self) {
        self.pos -= 1;
    }
    fn lex(&mut self) -> (Tok, String) {
        let mut ch = self.read();
        while is_whitespace(ch) {
            ch = self.read();
        }
        if ch == 0 {
            return (Tok::End, String::new());
        }
        self.unread();
        if is_special(ch) {
            self.scan_special()
        } else {
            self.scan_id()
        }
    }
    fn scan_id(&mut self) -> (Tok, String) {
        let mut buf = Vec::new();
        loop {
            let ch = self.read();
            if ch == 0 {
                break;
            }
            if is_special(ch) || is_whitespace(ch) {
                self.unread();
                break;
            }
            buf.push(ch);
        }
        let s = String::from_utf8_lossy(&buf).into_owned();
        match string_to_token(&s) {
            Some(t) => (t, s),
            None => (Tok::Identifier, s),
        }
    }
    fn scan_special(&mut self) -> (Tok, String) {
        let mut last: Option<(Tok, String)> = None;
        let mut buf = Vec::new();
        loop {
            let ch = self.read();
            if ch == 0 {
                break;
            }
            if is_special(ch) {
                buf.push(ch);
                let s = String::from_utf8_lossy(&buf).into_owned();
                if let Some(t) = string_to_token(&s) {
                    last = Some((t, s));
                } else if last.is_some() {
                    self.unread();
                    break;
                }
            } else {
                self.unread();
                break;
            }
        }
        match last {
            Some(t) => t,
            None => (
                Tok::Error,
                format!(
                    "error expected: keyword found '{}'",
                    String::from_utf8_lossy(&buf)
                ),
            ),
        }
    }
}

struct Parser {
    items: Vec<(Tok, String)>,
    pos: usize,
}

#[derive(PartialEq)]
enum Ctx {
    KeyAndOperator,
    Values,
}

impl Parser {
    fn lookahead(&self, ctx: Ctx) -> (Tok, String) {
        let (mut t, l) = self.items[self.pos.min(self.items.len() - 1)].clone();
        if ctx == Ctx::Values && matches!(t, Tok::In | Tok::NotIn) {
            t = Tok::Identifier;
        }
        (t, l)
    }
    fn consume(&mut self, ctx: Ctx) -> (Tok, String) {
        let r = self.lookahead(ctx);
        if self.pos < self.items.len() - 1 {
            self.pos += 1;
        }
        r
    }

    fn parse(&mut self) -> Result<Vec<Requirement>, String> {
        let mut reqs = Vec::new();
        loop {
            let (tok, lit) = self.lookahead(Ctx::Values);
            match tok {
                Tok::Identifier | Tok::DoesNotExist => {
                    let r = self
                        .parse_requirement()
                        .map_err(|e| format!("unable to parse requirement: {e}"))?;
                    reqs.push(r);
                    let (t, l) = self.consume(Ctx::Values);
                    match t {
                        Tok::End => return Ok(reqs),
                        Tok::Comma => {
                            let (t2, l2) = self.lookahead(Ctx::Values);
                            if t2 != Tok::Identifier && t2 != Tok::DoesNotExist {
                                return Err(format!(
                                    "found '{l2}', expected: identifier after ','"
                                ));
                            }
                        }
                        _ => return Err(format!("found '{l}', expected: ',' or 'end of string'")),
                    }
                }
                Tok::End => return Ok(reqs),
                _ => {
                    return Err(format!(
                        "found '{lit}', expected: !, identifier, or 'end of string'"
                    ))
                }
            }
        }
    }

    fn parse_requirement(&mut self) -> Result<Requirement, String> {
        let (key, op) = self.parse_key_and_infer_operator()?;
        if let Some(op) = op {
            return new_requirement(key, op, BTreeSet::new());
        }
        let op = self.parse_operator()?;
        let values = match op {
            Operator::In | Operator::NotIn => self.parse_values()?,
            _ => self.parse_exact_value()?,
        };
        new_requirement(key, op, values)
    }

    fn parse_key_and_infer_operator(&mut self) -> Result<(String, Option<Operator>), String> {
        let mut op = None;
        let (mut tok, mut lit) = self.consume(Ctx::Values);
        if tok == Tok::DoesNotExist {
            op = Some(Operator::DoesNotExist);
            (tok, lit) = self.consume(Ctx::Values);
        }
        if tok != Tok::Identifier {
            return Err(format!("found '{lit}', expected: identifier"));
        }
        validate_label_key(&lit)?;
        let (t, _) = self.lookahead(Ctx::Values);
        if (t == Tok::End || t == Tok::Comma) && op.is_none() {
            op = Some(Operator::Exists);
        }
        Ok((lit, op))
    }

    fn parse_operator(&mut self) -> Result<Operator, String> {
        let (tok, lit) = self.consume(Ctx::KeyAndOperator);
        Ok(match tok {
            Tok::In => Operator::In,
            Tok::Equals => Operator::Equals,
            Tok::DoubleEquals => Operator::DoubleEquals,
            Tok::GreaterThan => Operator::GreaterThan,
            Tok::LessThan => Operator::LessThan,
            Tok::NotIn => Operator::NotIn,
            Tok::NotEquals => Operator::NotEquals,
            _ => {
                return Err(format!(
                    "found '{lit}', expected: in, notin, =, ==, !=, gt, lt"
                ))
            }
        })
    }

    fn parse_values(&mut self) -> Result<BTreeSet<String>, String> {
        let (tok, lit) = self.consume(Ctx::Values);
        if tok != Tok::OpenPar {
            return Err(format!("found '{lit}' expected: '('"));
        }
        let (tok, lit) = self.lookahead(Ctx::Values);
        match tok {
            Tok::Identifier | Tok::Comma => {
                let s = self.parse_identifiers_list()?;
                let (t, _) = self.consume(Ctx::Values);
                if t != Tok::ClosedPar {
                    return Err(format!("found '{lit}', expected: ')'"));
                }
                Ok(s)
            }
            Tok::ClosedPar => {
                self.consume(Ctx::Values);
                Ok(BTreeSet::from([String::new()]))
            }
            _ => Err(format!("found '{lit}', expected: ',', ')' or identifier")),
        }
    }

    fn parse_identifiers_list(&mut self) -> Result<BTreeSet<String>, String> {
        let mut s = BTreeSet::new();
        loop {
            let (tok, lit) = self.consume(Ctx::Values);
            match tok {
                Tok::Identifier => {
                    s.insert(lit);
                    let (t2, l2) = self.lookahead(Ctx::Values);
                    match t2 {
                        Tok::Comma => continue,
                        Tok::ClosedPar => return Ok(s),
                        _ => return Err(format!("found '{l2}', expected: ',' or ')'")),
                    }
                }
                Tok::Comma => {
                    if s.is_empty() {
                        s.insert(String::new());
                    }
                    let (t2, _) = self.lookahead(Ctx::Values);
                    if t2 == Tok::ClosedPar {
                        s.insert(String::new());
                        return Ok(s);
                    }
                    if t2 == Tok::Comma {
                        self.consume(Ctx::Values);
                        s.insert(String::new());
                    }
                }
                _ => return Err(format!("found '{lit}', expected: ',', or identifier")),
            }
        }
    }

    fn parse_exact_value(&mut self) -> Result<BTreeSet<String>, String> {
        let (tok, _) = self.lookahead(Ctx::Values);
        if tok == Tok::End || tok == Tok::Comma {
            return Ok(BTreeSet::from([String::new()]));
        }
        let (tok, lit) = self.consume(Ctx::Values);
        if tok == Tok::Identifier {
            return Ok(BTreeSet::from([lit]));
        }
        Err(format!("found '{lit}', expected: identifier"))
    }
}

const NAME_FMT_MSG: &str = "name part must consist of alphanumeric characters, '-', '_' or '.', and must start and end with an alphanumeric character (e.g. 'MyName',  or 'my.name',  or '123-abc', regex used for validation is '([A-Za-z0-9][-A-Za-z0-9_.]*)?[A-Za-z0-9]')";
const VALUE_FMT_MSG: &str = "a valid label must be an empty string or consist of alphanumeric characters, '-', '_' or '.', and must start and end with an alphanumeric character (e.g. 'MyValue',  or 'my_value',  or '12345', regex used for validation is '(([A-Za-z0-9][-A-Za-z0-9_.]*)?[A-Za-z0-9])?')";

fn is_name_part(s: &str) -> bool {
    let b = s.as_bytes();
    if b.is_empty() || b.len() > 63 {
        return false;
    }
    let alnum = |c: u8| c.is_ascii_alphanumeric();
    alnum(b[0])
        && alnum(b[b.len() - 1])
        && b.iter()
            .all(|&c| alnum(c) || c == b'-' || c == b'_' || c == b'.')
}

fn is_dns1123_subdomain(s: &str) -> bool {
    if s.is_empty() || s.len() > 253 {
        return false;
    }
    s.split('.').all(|l| {
        let b = l.as_bytes();
        !b.is_empty()
            && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
            && (b[b.len() - 1].is_ascii_lowercase() || b[b.len() - 1].is_ascii_digit())
            && b.iter()
                .all(|&c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
    })
}

fn validate_label_key(k: &str) -> Result<(), String> {
    let (prefix, name) = match k.split_once('/') {
        Some((p, n)) => (Some(p), n),
        None => (None, k),
    };
    if let Some(p) = prefix {
        if !is_dns1123_subdomain(p) {
            return Err(format!(
                "key: Invalid value: {k:?}: prefix part a lowercase RFC 1123 subdomain must consist of lower case alphanumeric characters, '-' or '.', and must start and end with an alphanumeric character"
            ));
        }
    }
    if name.is_empty() {
        return Err(format!(
            "key: Invalid value: {k:?}: name part must be non-empty"
        ));
    }
    if name.len() > 63 {
        return Err(format!(
            "key: Invalid value: {k:?}: name part must be no more than 63 characters"
        ));
    }
    if name.contains('/') || !is_name_part(name) {
        return Err(format!("key: Invalid value: {k:?}: {NAME_FMT_MSG}"));
    }
    Ok(())
}

fn validate_label_value(v: &str) -> Result<(), String> {
    if v.is_empty() {
        return Ok(());
    }
    if v.len() > 63 {
        return Err(format!(
            "values[0]: Invalid value: {v:?}: must be no more than 63 characters"
        ));
    }
    if !is_name_part(v) {
        return Err(format!("values[0]: Invalid value: {v:?}: {VALUE_FMT_MSG}"));
    }
    Ok(())
}

fn new_requirement(
    key: String,
    op: Operator,
    values: BTreeSet<String>,
) -> Result<Requirement, String> {
    validate_label_key(&key)?;
    match op {
        Operator::In | Operator::NotIn => {
            if values.is_empty() {
                return Err(
                    "values: Invalid value: []string{}: for 'in', 'notin' operators, values set can't be empty"
                        .into(),
                );
            }
        }
        Operator::Equals | Operator::DoubleEquals | Operator::NotEquals => {
            if values.len() != 1 {
                return Err(
                    "values: Invalid value: exact-match compatibility requires one single value"
                        .into(),
                );
            }
        }
        Operator::Exists | Operator::DoesNotExist => {
            if !values.is_empty() {
                return Err(
                    "values: Invalid value: values set must be empty for exists and does not exist"
                        .into(),
                );
            }
        }
        Operator::GreaterThan | Operator::LessThan => {
            if values.len() != 1 {
                return Err(
                    "values: Invalid value: for 'Gt', 'Lt' operators, exactly one value is required"
                        .into(),
                );
            }
            for v in &values {
                if v.parse::<i64>().is_err() {
                    return Err(format!(
                        "values[0]: Invalid value: {v:?}: for 'Gt', 'Lt' operators, the value must be an integer"
                    ));
                }
            }
        }
    }
    for v in &values {
        validate_label_value(v)?;
    }
    Ok(Requirement {
        key,
        op,
        values: values.into_iter().collect(),
    })
}

/// Parse a label selector (labels.Parse). The empty string yields an empty
/// selector (matches everything).
pub fn parse_selector(s: &str) -> Result<Selector, String> {
    let mut lx = Lexer {
        s: s.as_bytes(),
        pos: 0,
    };
    let mut items = Vec::new();
    loop {
        let (t, l) = lx.lex();
        items.push((t, l));
        if t == Tok::End {
            break;
        }
    }
    let mut p = Parser { items, pos: 0 };
    let mut reqs = p.parse()?;
    reqs.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(Selector(reqs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_print() {
        let cases: &[(&str, Result<&str, &str>)] = &[
            ("", Ok("")),
            ("fred=blee", Ok("fred=blee")),
            ("fred=blee,zorg=duh", Ok("fred=blee,zorg=duh")),
            ("zorg=duh,fred=blee", Ok("fred=blee,zorg=duh")),
            (
                "fred in (blee,zorg),blee notin (zorg)",
                Ok("blee notin (zorg),fred in (blee,zorg)"),
            ),
            ("a in (c,b,b)", Ok("a in (b,c)")),
            ("app", Ok("app")),
            ("!app", Ok("!app")),
            ("a==b", Ok("a==b")),
            ("a!=b", Ok("a!=b")),
            ("a>1", Ok("a>1")),
            ("a<1", Ok("a<1")),
            ("a=", Ok("a=")),
            ("  a = b  ", Ok("a=b")),
            ("k8s.io/app=x", Ok("k8s.io/app=x")),
            (
                "=blee",
                Err("found '=', expected: !, identifier, or 'end of string'"),
            ),
            (
                "a=b,",
                Err("found '', expected: identifier after ','"),
            ),
            (
                "a in b",
                Err("unable to parse requirement: found 'b' expected: '('"),
            ),
            (
                "a b",
                Err("unable to parse requirement: found 'b', expected: in, notin, =, ==, !=, gt, lt"),
            ),
        ];
        for (s, e) in cases {
            let got = parse_selector(s).map(|sel| sel.to_string());
            assert_eq!(got, e.map(String::from).map_err(String::from), "{s}");
        }
    }

    #[test]
    fn invalid_values_and_keys() {
        assert!(parse_selector("a>x").is_err());
        assert!(parse_selector("a=-b").is_err());
        assert!(parse_selector("-a=b").is_err());
        assert!(parse_selector("A/b=c").is_err());
    }

    #[test]
    fn matches() {
        let mut l = BTreeMap::new();
        l.insert("app".to_string(), "fred".to_string());
        l.insert("n".to_string(), "5".to_string());
        let ok = |s: &str| parse_selector(s).unwrap().matches(&l);
        assert!(ok(""));
        assert!(ok("app=fred"));
        assert!(!ok("app=blee"));
        assert!(ok("app in (fred,blee)"));
        assert!(!ok("app notin (fred)"));
        assert!(ok("zorg notin (fred)"));
        assert!(ok("app!=blee"));
        assert!(ok("app"));
        assert!(!ok("!app"));
        assert!(ok("!zorg"));
        assert!(ok("n>4"));
        assert!(!ok("n<4"));
        assert!(ok("app=fred,n>1"));
    }
}
