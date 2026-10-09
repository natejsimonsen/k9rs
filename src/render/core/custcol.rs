//! Custom columns from views.yaml (k9s render/cust_col.go, cust_cols.go and
//! the column handling of render/base.go).
//!
//! A column spec is `NAME[:jsonpath][|FLAGS]` where FLAGS is up to three of
//! `N` (number: capacity + right aligned), `T` (time), `W` (wide), `S`
//! (show), `L`/`R` (align), `H` (hide). Columns without a JSONPath refer to
//! a column of the renderer's default header by name. Default columns not
//! listed are appended after the custom ones.
//!
//! JQ specs (`{.a | ... | ...}`) are not evaluated; such cells render `n/a`.

use std::sync::OnceLock;

use anyhow::{anyhow, Result};
use regex::Regex;
use serde_json::Value;

use super::jsonpath::JsonPath;
use crate::config::ViewSetting;
use crate::model::{Align, Header, HeaderColumn, Row};
use crate::render::helpers::{go_fmt, is_cluster_wide, parse_quantity, to_age_opt, to_mc, to_mi};
use crate::render::helpers::{MISSING_VALUE, NA_VALUE};

fn full_rx() -> &'static Regex {
    static RX: OnceLock<Regex> = OnceLock::new();
    RX.get_or_init(|| {
        Regex::new(r"^([A-Za-z0-9_\s%/-]+):?((?s:.)*?)\|?([NTWSLRH]{0,3})$").expect("regex")
    })
}

fn json_rx() -> &'static Regex {
    static RX: OnceLock<Regex> = OnceLock::new();
    RX.get_or_init(|| Regex::new(r"^\{\.?([^{}]+)\}$|^\.?([^{}]+)$").expect("regex"))
}

/// kubectl `RelaxedJSONPathExpression`: `a.b`, `.a.b`, `{a.b}` → `{.a.b}`.
pub fn relaxed_json_path(s: &str) -> Result<String, String> {
    if s.is_empty() {
        return Ok(String::new());
    }
    let Some(m) = json_rx().captures(s) else {
        return Err("unexpected path string, expected a 'name1.name2' or '.name1.name2' or '{name1.name2}' or '{.name1.name2}'".into());
    };
    let spec = m
        .get(1)
        .filter(|g| !g.as_str().is_empty())
        .or_else(|| m.get(2))
        .map(|g| g.as_str())
        .unwrap_or("");
    Ok(format!("{{.{spec}}}"))
}

fn col_flags(flags: &str) -> HeaderColumn {
    let mut c = HeaderColumn::default();
    for b in flags.chars() {
        match b {
            'H' => c.hide = true,
            'W' => {
                c.wide = true;
                c.show = false;
            }
            'S' => {
                c.show = true;
                c.wide = false;
            }
            'L' => c.align = Align::Left,
            'R' => c.align = Align::Right,
            'T' => c.time = true,
            'N' => {
                c.capacity = true;
                c.align = Align::Right;
            }
            _ => tracing::warn!(attr = %b, "Unknown column attribute"),
        }
    }
    c
}

/// Parse one column definition (k9s `parse`).
pub fn parse_col(s: &str) -> Result<ColumnSpec, String> {
    let invalid = || format!("invalid column definition {s:?}");
    let m = full_rx().captures(s).ok_or_else(invalid)?;
    let spec = relaxed_json_path(m.get(2).map(|g| g.as_str()).unwrap_or(""))?;
    let mut header = col_flags(m.get(3).map(|g| g.as_str()).unwrap_or(""));
    header.name = m.get(1).map(|g| g.as_str()).unwrap_or("").to_string();
    Ok(ColumnSpec { header, spec })
}

/// A custom column: header attributes plus an optional JSONPath template.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ColumnSpec {
    pub header: HeaderColumn,
    pub spec: String,
}

/// k9s model1 Attrs.Merge: overlay `b` (default column attrs) onto `a`.
fn merge(a: &mut HeaderColumn, b: &HeaderColumn) {
    a.mx = b.mx;
    a.mxc = b.mxc;
    a.mxm = b.mxm;
    a.decorator = b.decorator;
    a.vs = b.vs;
    if a.align == Align::Left {
        a.align = b.align;
    }
    if !a.hide {
        a.hide = b.hide;
    }
    if !a.show && !a.wide {
        a.wide = b.wide;
    }
    if !a.time {
        a.time = b.time;
    }
    if !a.capacity {
        a.capacity = b.capacity;
    }
}

