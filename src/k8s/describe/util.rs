//! Shared helpers: a JSON accessor with Go zero-value semantics, time and
//! duration formatting, label/selector formatting and the multiline
//! printers kubectl uses everywhere.

use std::collections::BTreeMap;

use jiff::{tz::TimeZone, Timestamp};
use serde_json::Value;

use super::gosort::sort_by;
use super::writer::{PrefixWriter, L0};
use super::Rel;

static NULL: Value = Value::Null;

/// Read-only view of a JSON node. Missing fields read as Go zero values.
#[derive(Clone, Copy, Debug)]
pub(crate) struct J<'a>(pub(crate) &'a Value);

impl<'a> J<'a> {
    /// Walk a dot separated path of struct fields.
    pub(crate) fn get(self, path: &str) -> J<'a> {
        if path.is_empty() {
            return self;
        }
        let mut cur = self.0;
        for part in path.split('.') {
            cur = match cur {
                Value::Object(m) => m.get(part).unwrap_or(&NULL),
                _ => &NULL,
            };
        }
        J(cur)
    }

    /// Look up a single key (map keys may contain dots).
    pub(crate) fn key(self, k: &str) -> J<'a> {
        match self.0 {
            Value::Object(m) => J(m.get(k).unwrap_or(&NULL)),
            _ => J(&NULL),
        }
    }

    /// Non-null, i.e. a Go pointer/slice/map that is not nil.
    pub(crate) fn exists(self, path: &str) -> bool {
        !self.get(path).0.is_null()
    }

    pub(crate) fn is_null(self) -> bool {
        self.0.is_null()
    }

    pub(crate) fn as_str(self) -> &'a str {
        self.0.as_str().unwrap_or("")
    }

    pub(crate) fn s(self, path: &str) -> &'a str {
        self.get(path).as_str()
    }

    pub(crate) fn opt_s(self, path: &str) -> Option<&'a str> {
        self.get(path).0.as_str()
    }

    pub(crate) fn as_i64(self) -> i64 {
        match self.0 {
            Value::Number(n) => n
                .as_i64()
                .or_else(|| n.as_f64().map(|f| f as i64))
                .unwrap_or(0),
            _ => 0,
        }
    }

    pub(crate) fn i(self, path: &str) -> i64 {
        self.get(path).as_i64()
    }

    pub(crate) fn opt_i(self, path: &str) -> Option<i64> {
        let v = self.get(path);
        match v.0 {
            Value::Number(_) => Some(v.as_i64()),
            _ => None,
        }
    }

    pub(crate) fn b(self, path: &str) -> bool {
        self.get(path).0.as_bool().unwrap_or(false)
    }

    pub(crate) fn opt_b(self, path: &str) -> Option<bool> {
        self.get(path).0.as_bool()
    }

    pub(crate) fn items(self) -> Vec<J<'a>> {
        match self.0 {
            Value::Array(a) => a.iter().map(J).collect(),
            _ => Vec::new(),
        }
    }

    pub(crate) fn arr(self, path: &str) -> Vec<J<'a>> {
        self.get(path).items()
    }

    pub(crate) fn strs(self, path: &str) -> Vec<String> {
        self.arr(path).into_iter().map(|v| go_value(v.0)).collect()
    }

    /// A `map[string]string` (sorted by key).
    pub(crate) fn smap(self, path: &str) -> BTreeMap<String, String> {
        match self.get(path).0 {
            Value::Object(m) => m.iter().map(|(k, v)| (k.clone(), go_value(v))).collect(),
            _ => BTreeMap::new(),
        }
    }

    /// Object keys, sorted.
    pub(crate) fn keys(self, path: &str) -> Vec<&'a str> {
        let mut keys: Vec<&str> = match self.get(path).0 {
            Value::Object(m) => m.keys().map(|k| k.as_str()).collect(),
            _ => Vec::new(),
        };
        keys.sort_unstable();
        keys
    }

    /// len() of an array or object (0 when missing).
    pub(crate) fn len(self, path: &str) -> usize {
        match self.get(path).0 {
            Value::Array(a) => a.len(),
            Value::Object(m) => m.len(),
            _ => 0,
        }
    }
}

