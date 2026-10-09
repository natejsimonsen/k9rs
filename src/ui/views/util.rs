//! Shared helpers for the non-table views: a tiny tview-style markup parser
//! (`[fg:bg:attrs]` tags), k9s title skinning, frame drawing, key labels,
//! hint ordering, text filtering (regex/fuzzy), a scrollable text pane and
//! file/clipboard helpers.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use regex::Regex;
use unicode_width::UnicodeWidthStr;

use crate::config::Styles;
use crate::ui::{Ctx, FlashLevel, Hint, ViewCmd};

// ---------------------------------------------------------------------------
// Styled text.

/// One styled run of text.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Seg {
    pub text: String,
    pub style: Style,
}

impl Seg {
    pub fn new(text: impl Into<String>, style: Style) -> Self {
        Seg {
            text: text.into(),
            style,
        }
    }
}

/// A styled line.
pub(crate) type SLine = Vec<Seg>;

/// Plain text of a styled line.
pub(crate) fn plain(line: &[Seg]) -> String {
    line.iter().map(|s| s.text.as_str()).collect()
}

/// Display width of a styled line.
pub(crate) fn line_width(line: &[Seg]) -> usize {
    line.iter().map(|s| s.text.width()).sum()
}

// ---------------------------------------------------------------------------
// Colors.

/// tcell color name lookup. The 16 basic names map onto the terminal
/// palette exactly like `config::styles` does; other names are exact RGB.
pub(crate) fn color_by_name(name: &str) -> Option<Color> {
    let n = name.to_ascii_lowercase();
    let c = match n.as_str() {
        "black" => Color::Black,
        "maroon" => Color::Red,
        "green" => Color::Green,
        "olive" => Color::Yellow,
        "navy" => Color::Blue,
        "purple" => Color::Magenta,
        "teal" => Color::Cyan,
        "silver" => Color::Gray,
        "gray" | "grey" => Color::DarkGray,
        "red" => Color::LightRed,
        "lime" => Color::LightGreen,
        "yellow" => Color::LightYellow,
        "blue" => Color::LightBlue,
        "fuchsia" => Color::LightMagenta,
        "aqua" => Color::LightCyan,
        "white" => Color::White,
        "default" => Color::Reset,
        _ => {
            let hex = W3C.iter().find(|(k, _)| *k == n).map(|(_, v)| *v)?;
            rgb(hex)
        }
    };
    Some(c)
}

