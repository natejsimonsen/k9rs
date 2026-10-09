//! Shared rendering helpers (k9s render/helpers.go, client/metrics.go and
//! apimachinery's duration.HumanDuration): ages, quantities, percentages,
//! bool/str formatting, label maps and JSON getters for `DynamicObject`s.
//!
//! # Ages
//! k9s computes ages when it renders a row (`ToAge` returns a human duration
//! like `5d3h`), not when it draws the table. We do the same: renderers put a
//! human duration in AGE (and other time) columns and nothing reformats them
//! at draw time. "Now" comes from [`now`], which tests can pin with
//! [`with_now`]. [`age_decorator`] mirrors k9s's (unused upstream)
//! `AgeDecorator`; it passes already-humanized values through unchanged.
//!
//! # Custom columns
//! [`ViewBase`] (re-exported from `render::core::custcol`) implements the k9s
//! `Base` renderer behavior: views.yaml column specs (`NAME:.json.path|WTR`)
//! and the NAMESPACE column rule of the shared contract (present only when the
//! namespace is cluster wide: all, blank or `-`).

use std::cell::Cell;
use std::collections::BTreeMap;
use std::fmt::Display;

use chrono::{DateTime, Utc};
use kube::api::DynamicObject;
use serde_json::Value;

pub use crate::model::table::{MISSING_VALUE, NA_VALUE, UNKNOWN_VALUE, UNSET_VALUE, ZERO_VALUE};
pub use crate::render::core::custcol::{ColumnSpec, ColumnSpecs, ViewBase};

/// One megabyte (client.MegaByte).
pub const MEGA_BYTE: i64 = 1024 * 1024;
/// k9s default GPU vendors (config.KnownGPUVendors), in lookup order.
pub const KNOWN_GPU_VENDORS: &[(&str, &str)] = &[
    ("nvidia", "nvidia.com/gpu"),
    ("nvidia-shared", "nvidia.com/gpu.shared"),
    ("amd", "amd.com/gpu"),
    ("intel", "gpu.intel.com/i915"),
];

// ---------------------------------------------------------------------------
// Time.

thread_local! {
    static NOW: Cell<Option<DateTime<Utc>>> = const { Cell::new(None) };
}

/// Current time used for ages. Returns the pinned time set by [`with_now`] or
/// [`set_now`] on this thread, else the wall clock.
pub fn now() -> DateTime<Utc> {
    NOW.with(|n| n.get()).unwrap_or_else(Utc::now)
}

/// Pin (or with `None`, unpin) the time returned by [`now`] on this thread.
pub fn set_now(t: Option<DateTime<Utc>>) {
    NOW.with(|n| n.set(t));
}

/// Run `f` with [`now`] pinned to `t`, restoring the previous value after.
pub fn with_now<R>(t: DateTime<Utc>, f: impl FnOnce() -> R) -> R {
    let prev = NOW.with(|n| n.replace(Some(t)));
    let out = f();
    NOW.with(|n| n.set(prev));
    out
}

/// apimachinery duration.HumanDuration: 5s, 2m3s, 4h, 3d, 2y...
pub fn human_duration(d: chrono::Duration) -> String {
    let seconds = d.num_seconds();
    if seconds < -1 {
        return "<invalid>".into();
    }
    if seconds < 0 {
        return "0s".into();
    }
    if seconds < 60 * 2 {
        return format!("{seconds}s");
    }
    let minutes = d.num_minutes();
    if minutes < 10 {
        let s = seconds % 60;
        if s == 0 {
            return format!("{minutes}m");
        }
        return format!("{minutes}m{s}s");
    }
    if minutes < 60 * 3 {
        return format!("{minutes}m");
    }
    let hours = d.num_hours();
    if hours < 8 {
        let m = minutes % 60;
        if m == 0 {
            return format!("{hours}h");
        }
        return format!("{hours}h{m}m");
    }
    if hours < 48 {
        return format!("{hours}h");
    }
    if hours < 24 * 8 {
        let h = hours % 24;
        if h == 0 {
            return format!("{}d", hours / 24);
        }
        return format!("{}d{}h", hours / 24, h);
    }
    if hours < 24 * 365 * 2 {
        return format!("{}d", hours / 24);
    }
    if hours < 24 * 365 * 8 {
        let dy = (hours / 24) % 365;
        if dy == 0 {
            return format!("{}y", hours / 24 / 365);
        }
        return format!("{}y{}d", hours / 24 / 365, dy);
    }
    format!("{}y", hours / 24 / 365)
}

/// Human age of `ts` relative to `now` (HumanDuration(now - ts)).
pub fn to_age(ts: DateTime<Utc>, now: DateTime<Utc>) -> String {
    human_duration(now - ts)
}

/// k9s `ToAge`: `<unknown>` for a missing time, else the age relative to [`now`].
pub fn to_age_opt(ts: Option<DateTime<Utc>>) -> String {
    match ts {
        None => UNKNOWN_VALUE.into(),
        Some(t) => to_age(t, now()),
    }
}

