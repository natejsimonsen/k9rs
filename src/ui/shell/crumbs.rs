//! Breadcrumbs line (k9s ui/crumbs.go).

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use super::text::{draw_segs, fill, seg, Align, Seg};
use crate::config::Styles;

/// Crumb segments: each crumb ` <name> ` (lowercased, spaces removed) followed
/// by a single body-colored space; the last crumb uses the active color.
pub(crate) fn crumb_segs(names: &[String], styles: &Styles) -> Vec<Seg> {
    // crumbs.go markup: a `default` color is `-`, the text view's own
    // color: white fg (built before the skin loads), body bg.
    let fg = if styles.crumbs_fg == Color::Reset {
        Color::White
    } else {
        styles.crumbs_fg
    };
    let last = names.len().saturating_sub(1);
    let mut out = Vec::new();
    for (i, n) in names.iter().enumerate() {
        let bg = match if i == last {
            styles.crumbs_active
        } else {
            styles.crumbs_bg
        } {
            Color::Reset => styles.bg,
            c => c,
        };
        let name = n.to_lowercase().replace(' ', "");
        out.push(seg(
            format!(" <{name}> "),
            Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD),
        ));
        out.push(seg(" ", Style::default().fg(fg).bg(styles.bg)));
    }
    out
}

pub(crate) fn draw(buf: &mut Buffer, area: Rect, names: &[String], styles: &Styles) {
    fill(buf, area, Style::default().bg(styles.bg));
    let segs = crumb_segs(names, styles);
    draw_segs(
        buf,
        area.x.saturating_add(1),
        area.y,
        area.width.saturating_sub(2),
        &segs,
        Align::Left,
        false,
    );
}

#[cfg(test)]
mod tests {
    use super::super::text::testutil::*;
    use super::*;

    #[test]
    fn active_crumb_colored() {
        let styles = Styles::default();
        let mut buf = Buffer::empty(Rect::new(0, 0, 30, 1));
        {
            let a = buf.area;
            draw(&mut buf, a, &["pods".into(), "Describe Me".into()], &styles)
        };
        assert_eq!(row(&buf, 0), "  <pods>   <describeme>       ");
        let (x, _) = find(&buf, "<pods>").unwrap();
        assert_eq!(bg(&buf, x, 0), styles.crumbs_bg);
        assert_eq!(fg(&buf, x, 0), styles.crumbs_fg);
        assert!(buf[(x, 0)].modifier.contains(Modifier::BOLD));
        let (x, _) = find(&buf, "<describeme>").unwrap();
        assert_eq!(bg(&buf, x, 0), styles.crumbs_active);
        assert_eq!(bg(&buf, x - 2, 0), styles.bg, "separator uses body bg");
    }

    /// kiss skin: `default` crumb colors fall back to white on body bg.
    #[test]
    fn default_colors() {
        let styles = Styles {
            crumbs_fg: Color::Reset,
            crumbs_bg: Color::Reset,
            bg: Color::Rgb(1, 2, 3),
            ..Default::default()
        };
        let s = crumb_segs(&["pods".into(), "yaml".into()], &styles);
        assert_eq!(s[0].style.fg, Some(Color::White));
        assert_eq!(s[0].style.bg, Some(Color::Rgb(1, 2, 3)));
    }
}
