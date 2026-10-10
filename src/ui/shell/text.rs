//! Low level drawing helpers mirroring tview text output: styled segments,
//! alignment and ellipsis truncation.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use unicode_width::UnicodeWidthStr;

/// tview SemigraphicsHorizontalEllipsis.
pub(crate) const ELLIPSIS: &str = "…";

/// A styled run of text.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Seg {
    pub text: String,
    pub style: Style,
}

pub(crate) fn seg(text: impl Into<String>, style: Style) -> Seg {
    Seg {
        text: text.into(),
        style,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Align {
    Left,
    Center,
}

pub(crate) fn width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

pub(crate) fn segs_width(segs: &[Seg]) -> usize {
    segs.iter().map(|s| width(&s.text)).sum()
}

/// Draw `segs` on row `y` within `[x, x+w)`. Overflowing text is cut and the
/// last visible cell replaced by an ellipsis (tview table cells). Returns the
/// number of columns printed.
pub(crate) fn draw_segs(
    buf: &mut Buffer,
    x: u16,
    y: u16,
    w: u16,
    segs: &[Seg],
    align: Align,
    ellipsis: bool,
) -> u16 {
    if w == 0 {
        return 0;
    }
    let total = segs_width(segs);
    let start = match align {
        Align::Left => x,
        Align::Center if total < w as usize => x + ((w as usize - total) / 2) as u16,
        Align::Center => x,
    };
    let max_x = x + w;
    let mut cx = start;
    let mut overflow = false;
    'outer: for s in segs {
        for g in unicode_segmentation_graphemes(&s.text) {
            let gw = width(g) as u16;
            if gw == 0 {
                continue;
            }
            if cx + gw > max_x {
                overflow = true;
                break 'outer;
            }
            if let Some(cell) = buf.cell_mut((cx, y)) {
                cell.set_symbol(g);
                cell.set_style(s.style);
            }
            if gw == 2 {
                if let Some(cell) = buf.cell_mut((cx + 1, y)) {
                    cell.reset();
                    cell.set_style(s.style);
                    // ratatui rewrites the trailing column of VS16 emoji,
                    // which splits them in tmux; tcell never does.
                    if g.contains('\u{fe0f}') {
                        cell.set_diff_option(ratatui::buffer::CellDiffOption::Skip);
                    }
                }
            }
            cx += gw;
        }
    }
    if overflow && ellipsis && cx > start {
        if let Some(cell) = buf.cell_mut((cx - 1, y)) {
            cell.set_symbol(ELLIPSIS);
        }
    }
    cx - start
}

/// Split into chars, keeping emoji variation selectors / ZWJ attached.
fn unicode_segmentation_graphemes(s: &str) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    let mut start = None;
    let mut idx_iter = s.char_indices().peekable();
    while let Some((i, c)) = idx_iter.next() {
        if start.is_none() {
            start = Some(i);
        }
        let joiner = c == '\u{200d}';
        let next_is_mark = matches!(idx_iter.peek(), Some((_, n)) if is_combining(*n));
        if !joiner && !next_is_mark {
            let st = start.take().unwrap_or(i);
            let end = i + c.len_utf8();
            out.push(&s[st..end]);
        }
    }
    if let Some(st) = start {
        out.push(&s[st..]);
    }
    out
}

fn is_combining(c: char) -> bool {
    matches!(
        c,
        '\u{fe0e}' | '\u{fe0f}' | '\u{200d}' | '\u{0300}'..='\u{036f}'
    )
}

/// Fill `area` with `style` (blank cells).
pub(crate) fn fill(buf: &mut Buffer, area: Rect, style: Style) {
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.reset();
                cell.set_style(style);
            }
        }
    }
}

/// runewidth.Truncate with an ellipsis tail.
pub(crate) fn truncate(s: &str, w: usize) -> String {
    if width(s) <= w {
        return s.to_string();
    }
    if w == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut cw = 0;
    for c in s.chars() {
        let c_w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if cw + c_w > w - 1 {
            break;
        }
        out.push(c);
        cw += c_w;
    }
    out.push_str(ELLIPSIS);
    out
}

/// Go `%q` quoting of a string.
pub(crate) fn go_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// tview WordWrap: wrap `text` (honoring newlines) into lines of at most `w`, breaking
/// after punctuation or at whitespace, else mid-word.
pub(crate) fn word_wrap(text: &str, w: usize) -> Vec<String> {
    wrap_ranges(text, w)
        .into_iter()
        .map(|(a, b)| text[a..b].to_string())
        .collect()
}

