//! Workload renderer helpers that `render::helpers` does not have: a few
//! optional JSON getters, pod spec container lists, the validating label
//! selector conversion, Go's `LabelSelectorRequirement.String()` and
//! `resource.Quantity.String()`. Paths are dotted, like `render::helpers`.

use kube::api::DynamicObject;
use serde_json::Value;

use crate::render::helpers::{array_at, get_path, label_selector_to_str, str_at, NA_VALUE};

/// Annotation `key` of an object (keys hold dots and slashes, so no path).
pub(super) fn annotation<'a>(o: &'a DynamicObject, key: &str) -> Option<&'a str> {
    o.metadata
        .annotations
        .as_ref()
        .and_then(|m| m.get(key))
        .map(String::as_str)
}

/// String at `path`, `None` when missing or not a string.
pub(super) fn opt_str_at<'a>(v: &'a Value, path: &str) -> Option<&'a str> {
    get_path(v, path).and_then(Value::as_str)
}

/// Bool at `path`, `None` when missing or not a bool.
pub(super) fn opt_bool_at(v: &Value, path: &str) -> Option<bool> {
    get_path(v, path).and_then(Value::as_bool)
}

/// String items of the array at `path` (non strings skipped).
pub(super) fn strs_at(v: &Value, path: &str) -> Vec<String> {
    array_at(v, path)
        .iter()
        .filter_map(|x| x.as_str().map(String::from))
        .collect()
}

/// Number of keys of the object at `path`, 0 when missing.
pub(super) fn obj_len_at(v: &Value, path: &str) -> usize {
    get_path(v, path)
        .and_then(Value::as_object)
        .map(|m| m.len())
        .unwrap_or(0)
}

/// Container names / images of a pod spec, init containers first if asked
/// (k9s podContainerNames / podImageNames).
pub(super) fn pod_names_images(spec: &Value, include_init: bool) -> (String, String) {
    let mut names = Vec::new();
    let mut images = Vec::new();
    let mut add = |p: &str| {
        for c in array_at(spec, p) {
            names.push(str_at(c, "name").to_string());
            images.push(str_at(c, "image").to_string());
        }
    };
    if include_init {
        add("initContainers");
    }
    add("containers");
    (names.join(","), images.join(","))
}

/// k9s asSelector: metav1.LabelSelectorAsSelector(s).String(), `n/a` when
/// the conversion fails. Like labels.NewRequirement, `In`/`NotIn` need
/// values and `Exists`/`DoesNotExist` must have none.
pub(super) fn as_selector(sel: Option<&Value>) -> String {
    let Some(sel) = sel.filter(|v| !v.is_null()) else {
        return String::new();
    };
    for e in array_at(sel, "matchExpressions") {
        let has_vals = !array_at(e, "values").is_empty();
        let ok = match str_at(e, "operator") {
            "In" | "NotIn" => has_vals,
            "Exists" | "DoesNotExist" => !has_vals,
            _ => false,
        };
        if !ok {
            return NA_VALUE.into();
        }
    }
    label_selector_to_str(Some(sel))
}

/// Go's generated LabelSelectorRequirement.String().
pub(super) fn requirement_go_string(e: &Value) -> String {
    format!(
        "&LabelSelectorRequirement{{Key:{},Operator:{},Values:[{}],}}",
        str_at(e, "key"),
        str_at(e, "operator"),
        strs_at(e, "values").join(" ")
    )
}

// Quantities -----------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum QFormat {
    DecimalSi,
    BinarySi,
    DecimalExponent,
}

