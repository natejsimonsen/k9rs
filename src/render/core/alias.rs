//! Alias renderer (k9s render/alias.go). Input: pseudo `Alias`.

use anyhow::Result;

use crate::k8s::Gvr;
use crate::model::{Header, HeaderColumn, Row};
use crate::render::pseudo::{str_field, strs_field};
use crate::render::{RenderInput, Renderer};

/// k9s render.Alias.
pub struct Alias;

impl Renderer for Alias {
    fn header(&self, _ns: &str) -> Header {
        Header::new(vec![
            HeaderColumn::new("RESOURCE"),
            HeaderColumn::new("GROUP"),
            HeaderColumn::new("VERSION"),
            HeaderColumn::new("COMMAND"),
        ])
    }

    fn render(&self, input: &RenderInput<'_>, _ns: &str, row: &mut Row) -> Result<()> {
        let o = input.obj;
        let gvr = Gvr::parse(str_field(o, "gvr"));
        let mut aliases = strs_field(o, "aliases");
        aliases.sort();
        row.id = gvr.to_string();
        row.fields = vec![
            gvr.r().to_string(),
            gvr.g().to_string(),
            gvr.v().to_string(),
            aliases.join(" "),
        ];
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ResEvent, RowEvent};
    use crate::render::core::testutil::*;

    #[test]
    fn render_alias() {
        let h = Alias.header("ns-1");
        assert_eq!(names(&h), ["RESOURCE", "GROUP", "VERSION", "COMMAND"]);
        assert_eq!(h, Alias.header("all"));
        let o = crate::render::pseudo::alias(
            &Gvr::parse("fred/v1/blee"),
            vec!["c".into(), "a".into(), "b".into()],
        );
        let mut r = Row::default();
        Alias
            .render(&RenderInput::new(&o), "fred/v1/blee", &mut r)
            .unwrap();
        assert_eq!(
            r,
            Row::with(
                "fred/v1/blee",
                vec!["blee".into(), "fred".into(), "v1".into(), "a b c".into()]
            )
        );
    }

    #[test]
    fn colorer() {
        let h = Header::new(vec![
            HeaderColumn::new("A"),
            HeaderColumn::new("B"),
            HeaderColumn::new("C"),
        ]);
        let r = Row::with("g/v/r", vec!["r".into(), "blee".into(), "g".into()]);
        let p = palette();
        assert_eq!(
            Alias.color_for("all", &h, &RowEvent::new(ResEvent::Add, r.clone()), &p),
            p.add
        );
        assert_eq!(
            Alias.color_for("all", &h, &RowEvent::new(ResEvent::Delete, r.clone()), &p),
            p.kill
        );
        assert_eq!(
            Alias.color_for("all", &h, &RowEvent::new(ResEvent::Update, r), &p),
            p.modify
        );
    }
}