/// Describer context: the clock, the display time zone and related objects.
pub(crate) struct Ctx<'a> {
    pub(crate) now: Timestamp,
    pub(crate) tz: TimeZone,
    pub(crate) rel: &'a Rel,
}

const ZERO_RFC1123Z: &str = "Mon, 01 Jan 0001 00:00:00 +0000";

pub(crate) fn parse_time(j: J) -> Option<Timestamp> {
    j.0.as_str().and_then(|s| s.parse::<Timestamp>().ok())
}

impl Ctx<'_> {
    /// `t.Time.Format(time.RFC1123Z)` in the local zone; zero time for null.
    pub(crate) fn rfc1123z(&self, j: J) -> String {
        match parse_time(j) {
            Some(ts) => ts
                .to_zoned(self.tz.clone())
                .strftime("%a, %d %b %Y %H:%M:%S %z")
                .to_string(),
            None => ZERO_RFC1123Z.to_string(),
        }
    }

    /// Go `time.Time.String()`: `2006-01-02 15:04:05.999999999 -0700 MST`.
    pub(crate) fn go_time_string(&self, j: J) -> String {
        match parse_time(j) {
            Some(ts) => {
                let z = ts.to_zoned(self.tz.clone());
                let mut s = z.strftime("%Y-%m-%d %H:%M:%S").to_string();
                let nanos = z.subsec_nanosecond();
                if nanos != 0 {
                    let frac = format!("{nanos:09}");
                    s.push('.');
                    s.push_str(frac.trim_end_matches('0'));
                }
                s.push_str(&z.strftime(" %z %Z").to_string());
                s
            }
            None => "0001-01-01 00:00:00 +0000 UTC".to_string(),
        }
    }

    /// translateTimestampSince: human duration since `j`, `<unknown>` if unset.
    pub(crate) fn since(&self, j: J) -> String {
        match parse_time(j) {
            Some(ts) => human_duration(self.nanos_between(ts, self.now)),
            None => "<unknown>".to_string(),
        }
    }

    pub(crate) fn nanos_between(&self, from: Timestamp, to: Timestamp) -> i64 {
        let d = to.as_nanosecond() - from.as_nanosecond();
        d.clamp(i64::MIN as i128, i64::MAX as i128) as i64
    }
}

const SECOND: i64 = 1_000_000_000;
const MINUTE: i64 = 60 * SECOND;
const HOUR: i64 = 60 * MINUTE;

/// apimachinery duration.HumanDuration.
pub(crate) fn human_duration(d: i64) -> String {
    let seconds = d / SECOND;
    if seconds < -1 {
        return "<invalid>".to_string();
    } else if seconds < 0 {
        return "0s".to_string();
    } else if seconds < 60 * 2 {
        return format!("{seconds}s");
    }
    let minutes = d / MINUTE;
    if minutes < 10 {
        let s = (d / SECOND) % 60;
        if s == 0 {
            return format!("{minutes}m");
        }
        return format!("{minutes}m{s}s");
    } else if minutes < 60 * 3 {
        return format!("{minutes}m");
    }
    let hours = d / HOUR;
    if hours < 8 {
        let m = (d / MINUTE) % 60;
        if m == 0 {
            return format!("{hours}h");
        }
        return format!("{hours}h{m}m");
    } else if hours < 48 {
        return format!("{hours}h");
    } else if hours < 24 * 8 {
        let h = hours % 24;
        if h == 0 {
            return format!("{}d", hours / 24);
        }
        return format!("{}d{}h", hours / 24, h);
    } else if hours < 24 * 365 * 2 {
        return format!("{}d", hours / 24);
    } else if hours < 24 * 365 * 8 {
        let dy = (hours / 24) % 365;
        if dy == 0 {
            return format!("{}y", hours / 24 / 365);
        }
        return format!("{}y{}d", hours / 24 / 365, dy);
    }
    format!("{}y", hours / 24 / 365)
}