/// Parse a resource quantity into (nano units, format), rounding up to nano.
fn parse_quantity(q: &str) -> Option<(i128, QFormat)> {
    let q = q.trim();
    if q.is_empty() {
        return None;
    }
    let (neg, rest) = match q.as_bytes()[0] {
        b'-' => (true, &q[1..]),
        b'+' => (false, &q[1..]),
        _ => (false, q),
    };
    let num_end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(rest.len());
    let (num, suffix) = rest.split_at(num_end);
    if num.is_empty() || num == "." || num.matches('.').count() > 1 {
        return None;
    }
    let (int_part, frac_part) = num.split_once('.').unwrap_or((num, ""));
    let digits = format!("{int_part}{frac_part}");
    let mantissa: i128 = digits.parse().ok()?;
    let scale = frac_part.len() as i32;
    // value = mantissa * 10^-scale * mult
    let (base2, exp10, format) = match suffix {
        "Ki" => (10, 0, QFormat::BinarySi),
        "Mi" => (20, 0, QFormat::BinarySi),
        "Gi" => (30, 0, QFormat::BinarySi),
        "Ti" => (40, 0, QFormat::BinarySi),
        "Pi" => (50, 0, QFormat::BinarySi),
        "Ei" => (60, 0, QFormat::BinarySi),
        "n" => (0, -9, QFormat::DecimalSi),
        "u" => (0, -6, QFormat::DecimalSi),
        "m" => (0, -3, QFormat::DecimalSi),
        "" => (0, 0, QFormat::DecimalSi),
        "k" => (0, 3, QFormat::DecimalSi),
        "M" => (0, 6, QFormat::DecimalSi),
        "G" => (0, 9, QFormat::DecimalSi),
        "T" => (0, 12, QFormat::DecimalSi),
        "P" => (0, 15, QFormat::DecimalSi),
        "E" => (0, 18, QFormat::DecimalSi),
        s if s.starts_with(['e', 'E']) => {
            let e: i32 = s[1..].parse().ok()?;
            (0, e, QFormat::DecimalExponent)
        }
        _ => return None,
    };
    let mut numer = mantissa.checked_mul(1i128.checked_shl(base2)?)?;
    let mut denom: i128 = 1;
    let p = exp10 + 9 - scale;
    if p >= 0 {
        numer = numer.checked_mul(10i128.checked_pow(p as u32)?)?;
    } else {
        denom = 10i128.checked_pow((-p) as u32)?;
    }
    let mut nano = numer / denom;
    if numer % denom != 0 {
        nano += 1;
    }
    Some((if neg { -nano } else { nano }, format))
}