fn index_of(h: &[HeaderColumn], name: &str) -> Option<usize> {
    h.iter().position(|c| c.name == name)
}

fn is_jq_spec(spec: &str) -> bool {
    spec.split('|').count() > 2
}

/// A list of custom columns (k9s ColumnSpecs).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ColumnSpecs(pub Vec<ColumnSpec>);

impl ColumnSpecs {
    /// Parse views.yaml column definitions (k9s `ColsSpecs.parseSpecs`).
    pub fn parse<S: AsRef<str>>(cols: &[S]) -> Result<ColumnSpecs, String> {
        cols.iter()
            .map(|c| parse_col(c.as_ref()))
            .collect::<Result<Vec<_>, _>>()
            .map(ColumnSpecs)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Custom columns first, then the remaining default columns; custom
    /// columns named like a default one take its attributes (k9s Header).
    pub fn header(&self, rh: &Header) -> Header {
        let mut hh: Vec<HeaderColumn> = self.0.iter().map(|c| c.header.clone()).collect();
        for h in rh.iter() {
            if let Some(i) = index_of(&hh, &h.name) {
                merge(&mut hh[i], h);
                continue;
            }
            hh.push(h.clone());
        }
        Header::new(hh)
    }

    /// Compute the custom row fields from the object JSON `o` and the default
    /// row (k9s `realize` + `hydrateRow`).
    pub fn realize(&self, o: Option<&Value>, rh: &Header, row: &Row) -> Result<Vec<String>> {
        let mut out: Vec<(String, String)> = Vec::with_capacity(self.0.len() + rh.len());
        for c in &self.0 {
            out.push((c.header.name.clone(), self.hydrate(c, o, rh, row)?));
        }
        let mut rest = Vec::new();
        for hc in rh.iter() {
            if out.iter().any(|(n, _)| *n == hc.name) {
                continue;
            }
            if let Some(i) = index_of(&rh.0, &hc.name) {
                rest.push((
                    hc.name.clone(),
                    row.fields.get(i).cloned().unwrap_or_default(),
                ));
            }
        }
        out.extend(rest);
        Ok(out.into_iter().map(|(_, v)| v).collect())
    }

    fn hydrate(&self, c: &ColumnSpec, o: Option<&Value>, rh: &Header, row: &Row) -> Result<String> {
        if c.spec.is_empty() {
            let Some(ix) = index_of(&rh.0, &c.header.name) else {
                tracing::warn!(name = %c.header.name, "Unable to find custom column");
                return Ok(NA_VALUE.into());
            };
            return Ok(row
                .fields
                .get(ix)
                .cloned()
                .unwrap_or_else(|| NA_VALUE.into()));
        }
        let Some(o) = o else {
            return Ok(NA_VALUE.into());
        };
        if is_jq_spec(&c.spec) {
            tracing::warn!(spec = %c.spec, "JQ column specs are not supported");
            return Ok(NA_VALUE.into());
        }
        let parser = JsonPath::parse(&c.spec)
            .map_err(|e| anyhow!("unable to parse custom column {}: {e}", c.header.name))?;
        let vals = parser.find_results(o);
        let mut values: Vec<String> = Vec::new();
        if vals.is_empty() || vals[0].is_empty() {
            values.push(MISSING_VALUE.into());
        }
        for vv in &vals {
            for v in vv {
                let mut s = String::new();
                if c.header.mxc {
                    if let Some(q) = v.as_str().and_then(parse_quantity) {
                        s = to_mc(q.milli_value());
                    }
                } else if c.header.mxm {
                    if let Some(q) = v.as_str().and_then(parse_quantity) {
                        s = to_mi(q.milli_value());
                    }
                } else if c.header.time {
                    if let Some(t) = v.as_str().and_then(crate::render::helpers::parse_time) {
                        s = to_age_opt(Some(t));
                    }
                }
                if s.is_empty() {
                    s = go_fmt(v);
                }
                values.push(s);
            }
        }
        Ok(values.join(","))
    }
}

/// k9s render.Base: custom column specs plus the contract's NAMESPACE rule.
/// Renderers build their full k9s header/row, then call [`ViewBase::header`]
/// and [`ViewBase::finish`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ViewBase {
    specs: ColumnSpecs,
}