/// word_wrap ignoring the byte ranges `tags` (tview color tags) for widths
/// and breakpoints; returned lines keep their tags.
pub(crate) fn word_wrap_tagged(text: &str, w: usize, tags: &[(usize, usize)]) -> Vec<String> {
    if tags.is_empty() {
        return word_wrap(text, w);
    }
    // stripped text plus each stripped byte's position in `text`.
    let (mut stripped, mut map) = (String::new(), Vec::new());
    let mut tt = tags.iter().peekable();
    let mut i = 0;
    while i < text.len() {
        if let Some(&&(a, b)) = tt.peek() {
            if a == i {
                i = b;
                tt.next();
                continue;
            }
        }
        let ch = text[i..].chars().next().expect("char");
        for _ in 0..ch.len_utf8() {
            map.push(i);
        }
        stripped.push(ch);
        i += ch.len_utf8();
    }
    map.push(text.len());
    wrap_ranges(&stripped, w)
        .into_iter()
        .map(|(a, b)| {
            let end = if b == stripped.len() {
                text.len()
            } else {
                map[b]
            };
            text[map[a]..end].to_string()
        })
        .collect()
}

fn wrap_ranges(text: &str, w: usize) -> Vec<(usize, usize)> {
    static BOUNDARY: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(([,\.\-:;!\?&#+]|\n)[ \t\f\r]*|([ \t\f\r]+))").unwrap()
    });
    // (start, end, is punctuation/newline)
    let bps: Vec<(usize, usize, bool)> = BOUNDARY
        .captures_iter(text)
        .map(|c| {
            let m = c.get(0).unwrap();
            (m.start(), m.end(), c.get(3).is_none())
        })
        .collect();
    let mut lines = Vec::new();
    let (mut bp_pos, mut last_bp, mut last_cont, mut start) = (0, 0, 0, 0);
    let (mut line_w, mut overflow, mut force) = (0, 0, false);
    for (pos, ch) in text.char_indices() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if bp_pos < bps.len() && pos == bps[bp_pos].0 {
            (last_bp, last_cont, overflow) = (bps[bp_pos].0, bps[bp_pos].1, 0);
            force = ch == '\n';
            if bps[bp_pos].2 && !force {
                last_bp += 1; // keep the punctuation on this line
            }
            bp_pos += 1;
        }
        if force || line_w > 0 && line_w + cw > w {
            let (mut brk, mut cont) = (last_bp, last_cont);
            if force {
                (brk, cont, last_bp, overflow) = (pos, pos + 1, 0, 0);
            } else if last_bp <= start {
                (brk, cont, overflow) = (pos, pos, 0);
            }
            lines.push((start, brk));
            (start, line_w, force) = (cont, overflow, false);
        }
        if last_bp > 0 && last_cont <= pos {
            overflow += cw;
        }
        line_w += cw;
        if pos < start {
            line_w -= cw;
        }
    }
    if start < text.len() {
        lines.push((start, text.len()));
    }
    lines
}

