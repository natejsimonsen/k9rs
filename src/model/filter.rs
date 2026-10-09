//! Row filtering (k9s `/` prompt): regex, fuzzy (`-f`), label (`-l`), inverse (`!`).
//!
//! Mirrors k9s `model1.TableData.Filter` and the selector helpers in
//! `internal/helpers.go`:
//! - a label selector (`-l ...`, or a bare `k=v[,k=v]` without spaces) is
//!   applied server side, so [`apply_filter`] leaves rows untouched;
//! - `-f word` fuzzy matches row ids (`ns/name`) and orders rows by score;
//! - anything else is a case-insensitive regex over the joined visible
//!   fields (AGE, hidden columns and NAMESPACE in a namespaced view excluded);
//!   `!` inverts. A regex containing a space, or one that fails to compile,
//!   leaves rows untouched (k9s logs and keeps the unfiltered rows).

#[path = "fuzzy.rs"]
pub mod fuzzy;
#[path = "labels.rs"]
pub mod labels;

use std::collections::BTreeMap;
use std::sync::OnceLock;

use regex::Regex;

use super::{Header, TableData};
use crate::k8s::{is_all_namespaces, is_cluster_scoped};

/// A parsed filter expression typed in the `/` prompt.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum FilterSpec {
    #[default]
    None,
    /// Plain regex (case-insensitive) matched against every visible field.
    Regex(String),
    /// Inverse regex: rows that do NOT match (`!foo`). Holds the regex without `!`.
    InverseRegex(String),
    /// Fuzzy match (`-f foo`). Holds the fuzzy word.
    Fuzzy(String),
    /// Label selector (`-l app=foo`), applied server-side. Holds the selector
    /// text with any `-l` prefix stripped.
    Label(String),
}

impl FilterSpec {
    /// Parse the text typed in the filter prompt.
    pub fn parse(input: &str) -> FilterSpec {
        if input.is_empty() {
            return FilterSpec::None;
        }
        if is_label_selector(input) {
            return FilterSpec::Label(trim_label_flag(input).to_string());
        }
        if let Some(f) = is_fuzzy_selector(input) {
            return FilterSpec::Fuzzy(f);
        }
        if is_inverse_selector(input) {
            return FilterSpec::InverseRegex(input[1..].to_string());
        }
        FilterSpec::Regex(input.to_string())
    }

    pub fn is_none(&self) -> bool {
        matches!(self, FilterSpec::None)
    }

    /// The label selector text, when this is a label filter.
    pub fn label_selector(&self) -> Option<&str> {
        match self {
            FilterSpec::Label(s) => Some(s),
            _ => None,
        }
    }

    /// The parsed label selector (ui.ExtractLabelSelector), when this is a
    /// label filter. Errors carry the apimachinery parser message.
    pub fn selector(&self) -> Option<Result<labels::Selector, String>> {
        self.label_selector().map(labels::parse_selector)
    }
}

fn fuzzy_rx() -> &'static Regex {
    static RX: OnceLock<Regex> = OnceLock::new();
    // Go: `\A-f\s?([\w-]+)\b` with ASCII classes.
    RX.get_or_init(|| Regex::new(r"\A-f[\t\n\x0C\r ]?([0-9A-Za-z_-]+)(?-u:\b)").unwrap())
}

/// internal.IsInverseSelector: query starts with `!`.
pub fn is_inverse_selector(s: &str) -> bool {
    s.starts_with('!')
}

/// internal.IsLabelSelector: `-l` prefix, or a space-free `k=v` list.
pub fn is_label_selector(s: &str) -> bool {
    if s.starts_with("-l") {
        return true;
    }
    !s.contains(' ') && to_labels(s).is_some()
}