/// k9s `timeToAge`: age of a time that is always set (file mod times...).
pub fn time_to_age(ts: DateTime<Utc>) -> String {
    to_age(ts, now())
}

/// k9s `toAgeHuman`: `<unknown>` when blank, `n/a` when not RFC3339, else the age.
pub fn to_age_human(s: &str) -> String {
    if s.is_empty() {
        return UNKNOWN_VALUE.into();
    }
    match parse_time(s) {
        Some(t) => time_to_age(t),
        None => NA_VALUE.into(),
    }
}

/// k9s `AgeDecorator` for time columns holding raw RFC3339 timestamps.
/// Blank → `<unknown>`, RFC3339 → human age, anything else (already a human
/// duration such as `5d`, or `<unknown>`/`n/a`) is returned unchanged.
pub fn age_decorator(field: &str) -> String {
    if field.is_empty() {
        return UNKNOWN_VALUE.into();
    }
    match parse_time(field) {
        Some(t) => time_to_age(t),
        None => field.to_string(),
    }
}

/// Parse an RFC3339 timestamp (with or without fractional seconds).
pub fn parse_time(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s.trim())
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Parse a timestamp JSON value (RFC3339 string).
pub fn value_time(v: Option<&Value>) -> Option<DateTime<Utc>> {
    v.and_then(Value::as_str).and_then(parse_time)
}

/// metadata.creationTimestamp of an object.
pub fn creation_timestamp(o: &DynamicObject) -> Option<DateTime<Utc>> {
    o.metadata
        .creation_timestamp
        .as_ref()
        .and_then(|t| from_k8s_time(&t.0))
}

/// metadata.deletionTimestamp of an object.
pub fn deletion_timestamp(o: &DynamicObject) -> Option<DateTime<Utc>> {
    o.metadata
        .deletion_timestamp
        .as_ref()
        .and_then(|t| from_k8s_time(&t.0))
}

/// Convert a k8s-openapi `Time` payload to chrono via its RFC3339 form.
fn from_k8s_time<T: Display>(t: &T) -> Option<DateTime<Utc>> {
    parse_time(&t.to_string())
}

/// k9s `ToAge(o.GetCreationTimestamp())`.
pub fn obj_age(o: &DynamicObject) -> String {
    to_age_opt(creation_timestamp(o))
}

// ---------------------------------------------------------------------------
// Strings.

/// k9s `blank`: true when empty or every value is blank.
pub fn blank<S: AsRef<str>>(ss: &[S]) -> bool {
    ss.iter().all(|s| s.as_ref().is_empty())
}

/// k9s `join`: join, skipping blank values (a single value is returned as is).
pub fn join<S: AsRef<str>>(ss: &[S], sep: &str) -> String {
    match ss.len() {
        0 => String::new(),
        1 => ss[0].as_ref().to_string(),
        _ => ss
            .iter()
            .map(AsRef::as_ref)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(sep),
    }
}

/// k9s `check`: `sub` when `s` is blank.
pub fn check(s: &str, sub: &str) -> String {
    if s.is_empty() {
        sub.to_string()
    } else {
        s.to_string()
    }
}

/// k9s `missing`: `<none>` when blank.
pub fn missing(s: &str) -> String {
    check(s, MISSING_VALUE)
}

/// k9s `na`: `n/a` when blank.
pub fn na(s: &str) -> String {
    check(s, NA_VALUE)
}

/// k9s `naStrings`: `n/a` when empty, else comma joined.
pub fn na_strings<S: AsRef<str>>(ss: &[S]) -> String {
    if ss.is_empty() {
        return NA_VALUE.into();
    }
    ss.iter().map(AsRef::as_ref).collect::<Vec<_>>().join(",")
}

/// k9s `boolToStr`.
pub fn bool_to_str(b: bool) -> String {
    if b { "true" } else { "false" }.into()
}

/// k9s `boolPtrToStr`: `false` when unset.
pub fn bool_ptr_to_str(b: Option<bool>) -> String {
    bool_to_str(b.unwrap_or(false))
}

/// k9s `boolPtrToStr` on a JSON value (missing or non-bool → `false`).
pub fn bool_value_to_str(v: Option<&Value>) -> String {
    bool_ptr_to_str(v.and_then(Value::as_bool))
}

/// k9s `strPtrToStr`: blank when unset.
pub fn str_ptr_to_str(s: Option<&str>) -> String {
    s.unwrap_or("").to_string()
}

/// k9s `IntToStr`.
pub fn int_to_str(i: i64) -> String {
    i.to_string()
}

/// k9s `AsPerc`: wrap in parens.
pub fn as_perc(p: &str) -> String {
    format!("({p})")
}

/// k9s `PrintPerc`: `42%`.
pub fn print_perc(p: i64) -> String {
    format!("{p}%")
}

/// k9s `AsStatus`: blank when ok, else the error message (VALID column).
pub fn as_status<E: Display>(r: Result<(), E>) -> String {
    match r {
        Ok(()) => String::new(),
        Err(e) => e.to_string(),
    }
}