/// Canonical quantity string (resource.Quantity.String()). Unparsable input
/// is returned as-is.
pub(super) fn canonical_quantity(q: &str) -> String {
    let Some((nano, mut format)) = parse_quantity(q) else {
        return q.to_string();
    };
    if nano == 0 {
        return "0".into();
    }
    const NANO: i128 = 1_000_000_000;
    if format == QFormat::BinarySi {
        // Small or fractional binary values print as decimal SI.
        let small = nano > -1024 * NANO && nano < 1024 * NANO;
        if small || nano % NANO != 0 {
            format = QFormat::DecimalSi;
        }
    }
    if format == QFormat::BinarySi {
        let mut v = nano / NANO;
        let mut e = 0;
        while e < 6 && v % 1024 == 0 {
            v /= 1024;
            e += 1;
        }
        let suf = ["", "Ki", "Mi", "Gi", "Ti", "Pi", "Ei"][e];
        return format!("{v}{suf}");
    }
    let mut m = nano;
    let mut exp: i32 = -9;
    while m % 10 == 0 {
        m /= 10;
        exp += 1;
    }
    let r = exp.rem_euclid(3);
    for _ in 0..r {
        m *= 10;
    }
    exp -= r;
    let suf = match format {
        QFormat::DecimalExponent => {
            if exp == 0 {
                String::new()
            } else {
                format!("e{exp}")
            }
        }
        _ => match exp {
            -9 => "n".into(),
            -6 => "u".into(),
            -3 => "m".into(),
            0 => String::new(),
            3 => "k".into(),
            6 => "M".into(),
            9 => "G".into(),
            12 => "T".into(),
            15 => "P".into(),
            18 => "E".into(),
            e => format!("e{e}"),
        },
    };
    format!("{m}{suf}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::helpers::{
        as_status, bool_to_str, go_fmt, human_duration, map_to_str, missing, na, na_strings,
        parse_time, str_map_at, to_age, to_age_opt, truncate,
    };
    use chrono::Duration;
    use serde_json::json;

    #[test]
    fn human_durations() {
        let uu = [
            (-5, "<invalid>"),
            (-1, "0s"),
            (0, "0s"),
            (119, "119s"),
            (120, "2m"),
            (150, "2m30s"),
            (600, "10m"),
            (3 * 3600 - 1, "179m"),
            (3 * 3600, "3h"),
            (3 * 3600 + 120, "3h2m"),
            (8 * 3600, "8h"),
            (47 * 3600, "47h"),
            (48 * 3600, "2d"),
            (50 * 3600, "2d2h"),
            (8 * 86400, "8d"),
            (729 * 86400, "729d"),
            (730 * 86400, "2y"),
            (731 * 86400, "2y1d"),
            (8 * 365 * 86400, "8y"),
        ];
        for (secs, e) in uu {
            assert_eq!(human_duration(Duration::seconds(secs)), e, "{secs}");
        }
    }

    #[test]
    fn ages() {
        let now = parse_time("2020-01-01T00:00:10Z").unwrap();
        assert_eq!(to_age_opt(None), "<unknown>");
        assert_eq!(
            to_age(parse_time("2020-01-01T00:00:00Z").unwrap(), now),
            "10s"
        );
    }

    #[test]
    fn maps_and_strings() {
        let m: std::collections::BTreeMap<String, String> =
            [("b".into(), "2".into()), ("a".into(), "1".into())].into();
        assert_eq!(map_to_str(&m), "a=1,b=2");
        assert_eq!(map_to_str(&std::collections::BTreeMap::new()), "");
        assert_eq!(
            map_to_str(&str_map_at(&json!({"z": "1", "y": "2"}), "")),
            "y=2,z=1"
        );
        assert_eq!(missing(""), "<none>");
        assert_eq!(na(""), "n/a");
        assert_eq!(na("x"), "x");
        assert_eq!(na_strings::<String>(&[]), "n/a");
        assert_eq!(na_strings(&["a", "b"]), "a,b");
        assert_eq!(bool_to_str(true), "true");
        assert_eq!(as_status(Err("boom")), "boom");
        assert_eq!(as_status::<String>(Ok(())), "");
        assert_eq!(opt_str_at(&json!({"a": {"b": "x"}}), "a.b"), Some("x"));
        assert_eq!(opt_str_at(&json!({"a": 1}), "a"), None);
        assert_eq!(opt_bool_at(&json!({"a": true}), "a"), Some(true));
        assert_eq!(strs_at(&json!({"a": ["x", 1, "y"]}), "a"), ["x", "y"]);
        assert_eq!(obj_len_at(&json!({"a": {"x": 1, "y": 2}}), "a"), 2);
        assert_eq!(obj_len_at(&json!({}), "a"), 0);
    }

    #[test]
    fn truncation() {
        assert_eq!(truncate("hello", 5), "hello");
        assert_eq!(truncate("hello world", 5), "hell…");
        assert_eq!(truncate("", 3), "");
    }

    #[test]
    fn selectors() {
        assert_eq!(as_selector(None), "");
        assert_eq!(as_selector(Some(&json!({}))), "");
        assert_eq!(
            as_selector(Some(&json!({"matchLabels": {"b": "2", "a": "1"}}))),
            "a=1,b=2"
        );
        let sel = json!({
            "matchLabels": {"app": "x"},
            "matchExpressions": [
                {"key": "tier", "operator": "In", "values": ["web", "api"]},
                {"key": "env", "operator": "NotIn", "values": ["prod"]},
                {"key": "zz", "operator": "Exists"},
                {"key": "aa", "operator": "DoesNotExist"}
            ]
        });
        assert_eq!(
            as_selector(Some(&sel)),
            "!aa,app=x,env notin (prod),tier in (api,web),zz"
        );
        let bad = json!({"matchExpressions": [{"key": "a", "operator": "Bogus"}]});
        assert_eq!(as_selector(Some(&bad)), "n/a");
        let bad = json!({"matchExpressions": [{"key": "a", "operator": "In"}]});
        assert_eq!(as_selector(Some(&bad)), "n/a");
        let bad =
            json!({"matchExpressions": [{"key": "a", "operator": "Exists", "values": ["x"]}]});
        assert_eq!(as_selector(Some(&bad)), "n/a");
        assert_eq!(as_selector(Some(&Value::Null)), "");
        assert_eq!(
            requirement_go_string(&json!({"key": "k", "operator": "In", "values": ["a", "b"]})),
            "&LabelSelectorRequirement{Key:k,Operator:In,Values:[a b],}"
        );
    }

    #[test]
    fn quantities() {
        let uu = [
            ("1Gi", "1Gi"),
            ("1024Mi", "1Gi"),
            ("1.5Gi", "1536Mi"),
            ("0.5Ki", "512"),
            ("500Ki", "500Ki"),
            ("1000", "1k"),
            ("1500", "1500"),
            ("1.5", "1500m"),
            ("2G", "2G"),
            ("10e3", "10e3"),
            ("1e3", "1e3"),
            ("100M", "100M"),
            ("0", "0"),
            ("0Gi", "0"),
            ("-1Gi", "-1Gi"),
            ("bogus", "bogus"),
            ("", ""),
        ];
        for (q, e) in uu {
            assert_eq!(canonical_quantity(q), e, "{q}");
        }
    }

    #[test]
    fn int_or_strings() {
        // IntOrString values render through go_fmt (callers skip null).
        assert_eq!(go_fmt(&json!(5)), "5");
        assert_eq!(go_fmt(&json!("50%")), "50%");
    }
}
