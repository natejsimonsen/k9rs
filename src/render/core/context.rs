//! Context renderer (k9s render/context.go). Input: pseudo `Context`.

use anyhow::Result;
use ratatui::style::Color;

use crate::model::{Header, HeaderColumn, Row, RowEvent};
use crate::render::pseudo::{bool_field, str_field};
use crate::render::{default_colorer, helpers::obj_name, Palette, RenderInput, Renderer};

/// k9s render.Context.
pub struct Context;

impl Renderer for Context {
    fn header(&self, _ns: &str) -> Header {
        Header::new(vec![
            HeaderColumn::new("NAME"),
            HeaderColumn::new("CLUSTER"),
            HeaderColumn::new("AUTHINFO"),
            HeaderColumn::new("NAMESPACE"),
        ])
    }

    fn render(&self, input: &RenderInput<'_>, _ns: &str, row: &mut Row) -> Result<()> {
        let o = input.obj;
        let name = obj_name(o);
        let mut shown = name.to_string();
        if bool_field(o, "active") {
            shown.push_str("(*)");
        }
        row.id = name.to_string();
        row.fields = vec![
            shown,
            str_field(o, "cluster").to_string(),
            str_field(o, "user").to_string(),
            str_field(o, "namespace").to_string(),
        ];
        Ok(())
    }

    fn color_for(&self, ns: &str, h: &Header, re: &RowEvent, p: &Palette) -> Color {
        let c = default_colorer(ns, h, re, p);
        if re
            .row
            .fields
            .first()
            .is_some_and(|f| f.trim().contains('*'))
        {
            return p.highlight;
        }
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::k8s::ContextInfo;
    use crate::model::ResEvent;
    use crate::render::core::testutil::*;

    fn ctx(active: bool) -> kube::api::DynamicObject {
        crate::render::pseudo::context(&ContextInfo {
            name: "c1".into(),
            cluster: "c1".into(),
            user: "u1".into(),
            namespace: "ns1".into(),
            active,
        })
    }

    #[test]
    fn render_ctx() {
        assert_eq!(Context.header("").len(), 4);
        let mut r = Row::new(4);
        Context
            .render(&RenderInput::new(&ctx(false)), "", &mut r)
            .unwrap();
        assert_eq!(
            r,
            Row::with(
                "c1",
                vec!["c1".into(), "c1".into(), "u1".into(), "ns1".into()]
            )
        );
        Context
            .render(&RenderInput::new(&ctx(true)), "", &mut r)
            .unwrap();
        assert_eq!(r.id, "c1");
        assert_eq!(r.fields[0], "c1(*)");
        let p = palette();
        let h = Context.header("");
        assert_eq!(
            Context.color_for("", &h, &RowEvent::new(ResEvent::Add, r.clone()), &p),
            p.highlight
        );
        r.fields[0] = "c1".into();
        assert_eq!(
            Context.color_for("", &h, &RowEvent::new(ResEvent::Add, r), &p),
            p.add
        );
    }
}