/// k9s `AsThousands`: `1,234,567`.
pub fn as_thousands(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if n < 0 {
        out.push('-');
    }
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// k9s `Truncate` (runewidth.Truncate with a `…` tail): cut `s` so its
/// display width, including the ellipsis, fits `width`.
pub fn truncate(s: &str, width: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    use unicode_width::UnicodeWidthStr;
    if s.width() <= width {
        return s.to_string();
    }
    let tail = "…";
    let tw = tail.width();
    if width < tw {
        return String::new();
    }
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if w + cw > width - tw {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push_str(tail);
    out
}

/// k9s `Pad`: right pad with spaces to `width` bytes, or truncate when longer.
pub fn pad(s: &str, width: usize) -> String {
    if s.len() == width {
        return s.to_string();
    }
    if s.len() > width {
        return truncate(s, width);
    }
    format!("{s}{}", " ".repeat(width - s.len()))
}

/// k9s `runesToNum`: parse an ASCII digit string (no validation, like k9s).
pub fn runes_to_num(s: &str) -> i64 {
    let mut r: i64 = 0;
    let mut m: i64 = 1;
    for c in s.chars().rev() {
        let v = (c as i64) - ('0' as i64);
        r = r.wrapping_add(v.wrapping_mul(m));
        m = m.wrapping_mul(10);
    }
    r
}

/// k9s `mapToStr`: `k1=v1,k2=v2` sorted by key; blank when empty.
pub fn map_to_str<'a, I>(m: I) -> String
where
    I: IntoIterator<Item = (&'a String, &'a String)>,
{
    let mut kv: Vec<(&String, &String)> = m.into_iter().collect();
    kv.sort_by(|a, b| a.0.cmp(b.0));
    kv.iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// k9s `mapToIfc`: `k1=v1 k2=v2` sorted by key over a JSON object, skipping
/// non-string values (k9s keeps the separator logic of the full key list).
pub fn map_to_ifc(m: Option<&Value>) -> String {
    let Some(Value::Object(mm)) = m else {
        return String::new();
    };
    let mut keys: Vec<&String> = mm.keys().collect();
    keys.sort();
    let mut s = String::new();
    for (i, k) in keys.iter().enumerate() {
        let Some(v) = mm[*k].as_str() else {
            continue;
        };
        s.push_str(k);
        s.push('=');
        s.push_str(v);
        if i < keys.len() - 1 {
            s.push(' ');
        }
    }
    s
}

/// k9s `toSelector`: flatten a map selector to `k=v,...` (sorted by key).
pub fn to_selector<'a, I>(m: I) -> String
where
    I: IntoIterator<Item = (&'a String, &'a String)>,
{
    map_to_str(m)
}

/// k9s `asSelector` (metav1.LabelSelectorAsSelector(..).String()) for a JSON
/// LabelSelector (`matchLabels`, `matchExpressions`). Missing or empty → "".
pub fn label_selector_to_str(sel: Option<&Value>) -> String {
    let Some(sel) = sel else {
        return String::new();
    };
    let mut reqs: Vec<(String, String)> = Vec::new();
    if let Some(Value::Object(ml)) = sel.get("matchLabels") {
        for (k, v) in ml {
            reqs.push((k.clone(), format!("{k}={}", v.as_str().unwrap_or(""))));
        }
    }
    if let Some(Value::Array(me)) = sel.get("matchExpressions") {
        for e in me {
            let key = e.get("key").and_then(Value::as_str).unwrap_or("");
            let op = e.get("operator").and_then(Value::as_str).unwrap_or("");
            let mut vals: Vec<String> = e
                .get("values")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            vals.sort();
            vals.dedup();
            let s = match op {
                "In" => format!("{key} in ({})", vals.join(",")),
                "NotIn" => format!("{key} notin ({})", vals.join(",")),
                "Exists" => key.to_string(),
                "DoesNotExist" => format!("!{key}"),
                _ => return NA_VALUE.into(),
            };
            reqs.push((key.to_string(), s));
        }
    }
    reqs.sort_by(|a, b| a.0.cmp(&b.0));
    reqs.into_iter()
        .map(|(_, s)| s)
        .collect::<Vec<_>>()
        .join(",")
}

/// Labels of an object as k9s `mapToStr`.
pub fn labels_str(o: &DynamicObject) -> String {
    o.metadata
        .labels
        .as_ref()
        .map(|l| map_to_str(l.iter()))
        .unwrap_or_default()
}

/// Shorthand for a list of string fields.
pub fn fields<const N: usize>(ff: [String; N]) -> Vec<String> {
    ff.into()
}

// ---------------------------------------------------------------------------
// Namespaces / ids.

/// k9s client.IsClusterWide: all, blank or cluster scope (`-`).
pub fn is_cluster_wide(ns: &str) -> bool {
    crate::k8s::is_all_namespaces(ns) || crate::k8s::is_cluster_scoped(ns)
}

/// Namespace of an object ("" when cluster scoped).
pub fn obj_ns(o: &DynamicObject) -> &str {
    o.metadata.namespace.as_deref().unwrap_or("")
}

/// Name of an object.
pub fn obj_name(o: &DynamicObject) -> &str {
    o.metadata.name.as_deref().unwrap_or("")
}

/// FQN of an object (`ns/name` or `name`).
pub fn obj_fqn(o: &DynamicObject) -> String {
    crate::k8s::fqn(obj_ns(o), obj_name(o))
}

// ---------------------------------------------------------------------------
// Quantities and metrics.

/// A parsed resource.Quantity: `mantissa × 10^exp10 × 2^exp2`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Quantity {
    mantissa: i128,
    exp10: i32,
    exp2: u32,
}

impl Quantity {
    /// resource.Quantity.Value(): the value rounded up to an integer.
    pub fn value(&self) -> i64 {
        self.scaled(0)
    }
    /// resource.Quantity.MilliValue(): value × 1000 rounded up.
    pub fn milli_value(&self) -> i64 {
        self.scaled(3)
    }
    /// True for a zero quantity.
    pub fn is_zero(&self) -> bool {
        self.mantissa == 0
    }

    fn scaled(&self, extra10: i32) -> i64 {
        let neg = self.mantissa < 0;
        let mut n = self.mantissa.unsigned_abs();
        for _ in 0..self.exp2 {
            n = n.saturating_mul(2);
        }
        let e = self.exp10 + extra10;
        let v: u128 = if e >= 0 {
            let mut v = n;
            for _ in 0..e {
                v = v.saturating_mul(10);
            }
            v
        } else {
            let mut d: u128 = 1;
            let mut overflow = false;
            for _ in 0..(-e) {
                match d.checked_mul(10) {
                    Some(x) => d = x,
                    None => {
                        overflow = true;
                        break;
                    }
                }
            }
            if overflow {
                u128::from(n > 0)
            } else {
                n.div_ceil(d)
            }
        };
        let v = i64::try_from(v).unwrap_or(i64::MAX);
        if neg {
            -v
        } else {
            v
        }
    }
}

/// Parse a Kubernetes quantity (`100m`, `1.5Gi`, `2`, `1e3`, `500Ki`...).
pub fn parse_quantity(s: &str) -> Option<Quantity> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let b = s.as_bytes();
    let mut i = 0;
    let mut neg = false;
    if b[0] == b'+' || b[0] == b'-' {
        neg = b[0] == b'-';
        i = 1;
    }
    let mut mantissa: i128 = 0;
    let mut frac = 0i32;
    let mut digits = 0;
    let mut seen_dot = false;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_digit() {
            mantissa = mantissa
                .checked_mul(10)?
                .checked_add(i128::from(c - b'0'))?;
            digits += 1;
            if seen_dot {
                frac += 1;
            }
        } else if c == b'.' && !seen_dot {
            seen_dot = true;
        } else {
            break;
        }
        i += 1;
    }
    if digits == 0 {
        return None;
    }
    let suffix = &s[i..];
    let (exp10, exp2) = match suffix {
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
        x if x.starts_with('e') || x.starts_with('E') => (x[1..].parse::<i32>().ok()?, 0),
        _ => return None,
    };
    if neg {
        mantissa = -mantissa;
    }
    Some(Quantity {
        mantissa,
        exp10: exp10 - frac,
        exp2,
    })
}