const fn rgb(hex: u32) -> Color {
    Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

/// W3C extended color names used by k9s skins and views (tcell ColorNames).
const W3C: &[(&str, u32)] = &[
    ("aliceblue", 0xF0F8FF),
    ("antiquewhite", 0xFAEBD7),
    ("aquamarine", 0x7FFFD4),
    ("azure", 0xF0FFFF),
    ("beige", 0xF5F5DC),
    ("bisque", 0xFFE4C4),
    ("blanchedalmond", 0xFFEBCD),
    ("blueviolet", 0x8A2BE2),
    ("brown", 0xA52A2A),
    ("burlywood", 0xDEB887),
    ("cadetblue", 0x5F9EA0),
    ("chartreuse", 0x7FFF00),
    ("chocolate", 0xD2691E),
    ("coral", 0xFF7F50),
    ("cornflowerblue", 0x6495ED),
    ("cornsilk", 0xFFF8DC),
    ("crimson", 0xDC143C),
    ("darkblue", 0x00008B),
    ("darkcyan", 0x008B8B),
    ("darkgoldenrod", 0xB8860B),
    ("darkgray", 0xA9A9A9),
    ("darkgreen", 0x006400),
    ("darkkhaki", 0xBDB76B),
    ("darkmagenta", 0x8B008B),
    ("darkolivegreen", 0x556B2F),
    ("darkorange", 0xFF8C00),
    ("darkorchid", 0x9932CC),
    ("darkred", 0x8B0000),
    ("darksalmon", 0xE9967A),
    ("darkseagreen", 0x8FBC8F),
    ("darkslateblue", 0x483D8B),
    ("darkslategray", 0x2F4F4F),
    ("darkturquoise", 0x00CED1),
    ("darkviolet", 0x9400D3),
    ("deeppink", 0xFF1493),
    ("deepskyblue", 0x00BFFF),
    ("dimgray", 0x696969),
    ("dodgerblue", 0x1E90FF),
    ("firebrick", 0xB22222),
    ("floralwhite", 0xFFFAF0),
    ("forestgreen", 0x228B22),
    ("gainsboro", 0xDCDCDC),
    ("ghostwhite", 0xF8F8FF),
    ("gold", 0xFFD700),
    ("goldenrod", 0xDAA520),
    ("greenyellow", 0xADFF2F),
    ("honeydew", 0xF0FFF0),
    ("hotpink", 0xFF69B4),
    ("indianred", 0xCD5C5C),
    ("indigo", 0x4B0082),
    ("ivory", 0xFFFFF0),
    ("khaki", 0xF0E68C),
    ("lavender", 0xE6E6FA),
    ("lavenderblush", 0xFFF0F5),
    ("lawngreen", 0x7CFC00),
    ("lemonchiffon", 0xFFFACD),
    ("lightblue", 0xADD8E6),
    ("lightcoral", 0xF08080),
    ("lightcyan", 0xE0FFFF),
    ("lightgoldenrodyellow", 0xFAFAD2),
    ("lightgray", 0xD3D3D3),
    ("lightgreen", 0x90EE90),
    ("lightpink", 0xFFB6C1),
    ("lightsalmon", 0xFFA07A),
    ("lightseagreen", 0x20B2AA),
    ("lightskyblue", 0x87CEFA),
    ("lightslategray", 0x778899),
    ("lightsteelblue", 0xB0C4DE),
    ("lightyellow", 0xFFFFE0),
    ("limegreen", 0x32CD32),
    ("linen", 0xFAF0E6),
    ("mediumaquamarine", 0x66CDAA),
    ("mediumblue", 0x0000CD),
    ("mediumorchid", 0xBA55D3),
    ("mediumpurple", 0x9370DB),
    ("mediumseagreen", 0x3CB371),
    ("mediumslateblue", 0x7B68EE),
    ("mediumspringgreen", 0x00FA9A),
    ("mediumturquoise", 0x48D1CC),
    ("mediumvioletred", 0xC71585),
    ("midnightblue", 0x191970),
    ("mintcream", 0xF5FFFA),
    ("mistyrose", 0xFFE4E1),
    ("moccasin", 0xFFE4B5),
    ("navajowhite", 0xFFDEAD),
    ("oldlace", 0xFDF5E6),
    ("olivedrab", 0x6B8E23),
    ("orange", 0xFFA500),
    ("orangered", 0xFF4500),
    ("orchid", 0xDA70D6),
    ("palegoldenrod", 0xEEE8AA),
    ("palegreen", 0x98FB98),
    ("paleturquoise", 0xAFEEEE),
    ("palevioletred", 0xDB7093),
    ("papayawhip", 0xFFEFD5),
    ("peachpuff", 0xFFDAB9),
    ("peru", 0xCD853F),
    ("pink", 0xFFC0CB),
    ("plum", 0xDDA0DD),
    ("powderblue", 0xB0E0E6),
    ("rebeccapurple", 0x663399),
    ("rosybrown", 0xBC8F8F),
    ("royalblue", 0x4169E1),
    ("saddlebrown", 0x8B4513),
    ("salmon", 0xFA8072),
    ("sandybrown", 0xF4A460),
    ("seagreen", 0x2E8B57),
    ("seashell", 0xFFF5EE),
    ("sienna", 0xA0522D),
    ("skyblue", 0x87CEEB),
    ("slateblue", 0x6A5ACD),
    ("slategray", 0x708090),
    ("snow", 0xFFFAFA),
    ("springgreen", 0x00FF7F),
    ("steelblue", 0x4682B4),
    ("tan", 0xD2B48C),
    ("thistle", 0xD8BFD8),
    ("tomato", 0xFF6347),
    ("turquoise", 0x40E0D0),
    ("violet", 0xEE82EE),
    ("wheat", 0xF5DEB3),
    ("whitesmoke", 0xF5F5F5),
    ("yellowgreen", 0x9ACD32),
];

/// Encode a color as a markup token understood by `parse_markup`.
pub(crate) fn color_tag(c: Color) -> String {
    match c {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Indexed(i) => format!("@{i}"),
        Color::Reset => "default".into(),
        Color::Black => "black".into(),
        Color::Red => "maroon".into(),
        Color::Green => "green".into(),
        Color::Yellow => "olive".into(),
        Color::Blue => "navy".into(),
        Color::Magenta => "purple".into(),
        Color::Cyan => "teal".into(),
        Color::Gray => "silver".into(),
        Color::DarkGray => "gray".into(),
        Color::LightRed => "red".into(),
        Color::LightGreen => "lime".into(),
        Color::LightYellow => "yellow".into(),
        Color::LightBlue => "blue".into(),
        Color::LightMagenta => "fuchsia".into(),
        Color::LightCyan => "aqua".into(),
        Color::White => "white".into(),
    }
}

fn parse_color_token(tok: &str) -> Option<Color> {
    if let Some(hex) = tok.strip_prefix('#') {
        return u32::from_str_radix(hex, 16).ok().map(rgb);
    }
    if let Some(i) = tok.strip_prefix('@') {
        return i.parse::<u8>().ok().map(Color::Indexed);
    }
    Some(color_by_name(tok).unwrap_or(Color::Reset))
}

// ---------------------------------------------------------------------------
// Markup.

fn tag_rx() -> &'static Regex {
    static RX: OnceLock<Regex> = OnceLock::new();
    RX.get_or_init(|| {
        Regex::new(
            r#"^\[([a-zA-Z]+|#[0-9a-fA-F]{6}|@\d+|-)?(?::([a-zA-Z]+|#[0-9a-fA-F]{6}|@\d+|-)?(?::([lbdiursLBDIURS]+|-)?)?)?\]"#,
        )
        .expect("tag regex")
    })
}

