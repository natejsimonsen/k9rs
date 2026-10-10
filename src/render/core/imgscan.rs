//! Image scan renderer (k9s render/img_scan.go). Input: pseudo `ImageScan`.

use anyhow::Result;
use ratatui::style::Color;

use super::CADET_BLUE;
use crate::model::{Header, HeaderColumn, Row, RowEvent};
use crate::render::pseudo::{str_field, strs_field};
use crate::render::{Palette, RenderInput, Renderer};

/// render.CVEParseIdx: the vulnerability id in a row id split on `|`.
pub const CVE_PARSE_IDX: usize = 5;

/// k9s render.ImageScan.
pub struct ImageScan;

impl Renderer for ImageScan {
    fn header(&self, _ns: &str) -> Header {
        Header::new(
            [
                "SEVERITY",
                "VULNERABILITY",
                "IMAGE",
                "LIBRARY",
                "VERSION",
                "FIXED-IN",
                "TYPE",
            ]
            .into_iter()
            .map(HeaderColumn::new)
            .collect(),
        )
    }

    fn render(&self, input: &RenderInput<'_>, _ns: &str, row: &mut Row) -> Result<()> {
        let o = input.obj;
        let img = str_field(o, "image");
        let r = strs_field(o, "row");
        let f = |i: usize| r.get(i).cloned().unwrap_or_default();
        row.id = format!("{img}|{}", r.join("|"));
        row.fields = vec![f(5), f(4), img.to_string(), f(0), f(1), f(2), f(3)];
        Ok(())
    }

    /// ImageScan.ColorerFunc: the severity always picks the color.
    fn color_for(&self, _ns: &str, h: &Header, re: &RowEvent, _p: &Palette) -> Color {
        let sev = super::field_of(h, re, "SEVERITY").unwrap_or("");
        match sev {
            crate::vul::SEV1 => Color::LightRed,
            crate::vul::SEV2 => Color::Indexed(208),
            crate::vul::SEV3 => Color::LightYellow,
            crate::vul::SEV4 => Color::Indexed(39),
            crate::vul::SEV5 => CADET_BLUE,
            _ => Color::Indexed(65),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ResEvent;

    #[test]
    fn render_scan_row() {
        let o = crate::render::pseudo::image_scan(
            "alpine:3",
            &["libcrypto3", "3.1", "3.2", "apk", "CVE-1", "SEV-2"].map(String::from),
        );
        let mut row = Row::default();
        let r = ImageScan;
        r.render(&RenderInput::new(&o), "", &mut row).unwrap();
        assert_eq!(row.id, "alpine:3|libcrypto3|3.1|3.2|apk|CVE-1|SEV-2");
        assert_eq!(row.id.split('|').nth(CVE_PARSE_IDX), Some("CVE-1"));
        assert_eq!(
            row.fields,
            [
                "SEV-2",
                "CVE-1",
                "alpine:3",
                "libcrypto3",
                "3.1",
                "3.2",
                "apk"
            ]
            .map(String::from)
        );
        let h = r.header("");
        let re = RowEvent::new(ResEvent::Add, row);
        assert_eq!(
            r.color_for("", &h, &re, &Palette::default()),
            Color::Indexed(208)
        );
    }
}