/// Parse a quantity JSON value (string or number). Invalid → zero.
pub fn value_quantity(v: Option<&Value>) -> Quantity {
    match v {
        Some(Value::String(s)) => parse_quantity(s).unwrap_or_default(),
        Some(Value::Number(n)) => parse_quantity(&n.to_string()).unwrap_or_default(),
        _ => Quantity::default(),
    }
}

/// `list[name]` of a ResourceList as millis (cpu style). Missing → 0.
pub fn res_milli(list: Option<&Value>, name: &str) -> i64 {
    value_quantity(list.and_then(|l| l.get(name))).milli_value()
}

/// `list[name]` of a ResourceList as an integer value (memory, gpu). Missing → 0.
pub fn res_value(list: Option<&Value>, name: &str) -> i64 {
    value_quantity(list.and_then(|l| l.get(name))).value()
}

/// k9s `extractGPU`: value of the first known GPU vendor resource in `list`.
pub fn extract_gpu(list: Option<&Value>) -> i64 {
    for (_, res) in KNOWN_GPU_VENDORS {
        if let Some(q) = list.and_then(|l| l.get(*res)) {
            return value_quantity(Some(q)).value();
        }
    }
    0
}

/// k9s `extractNodeGPU`: (main, shared) GPU values of a ResourceList.
pub fn extract_node_gpu(list: Option<&Value>) -> (Option<i64>, Option<i64>) {
    let (mut main, mut shared) = (None, None);
    for (vendor, res) in KNOWN_GPU_VENDORS {
        if let Some(q) = list.and_then(|l| l.get(*res)) {
            let v = value_quantity(Some(q)).value();
            if vendor.ends_with("shared") {
                shared = Some(v);
            } else {
                main = Some(v);
            }
        }
    }
    (main, shared)
}