fn region_rx() -> &'static Regex {
    static RX: OnceLock<Regex> = OnceLock::new();
    RX.get_or_init(|| Regex::new(r#"^\["([a-zA-Z0-9_,;: \-\.]*)"\]"#).expect("region regex"))
}

fn escape_rx() -> &'static Regex {
    static RX: OnceLock<Regex> = OnceLock::new();
    RX.get_or_init(|| Regex::new(r##"^\[([a-zA-Z0-9_,;: \-\."#]+)\[(\]+)"##).expect("escape regex"))
}

fn attrs(s: &str) -> Modifier {
    let mut m = Modifier::empty();
    for c in s.chars() {
        m |= match c.to_ascii_lowercase() {
            'b' => Modifier::BOLD,
            'd' => Modifier::DIM,
            'i' => Modifier::ITALIC,
            'u' => Modifier::UNDERLINED,
            'l' => Modifier::SLOW_BLINK,
            'r' => Modifier::REVERSED,
            's' => Modifier::CROSSED_OUT,
            _ => Modifier::empty(),
        };
    }
    m
}

/// Parse tview-style markup into styled lines. Tags are `[fg:bg:attrs]`;
/// like derailed/tview, every tag starts over from `base`, so an empty or
/// `-` field means the base value. Region tags `["id"]` are dropped. The style state carries across
/// lines, like tview.
pub(crate) fn parse_markup(s: &str, base: Style) -> Vec<SLine> {
    let mut out: Vec<SLine> = vec![Vec::new()];
    let mut style = base;
    let mut cur = String::new();
    let mut i = 0;
    let bytes = s.as_bytes();
    let flush = |out: &mut Vec<SLine>, cur: &mut String, style: Style| {
        if !cur.is_empty() {
            out.last_mut()
                .expect("line")
                .push(Seg::new(std::mem::take(cur), style));
        }
    };
    while i < s.len() {
        let rest = &s[i..];
        if bytes[i] == b'[' {
            if let Some(c) = tag_rx().captures(rest) {
                flush(&mut out, &mut cur, style);
                // derailed/tview styleFromTag starts from blank values, so
                // each tag restyles `base`: omitted or `-` fields fall back.
                style = base;
                if let Some(fg) = c
                    .get(1)
                    .filter(|f| !f.as_str().is_empty() && f.as_str() != "-")
                {
                    style.fg = parse_color_token(fg.as_str());
                }
                if let Some(bg) = c
                    .get(2)
                    .filter(|b| !b.as_str().is_empty() && b.as_str() != "-")
                {
                    style.bg = parse_color_token(bg.as_str());
                }
                if let Some(a) = c
                    .get(3)
                    .filter(|a| !a.as_str().is_empty() && a.as_str() != "-")
                {
                    let m = attrs(a.as_str());
                    style.add_modifier = m;
                    style.sub_modifier = base.sub_modifier - m;
                }
                i += c.get(0).expect("match").end();
                continue;
            }
            if let Some(c) = region_rx().captures(rest) {
                flush(&mut out, &mut cur, style);
                i += c.get(0).expect("match").end();
                continue;
            }
            if let Some(c) = escape_rx().captures(rest) {
                cur.push('[');
                cur.push_str(&c[1]);
                cur.push_str(&c[2][1..]);
                i += c.get(0).expect("match").end();
                continue;
            }
        }
        let ch = rest.chars().next().expect("char");
        if ch == '\n' {
            flush(&mut out, &mut cur, style);
            out.push(Vec::new());
        } else {
            cur.push(ch);
        }
        i += ch.len_utf8();
    }
    flush(&mut out, &mut cur, style);
    out
}

/// Parse markup expected to hold a single line.
pub(crate) fn markup_line(s: &str, base: Style) -> SLine {
    parse_markup(s, base).into_iter().flatten().collect()
}

/// k9s ui.SkinTitle: substitute skin colors into a title format.
pub(crate) fn skin_title(fmat: &str, s: &Styles) -> String {
    let bg = color_tag(s.title_bg);
    let fg = color_tag(s.title_fg);
    let mut f = fmat.replace("[fg:bg", &format!("[{fg}:{bg}"));
    f = f.replacen("[hilite", &format!("[{}", color_tag(s.title_highlight)), 1);
    f = f.replacen("[key", &format!("[{}", color_tag(s.menu_num_key)), 1);
    f = f.replacen("[filter", &format!("[{}", color_tag(s.title_filter)), 1);
    f = f.replacen("[count", &format!("[{}", color_tag(s.title_counter)), 1);
    f.replace(":bg:", &format!(":{bg}:"))
}

/// k9s ui.SearchFmt.
pub(crate) const SEARCH_FMT: &str = "<[filter:bg:r]/{}[fg:bg:-]> ";

/// Base style for frame titles.
pub(crate) fn title_base(s: &Styles) -> Style {
    Style::default().fg(s.title_fg).bg(s.title_bg)
}

/// Format the `/` filter suffix of a title.
pub(crate) fn search_title(buff: &str, s: &Styles) -> String {
    skin_title(&SEARCH_FMT.replace("{}", buff), s)
}

/// k9s render.AsThousands.
pub(crate) fn as_thousands(n: i64) -> String {
    let neg = n < 0;
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if neg {
        format!("-{out}")
    } else {
        out
    }
}

// ---------------------------------------------------------------------------
// Drawing.

/// Write a styled line at (x, y) clipped to `width`. Returns columns used.
pub(crate) fn put_line(buf: &mut Buffer, x: u16, y: u16, line: &[Seg], width: u16) -> u16 {
    let mut used = 0u16;
    for seg in line {
        if used >= width {
            break;
        }
        let (nx, _) = buf.set_stringn(x + used, y, &seg.text, (width - used) as usize, seg.style);
        used = nx.saturating_sub(x);
    }
    used
}

/// Fill `area` with `style`.
pub(crate) fn fill(buf: &mut Buffer, area: Rect, style: Style) {
    let area = area.intersection(buf.area);
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if let Some(c) = buf.cell_mut((x, y)) {
                c.reset();
                c.set_style(style);
            }
        }
    }
}

