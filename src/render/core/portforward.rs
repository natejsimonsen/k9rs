//! Port-forward renderer (k9s render/portforward.go). Input: pseudo
//! `PortForward`. The benchmark URL uses optional `benchHost` / `benchPath`
//! fields (k9s benchmark config `http.host` / `http.path`) when present.

use anyhow::Result;
use ratatui::style::Color;

use super::SKY_BLUE;
use crate::model::{Header, HeaderColumn, Row, RowEvent};
use crate::render::helpers::{as_thousands, obj_name, to_age_opt, value_time, ViewBase};
use crate::render::pseudo::{i64_field, str_field};
use crate::render::{Palette, RenderInput, Renderer};

pub(crate) fn header() -> Header {
    Header::new(vec![
        HeaderColumn::new("NAMESPACE"),
        HeaderColumn::new("NAME"),
        HeaderColumn::new("CONTAINER"),
        HeaderColumn::new("PORTS"),
        HeaderColumn::new("URL"),
        HeaderColumn::new("C"),
        HeaderColumn::new("N"),
        HeaderColumn::new("VALID").wide(),
        HeaderColumn::new("AGE").time(),
    ])
}

/// k9s render.PortForward.
pub struct PortForward;

impl Renderer for PortForward {
    fn header(&self, ns: &str) -> Header {
        ViewBase::default().header(&header(), ns)
    }

    fn render(&self, input: &RenderInput<'_>, ns: &str, row: &mut Row) -> Result<()> {
        let o = input.obj;
        let local = i64_field(o, "localPort").to_string();
        let ports = format!("{local}:{}", i64_field(o, "containerPort"));
        row.id = obj_name(o).to_string();
        row.fields = vec![
            str_field(o, "namespace").to_string(),
            str_field(o, "pod").to_string(),
            str_field(o, "container").to_string(),
            ports,
            url_for(
                str_field(o, "benchHost"),
                str_field(o, "benchPath"),
                &local,
                str_field(o, "address"),
            ),
            as_thousands(i64_field(o, "benchConcurrency")),
            as_thousands(i64_field(o, "benchRequests")),
            String::new(),
            to_age_opt(value_time(o.data.get("startedAt"))),
        ];
        ViewBase::default().finish(None, &header(), ns, row)
    }

    fn color_for(&self, _ns: &str, _h: &Header, _re: &RowEvent, _p: &Palette) -> Color {
        SKY_BLUE
    }
}

/// k9s UrlFor: benchmark URL for a port forward.
pub fn url_for(host: &str, path: &str, port: &str, address: &str) -> String {
    let host = if host.is_empty() { address } else { host };
    let path = if path.is_empty() { "/" } else { path };
    format!("http://{host}:{port}{path}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::k8s::{PortForwardInfo, PortForwardSpec};
    use crate::model::ResEvent;
    use crate::render::core::testutil::*;
    use crate::render::helpers::with_now;

    fn pf() -> kube::api::DynamicObject {
        let info = PortForwardInfo {
            spec: PortForwardSpec {
                namespace: "blee".into(),
                pod: "fred".into(),
                container: "co".into(),
                address: "localhost".into(),
                local_port: 8080,
                container_port: 80,
            },
            started_at: t0() - chrono::Duration::hours(2),
            active: true,
        };
        crate::render::pseudo::port_forward(&info, 1, 1200)
    }

    #[test]
    fn render_pf() {
        let mut o = pf();
        let mut r = Row::default();
        with_now(t0(), || {
            PortForward.render(&RenderInput::new(&o), "", &mut r)
        })
        .unwrap();
        assert_eq!(r.id, "blee/fred|co:8080:80");
        assert_eq!(
            r.fields,
            [
                "blee",
                "fred",
                "co",
                "8080:80",
                "http://localhost:8080/",
                "1",
                "1,200",
                "",
                "120m"
            ]
        );
        o.data["benchHost"] = serde_json::json!("0.0.0.0");
        o.data["benchPath"] = serde_json::json!("/health");
        PortForward
            .render(&RenderInput::new(&o), "blee", &mut r)
            .unwrap();
        assert_eq!(r.fields[3], "http://0.0.0.0:8080/health");
        assert_eq!(r.fields.len(), PortForward.header("blee").len());
        let re = RowEvent::new(ResEvent::Add, r);
        assert_eq!(
            PortForward.color_for("", &header(), &re, &palette()),
            SKY_BLUE
        );
    }

    #[test]
    fn urls() {
        assert_eq!(url_for("0.0.0.0", "/", "p1", ""), "http://0.0.0.0:p1/");
        assert_eq!(url_for("", "", "80", "localhost"), "http://localhost:80/");
    }
}