/// k9s `toMc`: millicores as a string (`0` when zero).
pub fn to_mc(v: i64) -> String {
    if v == 0 {
        return ZERO_VALUE.into();
    }
    v.to_string()
}

/// k9s `toMi`: bytes as MiB (`0` when zero).
pub fn to_mi(v: i64) -> String {
    if v == 0 {
        return ZERO_VALUE.into();
    }
    to_mb(v).to_string()
}

/// k9s `toMu`: `n/a` when zero.
pub fn to_mu(v: i64) -> String {
    if v == 0 {
        return NA_VALUE.into();
    }
    v.to_string()
}

/// client.ToMB: bytes to MiB.
pub fn to_mb(v: i64) -> i64 {
    v / MEGA_BYTE
}

/// client.ToPercentage: floor(v/dv*100), 0 when `dv` is 0.
pub fn to_percentage(v: i64, dv: i64) -> i64 {
    if dv == 0 {
        return 0;
    }
    ((v as f64 / dv as f64) * 100.0).floor() as i64
}

/// client.ToPercentageStr: like [`to_percentage`] but `n/a` when `dv` is 0.
pub fn to_percentage_str(v: i64, dv: i64) -> String {
    if dv == 0 {
        return NA_VALUE.into();
    }
    to_percentage(v, dv).to_string()
}

/// Usage over limit/request as a k9s percentage string (`n/a` when unset).
pub fn mx_perc(usage: i64, limit: i64) -> String {
    to_percentage_str(usage, limit)
}

// ---------------------------------------------------------------------------
// JSON getters.

/// Follow a dotted path (`spec.template.spec.containers.0.name`; `[n]`
/// indexes are accepted too) into a JSON value.
pub fn get_path<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    let path = path.trim_start_matches('.');
    if path.is_empty() {
        return Some(v);
    }
    let mut cur = v;
    for seg in path.split('.') {
        let mut seg = seg;
        // name[0][1]
        let (name, rest) = match seg.find('[') {
            Some(i) => (&seg[..i], &seg[i..]),
            None => (seg, ""),
        };
        if !name.is_empty() {
            cur = match cur {
                Value::Object(m) => m.get(name)?,
                Value::Array(a) => a.get(name.parse::<usize>().ok()?)?,
                _ => return None,
            };
        }
        seg = rest;
        while let Some(stripped) = seg.strip_prefix('[') {
            let end = stripped.find(']')?;
            let idx: usize = stripped[..end].parse().ok()?;
            cur = cur.as_array()?.get(idx)?;
            seg = &stripped[end + 1..];
        }
    }
    Some(cur)
}

/// String at `path`, "" when missing or not a string.
pub fn str_at<'a>(v: &'a Value, path: &str) -> &'a str {
    get_path(v, path).and_then(Value::as_str).unwrap_or("")
}

/// Integer at `path`, 0 when missing.
pub fn i64_at(v: &Value, path: &str) -> i64 {
    get_path(v, path).and_then(Value::as_i64).unwrap_or(0)
}

/// Optional integer at `path`.
pub fn opt_i64_at(v: &Value, path: &str) -> Option<i64> {
    get_path(v, path).and_then(Value::as_i64)
}

/// Bool at `path`, false when missing.
pub fn bool_at(v: &Value, path: &str) -> bool {
    get_path(v, path).and_then(Value::as_bool).unwrap_or(false)
}

/// Array at `path`, empty when missing.
pub fn array_at<'a>(v: &'a Value, path: &str) -> &'a [Value] {
    get_path(v, path)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// String map at `path` (labels-like objects), empty when missing.
pub fn str_map_at(v: &Value, path: &str) -> BTreeMap<String, String> {
    get_path(v, path)
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// Value at `path` under the object's top-level data (`spec.replicas`, ...).
/// For `metadata.*` paths use [`obj_get`].
pub fn data_get<'a>(o: &'a DynamicObject, path: &str) -> Option<&'a Value> {
    let path = path.trim_start_matches('.');
    let (head, rest) = match path.split_once('.') {
        Some((h, r)) => (h, r),
        None => (path, ""),
    };
    let top = o.data.get(head)?;
    get_path(top, rest)
}

/// String under the object's top-level data, "" when missing.
pub fn data_str<'a>(o: &'a DynamicObject, path: &str) -> &'a str {
    data_get(o, path).and_then(Value::as_str).unwrap_or("")
}

/// Integer under the object's top-level data, 0 when missing.
pub fn data_i64(o: &DynamicObject, path: &str) -> i64 {
    data_get(o, path).and_then(Value::as_i64).unwrap_or(0)
}

/// Bool under the object's top-level data, false when missing.
pub fn data_bool(o: &DynamicObject, path: &str) -> bool {
    data_get(o, path).and_then(Value::as_bool).unwrap_or(false)
}

