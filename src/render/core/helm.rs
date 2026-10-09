//! Helm renderers (k9s render/helm/chart.go, history.go). Inputs: pseudo
//! `HelmRelease` / `HelmHistory`. `chart` is expected as `name-version`.
//! `updated` may be RFC3339 or Helm's `2006-01-02 15:04:05.999999999 -0700 MST`
//! layout. History rows use `ns/release:revision` ids when the object carries
//! optional `release` (and namespace) fields, else the revision.

use anyhow::Result;
use chrono::{DateTime, Utc};

use crate::model::{Header, HeaderColumn, Row};
use crate::render::helpers::{as_status, obj_name, obj_ns, parse_time, to_age_opt, ViewBase};
use crate::render::pseudo::str_field;
use crate::render::{RenderInput, Renderer};

/// Parse a Helm timestamp (RFC3339 or Go's time.Time String layout).
fn helm_time(s: &str) -> Option<DateTime<Utc>> {
    if let Some(t) = parse_time(s) {
        return Some(t);
    }
    // "2024-01-02 15:04:05.123456 +0000 UTC" (drop zone name, optional monotonic suffix).
    let s = s.split(" m=").next().unwrap_or(s).trim();
    let parts: Vec<&str> = s.split(' ').collect();
    if parts.len() >= 3 {
        let joined = format!("{} {} {}", parts[0], parts[1], parts[2]);
        for fmt in ["%Y-%m-%d %H:%M:%S%.f %z", "%Y-%m-%d %H:%M:%S %z"] {
            if let Ok(t) = DateTime::parse_from_str(&joined, fmt) {
                return Some(t.with_timezone(&Utc));
            }
        }
    }
    None
}

pub(crate) fn chart_header() -> Header {
    Header::new(vec![
        HeaderColumn::new("NAMESPACE"),
        HeaderColumn::new("NAME"),
        HeaderColumn::new("REVISION"),
        HeaderColumn::new("STATUS"),
        HeaderColumn::new("CHART"),
        HeaderColumn::new("APP VERSION"),
        HeaderColumn::new("VALID").wide(),
        HeaderColumn::new("AGE").time(),
    ])
}

/// k9s helm.Chart.
pub struct Chart;

impl Renderer for Chart {
    fn header(&self, ns: &str) -> Header {
        ViewBase::default().header(&chart_header(), ns)
    }

    fn render(&self, input: &RenderInput<'_>, ns: &str, row: &mut Row) -> Result<()> {
        let o = input.obj;
        let (rns, name) = (obj_ns(o), obj_name(o));
        let status = str_field(o, "status");
        row.id = crate::k8s::fqn(rns, name);
        row.fields = vec![
            rns.to_string(),
            name.to_string(),
            str_field(o, "revision").to_string(),
            status.to_string(),
            str_field(o, "chart").to_string(),
            str_field(o, "appVersion").to_string(),
            as_status(chart_diagnose(status)),
            to_age_opt(helm_time(str_field(o, "updated"))),
        ];
        ViewBase::default().finish(None, &chart_header(), ns, row)
    }
}

fn chart_diagnose(s: &str) -> Result<(), &'static str> {
    if s != "deployed" {
        return Err("chart is in an invalid state");
    }
    Ok(())
}

/// k9s helm.History.
pub struct History;

impl Renderer for History {
    fn header(&self, _ns: &str) -> Header {
        Header::new(vec![
            HeaderColumn::new("REVISION"),
            HeaderColumn::new("STATUS"),
            HeaderColumn::new("CHART"),
            HeaderColumn::new("APP VERSION"),
            HeaderColumn::new("DESCRIPTION"),
            HeaderColumn::new("VALID").wide(),
        ])
    }

    fn render(&self, input: &RenderInput<'_>, _ns: &str, row: &mut Row) -> Result<()> {
        let o = input.obj;
        let rev = match str_field(o, "revision") {
            "" => obj_name(o),
            r => r,
        };
        let release = str_field(o, "release");
        row.id = if release.is_empty() {
            rev.to_string()
        } else {
            format!("{}:{rev}", crate::k8s::fqn(obj_ns(o), release))
        };
        row.fields = vec![
            rev.to_string(),
            str_field(o, "status").to_string(),
            str_field(o, "chart").to_string(),
            str_field(o, "appVersion").to_string(),
            str_field(o, "description").to_string(),
            String::new(),
        ];
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::core::testutil::*;
    use crate::render::helpers::with_now;

    #[test]
    fn chart() {
        let o = crate::render::pseudo::helm_release(
            "r1",
            "ns1",
            "3",
            "deployed",
            "nginx-1.2.3",
            "1.25",
            "2019-12-31T00:00:00Z",
        );
        let mut r = Row::default();
        with_now(t0(), || Chart.render(&RenderInput::new(&o), "all", &mut r)).unwrap();
        assert_eq!(r.id, "ns1/r1");
        assert_eq!(
            r.fields,
            [
                "ns1",
                "r1",
                "3",
                "deployed",
                "nginx-1.2.3",
                "1.25",
                "",
                "24h"
            ]
        );
        assert_eq!(names(&Chart.header("all"))[0], "NAMESPACE");
        let o = crate::render::pseudo::helm_release(
            "r1",
            "ns1",
            "3",
            "failed",
            "nginx-1.2.3",
            "1.25",
            "2019-12-31 12:00:00.123 +0000 UTC",
        );
        with_now(t0(), || Chart.render(&RenderInput::new(&o), "ns1", &mut r)).unwrap();
        assert_eq!(
            r.fields,
            [
                "r1",
                "3",
                "failed",
                "nginx-1.2.3",
                "1.25",
                "chart is in an invalid state",
                "11h"
            ]
        );
        assert_eq!(Chart.header("ns1").len(), 7);
        let o = crate::render::pseudo::helm_release("r1", "ns1", "3", "deployed", "c", "1", "");
        Chart.render(&RenderInput::new(&o), "ns1", &mut r).unwrap();
        assert_eq!(r.fields[6], "<unknown>");
    }

    #[test]
    fn history() {
        let mut o = crate::render::pseudo::helm_history(
            "2",
            "superseded",
            "nginx-1.2.3",
            "1.25",
            "",
            "Upgrade complete",
        );
        let mut r = Row::default();
        History.render(&RenderInput::new(&o), "", &mut r).unwrap();
        assert_eq!(r.id, "2");
        assert_eq!(
            r.fields,
            [
                "2",
                "superseded",
                "nginx-1.2.3",
                "1.25",
                "Upgrade complete",
                ""
            ]
        );
        assert_eq!(History.header("").len(), 6);
        o.data["release"] = serde_json::json!("r1");
        o.metadata.namespace = Some("ns1".into());
        History.render(&RenderInput::new(&o), "", &mut r).unwrap();
        assert_eq!(r.id, "ns1/r1:2");
    }

    #[test]
    fn times() {
        assert!(helm_time("2024-01-02T15:04:05Z").is_some());
        assert!(helm_time("2024-01-02 15:04:05.123456789 -0700 MST").is_some());
        assert!(helm_time("2024-01-02 15:04:05 +0000 UTC m=+0.1").is_some());
        assert!(helm_time("nope").is_none());
    }
}