/// Draw a tview-like box: background, single-line border and a centered
/// title over the top edge. Returns the inner area (inside the border).
pub(crate) fn draw_box(
    buf: &mut Buffer,
    area: Rect,
    title: &[Seg],
    border: Style,
    bg: Style,
) -> Rect {
    let area = area.intersection(buf.area);
    fill(buf, area, bg);
    if area.width < 2 || area.height < 2 {
        return Rect::new(area.x, area.y, 0, 0);
    }
    let (l, r, t, b) = (area.left(), area.right() - 1, area.top(), area.bottom() - 1);
    for x in l + 1..r {
        buf.set_string(x, t, "─", border);
        buf.set_string(x, b, "─", border);
    }
    for y in t + 1..b {
        buf.set_string(l, y, "│", border);
        buf.set_string(r, y, "│", border);
    }
    buf.set_string(l, t, "┌", border);
    buf.set_string(r, t, "┐", border);
    buf.set_string(l, b, "└", border);
    buf.set_string(r, b, "┘", border);
    let avail = area.width.saturating_sub(2);
    if !title.is_empty() && avail > 0 {
        let w = line_width(title).min(avail as usize) as u16;
        let x = l + 1 + (avail - w) / 2;
        put_line(buf, x, t, title, w);
    }
    Rect::new(l + 1, t + 1, area.width - 2, area.height - 2)
}

/// Shrink a rect horizontally by `n` on each side (tview border padding).
pub(crate) fn pad_x(r: Rect, n: u16) -> Rect {
    if r.width <= 2 * n {
        return Rect::new(r.x, r.y, 0, r.height);
    }
    Rect::new(r.x + n, r.y, r.width - 2 * n, r.height)
}

/// Print `line` horizontally centered in `[x, x+width)` (tview AlignCenter).
pub(crate) fn put_centered(buf: &mut Buffer, x: u16, y: u16, width: u16, line: &[Seg]) {
    let w = line_width(line).min(width as usize) as u16;
    put_line(buf, x + (width - w) / 2, y, line, w);
}

// ---------------------------------------------------------------------------
// Keys and hints.

/// k9s-style label for a key (lowercased tcell/k9s key names).
pub(crate) fn key_label(k: &KeyEvent) -> String {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let shift = k.modifiers.contains(KeyModifiers::SHIFT);
    match k.code {
        KeyCode::Char(c) if ctrl => format!("ctrl-{}", c.to_ascii_lowercase()),
        KeyCode::Char(' ') => "space".into(),
        KeyCode::Char(c) if c.is_ascii_uppercase() => format!("shift-{}", c.to_ascii_lowercase()),
        KeyCode::Char(c) if shift && c.is_ascii_lowercase() => format!("shift-{c}"),
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Enter => "enter".into(),
        KeyCode::Esc => "esc".into(),
        KeyCode::Tab if shift => "backtab".into(),
        KeyCode::Tab => "tab".into(),
        KeyCode::BackTab => "backtab".into(),
        KeyCode::Up => "up".into(),
        KeyCode::Down => "down".into(),
        KeyCode::Left => "left".into(),
        KeyCode::Right => "right".into(),
        KeyCode::PageUp => "pgup".into(),
        KeyCode::PageDown => "pgdn".into(),
        KeyCode::Home => "home".into(),
        KeyCode::End => "end".into(),
        KeyCode::Delete => "delete".into(),
        KeyCode::Backspace => "backspace".into(),
        KeyCode::F(n) => format!("f{n}"),
        _ => String::new(),
    }
}

/// k9s model.MenuHints ordering: numeric mnemonics first (ascending), then
/// by description.
pub(crate) fn sort_hints(hh: &mut [Hint]) {
    hh.sort_by(
        |a, b| match (a.mnemonic.parse::<i64>(), b.mnemonic.parse::<i64>()) {
            (Ok(x), Ok(y)) => x.cmp(&y),
            (Ok(_), Err(_)) => std::cmp::Ordering::Less,
            (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
            _ => a.description.cmp(&b.description),
        },
    );
}

pub(crate) fn flash(level: FlashLevel, msg: impl Into<String>) -> ViewCmd {
    ViewCmd::Flash(level, msg.into())
}

// ---------------------------------------------------------------------------
// Filtering (k9s model.rxFilter / fuzzy.Find).

/// A search match on line `line`: one or more contiguous byte ranges.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TextMatch {
    pub line: usize,
    pub ranges: Vec<(usize, usize)>,
}

