//! Screen dump renderer (k9s render/screen_dump.go). Input: pseudo `ScreenDump`.

use anyhow::Result;
use ratatui::style::Color;

use super::NAVAJO_WHITE;
use crate::model::{Header, HeaderColumn, Row, RowEvent};
use crate::render::helpers::{obj_name, to_age_opt, value_time};
use crate::render::pseudo::str_field;
use crate::render::{Palette, RenderInput, Renderer};

/// k9s render.ScreenDump.
pub struct ScreenDump;

impl Renderer for ScreenDump {
    fn header(&self, _ns: &str) -> Header {
        Header::new(vec![
            HeaderColumn::new("NAME"),
            HeaderColumn::new("DIR"),
            HeaderColumn::new("VALID").wide(),
            HeaderColumn::new("AGE").time(),
        ])
    }

    fn render(&self, input: &RenderInput<'_>, _ns: &str, row: &mut Row) -> Result<()> {
        let o = input.obj;
        let dir = str_field(o, "dir");
        let name = obj_name(o);
        let path = match str_field(o, "path") {
            "" => std::path::Path::new(dir)
                .join(name)
                .to_string_lossy()
                .into_owned(),
            p => p.to_string(),
        };
        row.id = path;
        row.fields = vec![
            name.to_string(),
            dir.to_string(),
            String::new(),
            to_age_opt(value_time(o.data.get("modified"))),
        ];
        Ok(())
    }

    fn color_for(&self, _ns: &str, _h: &Header, _re: &RowEvent, _p: &Palette) -> Color {
        NAVAJO_WHITE
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ResEvent;
    use crate::render::core::testutil::*;
    use crate::render::helpers::with_now;

    #[test]
    fn render_dump() {
        let t = t0();
        let o = crate::render::pseudo::screen_dump(
            "fred/blee",
            "bob",
            t - chrono::Duration::minutes(5),
        );
        let mut r = Row::default();
        with_now(t, || {
            ScreenDump.render(&RenderInput::new(&o), "fred", &mut r)
        })
        .unwrap();
        assert_eq!(r.id, "fred/blee/bob");
        assert_eq!(r.fields, ["bob", "fred/blee", "", "5m"]);
        let re = RowEvent::new(ResEvent::Add, r);
        assert_eq!(
            ScreenDump.color_for("", &ScreenDump.header(""), &re, &palette()),
            NAVAJO_WHITE
        );
    }
}
