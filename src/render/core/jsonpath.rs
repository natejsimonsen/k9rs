//! A JSONPath evaluator compatible with the subset of client-go
//! `util/jsonpath` (with `AllowMissingKeys(true)`) that k9s custom columns use:
//! `{.a.b}`, `..name` (recursive), `.*` / `[*]`, `[n]`, `[-n]`,
//! `[start:end:step]`, `[0,2]`, `['a','b']`, `[?(@.x == 'y')]` filters
//! (`==`, `!=`, `<`, `>`, `<=`, `>=`, or bare existence), `$` / `@` roots,
//! `\.` escapes in field names, literal text between `{...}` actions and
//! `{range EXPR}...{end}` blocks.

use serde_json::Value;

/// A parsed template: literal text and `{...}` actions.
#[derive(Clone, Debug, PartialEq)]
pub struct JsonPath {
    nodes: Vec<Node>,
}

#[derive(Clone, Debug, PartialEq)]
enum Node {
    Text(String),
    Expr(Vec<Seg>),
    /// `{range EXPR}` body `{end}`: the body runs once per result of EXPR.
    Range(Vec<Seg>, Vec<Node>),
}

#[derive(Clone, Debug, PartialEq)]
enum Seg {
    Field(String),
    Recursive(String),
    Wildcard,
    RecursiveWildcard,
    Indexes(Vec<i64>),
    Slice(Option<i64>, Option<i64>, Option<i64>),
    Names(Vec<String>),
    Filter(Filter),
}

#[derive(Clone, Debug, PartialEq)]
struct Filter {
    left: Vec<Seg>,
    op: Option<(String, Literal)>,
}

#[derive(Clone, Debug, PartialEq)]
enum Literal {
    Str(String),
    Num(f64),
    Bool(bool),
    Path(Vec<Seg>),
}

impl JsonPath {
    /// Parse a template such as `{.metadata.name}`.
    pub fn parse(s: &str) -> Result<JsonPath, String> {
        // Open ranges: (range expr, nodes before it).
        let mut stack: Vec<(Vec<Seg>, Vec<Node>)> = Vec::new();
        let mut nodes = Vec::new();
        let mut rest = s;
        while !rest.is_empty() {
            match rest.find('{') {
                None => {
                    nodes.push(Node::Text(rest.to_string()));
                    break;
                }
                Some(i) => {
                    if i > 0 {
                        nodes.push(Node::Text(rest[..i].to_string()));
                    }
                    let after = &rest[i + 1..];
                    let end =
                        find_close(after).ok_or_else(|| format!("unclosed action in {s:?}"))?;
                    let inner = after[..end].trim();
                    if let Some(r) = inner.strip_prefix("range ") {
                        stack.push((parse_expr(r.trim())?, std::mem::take(&mut nodes)));
                    } else if inner == "end" {
                        let (segs, outer) = stack
                            .pop()
                            .ok_or_else(|| "not in range, nothing to end".to_string())?;
                        let body = std::mem::replace(&mut nodes, outer);
                        nodes.push(Node::Range(segs, body));
                    } else {
                        for expr in split_top(inner, ' ') {
                            if expr.is_empty() {
                                continue;
                            }
                            nodes.push(Node::Expr(parse_expr(expr)?));
                        }
                    }
                    rest = &after[end + 1..];
                }
            }
        }
        // An unclosed range runs to the end of the template.
        while let Some((segs, outer)) = stack.pop() {
            let body = std::mem::replace(&mut nodes, outer);
            nodes.push(Node::Range(segs, body));
        }
        Ok(JsonPath { nodes })
    }

    /// Evaluate against `root`: one result list per template node
    /// (client-go `FindResults`). Missing keys yield empty lists.
    pub fn find_results(&self, root: &Value) -> Vec<Vec<Value>> {
        let mut out = Vec::new();
        run(&self.nodes, root, root, &mut out);
        out
    }
}