fn fuzzy_rx() -> &'static Regex {
    static RX: OnceLock<Regex> = OnceLock::new();
    RX.get_or_init(|| Regex::new(r"\A-f\s?([\w-]+)\b").expect("fuzzy regex"))
}

/// k9s internal.IsFuzzySelector.
pub(crate) fn fuzzy_selector(q: &str) -> Option<String> {
    fuzzy_rx().captures(q).map(|c| c[1].to_string())
}

/// k9s internal.IsLabelSelector.
pub(crate) fn is_label_selector(q: &str) -> bool {
    static RX: OnceLock<Regex> = OnceLock::new();
    RX.get_or_init(|| Regex::new(r"\A\-l").expect("label regex"))
        .is_match(q)
}

/// Case-insensitive regex compiled the way k9s does (`(?i)` + q).
pub(crate) fn ci_regex(q: &str) -> Option<Regex> {
    Regex::new(&format!("(?i){q}")).ok()
}

/// Fuzzy match `q` against `s`, returning matched byte indexes.
pub(crate) fn fuzzy_indices(q: &str, s: &str) -> Option<(i64, Vec<usize>)> {
    let m = SkimMatcherV2::default();
    let (score, idx) = m.fuzzy_indices(s, q)?;
    let offsets: Vec<usize> = s.char_indices().map(|(b, _)| b).collect();
    Some((
        score,
        idx.into_iter()
            .filter_map(|i| offsets.get(i).copied())
            .collect(),
    ))
}

/// dao.ContinuousRanges over byte indexes; `width_of` gives the byte length
/// of the char starting at an index so ranges end on char boundaries.
pub(crate) fn continuous_ranges(indexes: &[usize], s: &str) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = Vec::new();
    for &i in indexes {
        let len = s[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
        match out.last_mut() {
            Some(last) if last.1 == i => last.1 = i + len,
            _ => out.push((i, i + len)),
        }
    }
    out
}

/// Filter lines like k9s Text/Describe/YAML models.
pub(crate) fn filter_lines(q: &str, lines: &[String]) -> Vec<TextMatch> {
    if q.is_empty() {
        return Vec::new();
    }
    if let Some(f) = fuzzy_selector(q) {
        let f = f.trim();
        let mut mm: Vec<(i64, TextMatch)> = lines
            .iter()
            .enumerate()
            .filter_map(|(i, l)| {
                fuzzy_indices(f, l).map(|(score, idx)| {
                    (
                        score,
                        TextMatch {
                            line: i,
                            ranges: continuous_ranges(&idx, l),
                        },
                    )
                })
            })
            .collect();
        mm.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.line.cmp(&b.1.line)));
        return mm.into_iter().map(|(_, m)| m).collect();
    }
    let Some(rx) = ci_regex(q) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        for m in rx.find_iter(l) {
            if m.start() == m.end() {
                continue;
            }
            out.push(TextMatch {
                line: i,
                ranges: vec![(m.start(), m.end())],
            });
        }
    }
    out
}

/// Natural ("human") string ordering, like fvbommel/sortorder.NaturalLess.
pub(crate) fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (ab, bb) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut j) = (0, 0);
    while i < ab.len() && j < bb.len() {
        let (ca, cb) = (ab[i], bb[j]);
        if ca.is_ascii_digit() && cb.is_ascii_digit() {
            let si = i;
            while i < ab.len() && ab[i] == b'0' {
                i += 1;
            }
            let sj = j;
            while j < bb.len() && bb[j] == b'0' {
                j += 1;
            }
            let ni = i;
            while i < ab.len() && ab[i].is_ascii_digit() {
                i += 1;
            }
            let nj = j;
            while j < bb.len() && bb[j].is_ascii_digit() {
                j += 1;
            }
            let (da, db) = (&ab[ni..i], &bb[nj..j]);
            let ord = da.len().cmp(&db.len()).then_with(|| da.cmp(db));
            if ord != Ordering::Equal {
                return ord;
            }
            let za = ni - si;
            let zb = nj - sj;
            if za != zb {
                return za.cmp(&zb);
            }
            continue;
        }
        if ca != cb {
            return ca.cmp(&cb);
        }
        i += 1;
        j += 1;
    }
    (ab.len() - i).cmp(&(bb.len() - j))
}

// ---------------------------------------------------------------------------
// Scrollable text pane (tview.TextView subset).

#[derive(Default)]
pub(crate) struct TextPane {
    pub lines: Vec<SLine>,
    pub wrap: bool,
    /// First visible visual row.
    pub row: usize,
    /// Horizontal offset when not wrapping.
    pub col: usize,
    /// Keep the view pinned to the bottom.
    pub track_end: bool,
    pub center: bool,
    /// Max retained lines (0 = unlimited), tview SetMaxLines.
    pub max_lines: usize,
    /// Scroll so this logical line is visible on next draw.
    pub reveal: Option<usize>,
    /// Text ends with a newline: tview shows one more, empty, row.
    pub trailing_newline: bool,
    height: usize,
    width: usize,
}

impl TextPane {
    pub fn new(wrap: bool) -> Self {
        TextPane {
            wrap,
            ..Default::default()
        }
    }

    pub fn set_lines(&mut self, lines: Vec<SLine>) {
        self.lines = lines;
        self.trim();
    }

