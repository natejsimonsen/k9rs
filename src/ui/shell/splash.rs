//! Splash screen (k9s ui/splash.go): big logo + revision, shown ~1s.

use std::time::Duration;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use super::text::{draw_segs, fill, seg, Align};
use crate::config::Styles;
use crate::logo::LOGO_BIG;

/// view/app.go splashDelay.
pub(crate) const SPLASH_DELAY: Duration = Duration::from_secs(1);

pub(crate) fn draw(buf: &mut Buffer, area: Rect, version: &str, styles: &Styles) {
    fill(buf, area, Style::default().bg(styles.bg));
    let logo = Style::default()
        .fg(styles.logo)
        .bg(styles.bg)
        .add_modifier(Modifier::BOLD);
    // Logo text view is 10 rows: two blank lines then the logo.
    for (i, l) in LOGO_BIG.iter().enumerate() {
        let y = area.y + 2 + i as u16;
        if y >= area.bottom() {
            return;
        }
        draw_segs(
            buf,
            area.x,
            y,
            area.width,
            &[seg(*l, logo)],
            Align::Center,
            false,
        );
    }
    let y = area.y + 10;
    if y >= area.bottom() {
        return;
    }
    let b = Style::default().bg(styles.bg).add_modifier(Modifier::BOLD);
    draw_segs(
        buf,
        area.x,
        y,
        area.width,
        &[
            seg("Revision ", b.fg(styles.fg)),
            seg(version, b.fg(Color::LightRed)),
        ],
        Align::Center,
        false,
    );
}

#[cfg(test)]
mod tests {
    use super::super::text::testutil::*;
    use super::*;

    #[test]
    fn splash_layout() {
        let st = Styles::default();
        let mut buf = Buffer::empty(Rect::new(0, 0, 60, 12));
        {
            let a = buf.area;
            draw(&mut buf, a, "v0.50.18", &st)
        };
        let rows = all_rows(&buf);
        assert_eq!(rows[0].trim(), "");
        assert_eq!(rows[2], format!("      {}      ", LOGO_BIG[0]));
        assert_eq!(fg(&buf, 7, 3), st.logo);
        assert_eq!(rows[10].trim(), "Revision v0.50.18");
        let (x, y) = find(&buf, "v0.50.18").unwrap();
        assert_eq!(fg(&buf, x, y), Color::LightRed);
        let (x, y) = find(&buf, "Revision").unwrap();
        assert_eq!(fg(&buf, x, y), st.fg);
    }
}