/// client-go FindResults: one list per node; a range appends its body's
/// lists once per value.
fn run(nodes: &[Node], cur: &Value, root: &Value, out: &mut Vec<Vec<Value>>) {
    for n in nodes {
        match n {
            Node::Text(t) => out.push(vec![Value::String(t.clone())]),
            Node::Expr(segs) => out.push(eval(segs, cur, root).into_iter().cloned().collect()),
            Node::Range(segs, body) => {
                for v in eval(segs, cur, root) {
                    run(body, v, root, out);
                }
            }
        }
    }
}

fn find_close(s: &str) -> Option<usize> {
    let mut depth = 0;
    let mut quote: Option<char> = None;
    for (i, c) in s.char_indices() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                '\'' | '"' => quote = Some(c),
                '{' => depth += 1,
                '}' => {
                    if depth == 0 {
                        return Some(i);
                    }
                    depth -= 1;
                }
                _ => {}
            },
        }
    }
    None
}

/// Split on `sep` outside of brackets/parens/quotes.
fn split_top(s: &str, sep: char) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut start) = (0i32, 0usize);
    let mut quote: Option<char> = None;
    for (i, c) in s.char_indices() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' => quote = Some(c),
            '[' | '(' => depth += 1,
            ']' | ')' => depth -= 1,
            c if c == sep && depth == 0 => {
                out.push(&s[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

fn is_terminator(c: char) -> bool {
    matches!(
        c,
        '.' | '[' | ']' | '{' | '}' | '(' | ')' | ',' | ' ' | '=' | '!' | '<' | '>' | '|'
    )
}

fn parse_expr(s: &str) -> Result<Vec<Seg>, String> {
    if s == "range" || s == "end" {
        return Err(format!("invalid jsonpath action {s:?}"));
    }
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut segs = Vec::new();
    if i < chars.len() && (chars[i] == '$' || chars[i] == '@') {
        i += 1;
    }
    while i < chars.len() {
        match chars[i] {
            '.' => {
                if i + 1 < chars.len() && chars[i + 1] == '.' {
                    i += 2;
                    if i < chars.len() && chars[i] == '*' {
                        segs.push(Seg::RecursiveWildcard);
                        i += 1;
                        continue;
                    }
                    let (name, n) = read_ident(&chars[i..]);
                    i += n;
                    if name.is_empty() {
                        // `..[...]`: recurse then apply the bracket.
                        segs.push(Seg::RecursiveWildcard);
                    } else {
                        segs.push(Seg::Recursive(name));
                    }
                    continue;
                }
                i += 1;
                if i < chars.len() && chars[i] == '*' {
                    segs.push(Seg::Wildcard);
                    i += 1;
                    continue;
                }
                let (name, n) = read_ident(&chars[i..]);
                i += n;
                if !name.is_empty() {
                    segs.push(Seg::Field(name));
                }
            }
            '[' => {
                let rest: String = chars[i + 1..].iter().collect();
                let end =
                    find_bracket_close(&rest).ok_or_else(|| format!("unclosed [ in {s:?}"))?;
                let inner = &rest[..end];
                segs.push(parse_bracket(inner)?);
                i += 1 + inner.chars().count() + 1;
            }
            c if c.is_whitespace() => i += 1,
            _ => {
                // Bare field name (relaxed paths always start with '.').
                let (name, n) = read_ident(&chars[i..]);
                if n == 0 {
                    return Err(format!("unrecognized character {:?} in {s:?}", chars[i]));
                }
                i += n;
                segs.push(Seg::Field(name));
            }
        }
    }
    Ok(segs)
}

fn read_ident(chars: &[char]) -> (String, usize) {
    let mut name = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() {
            name.push(chars[i + 1]);
            i += 2;
            continue;
        }
        if is_terminator(c) {
            break;
        }
        name.push(c);
        i += 1;
    }
    (name, i)
}

fn find_bracket_close(s: &str) -> Option<usize> {
    let mut depth = 0;
    let mut quote: Option<char> = None;
    for (i, c) in s.char_indices() {
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '\'' | '"' => quote = Some(c),
            '[' => depth += 1,
            ']' => {
                if depth == 0 {
                    return Some(i);
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    None
}

fn unquote(s: &str) -> Option<String> {
    let s = s.trim();
    if s.len() >= 2
        && ((s.starts_with('\'') && s.ends_with('\'')) || (s.starts_with('"') && s.ends_with('"')))
    {
        return Some(s[1..s.len() - 1].to_string());
    }
    None
}

fn parse_bracket(inner: &str) -> Result<Seg, String> {
    let t = inner.trim();
    if t == "*" {
        return Ok(Seg::Wildcard);
    }
    if let Some(f) = t.strip_prefix('?') {
        let f = f.trim();
        let f = f
            .strip_prefix('(')
            .and_then(|x| x.strip_suffix(')'))
            .ok_or_else(|| format!("invalid filter {inner:?}"))?;
        return parse_filter(f.trim()).map(Seg::Filter);
    }
    if t.starts_with('\'') || t.starts_with('"') {
        let names = split_top(t, ',')
            .into_iter()
            .map(|p| unquote(p).ok_or_else(|| format!("invalid name {p:?}")))
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(Seg::Names(names));
    }
    if t.contains(':') {
        let parts: Vec<&str> = t.split(':').collect();
        if parts.len() > 3 {
            return Err(format!("invalid slice {inner:?}"));
        }
        let num = |p: &str| -> Result<Option<i64>, String> {
            let p = p.trim();
            if p.is_empty() {
                Ok(None)
            } else {
                p.parse::<i64>()
                    .map(Some)
                    .map_err(|_| format!("invalid slice {inner:?}"))
            }
        };
        return Ok(Seg::Slice(
            num(parts[0])?,
            num(parts.get(1).copied().unwrap_or(""))?,
            num(parts.get(2).copied().unwrap_or(""))?,
        ));
    }
    let idx = t
        .split(',')
        .map(|p| p.trim().parse::<i64>())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| format!("invalid array index {inner:?}"))?;
    Ok(Seg::Indexes(idx))
}

fn parse_filter(f: &str) -> Result<Filter, String> {
    for op in ["==", "!=", "<=", ">=", "<", ">"] {
        if let Some(pos) = find_op(f, op) {
            let left = parse_expr(f[..pos].trim())?;
            let r = f[pos + op.len()..].trim();
            let lit = if let Some(s) = unquote(r) {
                Literal::Str(s)
            } else if r == "true" || r == "false" {
                Literal::Bool(r == "true")
            } else if let Ok(n) = r.parse::<f64>() {
                Literal::Num(n)
            } else if r.starts_with('@') || r.starts_with('$') || r.starts_with('.') {
                Literal::Path(parse_expr(r)?)
            } else {
                return Err(format!("invalid filter literal {r:?}"));
            };
            return Ok(Filter {
                left,
                op: Some((op.to_string(), lit)),
            });
        }
    }
    Ok(Filter {
        left: parse_expr(f)?,
        op: None,
    })
}

fn find_op(s: &str, op: &str) -> Option<usize> {
    let mut quote: Option<char> = None;
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i] as char;
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        if c == '\'' || c == '"' {
            quote = Some(c);
        } else if s[i..].starts_with(op) {
            // Don't split `<=` as `<` etc.
            if (op == "<" || op == ">") && s[i + 1..].starts_with('=') {
                i += 2;
                continue;
            }
            return Some(i);
        }
        i += 1;
    }
    None
}

fn eval<'a>(segs: &[Seg], cur: &'a Value, root: &'a Value) -> Vec<&'a Value> {
    let mut vals = vec![cur];
    for seg in segs {
        let mut next = Vec::new();
        for v in vals {
            apply(seg, v, root, &mut next);
        }
        vals = next;
    }
    vals
}

fn descendants<'a>(v: &'a Value, out: &mut Vec<&'a Value>) {
    out.push(v);
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            for k in keys {
                descendants(&m[k], out);
            }
        }
        Value::Array(a) => a.iter().for_each(|x| descendants(x, out)),
        _ => {}
    }
}

