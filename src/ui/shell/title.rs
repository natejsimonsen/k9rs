//! View frame + title styling (ui/table_helper.go TitleFmt / NSTitleFmt /
//! SearchFmt applied to the plain title a view returns).

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use super::text::{draw_segs, fill, seg, segs_width, Align, Seg};
use crate::config::Styles;

/// Split a plain title (`name(ns)[count] <filter>`) into k9s colored segments.
/// `title_fg` is the view's box title color, used for `default` skin colors.
pub(crate) fn title_segs(title: &str, filter: &str, title_fg: Color, styles: &Styles) -> Vec<Seg> {
    // A `default` skin color is `-` in k9s markup: the box title color.
    let fg = |c: Color| if c == Color::Reset { title_fg } else { c };
    let bg = if styles.title_bg == Color::Reset {
        styles.bg
    } else {
        styles.title_bg
    };
    // `[fg:bg:-]` resets attributes, so the bold border does not leak in.
    let base = Style::default()
        .fg(fg(styles.title_fg))
        .bg(bg)
        .remove_modifier(Modifier::all());
    let bold = base.add_modifier(Modifier::BOLD);
    let hilite = Style::default()
        .fg(fg(styles.title_highlight))
        .bg(bg)
        .add_modifier(Modifier::BOLD);
    let counter = Style::default()
        .fg(fg(styles.title_counter))
        .bg(bg)
        .add_modifier(Modifier::BOLD);
    let filter_style = Style::default()
        .fg(fg(styles.title_filter))
        .bg(bg)
        .add_modifier(Modifier::REVERSED);

    let t = title.trim();
    if t.is_empty() {
        return Vec::new();
    }
    // view/help.go helpTitleFmt " [aqua::b]%s ": palette aqua, not the skin.
    if t == "Help" {
        // No bg in the tag: the help table background (body bg).
        let aqua = Style::default()
            .fg(Color::LightCyan)
            .bg(styles.bg)
            .add_modifier(Modifier::BOLD);
        return vec![seg(" ", base.bg(styles.bg)), seg(t, aqua), seg(" ", aqua)];
    }
    let (main, mut filt) = split_filter(t);
    if filt.is_none() && !filter.is_empty() {
        filt = Some(filter.to_string());
    }

    let mut out = vec![seg(" ", base)];
    match parse_main(main) {
        Some((name, ns, count)) => {
            match ns {
                Some(ns) => {
                    out.push(seg(format!("{name}("), bold));
                    out.push(seg(ns, hilite));
                    out.push(seg(")", base));
                }
                None => out.push(seg(name, bold)),
            }
            if let Some(c) = count {
                out.push(seg("[", base));
                out.push(seg(c, counter));
                out.push(seg("]", base));
            }
        }
        None => out.push(seg(main, bold)),
    }
    out.push(seg(" ", base));
    if let Some(f) = filt {
        let f = f.strip_prefix('/').unwrap_or(&f).to_string();
        out.push(seg("<", base));
        out.push(seg(format!("/{f}"), filter_style));
        out.push(seg("> ", base));
    }
    out
}

/// Separate a trailing ` <filter>` from the title.
fn split_filter(t: &str) -> (&str, Option<String>) {
    if t.ends_with('>') {
        if let Some(i) = t.rfind('<') {
            let inner = &t[i + 1..t.len() - 1];
            let head = t[..i].trim_end();
            if !head.is_empty() && !inner.is_empty() {
                return (head, Some(inner.to_string()));
            }
        }
    }
    (t, None)
}

/// `name(ns)[count]`, `name[count]`, `name(ns)` → parts.
fn parse_main(t: &str) -> Option<(String, Option<String>, Option<String>)> {
    let mut rest = t;
    let mut count = None;
    if rest.ends_with(']') {
        let i = rest.rfind('[')?;
        count = Some(rest[i + 1..rest.len() - 1].to_string());
        rest = &rest[..i];
    }
    let mut ns = None;
    if rest.ends_with(')') {
        let i = rest.find('(')?;
        ns = Some(rest[i + 1..rest.len() - 1].to_string());
        rest = &rest[..i];
    }
    if ns.is_none() && count.is_none() {
        return None;
    }
    if rest.is_empty() {
        return None;
    }
    Some((rest.to_string(), ns, count))
}

