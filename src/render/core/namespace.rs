//! Namespace renderer (k9s render/ns.go).

use anyhow::Result;
use ratatui::style::Color;

use crate::config::ViewSetting;
use crate::model::{Header, HeaderColumn, ResEvent, Row, RowEvent};
use crate::render::helpers::*;
use crate::render::{default_colorer, Palette, RenderInput, Renderer};

pub(crate) fn header() -> Header {
    Header::new(vec![
        HeaderColumn::new("NAME"),
        HeaderColumn::new("STATUS"),
        HeaderColumn::new("LABELS").wide(),
        HeaderColumn::new("VALID").wide(),
        HeaderColumn::new("AGE").time(),
    ])
}

/// k9s render.Namespace.
#[derive(Default)]
pub struct Namespace {
    base: ViewBase,
}

impl Renderer for Namespace {
    fn header(&self, ns: &str) -> Header {
        self.base.header(&header(), ns)
    }

    fn render(&self, input: &RenderInput<'_>, ns: &str, row: &mut Row) -> Result<()> {
        let o = input.obj;
        let phase = data_str(o, "status.phase");
        row.id = meta_fqn(o);
        row.fields = vec![
            obj_name(o).to_string(),
            phase.to_string(),
            labels_str(o),
            as_status(diagnose(phase)),
            obj_age(o),
        ];
        let raw = obj_to_value(o);
        self.base.finish(Some(&raw), &header(), ns, row)
    }

    fn color_for(&self, ns: &str, h: &Header, re: &RowEvent, p: &Palette) -> Color {
        let mut c = default_colorer(ns, h, re, p);
        if c == p.err {
            return c;
        }
        if re.kind == ResEvent::Update {
            c = p.std;
        }
        if re
            .row
            .fields
            .first()
            .is_some_and(|f| f.trim().contains('*'))
        {
            c = p.highlight;
        }
        c
    }

    fn set_view_setting(&mut self, vs: Option<ViewSetting>) {
        self.base.set_view_setting(vs.as_ref());
    }
}

fn diagnose(phase: &str) -> Result<(), &'static str> {
    if phase != "Active" && phase != "Terminating" {
        return Err("namespace not ready");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::core::testutil::*;

    #[test]
    fn render_ns() {
        let o = load("ns");
        let mut r = Row::default();
        with_now(t0(), || {
            Namespace::default().render(&RenderInput::new(&o), "-", &mut r)
        })
        .unwrap();
        // k9s render tests: cluster-scoped ids keep the "-/" prefix.
        assert_eq!(r.id, "-/kube-system");
        assert_eq!(r.fields[..2], ["kube-system", "Active"]);
        assert_eq!(r.fields[3], "");
        assert_eq!(r.fields[4], "329d");
        assert_eq!(r.fields.len(), Namespace::default().header("-").len());
    }

    #[test]
    fn not_ready() {
        assert_eq!(diagnose("Pending"), Err("namespace not ready"));
        assert_eq!(diagnose("Terminating"), Ok(()));
    }

    #[test]
    fn colorer() {
        let h = Header::new(vec![HeaderColumn::new("NAME"), HeaderColumn::new("STATUS")]);
        let p = palette();
        let n = Namespace::default();
        let re =
            |k, name: &str| RowEvent::new(k, Row::with(name, vec![name.into(), "Active".into()]));
        assert_eq!(n.color_for("", &h, &re(ResEvent::Add, "blee"), &p), p.add);
        assert_eq!(
            n.color_for("", &h, &re(ResEvent::Update, "blee"), &p),
            p.std
        );
        assert_eq!(
            n.color_for("", &h, &re(ResEvent::Add, "blee*"), &p),
            p.highlight
        );
        let hv = header();
        let bad = RowEvent::new(
            ResEvent::Add,
            Row::with(
                "x",
                vec![
                    "x*".into(),
                    "Pending".into(),
                    "".into(),
                    "namespace not ready".into(),
                    "1d".into(),
                ],
            ),
        );
        assert_eq!(n.color_for("", &hv, &bad, &p), p.err);
    }
}