fn apply<'a>(seg: &Seg, v: &'a Value, root: &'a Value, out: &mut Vec<&'a Value>) {
    match seg {
        Seg::Field(name) => {
            if let Some(x) = v.as_object().and_then(|m| m.get(name)) {
                out.push(x);
            }
        }
        Seg::Names(names) => {
            if let Some(m) = v.as_object() {
                out.extend(names.iter().filter_map(|n| m.get(n)));
            }
        }
        Seg::Wildcard => match v {
            Value::Object(m) => {
                let mut keys: Vec<&String> = m.keys().collect();
                keys.sort();
                out.extend(keys.into_iter().map(|k| &m[k]));
            }
            Value::Array(a) => out.extend(a.iter()),
            _ => {}
        },
        Seg::Recursive(name) => {
            let mut all = Vec::new();
            descendants(v, &mut all);
            for d in all {
                if let Some(x) = d.as_object().and_then(|m| m.get(name)) {
                    out.push(x);
                }
            }
        }
        Seg::RecursiveWildcard => {
            let mut all = Vec::new();
            descendants(v, &mut all);
            out.extend(all);
        }
        Seg::Indexes(idx) => {
            if let Some(a) = v.as_array() {
                let len = a.len() as i64;
                for &i in idx {
                    let i = if i < 0 { len + i } else { i };
                    if (0..len).contains(&i) {
                        out.push(&a[i as usize]);
                    }
                }
            }
        }
        Seg::Slice(start, end, step) => {
            if let Some(a) = v.as_array() {
                let len = a.len() as i64;
                let norm = |x: i64| if x < 0 { (len + x).max(0) } else { x.min(len) };
                let s = start.map(norm).unwrap_or(0);
                let e = end.map(norm).unwrap_or(len);
                let st = step.unwrap_or(1).max(1);
                let mut i = s;
                while i < e {
                    out.push(&a[i as usize]);
                    i += st;
                }
            }
        }
        Seg::Filter(f) => {
            if let Some(a) = v.as_array() {
                for item in a {
                    if filter_pass(f, item, root) {
                        out.push(item);
                    }
                }
            }
        }
    }
}