/// internal.IsFuzzySelector: returns the fuzzy word of `-f word`.
pub fn is_fuzzy_selector(s: &str) -> Option<String> {
    fuzzy_rx()
        .captures(s)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

fn trim_label_flag(s: &str) -> &str {
    match s.strip_prefix("-l") {
        Some(rest) => rest.trim(),
        None => s,
    }
}

/// ui.ExtractLabelSelector: parse `[-l ]selector`.
pub fn extract_label_selector(s: &str) -> Result<labels::Selector, String> {
    labels::parse_selector(trim_label_flag(s))
}

/// cmd.ToLabels: parse `k=v,k2==v2,k3!=v3` into a map; `None` when no pair parses.
pub fn to_labels(s: &str) -> Option<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for l in s.split(',') {
        if let Some((k, v)) = split_kv(l) {
            out.insert(k, v);
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn split_kv(s: &str) -> Option<(String, String)> {
    let sep = if s.contains("!=") {
        "!="
    } else if s.contains("==") {
        "=="
    } else if s.contains('=') {
        "="
    } else {
        return None;
    };
    let (k, v) = s.split_once(sep)?;
    if k.is_empty() || v.is_empty() {
        return None;
    }
    Some((k.trim().to_string(), v.trim().to_string()))
}

/// Header.FilterColIndices(ns, wide=true): columns searched by regex filters.
pub fn filter_col_indices(h: &Header, ns: &str) -> Vec<usize> {
    let nsed = !is_all_namespaces(ns) && !is_cluster_scoped(ns);
    h.iter()
        .enumerate()
        .filter(|(_, c)| !(c.name == "AGE" || c.hide || (nsed && c.name == "NAMESPACE")))
        .map(|(i, _)| i)
        .collect()
}

/// Apply `spec` to `data`, returning the matching rows (header kept).
pub fn apply_filter(data: &TableData, spec: &FilterSpec) -> TableData {
    match spec {
        FilterSpec::None | FilterSpec::Label(_) => data.clone(),
        FilterSpec::Fuzzy(q) => fuzzy_filter(data, q),
        FilterSpec::Regex(q) => rx_filter(data, q, false).unwrap_or_else(|| data.clone()),
        FilterSpec::InverseRegex(q) => rx_filter(data, q, true).unwrap_or_else(|| data.clone()),
    }
}

/// Keep only rows whose VALID column is non-blank (k9s toast filter, `ctrl-z`).
/// No VALID column yields no rows, as in k9s.
pub fn filter_toast(data: &TableData) -> TableData {
    let mut out = TableData {
        rows: Vec::new(),
        ..data.clone()
    };
    let Some(idx) = data.header.iter().position(|c| c.name == "VALID") else {
        return out;
    };
    out.rows = data
        .rows
        .iter()
        .filter(|re| re.row.fields.get(idx).is_some_and(|f| !f.is_empty()))
        .cloned()
        .collect();
    out
}

fn rx_filter(data: &TableData, q: &str, inverse: bool) -> Option<TableData> {
    if q.contains(' ') {
        return Some(data.clone());
    }
    let rx = match Regex::new(&format!("(?i)({q})")) {
        Ok(rx) => rx,
        Err(e) => {
            tracing::error!("RX filter failed: invalid rx filter {q:?}: {e}");
            return None;
        }
    };
    let vidx = if data.header.is_empty() {
        Vec::new()
    } else {
        filter_col_indices(&data.header, &data.namespace)
    };
    let rows = data
        .rows
        .iter()
        .filter(|re| {
            let joined = re
                .row
                .fields
                .iter()
                .enumerate()
                .filter(|(i, _)| vidx.contains(i))
                .map(|(_, f)| f.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            rx.is_match(&joined) != inverse
        })
        .cloned()
        .collect();
    Some(TableData {
        rows,
        ..data.clone_empty()
    })
}

fn fuzzy_filter(data: &TableData, q: &str) -> TableData {
    let q = q.trim();
    let ids: Vec<&str> = data.rows.iter().map(|re| re.row.id.as_str()).collect();
    let rows = fuzzy::find(q, &ids)
        .into_iter()
        .filter_map(|m| data.rows.get(m.index).cloned())
        .collect();
    TableData {
        rows,
        ..data.clone_empty()
    }
}

trait CloneEmpty {
    fn clone_empty(&self) -> TableData;
}

impl CloneEmpty for TableData {
    fn clone_empty(&self) -> TableData {
        TableData {
            header: self.header.clone(),
            rows: Vec::new(),
            namespace: self.namespace.clone(),
            gvr: self.gvr.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::k8s::Gvr;
    use crate::model::{HeaderColumn, ResEvent, Row, RowEvent};

    fn table(ns: &str) -> TableData {
        let h = Header::new(vec![
            HeaderColumn::new("NAMESPACE"),
            HeaderColumn::new("NAME"),
            HeaderColumn::new("STATUS"),
            HeaderColumn::new("IP").wide(),
            HeaderColumn::new("SECRET").hide(),
            HeaderColumn::new("AGE").time(),
        ]);
        let mut td = TableData::new(Gvr::parse("v1/pods"), h, ns);
        for (ns, n, st, ip, sec, age) in [
            ("default", "fred-1", "Running", "10.0.0.1", "hush", "2m"),
            ("default", "blee", "Error", "10.0.0.2", "hush", "5m"),
            (
                "kube-system",
                "zorg-fred",
                "Running",
                "10.0.0.3",
                "hush",
                "1h",
            ),
            (
                "kube-system",
                "coredns",
                "Pending",
                "10.0.0.4",
                "hush",
                "3d",
            ),
        ] {
            td.rows.push(RowEvent::new(
                ResEvent::Unchanged,
                Row::with(
                    format!("{ns}/{n}"),
                    [ns, n, st, ip, sec, age]
                        .iter()
                        .map(|s| s.to_string())
                        .collect(),
                ),
            ));
        }
        td
    }

    fn names(td: &TableData) -> Vec<&str> {
        td.rows.iter().map(|r| r.row.fields[1].as_str()).collect()
    }

    #[test]
    fn parse_specs() {
        let cases = [
            ("", FilterSpec::None),
            ("fred", FilterSpec::Regex("fred".into())),
            ("!fred", FilterSpec::InverseRegex("fred".into())),
            ("-f fred", FilterSpec::Fuzzy("fred".into())),
            ("-ffred", FilterSpec::Fuzzy("fred".into())),
            ("-f fred blee", FilterSpec::Fuzzy("fred".into())),
            ("-f", FilterSpec::Regex("-f".into())),
            ("-l app=fred", FilterSpec::Label("app=fred".into())),
            (
                "-lapp=fred,env=blee",
                FilterSpec::Label("app=fred,env=blee".into()),
            ),
            (
                "app=fred,env=blee",
                FilterSpec::Label("app=fred,env=blee".into()),
            ),
            ("-l app in (a,b)", FilterSpec::Label("app in (a,b)".into())),
            ("=fred", FilterSpec::Regex("=fred".into())),
            ("fred=", FilterSpec::Regex("fred=".into())),
            ("a = b", FilterSpec::Regex("a = b".into())),
        ];
        for (s, e) in cases {
            assert_eq!(FilterSpec::parse(s), e, "{s:?}");
        }
    }

    #[test]
    fn is_label_selector_cases() {
        // internal/helpers_test.go TestIsLabelSelector
        let cases = [
            ("", false),
            ("-l app=fred,env=blee", true),
            ("app=fred,env=blee", true),
            ("-lapp=fred,env=blee", true),
            ("-f app=fred,env=blee", false),
            ("=fred", false),
            ("fred=", false),
        ];
        for (s, ok) in cases {
            assert_eq!(is_label_selector(s), ok, "{s:?}");
        }
    }

    #[test]
    fn to_labels_cases() {
        // view/cmd/helpers_test.go Test_toLabels
        let m = |pairs: &[(&str, &str)]| {
            Some(
                pairs
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect::<BTreeMap<_, _>>(),
            )
        };
        assert_eq!(to_labels(""), None);
        assert_eq!(to_labels("="), None);
        assert_eq!(to_labels("=,"), None);
        assert_eq!(to_labels(","), None);
        assert_eq!(to_labels(",="), None);
        assert_eq!(to_labels("a=b"), m(&[("a", "b")]));
        assert_eq!(to_labels("a=b,c=d"), m(&[("a", "b"), ("c", "d")]));
        assert_eq!(to_labels("a=,c=d"), m(&[("c", "d")]));
        assert_eq!(to_labels("a=b,=d"), m(&[("a", "b")]));
        assert_eq!(to_labels("a!=b,c==d"), m(&[("a", "b"), ("c", "d")]));
    }

    #[test]
    fn regex_filter_all_namespaces() {
        let td = table("");
        assert_eq!(
            names(&apply_filter(&td, &FilterSpec::parse("fred"))),
            vec!["fred-1", "zorg-fred"]
        );
        assert_eq!(
            names(&apply_filter(&td, &FilterSpec::parse("FRED"))),
            vec!["fred-1", "zorg-fred"]
        );
        assert_eq!(
            names(&apply_filter(&td, &FilterSpec::parse("!fred"))),
            vec!["blee", "coredns"]
        );
        // NAMESPACE is searched in all-namespaces mode.
        assert_eq!(
            names(&apply_filter(&td, &FilterSpec::parse("kube-sys"))),
            vec!["zorg-fred", "coredns"]
        );
        // Wide columns are searched.
        assert_eq!(
            names(&apply_filter(&td, &FilterSpec::parse(r"0\.0\.2"))),
            vec!["blee"]
        );
        // AGE and hidden columns are not.
        assert!(apply_filter(&td, &FilterSpec::parse("3d")).rows.is_empty());
        assert!(apply_filter(&td, &FilterSpec::parse("hush"))
            .rows
            .is_empty());
        // Fields are joined with a space.
        assert_eq!(
            names(&apply_filter(&td, &FilterSpec::parse("blee.Error"))),
            vec!["blee"]
        );
        assert_eq!(
            names(&apply_filter(&td, &FilterSpec::parse("Run|Pend"))),
            vec!["fred-1", "zorg-fred", "coredns"]
        );
    }

    #[test]
    fn regex_filter_namespaced_skips_namespace_col() {
        let td = table("kube-system");
        assert!(apply_filter(&td, &FilterSpec::parse("kube-sys"))
            .rows
            .is_empty());
        let td = table("all");
        assert_eq!(
            apply_filter(&td, &FilterSpec::parse("kube-sys")).rows.len(),
            2
        );
        let td = table("-");
        assert_eq!(
            apply_filter(&td, &FilterSpec::parse("kube-sys")).rows.len(),
            2
        );
    }

    #[test]
    fn regex_filter_fallbacks() {
        let td = table("");
        // Invalid regex leaves rows unfiltered.
        assert_eq!(apply_filter(&td, &FilterSpec::parse("fred(")).rows.len(), 4);
        // Spaces disable the regex filter.
        assert_eq!(
            apply_filter(&td, &FilterSpec::parse("fred blee"))
                .rows
                .len(),
            4
        );
        // Label selectors are server side.
        assert_eq!(
            apply_filter(&td, &FilterSpec::parse("app=fred")).rows.len(),
            4
        );
        assert_eq!(apply_filter(&td, &FilterSpec::None), td);
        // Header is preserved.
        assert_eq!(
            apply_filter(&td, &FilterSpec::parse("zzz")).header,
            td.header
        );
    }

    #[test]
    fn fuzzy_filter_on_ids() {
        let td = table("");
        let out = apply_filter(&td, &FilterSpec::parse("-f fred"));
        assert_eq!(names(&out), vec!["fred-1", "zorg-fred"]);
        let out = apply_filter(&td, &FilterSpec::parse("-f kcd"));
        assert_eq!(names(&out), vec!["coredns"]);
        let out = apply_filter(&td, &FilterSpec::parse("-f zzz"));
        assert!(out.rows.is_empty());
        assert_eq!(out.header, td.header);
    }

    #[test]
    fn label_selector_extraction() {
        let s = FilterSpec::parse("-l app=fred,env in (a,b)");
        assert_eq!(s.label_selector(), Some("app=fred,env in (a,b)"));
        assert_eq!(
            s.selector().unwrap().unwrap().to_string(),
            "app=fred,env in (a,b)"
        );
        assert!(FilterSpec::parse("fred").selector().is_none());
        assert_eq!(
            extract_label_selector("-l  a=b").unwrap().to_string(),
            "a=b"
        );
        assert_eq!(extract_label_selector("a=b").unwrap().to_string(), "a=b");
        assert!(extract_label_selector("-l =b").is_err());
    }

    #[test]
    fn toast_filter() {
        let h = Header::new(vec![HeaderColumn::new("NAME"), HeaderColumn::new("VALID")]);
        let mut td = TableData::new(Gvr::parse("v1/pods"), h, "");
        td.rows.push(RowEvent::new(
            ResEvent::Add,
            Row::with("a", vec!["a".into(), "".into()]),
        ));
        td.rows.push(RowEvent::new(
            ResEvent::Add,
            Row::with("b", vec!["b".into(), "bad".into()]),
        ));
        let out = filter_toast(&td);
        assert_eq!(out.rows.len(), 1);
        assert_eq!(out.rows[0].row.id, "b");
        assert!(filter_toast(&table("")).rows.is_empty());
    }
}
