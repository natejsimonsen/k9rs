//! Dir renderer (k9s render/dir.go). Input: pseudo `DirEntry`.

use anyhow::Result;
use ratatui::style::Color;

use super::CADET_BLUE;
use crate::model::{Header, HeaderColumn, Row, RowEvent};
use crate::render::helpers::obj_name;
use crate::render::pseudo::{bool_field, str_field};
use crate::render::{Palette, RenderInput, Renderer};

/// k9s render.Dir.
pub struct Dir;

impl Renderer for Dir {
    fn header(&self, _ns: &str) -> Header {
        Header::new(vec![HeaderColumn::new("NAME")])
    }

    fn render(&self, input: &RenderInput<'_>, _ns: &str, row: &mut Row) -> Result<()> {
        let o = input.obj;
        let icon = if bool_field(o, "isDir") {
            "📁 "
        } else {
            "🦄 "
        };
        row.id = str_field(o, "path").to_string();
        row.fields = vec![format!("{icon}{}", obj_name(o))];
        Ok(())
    }

    fn color_for(&self, _ns: &str, _h: &Header, _re: &RowEvent, _p: &Palette) -> Color {
        CADET_BLUE
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ResEvent;
    use crate::render::core::testutil::*;

    #[test]
    fn render_dir() {
        let t = t0();
        let d = crate::render::pseudo::dir_entry("/a/b", "b", true, 0, t);
        let f = crate::render::pseudo::dir_entry("/a/c.yaml", "c.yaml", false, 10, t);
        let mut r = Row::default();
        Dir.render(&RenderInput::new(&d), "", &mut r).unwrap();
        assert_eq!(r, Row::with("/a/b", vec!["📁 b".into()]));
        Dir.render(&RenderInput::new(&f), "", &mut r).unwrap();
        assert_eq!(r, Row::with("/a/c.yaml", vec!["🦄 c.yaml".into()]));
        let re = RowEvent::new(ResEvent::Add, r);
        assert_eq!(
            Dir.color_for("", &Dir.header(""), &re, &palette()),
            CADET_BLUE
        );
    }
}