fn filter_pass(f: &Filter, item: &Value, root: &Value) -> bool {
    let lefts = eval(&f.left, item, root);
    let Some((op, lit)) = &f.op else {
        return !lefts.is_empty();
    };
    let Some(left) = lefts.first() else {
        return false;
    };
    let right: Value = match lit {
        Literal::Str(s) => Value::String(s.clone()),
        Literal::Bool(b) => Value::Bool(*b),
        Literal::Num(n) => serde_json::Number::from_f64(*n)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        Literal::Path(p) => match eval(p, item, root).first() {
            Some(v) => (*v).clone(),
            None => return false,
        },
    };
    let ord = match (left, &right) {
        (Value::Number(a), Value::Number(b)) => a
            .as_f64()
            .unwrap_or(0.0)
            .partial_cmp(&b.as_f64().unwrap_or(0.0)),
        (Value::String(a), Value::String(b)) => Some(a.cmp(b)),
        (Value::Bool(a), Value::Bool(b)) => Some(a.cmp(b)),
        _ => None,
    };
    match (op.as_str(), ord) {
        ("==", Some(o)) => o.is_eq(),
        ("!=", Some(o)) => o.is_ne(),
        ("!=", None) => true,
        ("<", Some(o)) => o.is_lt(),
        (">", Some(o)) => o.is_gt(),
        ("<=", Some(o)) => o.is_le(),
        (">=", Some(o)) => o.is_ge(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run(p: &str, v: &Value) -> Vec<Vec<Value>> {
        JsonPath::parse(p).unwrap().find_results(v)
    }

    fn doc() -> Value {
        json!({
            "metadata": {"name": "fred", "labels": {"app": "a1", "k8s.io/name": "n"}},
            "spec": {
                "addresses": [
                    {"type": "InternalIP", "ip": "10.0.0.1"},
                    {"type": "CiliumInternalIP", "ip": "10.0.0.2"},
                    {"type": "ExternalIP", "ip": "1.2.3.4"}
                ],
                "ports": [{"port": 80}, {"port": 443}, {"port": 8080}]
            }
        })
    }

    #[test]
    fn fields_and_missing() {
        let d = doc();
        assert_eq!(run("{.metadata.name}", &d), vec![vec![json!("fred")]]);
        assert_eq!(run("{.metadata.nope}", &d), vec![Vec::<Value>::new()]);
        assert_eq!(run("{$.metadata.name}", &d), vec![vec![json!("fred")]]);
        assert_eq!(
            run(r"{.metadata.labels.k8s\.io/name}", &d),
            vec![vec![json!("n")]]
        );
        assert_eq!(
            run("{.metadata.labels['k8s.io/name']}", &d),
            vec![vec![json!("n")]]
        );
    }

    #[test]
    fn arrays() {
        let d = doc();
        assert_eq!(run("{.spec.ports[1].port}", &d), vec![vec![json!(443)]]);
        assert_eq!(run("{.spec.ports[-1].port}", &d), vec![vec![json!(8080)]]);
        assert_eq!(
            run("{.spec.ports[*].port}", &d),
            vec![vec![json!(80), json!(443), json!(8080)]]
        );
        assert_eq!(
            run("{.spec.ports[0:2].port}", &d),
            vec![vec![json!(80), json!(443)]]
        );
        assert_eq!(
            run("{.spec.ports[0,2].port}", &d),
            vec![vec![json!(80), json!(8080)]]
        );
        assert_eq!(run("{.spec.ports[9].port}", &d), vec![Vec::<Value>::new()]);
        assert_eq!(run("{..port}", &d).len(), 1);
        assert_eq!(run("{..port}", &d)[0].len(), 3);
    }

    #[test]
    fn filters() {
        let d = doc();
        assert_eq!(
            run("{.spec.addresses[?(@.type == 'CiliumInternalIP')].ip}", &d),
            vec![vec![json!("10.0.0.2")]]
        );
        assert_eq!(
            run("{.spec.addresses[?(@.type != 'CiliumInternalIP')].ip}", &d),
            vec![vec![json!("10.0.0.1"), json!("1.2.3.4")]]
        );
        assert_eq!(
            run("{.spec.ports[?(@.port > 100)].port}", &d),
            vec![vec![json!(443), json!(8080)]]
        );
        assert_eq!(
            run("{.spec.ports[?(@.port<=443)].port}", &d),
            vec![vec![json!(80), json!(443)]]
        );
        assert_eq!(run("{.spec.addresses[?(@.ip)].type}", &d)[0].len(), 3);
    }

    #[test]
    fn text_and_multi() {
        let d = doc();
        assert_eq!(
            run("name={.metadata.name}", &d),
            vec![vec![json!("name=")], vec![json!("fred")]]
        );
        assert_eq!(
            run("{.metadata.name} {.metadata.labels.app}", &d),
            vec![vec![json!("fred")], vec![json!(" ")], vec![json!("a1")]]
        );
        assert_eq!(run("{.metadata.*}", &d)[0].len(), 2);
    }

    #[test]
    fn range_end() {
        let d = json!({"spec": {"containers": [
            {"name": "a", "image": "i1", "ports": [{"p": 1}, {"p": 2}]},
            {"name": "b", "image": "i2"}
        ]}});
        let p = JsonPath::parse("{range .spec.containers[*]}{.name}={.image} {end}").unwrap();
        assert_eq!(
            p.find_results(&d),
            vec![
                vec![json!("a")],
                vec![json!("=")],
                vec![json!("i1")],
                vec![json!(" ")],
                vec![json!("b")],
                vec![json!("=")],
                vec![json!("i2")],
                vec![json!(" ")],
            ]
        );
        // Nested ranges and empty ranges.
        let p =
            JsonPath::parse("{range .spec.containers[*]}{range .ports[*]}{.p}{end}{end}").unwrap();
        assert_eq!(p.find_results(&d), vec![vec![json!(1)], vec![json!(2)]]);
        let p = JsonPath::parse("{range .nope[*]}{.x}{end}").unwrap();
        assert!(p.find_results(&d).is_empty());
    }

    #[test]
    fn errors() {
        assert!(JsonPath::parse("{.a").is_err());
        assert!(JsonPath::parse("{.a[}").is_err());
        // client-go supports `{range}`; only a stray `{end}` is an error
        // (this used to reject range).
        assert!(JsonPath::parse("{end}").is_err());
        assert!(JsonPath::parse("{.a[x]}").is_err());
    }
}