/// Draw a bordered frame with a centered, k9s-styled title and return the
/// inner area (border + 1 col horizontal padding, as tview SetBorderPadding(0,0,1,1)).
pub(crate) fn draw_frame(
    buf: &mut Buffer,
    area: Rect,
    title: &[Seg],
    focused: bool,
    styles: &Styles,
) -> Rect {
    fill(buf, area, Style::default().fg(styles.fg).bg(styles.bg));
    if area.width < 2 || area.height < 2 {
        return Rect::new(area.x, area.y, 0, 0);
    }
    let color = if focused {
        styles.border_focus
    } else {
        styles.border
    };
    let bs = Style::default()
        .fg(color)
        .bg(styles.bg)
        .add_modifier(Modifier::BOLD);
    let (l, r, t, b) = (area.left(), area.right() - 1, area.top(), area.bottom() - 1);
    let put = |buf: &mut Buffer, x: u16, y: u16, s: &str| {
        if let Some(c) = buf.cell_mut((x, y)) {
            c.set_symbol(s);
            c.set_style(bs);
        }
    };
    for x in l + 1..r {
        put(buf, x, t, "─");
        put(buf, x, b, "─");
    }
    for y in t + 1..b {
        put(buf, l, y, "│");
        put(buf, r, y, "│");
    }
    put(buf, l, t, "┌");
    put(buf, r, t, "┐");
    put(buf, l, b, "└");
    put(buf, r, b, "┘");
    let inner_w = area.width - 2;
    if !title.is_empty() && inner_w > 0 {
        let tw = segs_width(title) as u16;
        let x = if tw < inner_w {
            l + 1 + (inner_w - tw) / 2
        } else {
            l + 1
        };
        draw_segs(buf, x, t, inner_w.min(tw), title, Align::Left, true);
    }
    Rect::new(
        area.x + 2,
        area.y + 1,
        area.width.saturating_sub(4),
        area.height - 2,
    )
}

/// tview Box without focus: a frame a view drew in the focus color gets
/// the plain border color (the `:`/`/` prompt or a dialog has focus).
pub(crate) fn blur_frame(buf: &mut Buffer, area: Rect, styles: &Styles, color: Color) {
    if area.width < 2 || area.height < 2 {
        return;
    }
    let (l, r, t, b) = (area.left(), area.right() - 1, area.top(), area.bottom() - 1);
    let mut fix = |x: u16, y: u16| {
        if let Some(c) = buf.cell_mut((x, y)) {
            // Border glyphs only: title text may share the focus color.
            let line = c
                .symbol()
                .chars()
                .next()
                .is_some_and(|ch| ('\u{2500}'..='\u{257F}').contains(&ch));
            if line && c.fg == styles.border_focus {
                c.fg = color;
            }
        }
    };
    for x in l..=r {
        fix(x, t);
        fix(x, b);
    }
    for y in t..=b {
        fix(l, y);
        fix(r, y);
    }
}

#[cfg(test)]
mod tests {
    use super::super::text::testutil::*;
    use super::*;

    fn text(s: &[Seg]) -> String {
        s.iter().map(|s| s.text.clone()).collect()
    }