/// tcell never writes a wide grapheme's trailing cell, so it shows the
/// grapheme's background. ratatui blanks it with a reset style and, for VS16
/// emoji (e.g. the xray service icon), emits it explicitly: give trailing
/// cells the grapheme's style.
pub(crate) fn style_wide_trailers(buf: &mut Buffer) {
    let area = buf.area;
    for y in area.top()..area.bottom() {
        let mut x = area.left();
        while x < area.right() {
            let (w, style) = {
                let c = &buf[(x, y)];
                (width(c.symbol()).max(1) as u16, c.style())
            };
            for t in 1..w {
                if let Some(c) = buf.cell_mut((x + t, y)) {
                    if c.symbol() == " " {
                        c.set_style(style);
                    }
                }
            }
            x += w;
        }
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use ratatui::buffer::Buffer;
    use ratatui::style::Color;

    /// Row `y` of the buffer as text (wide chars take one entry).
    pub fn row(buf: &Buffer, y: u16) -> String {
        let mut s = String::new();
        let mut skip = 0;
        for x in 0..buf.area.width {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            let sym = buf[(x, y)].symbol();
            skip = super::width(sym).saturating_sub(1);
            s.push_str(sym);
        }
        s
    }

    pub fn all_rows(buf: &Buffer) -> Vec<String> {
        (0..buf.area.height).map(|y| row(buf, y)).collect()
    }

    /// Find the first occurrence of `needle` in the buffer, returning (x, y).
    pub fn find(buf: &Buffer, needle: &str) -> Option<(u16, u16)> {
        for y in 0..buf.area.height {
            let mut cols = Vec::new();
            let mut line = String::new();
            let mut skip = 0;
            for x in 0..buf.area.width {
                if skip > 0 {
                    skip -= 1;
                    continue;
                }
                let sym = buf[(x, y)].symbol();
                skip = super::width(sym).saturating_sub(1);
                for _ in sym.chars() {
                    cols.push(x);
                }
                line.push_str(sym);
            }
            if let Some(byte_idx) = line.find(needle) {
                let char_idx = line[..byte_idx].chars().count();
                return Some((cols[char_idx], y));
            }
        }
        None
    }

    pub fn fg(buf: &Buffer, x: u16, y: u16) -> Color {
        buf[(x, y)].fg
    }

    pub fn bg(buf: &Buffer, x: u16, y: u16) -> Color {
        buf[(x, y)].bg
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::*;
    use super::*;
    use ratatui::style::Color;

    #[test]
    fn draw_with_ellipsis() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 6, 1));
        let n = draw_segs(
            &mut buf,
            0,
            0,
            6,
            &[seg("abcdefgh", Style::default().fg(Color::Red))],
            Align::Left,
            true,
        );
        assert_eq!(n, 6);
        assert_eq!(row(&buf, 0), "abcde…");
        assert_eq!(fg(&buf, 5, 0), Color::Red);
    }

    #[test]
    fn draw_centered() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 7, 1));
        draw_segs(
            &mut buf,
            0,
            0,
            7,
            &[seg("abc", Style::default())],
            Align::Center,
            false,
        );
        assert_eq!(row(&buf, 0), "  abc  ");
    }

    #[test]
    fn wide_chars() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 6, 1));
        draw_segs(
            &mut buf,
            0,
            0,
            6,
            &[seg("🐶> x", Style::default())],
            Align::Left,
            false,
        );
        assert_eq!(row(&buf, 0), "🐶> x ");
    }

    #[test]
    fn wide_trailers_take_the_grapheme_style() {
        use ratatui::style::Color;
        let mut b = Buffer::empty(Rect::new(0, 0, 6, 1));
        b.set_string(0, 0, "💁‍♀️ x", Style::default().bg(Color::Blue));
        assert_eq!(b[(1, 0)].bg, Color::Reset);
        style_wide_trailers(&mut b);
        assert_eq!(b[(1, 0)].bg, Color::Blue);
        assert_eq!(b[(1, 0)].symbol(), " ");
    }

    #[test]
    fn helpers() {
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("abc", 4), "abc");
        assert_eq!(go_quote("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(word_wrap("aaa bbb ccc", 7), vec!["aaa bbb", "ccc"]);
        assert_eq!(word_wrap("a\nb", 7), vec!["a", "b"]);
        assert_eq!(word_wrap("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        // tview breaks after punctuation, keeping it on the line.
        assert_eq!(
            word_wrap(
                r#"denied on resource "kube-system":"apiextensions.k8s.io/v1/crds" >"#,
                53
            ),
            vec![
                r#"denied on resource "kube-system":"apiextensions.k8s."#,
                r#"io/v1/crds" >"#
            ]
        );
        assert_eq!(word_wrap("ab kube-system", 8), vec!["ab kube-", "system"]);
        // Tags take no width and stay in the line they precede a break in.
        let t = "ab [red::b]cd[-::-] ef";
        let tags = [(3, 11), (13, 19)];
        assert_eq!(
            word_wrap_tagged(t, 5, &tags),
            vec!["ab [red::b]cd[-::-]", "ef"]
        );
        // Live k9s error dialog (width 53): the colon overflows and is clipped.
        let msg = "< Ruroh? (list) access denied for user on resource \"\":events.k8s.io/v1/events in namespace \"\" >";
        assert_eq!(
            word_wrap(msg, 53),
            vec![
                "< Ruroh? (list) access denied for user on resource \"\":",
                "events.k8s.io/v1/events in namespace \"\" >"
            ]
        );
        assert_eq!(word_wrap("ab-cd", 3), vec!["ab-", "cd"]);
        assert_eq!(word_wrap("a\n", 3), vec!["a"]);
    }
}