/// The whole object as JSON (apiVersion, kind, metadata and data).
pub fn obj_to_value(o: &DynamicObject) -> Value {
    serde_json::to_value(o).unwrap_or(Value::Null)
}

/// Value at any path of the object, including `metadata.*` (owned copy).
pub fn obj_get(o: &DynamicObject, path: &str) -> Option<Value> {
    let p = path.trim_start_matches('.');
    if p == "metadata" || p.starts_with("metadata.") || p.starts_with("metadata[") {
        let meta = serde_json::to_value(&o.metadata).ok()?;
        let rest = p.strip_prefix("metadata").unwrap_or("");
        return get_path(&meta, rest).cloned();
    }
    data_get(o, p).cloned()
}

/// Go `fmt.Sprintf("%v", v)` of a decoded JSON value (unstructured content):
/// strings raw, maps as `map[k:v]` (sorted keys), arrays as `[a b]`, null as
/// `<nil>`, floats in Go's shortest `%v` form.
pub fn go_fmt(v: &Value) -> String {
    match v {
        Value::Null => "<nil>".into(),
        Value::Bool(b) => b.to_string(),
        Value::String(s) => s.clone(),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.to_string()
            } else if let Some(u) = n.as_u64() {
                u.to_string()
            } else {
                go_fmt_float(n.as_f64().unwrap_or(0.0))
            }
        }
        Value::Array(a) => format!("[{}]", a.iter().map(go_fmt).collect::<Vec<_>>().join(" ")),
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            format!(
                "map[{}]",
                keys.iter()
                    .map(|k| format!("{k}:{}", go_fmt(&m[*k])))
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        }
    }
}