    #[test]
    fn parses_k9s_titles() {
        let st = Styles::default();
        let s = title_segs(" pods(default)[12] ", "", st.fg, &st);
        assert_eq!(text(&s), " pods(default)[12] ");
        assert_eq!(s[1].style.fg, Some(st.title_fg));
        assert_eq!(s[2].text, "default");
        assert_eq!(s[2].style.fg, Some(st.title_highlight));
        assert_eq!(s[5].text, "12");
        assert_eq!(s[5].style.fg, Some(st.title_counter));
        assert!(s[3].style.sub_modifier.contains(Modifier::BOLD));
        assert!(s[1].style.add_modifier.contains(Modifier::BOLD));

        let s = title_segs("nodes[3]", "", st.fg, &st);
        assert_eq!(text(&s), " nodes[3] ");

        let s = title_segs("pods(all)[1] </nginx>", "", st.fg, &st);
        assert_eq!(text(&s), " pods(all)[1] </nginx> ");
        let f = s.iter().find(|s| s.text == "/nginx").unwrap();
        assert_eq!(f.style.fg, Some(st.title_filter));
        assert!(f.style.add_modifier.contains(Modifier::REVERSED));

        let s = title_segs("Describe(default/nginx)", "", st.fg, &st);
        assert_eq!(text(&s), " Describe(default/nginx) ");

        let s = title_segs("Help", "", st.fg, &st);
        assert_eq!(text(&s), " Help ");
        assert!(s[1].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(s[1].style.fg, Some(ratatui::style::Color::LightCyan));

        let s = title_segs("pods(default)[2]", "fred", st.fg, &st);
        assert_eq!(text(&s), " pods(default)[2] </fred> ");
        assert!(title_segs("", "", st.fg, &st).is_empty());
    }

    #[test]
    fn frame_border_and_title() {
        let st = Styles::default();
        let mut buf = Buffer::empty(Rect::new(0, 0, 30, 5));
        let t = title_segs("pods(default)[1]", "", st.fg, &st);
        let inner = {
            let a = buf.area;
            draw_frame(&mut buf, a, &t, true, &st)
        };
        assert_eq!(inner, Rect::new(2, 1, 26, 3));
        let r0 = row(&buf, 0);
        assert!(r0.starts_with("┌"));
        assert!(r0.contains(" pods(default)[1] "));
        assert_eq!(fg(&buf, 0, 0), st.border_focus);
        {
            let a = buf.area;
            blur_frame(&mut buf, a, &st, st.border);
        }
        assert_eq!(fg(&buf, 0, 0), st.border);
        assert_eq!(row(&buf, 4), format!("└{}┘", "─".repeat(28)));
        let mut buf = Buffer::empty(Rect::new(0, 0, 30, 5));
        {
            let a = buf.area;
            draw_frame(&mut buf, a, &t, false, &st)
        };
        assert_eq!(fg(&buf, 0, 0), st.border);
    }

    /// Blur recolors border glyphs only, even when the title shares the
    /// focus color (solarized-16: title fg == focusColor).
    #[test]
    fn blur_keeps_title_colors() {
        let mut st = Styles::default();
        st.title_fg = st.border_focus;
        let mut buf = Buffer::empty(Rect::new(0, 0, 30, 5));
        let t = title_segs("pods(default)[1]", "", st.fg, &st);
        let a = buf.area;
        draw_frame(&mut buf, a, &t, true, &st);
        blur_frame(&mut buf, a, &st, st.border);
        let (x, y) = find(&buf, "pods(").unwrap();
        assert_eq!(fg(&buf, x, y), st.border_focus);
        assert_eq!(fg(&buf, 0, 0), st.border);
    }

    /// `default` skin colors (`-` in k9s markup) take the box title color.
    #[test]
    fn default_colors_use_title_color() {
        let mut st = Styles {
            title_fg: Color::Reset,
            title_counter: Color::Reset,
            title_bg: Color::Reset,
            ..Default::default()
        };
        let s = title_segs("pods(default)[2]", "", Color::LightCyan, &st);
        assert_eq!(s[1].style.fg, Some(Color::LightCyan));
        assert_eq!(s[5].style.fg, Some(Color::LightCyan));
        assert_eq!(s[1].style.bg, Some(st.bg));
        // Help: [aqua::b] on the table (body) background.
        st.title_bg = Color::Rgb(1, 2, 3);
        let s = title_segs("Help", "", st.fg, &st);
        assert_eq!(s[1].style.bg, Some(st.bg));
    }
}