impl ViewBase {
    /// Apply views.yaml settings. Invalid specs are logged and ignored (k9s).
    pub fn set_view_setting(&mut self, vs: Option<&ViewSetting>) {
        let cols: &[String] = vs.map(|v| v.columns.as_slice()).unwrap_or(&[]);
        match ColumnSpecs::parse(cols) {
            Ok(s) => self.specs = s,
            Err(e) => tracing::error!(error = %e, "Unable to grok custom columns"),
        }
    }

    pub fn specs(&self) -> &ColumnSpecs {
        &self.specs
    }

    /// Header with custom columns applied but NAMESPACE kept (k9s doHeader).
    pub fn full_header(&self, default: &Header) -> Header {
        if self.specs.is_empty() {
            default.clone()
        } else {
            self.specs.header(default)
        }
    }

    /// Final header for namespace `ns`: custom columns applied, NAMESPACE
    /// dropped unless `ns` is cluster wide.
    pub fn header(&self, default: &Header, ns: &str) -> Header {
        let mut h = self.full_header(default);
        if let Some(i) = ns_index(&h, ns) {
            h.0.remove(i);
        }
        h
    }

    /// Apply custom columns to a default row (`obj` is the object JSON for
    /// JSONPath specs) then drop NAMESPACE unless `ns` is cluster wide.
    pub fn finish(
        &self,
        obj: Option<&Value>,
        default: &Header,
        ns: &str,
        row: &mut Row,
    ) -> Result<()> {
        if !self.specs.is_empty() {
            row.fields = self.specs.realize(obj, default, row)?;
        }
        let h = self.full_header(default);
        if let Some(i) = ns_index(&h, ns) {
            if i < row.fields.len() {
                row.fields.remove(i);
            }
        }
        Ok(())
    }
}