/// Go `%v` of a float64 (strconv 'g' shortest: exponent form when exp < -4 || exp >= 6).
fn go_fmt_float(f: f64) -> String {
    if f.is_nan() {
        return "NaN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "+Inf" } else { "-Inf" }.into();
    }
    if f == 0.0 {
        return "0".into();
    }
    // Shortest round-trip digits via Rust's `{:e}` formatting.
    let e = format!("{f:e}");
    let (mant, exp) = e.split_once('e').unwrap_or((&e, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    if !(-4..6).contains(&exp) {
        let sign = if exp < 0 { '-' } else { '+' };
        return format!("{mant}e{sign}{:02}", exp.abs());
    }
    format!("{f}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    fn t(s: &str) -> DateTime<Utc> {
        parse_time(s).unwrap()
    }

    #[test]
    fn human_durations() {
        let base = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
        let cases: &[(i64, &str)] = &[
            (-5, "<invalid>"),
            (-1, "0s"),
            (0, "0s"),
            (10, "10s"),
            (119, "119s"),
            (120, "2m"),
            (125, "2m5s"),
            (9 * 60 + 59, "9m59s"),
            (10 * 60 + 30, "10m"),
            (179 * 60, "179m"),
            (3 * 3600, "3h"),
            (3 * 3600 + 25 * 60, "3h25m"),
            (8 * 3600 + 5 * 60, "8h"),
            (47 * 3600, "47h"),
            (48 * 3600, "2d"),
            (50 * 3600, "2d2h"),
            (8 * 86400, "8d"),
            (729 * 86400, "729d"),
            (730 * 86400, "2y"),
            (731 * 86400, "2y1d"),
            (8 * 365 * 86400 + 86400 * 10, "8y"),
        ];
        for (secs, e) in cases {
            let ts = base - chrono::Duration::seconds(*secs);
            assert_eq!(to_age(ts, base), *e, "{secs}s");
        }
    }

    #[test]
    fn ages_with_pinned_now() {
        let n = t("2020-01-01T00:00:10Z");
        with_now(n, || {
            assert_eq!(to_age_opt(None), UNKNOWN_VALUE);
            assert_eq!(to_age_opt(Some(t("2020-01-01T00:00:00Z"))), "10s");
            assert_eq!(to_age_human(""), UNKNOWN_VALUE);
            assert_eq!(to_age_human("2020-01-01T00:00:00.123Z"), "9s");
            assert_eq!(to_age_human("blee"), NA_VALUE);
            assert_eq!(age_decorator("2020-01-01T00:00:00Z"), "10s");
            assert_eq!(age_decorator("5d"), "5d");
            assert_eq!(age_decorator(""), UNKNOWN_VALUE);
            assert_eq!(time_to_age(t("2019-12-31T23:59:00Z")), "70s");
        });
        set_now(None);
    }

    #[test]
    fn join_blank() {
        let e: [&str; 0] = [];
        assert_eq!(join(&e, ","), "");
        assert_eq!(join(&["a", "b", "c"], ","), "a,b,c");
        assert_eq!(join(&["", "", ""], ","), "");
        assert_eq!(join(&["a", "", "c"], ","), "a,c");
        assert_eq!(join(&["", "a", "c"], ","), "a,c");
        assert!(blank(&e));
        assert!(!blank(&["fred", "blee"]));
        assert!(!blank(&["fred", ""]));
        assert!(blank(&["", ""]));
    }

    #[test]
    fn small_helpers() {
        assert_eq!(bool_ptr_to_str(None), "false");
        assert_eq!(bool_ptr_to_str(Some(true)), "true");
        assert_eq!(bool_ptr_to_str(Some(false)), "false");
        assert_eq!(bool_to_str(true), "true");
        assert_eq!(missing("fred"), "fred");
        assert_eq!(missing(""), MISSING_VALUE);
        assert_eq!(na("fred"), "fred");
        assert_eq!(na(""), NA_VALUE);
        assert_eq!(na_strings::<&str>(&[]), NA_VALUE);
        assert_eq!(na_strings(&["a", "b"]), "a,b");
        assert_eq!(int_to_str(0), "0");
        assert_eq!(int_to_str(10), "10");
        assert_eq!(as_perc("10"), "(10)");
        assert_eq!(print_perc(10), "10%");
        assert_eq!(as_status::<String>(Ok(())), "");
        assert_eq!(as_status(Err("boom")), "boom");
        assert_eq!(str_ptr_to_str(None), "");
        assert_eq!(str_ptr_to_str(Some("x")), "x");
        assert_eq!(bool_value_to_str(Some(&json!(true))), "true");
        assert_eq!(bool_value_to_str(None), "false");
    }

    #[test]
    fn thousands() {
        assert_eq!(as_thousands(0), "0");
        assert_eq!(as_thousands(999), "999");
        assert_eq!(as_thousands(1000), "1,000");
        assert_eq!(as_thousands(1234567), "1,234,567");
        assert_eq!(as_thousands(-1234), "-1,234");
    }

    #[test]
    fn truncate_pad() {
        assert_eq!(truncate("fred", 4), "fred");
        assert_eq!(truncate("fred", 10), "fred");
        assert_eq!(truncate("fred", 3), "fr…");
        assert_eq!(pad("fred", 6), "fred  ");
        assert_eq!(pad("fred", 4), "fred");
        assert_eq!(pad("fred", 3), "fr…");
    }

    #[test]
    fn runes() {
        assert_eq!(runes_to_num(""), 0);
        assert_eq!(runes_to_num("100"), 100);
        assert_eq!(runes_to_num("64"), 64);
        assert_eq!(runes_to_num("52640"), 52640);
    }

    #[test]
    fn maps() {
        let m: BTreeMap<String, String> = [("blee", "duh"), ("aa", "bb")]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        assert_eq!(map_to_str(m.iter()), "aa=bb,blee=duh");
        assert_eq!(map_to_str(BTreeMap::new().iter()), "");
        let hm: std::collections::HashMap<String, String> =
            m.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        assert_eq!(map_to_str(hm.iter()), "aa=bb,blee=duh");
        assert_eq!(to_selector(m.iter()), "aa=bb,blee=duh");
        assert_eq!(map_to_ifc(Some(&json!({"b": "2", "a": "1"}))), "a=1 b=2");
        assert_eq!(map_to_ifc(Some(&json!({}))), "");
        assert_eq!(map_to_ifc(None), "");
        assert_eq!(map_to_ifc(Some(&json!("x"))), "");
    }

    #[test]
    fn selectors() {
        assert_eq!(label_selector_to_str(None), "");
        assert_eq!(label_selector_to_str(Some(&json!({}))), "");
        assert_eq!(
            label_selector_to_str(Some(&json!({"matchLabels": {"b": "2", "a": "1"}}))),
            "a=1,b=2"
        );
        assert_eq!(
            label_selector_to_str(Some(&json!({
                "matchLabels": {"app": "x"},
                "matchExpressions": [
                    {"key": "env", "operator": "In", "values": ["prod", "dev"]},
                    {"key": "tier", "operator": "NotIn", "values": ["fe"]},
                    {"key": "a", "operator": "Exists"},
                    {"key": "z", "operator": "DoesNotExist"}
                ]
            }))),
            "a,app=x,env in (dev,prod),tier notin (fe),!z"
        );
    }

    #[test]
    fn quantities() {
        let q = |s: &str| parse_quantity(s).unwrap();
        assert_eq!(q("100m").milli_value(), 100);
        assert_eq!(q("100m").value(), 1);
        assert_eq!(q("4").milli_value(), 4000);
        assert_eq!(q("1.5").milli_value(), 1500);
        assert_eq!(q("50Mi").value(), 50 * MEGA_BYTE);
        assert_eq!(q("8063156Ki").value(), 8063156 * 1024);
        assert_eq!(q("1G").value(), 1_000_000_000);
        assert_eq!(q("1e3").value(), 1000);
        assert_eq!(q("250u").milli_value(), 1);
        assert_eq!(q("1500n").value(), 1);
        assert_eq!(q("-1").value(), -1);
        assert_eq!(q("0").value(), 0);
        assert_eq!(q("1Ei").value(), 1 << 60);
        assert!(parse_quantity("").is_none());
        assert!(parse_quantity("abc").is_none());
        assert!(parse_quantity("1X").is_none());
        assert_eq!(value_quantity(Some(&json!(2))).milli_value(), 2000);
        assert_eq!(value_quantity(None).value(), 0);
        let rl = json!({"cpu": "250m", "memory": "64Mi", "nvidia.com/gpu": "2"});
        assert_eq!(res_milli(Some(&rl), "cpu"), 250);
        assert_eq!(res_value(Some(&rl), "memory"), 64 * MEGA_BYTE);
        assert_eq!(extract_gpu(Some(&rl)), 2);
        assert_eq!(extract_gpu(None), 0);
        let nl = json!({"nvidia.com/gpu": "4", "nvidia.com/gpu.shared": "8"});
        assert_eq!(extract_node_gpu(Some(&nl)), (Some(4), Some(8)));
    }

    #[test]
    fn metrics_formatting() {
        assert_eq!(to_mc(0), "0");
        assert_eq!(to_mc(2), "2");
        assert_eq!(to_mc(1000), "1000");
        assert_eq!(to_mi(0), "0");
        assert_eq!(to_mi(2 * MEGA_BYTE), "2");
        assert_eq!(to_mi(1000 * MEGA_BYTE), "1000");
        assert_eq!(to_mu(0), NA_VALUE);
        assert_eq!(to_mu(3), "3");
        assert_eq!(to_percentage(50, 70), 71);
        assert_eq!(to_percentage(1, 0), 0);
        assert_eq!(to_percentage_str(1, 0), NA_VALUE);
        assert_eq!(to_percentage_str(50, 170), "29");
        assert_eq!(mx_perc(10, 20), "50");
    }

    #[test]
    fn json_getters() {
        let v = json!({"spec": {"containers": [{"name": "a"}, {"name": "b"}], "n": 3, "ok": true}});
        assert_eq!(str_at(&v, "spec.containers.1.name"), "b");
        assert_eq!(str_at(&v, "spec.containers[0].name"), "a");
        assert_eq!(str_at(&v, ".spec.containers[5].name"), "");
        assert_eq!(i64_at(&v, "spec.n"), 3);
        assert_eq!(opt_i64_at(&v, "spec.x"), None);
        assert!(bool_at(&v, "spec.ok"));
        assert_eq!(array_at(&v, "spec.containers").len(), 2);
        assert!(array_at(&v, "spec.nope").is_empty());
        assert_eq!(get_path(&v, ""), Some(&v));

        let o: DynamicObject = serde_json::from_value(json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": {"name": "p", "namespace": "ns", "labels": {"b": "2", "a": "1"},
                         "creationTimestamp": "2020-01-01T00:00:00Z"},
            "spec": {"nodeName": "n1", "replicas": 2, "x": true}
        }))
        .unwrap();
        assert_eq!(data_str(&o, "spec.nodeName"), "n1");
        assert_eq!(data_i64(&o, "spec.replicas"), 2);
        assert!(data_bool(&o, "spec.x"));
        assert_eq!(obj_get(&o, "metadata.name"), Some(json!("p")));
        assert_eq!(obj_get(&o, ".spec.nodeName"), Some(json!("n1")));
        assert_eq!(labels_str(&o), "a=1,b=2");
        assert_eq!(obj_fqn(&o), "ns/p");
        assert_eq!(creation_timestamp(&o), Some(t("2020-01-01T00:00:00Z")));
        assert_eq!(deletion_timestamp(&o), None);
        with_now(t("2020-01-03T00:00:00Z"), || assert_eq!(obj_age(&o), "2d"));
        assert_eq!(obj_to_value(&o)["metadata"]["name"], json!("p"));
        str_map_at(&obj_to_value(&o), "metadata.labels")
            .get("a")
            .expect("label");
    }

    #[test]
    fn go_formatting() {
        assert_eq!(go_fmt(&json!(null)), "<nil>");
        assert_eq!(go_fmt(&json!("s")), "s");
        assert_eq!(go_fmt(&json!(12)), "12");
        assert_eq!(go_fmt(&json!(1.5)), "1.5");
        assert_eq!(go_fmt(&json!(1.0)), "1");
        assert_eq!(go_fmt(&json!(1e21)), "1e+21");
        assert_eq!(go_fmt(&json!(1e6)), "1e+06");
        assert_eq!(go_fmt(&json!(123456.5)), "123456.5");
        assert_eq!(go_fmt(&json!(0.00001)), "1e-05");
        assert_eq!(go_fmt(&json!([1, "a"])), "[1 a]");
        assert_eq!(
            go_fmt(&json!({"b": 1, "a": {"c": "d"}})),
            "map[a:map[c:d] b:1]"
        );
        assert_eq!(go_fmt(&json!(true)), "true");
    }

    #[test]
    fn cluster_wide() {
        assert!(is_cluster_wide("all"));
        assert!(is_cluster_wide(""));
        assert!(is_cluster_wide("-"));
        assert!(!is_cluster_wide("default"));
    }
}