    pub fn clear(&mut self) {
        self.lines.clear();
        self.row = 0;
        self.col = 0;
    }

    pub fn append(&mut self, line: SLine) {
        self.lines.push(line);
        self.trim();
    }

    fn trim(&mut self) {
        if self.max_lines > 0 && self.lines.len() > self.max_lines {
            let n = self.lines.len() - self.max_lines;
            self.lines.drain(..n);
        }
    }

    pub fn scroll_to_beginning(&mut self) {
        self.row = 0;
        self.col = 0;
        self.track_end = false;
    }

    pub fn scroll_to_end(&mut self) {
        self.track_end = true;
        self.col = 0;
    }

    /// Plain text content (tview GetText(true)).
    pub fn text(&self) -> String {
        self.lines
            .iter()
            .map(|l| plain(l))
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn page(&self) -> usize {
        self.height.max(1)
    }

    /// Handle a navigation key (tview TextView.InputHandler); returns true
    /// when consumed.
    pub fn scroll_key(&mut self, label: &str) -> bool {
        let page = self.page();
        let down = |p: &mut Self, n: usize| p.row += n;
        let up = |p: &mut Self, n: usize| {
            p.track_end = false;
            p.row = p.row.saturating_sub(n);
        };
        match label {
            "j" | "down" | "ctrl-e" => down(self, 1),
            "k" | "up" | "ctrl-y" => up(self, 1),
            "g" | "home" => self.scroll_to_beginning(),
            "shift-g" | "end" => self.track_end = true,
            "pgdn" => down(self, page),
            "ctrl-f" => down(self, page.saturating_sub(1)),
            "ctrl-d" => down(self, page / 2),
            "pgup" => up(self, page),
            "ctrl-b" => up(self, page.saturating_sub(1)),
            "ctrl-u" => up(self, page / 2),
            "h" | "left" if !self.wrap => self.col = self.col.saturating_sub(1),
            "l" | "right" if !self.wrap => self.col += 1,
            _ => return false,
        }
        true
    }

    fn rows_of(&self, line: &[Seg], width: usize) -> usize {
        if !self.wrap || width == 0 {
            return 1;
        }
        line_width(line).div_ceil(width).max(1)
    }

    /// Visual rows of a logical line.
    fn wrap_line(line: &[Seg], width: usize) -> Vec<SLine> {
        let mut rows: Vec<SLine> = vec![Vec::new()];
        let mut used = 0;
        for seg in line {
            let mut cur = String::new();
            for ch in seg.text.chars() {
                let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                if used + w > width && used > 0 {
                    if !cur.is_empty() {
                        rows.last_mut()
                            .expect("row")
                            .push(Seg::new(std::mem::take(&mut cur), seg.style));
                    }
                    rows.push(Vec::new());
                    used = 0;
                }
                cur.push(ch);
                used += w;
            }
            if !cur.is_empty() {
                rows.last_mut().expect("row").push(Seg::new(cur, seg.style));
            }
        }
        rows
    }

    fn skip_cols(line: &[Seg], n: usize) -> SLine {
        let mut skipped = 0;
        let mut out = Vec::new();
        for seg in line {
            if skipped >= n {
                out.push(seg.clone());
                continue;
            }
            let mut text = String::new();
            for ch in seg.text.chars() {
                if skipped < n {
                    skipped += unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                } else {
                    text.push(ch);
                }
            }
            if !text.is_empty() {
                out.push(Seg::new(text, seg.style));
            }
        }
        out
    }

    /// Draw into `area`.
    pub fn draw(&mut self, buf: &mut Buffer, area: Rect) {
        self.height = area.height as usize;
        self.width = area.width as usize;
        let width = self.width;
        if width == 0 || self.height == 0 {
            return;
        }
        let counts: Vec<usize> = self.lines.iter().map(|l| self.rows_of(l, width)).collect();
        let mut total: usize = counts.iter().sum();
        if self.trailing_newline && !self.lines.is_empty() {
            total += 1;
        }
        if let Some(target) = self.reveal.take() {
            // tview TextView scrollToHighlights: center the highlight when it
            // fits, else show its start.
            let start: usize = counts.iter().take(target).sum();
            let rows = counts.get(target).copied().unwrap_or(1);
            self.row = if rows < self.height {
                let (from, to) = (start as i64, (start + rows - 1) as i64);
                ((from + to - self.height as i64) / 2).max(0) as usize
            } else {
                start
            };
            self.track_end = false;
        }
        let max_row = total.saturating_sub(self.height);
        if self.track_end || self.row > max_row {
            self.row = max_row;
        }
        let mut visual = 0;
        let mut y = area.y;
        for (i, line) in self.lines.iter().enumerate() {
            if y >= area.bottom() {
                break;
            }
            let n = counts[i];
            if visual + n <= self.row {
                visual += n;
                continue;
            }
            let rows = if self.wrap {
                Self::wrap_line(line, width)
            } else {
                vec![Self::skip_cols(line, self.col)]
            };
            for (k, row) in rows.iter().enumerate() {
                if visual + k < self.row {
                    continue;
                }
                if y >= area.bottom() {
                    break;
                }
                if self.center {
                    put_centered(buf, area.x, y, area.width, row);
                } else {
                    put_line(buf, area.x, y, row, area.width);
                }
                y += 1;
            }
            visual += n;
        }
    }
}

// ---------------------------------------------------------------------------
// Files and clipboard.

/// k9s data.SanitizeFileName: `:` and `/` runs become `-`.
pub(crate) fn sanitize_file_name(name: &str) -> String {
    static RX: OnceLock<Regex> = OnceLock::new();
    RX.get_or_init(|| Regex::new(r"[:/]+").expect("sanitize regex"))
        .replace_all(name, "-")
        .into_owned()
}

/// k9s K9s.ContextScreenDumpDir: `<dumps>/<cluster>/<context>`.
pub(crate) fn screen_dump_dir(ctx: &Ctx) -> PathBuf {
    let configured = ctx
        .config
        .read()
        .map(|c| c.k9s.screen_dump_dir.clone())
        .unwrap_or_default();
    let base = if configured.is_empty() {
        crate::config::paths::dumps_dir()
    } else {
        PathBuf::from(configured)
    };
    let ci = &ctx.cluster_info;
    if ci.context.is_empty() {
        return base.join("na");
    }
    base.join(sanitize_file_name(&ci.cluster))
        .join(sanitize_file_name(&ci.context))
}

fn write_file(dir: &Path, file: &str, body: &str) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(file);
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    use std::io::Write;
    let mut f = opts.open(&path)?;
    f.write_all(body.as_bytes())?;
    Ok(path)
}