fn ns_index(h: &Header, ns: &str) -> Option<usize> {
    if is_cluster_wide(ns) {
        return None;
    }
    index_of(&h.0, "NAMESPACE")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn col(name: &str) -> HeaderColumn {
        HeaderColumn::new(name)
    }

    #[test]
    fn parse_cases() {
        let ok = |s: &str| parse_col(s).unwrap();
        assert_eq!(
            parse_col(""),
            Err(r#"invalid column definition """#.to_string())
        );
        assert_eq!(
            ok("fred"),
            ColumnSpec {
                header: col("fred"),
                spec: "".into()
            }
        );
        let c = ok("fred|W");
        assert!(c.header.wide && !c.header.hide);
        let c = ok("fred|WH");
        assert!(c.header.wide && c.header.hide);
        let c = ok("fred|S");
        assert!(c.header.show && !c.header.wide);
        let c = ok("AGE|TR");
        assert!(c.header.time);
        assert_eq!(c.header.align, Align::Right);
        let c = ok("fred|WR");
        assert!(c.header.wide);
        assert_eq!(c.header.align, Align::Right);
        let c = ok("BLEE:.spec.addresses[?(@.type == 'CiliumInternalIP')].ip");
        assert_eq!(c.header.name, "BLEE");
        assert_eq!(
            c.spec,
            "{.spec.addresses[?(@.type == 'CiliumInternalIP')].ip}"
        );
        assert_eq!(c.header.align, Align::Left);
        let c = ok("BLEE:.spec.addresses[?(@.type == 'CiliumInternalIP')].ip|WR");
        assert_eq!(
            c.spec,
            "{.spec.addresses[?(@.type == 'CiliumInternalIP')].ip}"
        );
        assert!(c.header.wide);
        assert_eq!(c.header.align, Align::Right);
        let c = ok("fred:.metadata.name|NW");
        assert_eq!(c.spec, "{.metadata.name}");
        assert!(c.header.capacity && c.header.wide);
        assert_eq!(c.header.align, Align::Right);
        let c = ok("fred:.metadata.name|T");
        assert!(c.header.time && !c.header.wide);
        let c = ok("fred:.metadata.name");
        assert_eq!(c.header, col("fred"));
        let c = ok("fred||.metadata.name|W");
        assert_eq!(c.spec, "{.||.metadata.name}");
        assert!(c.header.wide);
        assert_eq!(
            parse_col(":.metadata.name.fred|TW"),
            Err(r#"invalid column definition ":.metadata.name.fred|TW""#.to_string())
        );
        let c = ok(r"fred:.metadata.name.k8s:fred\.blee|TW");
        assert_eq!(c.spec, r"{.metadata.name.k8s:fred\.blee}");
        assert!(c.header.time && c.header.wide);
        let c = ok("fred|T");
        assert!(c.header.time);
        assert_eq!(c.spec, "");
    }

    #[test]
    fn parse_specs() {
        assert!(ColumnSpecs::parse::<&str>(&[]).unwrap().is_empty());
        let s = ColumnSpecs::parse(&["a", "b:.metadata.name|NW", "c"]).unwrap();
        assert_eq!(s.0.len(), 3);
        assert_eq!(s.0[1].spec, "{.metadata.name}");
        assert!(s.0[1].header.capacity);
        assert_eq!(
            ColumnSpecs::parse(&["a", "b:{{crap.bozo}}|NW", "c"]),
            Err("unexpected path string, expected a 'name1.name2' or '.name1.name2' or '{name1.name2}' or '{.name1.name2}'".to_string())
        );
    }

    #[test]
    fn header_merge_and_realize() {
        let rh = Header::new(vec![
            col("NAMESPACE"),
            col("A"),
            col("B").align(Align::Right),
            col("AGE").time(),
        ]);
        let specs =
            ColumnSpecs::parse(&["NAMESPACE", "BLEE:.metadata.name", "B|W", "ZORG:.nope"]).unwrap();
        let h = specs.header(&rh);
        let names: Vec<&str> = h.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["NAMESPACE", "BLEE", "B", "ZORG", "A", "AGE"]);
        assert_eq!(h.0[2].align, Align::Right);
        assert!(h.0[2].wide);
        let row = Row::with(
            "ns1/fred",
            vec!["ns1".into(), "a".into(), "b".into(), "1d".into()],
        );
        let o = json!({"metadata": {"name": "fred"}});
        let ff = specs.realize(Some(&o), &rh, &row).unwrap();
        assert_eq!(ff, ["ns1", "fred", "b", "<none>", "a", "1d"]);
        // nil object
        let ff = specs.realize(None, &rh, &row).unwrap();
        assert_eq!(ff[1], NA_VALUE);
        // unknown no-spec column
        let s2 = ColumnSpecs::parse(&["NOPE"]).unwrap();
        assert_eq!(s2.realize(None, &rh, &row).unwrap()[0], NA_VALUE);
    }

    #[test]
    fn realize_time_and_multi() {
        let rh = Header::new(vec![col("NAME")]);
        let row = Row::with("x", vec!["x".into()]);
        let specs = ColumnSpecs::parse(&[
            "CREATED:.metadata.creationTimestamp|T",
            "PORTS:.spec.ports[*].port",
            "JQ:{.a | .b | .c}",
        ])
        .unwrap();
        let o = json!({"metadata": {"creationTimestamp": "2020-01-01T00:00:00Z"}, "spec": {"ports": [{"port": 80}, {"port": 443}]}});
        let now = crate::render::helpers::parse_time("2020-01-01T00:01:00Z").unwrap();
        let ff =
            crate::render::helpers::with_now(now, || specs.realize(Some(&o), &rh, &row).unwrap());
        assert_eq!(ff, ["60s", "80,443", NA_VALUE, "x"]);
    }

    #[test]
    fn view_base_namespace_rule() {
        let rh = Header::new(vec![col("NAMESPACE"), col("NAME"), col("AGE").time()]);
        let mut b = ViewBase::default();
        assert_eq!(b.header(&rh, "default").len(), 2);
        assert_eq!(b.header(&rh, "all").len(), 3);
        assert_eq!(b.header(&rh, "-").len(), 3);
        let mut row = Row::with("ns/a", vec!["ns".into(), "a".into(), "1d".into()]);
        b.finish(None, &rh, "default", &mut row).unwrap();
        assert_eq!(row.fields, ["a", "1d"]);

        b.set_view_setting(Some(&ViewSetting {
            columns: vec!["NAME".into(), "AGE".into()],
            sort_column: String::new(),
        }));
        let h = b.header(&rh, "all");
        let names: Vec<&str> = h.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["NAME", "AGE", "NAMESPACE"]);
        let mut row = Row::with("ns/a", vec!["ns".into(), "a".into(), "1d".into()]);
        b.finish(None, &rh, "all", &mut row).unwrap();
        assert_eq!(row.fields, ["a", "1d", "ns"]);
        // invalid settings are ignored
        b.set_view_setting(Some(&ViewSetting {
            columns: vec!["".into()],
            sort_column: String::new(),
        }));
        assert_eq!(b.specs().0.len(), 2);
        b.set_view_setting(None);
        assert!(b.specs().is_empty());
    }
}