/// Go `%v` of a decoded JSON value (unstructured content uses int64 for
/// integral numbers and float64 otherwise).
pub(crate) fn go_value(v: &Value) -> String {
    match v {
        Value::Null => "<nil>".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.to_string()
            } else {
                go_float(n.as_f64().unwrap_or(0.0))
            }
        }
        Value::String(s) => s.clone(),
        Value::Array(a) => {
            let parts: Vec<String> = a.iter().map(go_value).collect();
            format!("[{}]", parts.join(" "))
        }
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys
                .into_iter()
                .map(|k| format!("{}:{}", k, go_value(&m[k])))
                .collect();
            format!("map[{}]", parts.join(" "))
        }
    }
}

/// Go `%v` of a float64 (strconv 'g' with shortest precision).
pub(crate) fn go_float(f: f64) -> String {
    if f.is_nan() {
        return "NaN".to_string();
    }
    if f.is_infinite() {
        return if f > 0.0 { "+Inf" } else { "-Inf" }.to_string();
    }
    if f == 0.0 {
        return if f.is_sign_negative() { "-0" } else { "0" }.to_string();
    }
    let sci = format!("{f:e}");
    let (mant, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    if !(-4..6).contains(&exp) {
        return go_exp(mant, exp);
    }
    format!("{f}")
}

fn go_exp(mant: &str, exp: i32) -> String {
    let sign = if exp < 0 { '-' } else { '+' };
    format!("{mant}e{sign}{:02}", exp.abs())
}

/// Go `%v` of a []string: `[a b]`.
pub(crate) fn go_slice(items: &[String]) -> String {
    format!("[{}]", items.join(" "))
}

/// Go `%v` of a map[string]string: `map[a:b c:d]`.
pub(crate) fn go_map(m: &BTreeMap<String, String>) -> String {
    let parts: Vec<String> = m.iter().map(|(k, v)| format!("{k}:{v}")).collect();
    format!("map[{}]", parts.join(" "))
}

/// IntOrString.String() of a pointer field (`<nil>` when unset).
pub(crate) fn intstr_ptr(j: J) -> String {
    match j.0 {
        Value::Null => "<nil>".to_string(),
        Value::String(s) => s.clone(),
        other => J(other).as_i64().to_string(),
    }
}

/// IntOrString.String() of a value field (zero is `0`).
pub(crate) fn intstr(j: J) -> String {
    match j.0 {
        Value::Null => "0".to_string(),
        _ => intstr_ptr(j),
    }
}

/// labels.FormatLabels: sorted `k=v` pairs, `<none>` when empty.
pub(crate) fn format_labels(m: &BTreeMap<String, String>) -> String {
    let mut pairs: Vec<String> = m.iter().map(|(k, v)| format!("{k}={v}")).collect();
    pairs.sort();
    if pairs.is_empty() {
        "<none>".to_string()
    } else {
        pairs.join(",")
    }
}

struct Requirement {
    key: String,
    op: &'static str,
    values: Vec<String>,
}

impl Requirement {
    fn render(&self) -> String {
        let mut s = String::new();
        if self.op == "!" {
            s.push('!');
        }
        s.push_str(&self.key);
        match self.op {
            "=" => s.push('='),
            "in" => s.push_str(" in "),
            "notin" => s.push_str(" notin "),
            _ => return s,
        }
        let paren = self.op == "in" || self.op == "notin";
        if paren {
            s.push('(');
        }
        let mut vals = self.values.clone();
        vals.sort();
        s.push_str(&vals.join(","));
        if paren {
            s.push(')');
        }
        s
    }
}

/// metav1.LabelSelectorAsSelector(sel).String(). Nil and empty selectors
/// render as "". An invalid operator is an error like in Go.
pub(crate) fn selector_string(sel: J) -> Result<String, String> {
    if sel.is_null() {
        return Ok(String::new());
    }
    let mut reqs = Vec::new();
    for (k, v) in sel.smap("matchLabels") {
        reqs.push(Requirement {
            key: k,
            op: "=",
            values: vec![v],
        });
    }
    for e in sel.arr("matchExpressions") {
        let op = match e.s("operator") {
            "In" => "in",
            "NotIn" => "notin",
            "Exists" => "exists",
            "DoesNotExist" => "!",
            other => return Err(format!("{other:?} is not a valid label selector operator")),
        };
        reqs.push(Requirement {
            key: e.s("key").to_string(),
            op,
            values: e.strs("values"),
        });
    }
    sort_by(&mut reqs, |a, b| a.key < b.key);
    let parts: Vec<String> = reqs.iter().map(Requirement::render).collect();
    Ok(parts.join(","))
}

/// metav1.FormatLabelSelector.
pub(crate) fn format_label_selector(sel: J) -> String {
    match selector_string(sel) {
        Ok(s) if s.is_empty() => "<none>".to_string(),
        Ok(s) => s,
        Err(_) => "<error>".to_string(),
    }
}

/// printLabelsMultilineWithIndent.
pub(crate) fn print_labels_multiline_indent(
    w: &mut PrefixWriter,
    initial_indent: &str,
    title: &str,
    inner_indent: &str,
    labels: &BTreeMap<String, String>,
) {
    w.write(L0, &format!("{initial_indent}{title}:{inner_indent}"));
    if labels.is_empty() {
        w.write_line("<none>");
        return;
    }
    for (i, (k, v)) in labels.iter().enumerate() {
        if i != 0 {
            w.write(L0, initial_indent);
            w.write(L0, inner_indent);
        }
        w.write(L0, &format!("{k}={v}\n"));
    }
}

pub(crate) fn print_labels_multiline(
    w: &mut PrefixWriter,
    title: &str,
    labels: &BTreeMap<String, String>,
) {
    print_labels_multiline_indent(w, "", title, "\t", labels);
}

const MAX_ANNOTATION_LEN: usize = 140;
const LAST_APPLIED: &str = "kubectl.kubernetes.io/last-applied-configuration";

fn shorten(s: &str, max: usize) -> String {
    if s.len() > max {
        format!("{}...", String::from_utf8_lossy(&s.as_bytes()[..max]))
    } else {
        s.to_string()
    }
}

/// printAnnotationsMultiline.
pub(crate) fn print_annotations_multiline(
    w: &mut PrefixWriter,
    title: &str,
    annotations: &BTreeMap<String, String>,
) {
    w.write(L0, &format!("{title}:\t"));
    let keys: Vec<&String> = annotations
        .keys()
        .filter(|k| k.as_str() != LAST_APPLIED)
        .collect();
    if keys.is_empty() {
        w.write_line("<none>");
        return;
    }
    let indent = "\t";
    for (i, key) in keys.into_iter().enumerate() {
        if i != 0 {
            w.write(L0, indent);
        }
        let raw = &annotations[key];
        let value = raw.strip_suffix('\n').unwrap_or(raw);
        if value.len() + key.len() + 2 > MAX_ANNOTATION_LEN || value.contains('\n') {
            w.write(L0, &format!("{key}:\n"));
            for s in value.split('\n') {
                w.write(
                    L0,
                    &format!("{indent}  {}\n", shorten(s, MAX_ANNOTATION_LEN - 2)),
                );
            }
        } else {
            w.write(L0, &format!("{key}: {value}\n"));
        }
    }
}

/// Taint.ToString.
pub(crate) fn taint_string(t: J) -> String {
    let (key, value, effect) = (t.s("key"), t.s("value"), t.s("effect"));
    if effect.is_empty() {
        if value.is_empty() {
            return key.to_string();
        }
        return format!("{key}={value}:");
    }
    if value.is_empty() {
        return format!("{key}:{effect}");
    }
    format!("{key}={value}:{effect}")
}

/// printTaintsMultilineWithIndent.
pub(crate) fn print_taints_multiline(w: &mut PrefixWriter, title: &str, taints: Vec<J>) {
    w.write(L0, &format!("{title}:\t"));
    if taints.is_empty() {
        w.write_line("<none>");
        return;
    }
    let mut taints = taints;
    sort_by(&mut taints, |a, b| {
        format!("{},{}", a.s("effect"), a.s("key")) < format!("{},{}", b.s("effect"), b.s("key"))
    });
    for (i, t) in taints.iter().enumerate() {
        if i != 0 {
            w.write(L0, "\t");
        }
        w.write(L0, &format!("{}\n", taint_string(*t)));
    }
}

/// printTolerationsMultilineWithIndent.
pub(crate) fn print_tolerations_multiline(
    w: &mut PrefixWriter,
    initial_indent: &str,
    title: &str,
    tolerations: Vec<J>,
) {
    w.write(L0, &format!("{initial_indent}{title}:\t"));
    if tolerations.is_empty() {
        w.write_line("<none>");
        return;
    }
    let mut tolerations = tolerations;
    sort_by(&mut tolerations, |a, b| a.s("key") < b.s("key"));
    for (i, t) in tolerations.iter().enumerate() {
        if i != 0 {
            w.write(L0, initial_indent);
            w.write(L0, "\t");
        }
        let (key, value, effect) = (t.s("key"), t.s("value"), t.s("effect"));
        w.write(L0, key);
        if !value.is_empty() {
            w.write(L0, &format!("={value}"));
        }
        if !effect.is_empty() {
            w.write(L0, &format!(":{effect}"));
        }
        if t.s("operator") == "Exists" && value.is_empty() {
            if !key.is_empty() || !effect.is_empty() {
                w.write(L0, " op=Exists");
            } else {
                w.write(L0, "op=Exists");
            }
        }
        if let Some(secs) = t.opt_i("tolerationSeconds") {
            w.write(L0, &format!(" for {secs}s"));
        }
        w.write(L0, "\n");
    }
}

/// printPodsMultiline (pods sorted by name).
pub(crate) fn print_pods_multiline(w: &mut PrefixWriter, title: &str, names: &[String]) {
    w.write(L0, &format!("{title}:\t"));
    if names.is_empty() {
        w.write_line("<none>");
        return;
    }
    let mut names = names.to_vec();
    sort_by(&mut names, |a, b| a < b);
    for (i, n) in names.iter().enumerate() {
        if i != 0 {
            w.write(L0, "\t");
        }
        w.write(L0, &format!("{n}\n"));
    }
}

/// printController: `Kind/name` of the controller owner reference.
pub(crate) fn controller_of(obj: J) -> Option<(String, String, String)> {
    obj.arr("metadata.ownerReferences")
        .into_iter()
        .find(|r| r.b("controller"))
        .map(|r| {
            (
                r.s("kind").to_string(),
                r.s("name").to_string(),
                r.s("uid").to_string(),
            )
        })
}

pub(crate) fn print_controller(obj: J) -> String {
    controller_of(obj)
        .map(|(k, n, _)| format!("{k}/{n}"))
        .unwrap_or_default()
}

/// printBool.
pub(crate) fn print_bool(b: bool) -> &'static str {
    if b {
        "True"
    } else {
        "False"
    }
}