/// k9s view.saveYAML: `<dir>/<sanitized name>--<unix secs>.yaml`.
pub(crate) fn save_yaml(dir: &Path, name: &str, raw: &str) -> anyhow::Result<PathBuf> {
    let f = format!(
        "{}--{}.yaml",
        sanitize_file_name(name),
        chrono::Utc::now().timestamp()
    );
    write_file(dir, &f, raw)
}

/// k9s view.saveData: `<dir>/<sanitized fqn-unixnano.log>`.
pub(crate) fn save_log(dir: &Path, fqn: &str, raw: &str) -> anyhow::Result<PathBuf> {
    let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
    let f = sanitize_file_name(&format!("{fqn}-{nanos}.log"));
    write_file(dir, &f, raw)
}

/// Copy to the system clipboard (k9s cpCmd).
pub(crate) fn copy_to_clipboard(text: &str) -> ViewCmd {
    let res = arboard::Clipboard::new().and_then(|mut c| c.set_text(text.to_string()));
    match res {
        Ok(()) => flash(FlashLevel::Info, "Content copied to clipboard..."),
        Err(e) => flash(FlashLevel::Err, e.to_string()),
    }
}

/// Go's %q for simple strings.
pub(crate) fn go_quote(s: &str) -> String {
    format!("{s:?}")
}

// ---------------------------------------------------------------------------
// Cow (view/cow.go).

const COW: [&str; 5] = [
    r"\   ^__^            ",
    r" \  (oo)\_______    ",
    r"    (__)\       )\/\",
    r"        ||----w |   ",
    r"        ||     ||   ",
];