/// printBoolPtr.
pub(crate) fn print_bool_ptr(b: Option<bool>) -> &'static str {
    match b {
        Some(b) => print_bool(b),
        None => "<unset>",
    }
}

pub(crate) fn string_or_none(s: &str) -> String {
    if s.is_empty() {
        "<none>".to_string()
    } else {
        s.to_string()
    }
}

/// net.JoinHostPort.
pub(crate) fn join_host_port(host: &str, port: &str) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn human_durations() {
        let s = |n: i64| human_duration(n * SECOND);
        assert_eq!(s(-5), "<invalid>");
        assert_eq!(s(0), "0s");
        assert_eq!(s(119), "119s");
        assert_eq!(s(120), "2m");
        assert_eq!(s(150), "2m30s");
        assert_eq!(s(600), "10m");
        assert_eq!(s(179 * 60), "179m");
        assert_eq!(s(3 * 3600 + 5 * 60), "3h5m");
        assert_eq!(s(8 * 3600), "8h");
        assert_eq!(s(47 * 3600), "47h");
        assert_eq!(s(50 * 3600), "2d2h");
        assert_eq!(s(10 * 86400), "10d");
        assert_eq!(s(800 * 86400), "2y70d");
        assert_eq!(s(9 * 365 * 86400), "9y");
    }

    #[test]
    fn go_values() {
        assert_eq!(go_value(&json!(1.5)), "1.5");
        assert_eq!(go_value(&json!(1000000.0)), "1e+06");
        assert_eq!(go_value(&json!(100000.0)), "100000");
        assert_eq!(go_value(&json!(0.00001)), "1e-05");
        assert_eq!(go_value(&json!(3)), "3");
        assert_eq!(go_value(&json!(null)), "<nil>");
        assert_eq!(go_value(&json!(["a", 1, true])), "[a 1 true]");
        assert_eq!(go_value(&json!({"b": 1, "a": "x"})), "map[a:x b:1]");
    }

    #[test]
    fn selectors() {
        let sel = json!({
            "matchLabels": {"id2": "app2", "id1": "app1"},
            "matchExpressions": [
                {"key": "foo", "operator": "In", "values": ["bar2", "bar1"]},
                {"key": "foo2", "operator": "NotIn", "values": ["bar1", "bar2"]},
                {"key": "x", "operator": "Exists"},
                {"key": "y", "operator": "DoesNotExist"}
            ]
        });
        assert_eq!(
            selector_string(J(&sel)).unwrap(),
            "foo in (bar1,bar2),foo2 notin (bar1,bar2),id1=app1,id2=app2,x,!y"
        );
        assert_eq!(format_label_selector(J(&json!({}))), "<none>");
        assert_eq!(format_label_selector(J(&Value::Null)), "<none>");
        let bad = json!({"matchExpressions": [{"key": "a", "operator": "Foo"}]});
        assert_eq!(format_label_selector(J(&bad)), "<error>");
    }

    #[test]
    fn labels_format_sorts_pairs() {
        let m: BTreeMap<String, String> = [("a", "1"), ("a.b", "2")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        assert_eq!(format_labels(&m), "a.b=2,a=1");
        assert_eq!(format_labels(&BTreeMap::new()), "<none>");
    }

    #[test]
    fn taints() {
        assert_eq!(taint_string(J(&json!({"key": "k"}))), "k");
        assert_eq!(taint_string(J(&json!({"key": "k", "value": "v"}))), "k=v:");
        assert_eq!(
            taint_string(J(&json!({"key": "k", "effect": "NoSchedule"}))),
            "k:NoSchedule"
        );
        assert_eq!(
            taint_string(J(
                &json!({"key": "k", "value": "v", "effect": "NoSchedule"})
            )),
            "k=v:NoSchedule"
        );
    }

    #[test]
    fn times() {
        let rel = Rel::default();
        let ctx = Ctx {
            now: "2024-01-01T12:00:00Z".parse().unwrap(),
            tz: TimeZone::UTC,
            rel: &rel,
        };
        let t = json!("2024-01-01T10:30:00Z");
        assert_eq!(ctx.rfc1123z(J(&t)), "Mon, 01 Jan 2024 10:30:00 +0000");
        assert_eq!(ctx.rfc1123z(J(&Value::Null)), ZERO_RFC1123Z);
        assert_eq!(ctx.since(J(&t)), "90m");
        assert_eq!(ctx.since(J(&Value::Null)), "<unknown>");
        assert_eq!(ctx.go_time_string(J(&t)), "2024-01-01 10:30:00 +0000 UTC");
        let fixed = TimeZone::fixed(jiff::tz::offset(-7));
        let ctx = Ctx { tz: fixed, ..ctx };
        assert_eq!(ctx.rfc1123z(J(&t)), "Mon, 01 Jan 2024 03:30:00 -0700");
    }
}