/// k9s cowTalk rendered as styled lines (red, centered by the caller).
pub(crate) fn cow_talk(says: &str, w: usize, base: Style) -> Vec<SLine> {
    let red = base.fg(Color::LightRed);
    let bold = red.add_modifier(Modifier::BOLD);
    let n = says.chars().count() + 8;
    let mut out = vec![
        vec![Seg::new(format!(" {}", "─".repeat(n)), red)],
        vec![
            Seg::new("< ", red),
            Seg::new(format!("Ruroh? {says}"), bold),
            Seg::new(" >", red),
        ],
        vec![Seg::new(format!(" {}", "─".repeat(n)), red)],
    ];
    let r = if (w / 2) >= 8 { w / 2 - 8 } else { w / 2 };
    let spacer = " ".repeat(r);
    for l in COW {
        out.push(vec![Seg::new(format!("{spacer}{l}"), bold)]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_restyle_from_base_like_derailed_tview() {
        let base = Style::default().fg(Color::Blue);
        // `[-::]` after a bold green tag drops both back to base, and a
        // tag naming only attributes keeps the base color.
        let l = markup_line("[::b]A:[green::b]On[-::] x[red]r[::b]s", base);
        let st = |t: &str| l.iter().find(|s| s.text == t).unwrap().style;
        assert_eq!(st("A:").fg, Some(Color::Blue));
        assert!(st("A:").add_modifier.contains(Modifier::BOLD));
        assert_eq!(st("On").fg, Some(Color::Green));
        assert_eq!(st(" x").fg, Some(Color::Blue));
        assert!(!st(" x").add_modifier.contains(Modifier::BOLD));
        assert_eq!(st("s").fg, Some(Color::Blue));
    }

    #[test]
    fn markup_tags() {
        let base = Style::default().fg(Color::White).bg(Color::Black);
        let l = markup_line("[red::b]A[-::-]B[::]C[1:3]", base);
        assert_eq!(
            l[0],
            Seg::new("A", base.fg(Color::LightRed).add_modifier(Modifier::BOLD))
        );
        assert_eq!(plain(&l[1..]), "BC[1:3]");
        assert_eq!(l[1].style.fg, Some(Color::White));
        assert!(l[1].style.add_modifier.is_empty());
        let l = markup_line("x[[green:bg:b]tail]", base);
        assert_eq!(plain(&l), "x[tail]");
        let l = markup_line("[#ff0066:]Z", base);
        assert_eq!(l[0].style.fg, Some(Color::Rgb(0xff, 0, 0x66)));
        let lines = parse_markup("a\n[\"r1\"]b[\"\"]", base);
        assert_eq!(lines.len(), 2);
        assert_eq!(plain(&lines[1]), "b");
    }

    #[test]
    fn skinning() {
        let s = Styles::default();
        let t = skin_title(
            "[fg:bg:b] Describe([hilite:bg:b]a/b[fg:bg:-])[fg:bg:-] ",
            &s,
        );
        let l = markup_line(&t, title_base(&s));
        assert_eq!(plain(&l), " Describe(a/b) ");
        assert_eq!(l[1].style.fg, Some(s.title_highlight));
        assert!(l[1].style.add_modifier.contains(Modifier::BOLD));
        assert!(!l[2].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn thousands_and_natural() {
        assert_eq!(as_thousands(1234567), "1,234,567");
        assert_eq!(as_thousands(12), "12");
        assert!(natural_cmp("pod-2", "pod-10").is_lt());
        assert!(natural_cmp("a", "b").is_lt());
        assert!(natural_cmp("x10", "x9").is_gt());
    }

    #[test]
    fn filters() {
        let lines = vec!["foo bar".to_string(), "Bar".to_string()];
        let m = filter_lines("bar", &lines);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].ranges, vec![(4, 7)]);
        assert!(filter_lines("(", &lines).is_empty());
        let m = filter_lines("-f fb", &lines);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].line, 0);
        assert_eq!(sanitize_file_name("a/b:c::d"), "a-b-c-d");
    }

    #[test]
    fn keys() {
        use crossterm::event::KeyEvent;
        let k = |c, m| key_label(&KeyEvent::new(c, m));
        assert_eq!(k(KeyCode::Char('N'), KeyModifiers::SHIFT), "shift-n");
        assert_eq!(k(KeyCode::Char('s'), KeyModifiers::CONTROL), "ctrl-s");
        assert_eq!(k(KeyCode::Char(' '), KeyModifiers::NONE), "space");
        assert_eq!(k(KeyCode::Esc, KeyModifiers::NONE), "esc");
    }

    #[test]
    fn pane_wraps_and_scrolls() {
        let mut p = TextPane::new(true);
        p.set_lines(vec![vec![Seg::new("abcdefgh", Style::default())]; 3]);
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 2));
        p.draw(&mut buf, Rect::new(0, 0, 4, 2));
        assert_eq!(buf[(0, 1)].symbol(), "e");
        p.scroll_key("shift-g");
        p.draw(&mut buf, Rect::new(0, 0, 4, 2));
        assert_eq!(p.row, 4);
    }

    #[test]
    fn trailing_newline_adds_a_blank_last_row() {
        let mut p = TextPane::new(false);
        p.trailing_newline = true;
        p.set_lines(vec![vec![Seg::new("x", Style::default())]; 5]);
        p.scroll_to_end();
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 3));
        p.draw(&mut buf, Rect::new(0, 0, 4, 3));
        assert_eq!(p.row, 3);
        assert_eq!(buf[(0, 1)].symbol(), "x");
        assert_eq!(buf[(0, 2)].symbol(), " ");
    }

    #[test]
    fn pane_paging_like_tview() {
        let mut p = TextPane::new(false);
        p.set_lines(vec![vec![Seg::new("x", Style::default())]; 100]);
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 10));
        p.draw(&mut buf, Rect::new(0, 0, 4, 10));
        for (k, want) in [
            ("ctrl-f", 9),
            ("pgdn", 19),
            ("ctrl-d", 24),
            ("ctrl-u", 19),
            ("ctrl-b", 10),
            ("pgup", 0),
            ("ctrl-e", 1),
            ("ctrl-y", 0),
        ] {
            assert!(p.scroll_key(k), "{k}");
            p.draw(&mut buf, Rect::new(0, 0, 4, 10));
            assert_eq!(p.row, want, "{k}");
        }
    }

    #[test]
    fn reveal_centers_like_tview() {
        let mut p = TextPane::new(false);
        p.set_lines(vec![vec![Seg::new("x", Style::default())]; 100]);
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 10));
        // Line 30 in a 10 row view: offset (30 + 30 - 10) / 2 = 25.
        p.reveal = Some(30);
        p.draw(&mut buf, Rect::new(0, 0, 4, 10));
        assert_eq!(p.row, 25);
        // Even when already visible.
        p.reveal = Some(28);
        p.draw(&mut buf, Rect::new(0, 0, 4, 10));
        assert_eq!(p.row, 23);
        // Near the top: clamped to 0.
        p.reveal = Some(2);
        p.draw(&mut buf, Rect::new(0, 0, 4, 10));
        assert_eq!(p.row, 0);
        // Near the end: clamped to the last page.
        p.reveal = Some(99);
        p.draw(&mut buf, Rect::new(0, 0, 4, 10));
        assert_eq!(p.row, 90);
    }
}
