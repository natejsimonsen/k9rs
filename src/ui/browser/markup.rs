//! Text helpers used when drawing the table: tview color tags
//! (`[fg:bg:attrs]`), k9s cell deltas (ui/deltas.go), padding/truncation
//! (ui/padding.go), human durations (apimachinery HumanDuration) and
//! thousands separators (render.AsThousands).

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use unicode_width::UnicodeWidthStr;

/// Horizontal ellipsis used by tview when truncating.
pub(crate) const ELLIPSIS: &str = "…";
/// k9s ui.DeltaSign.
pub(crate) const DELTA_SIGN: &str = "Δ";
/// k9s ui.PlusSign.
pub(crate) const PLUS_SIGN: &str = "[red::b]↑";
/// k9s ui.MinusSign.
pub(crate) const MINUS_SIGN: &str = "[green::b]↓";

/// One parsed tview tag.
#[derive(Clone, Debug, Default, PartialEq)]
struct Tag {
    fg: Option<TagColor>,
    bg: Option<TagColor>,
    attrs: Option<TagAttrs>,
}

#[derive(Clone, Debug, PartialEq)]
enum TagColor {
    Reset,
    Color(Color),
}

#[derive(Clone, Debug, PartialEq)]
enum TagAttrs {
    Reset,
    Set(Modifier),
}

fn is_color_token(s: &str) -> bool {
    if s.is_empty() || s == "-" {
        return true;
    }
    if let Some(hex) = s.strip_prefix('#') {
        return hex.len() == 6 && hex.chars().all(|c| c.is_ascii_alphanumeric());
    }
    s.chars().all(|c| c.is_ascii_alphabetic())
}

fn is_attr_token(s: &str) -> bool {
    s.is_empty() || s == "-" || s.chars().all(|c| "lbidrus".contains(c))
}

/// Try to parse a tag body (between `[` and `]`).
fn parse_tag(body: &str) -> Option<Tag> {
    let parts: Vec<&str> = body.split(':').collect();
    if parts.len() > 3 {
        return None;
    }
    if parts.len() == 1 && parts[0].is_empty() {
        // `[]` is not a tag.
        return None;
    }
    let fg = parts[0];
    if !is_color_token(fg) {
        return None;
    }
    let bg = parts.get(1).copied();
    if let Some(b) = bg {
        if !is_color_token(b) {
            return None;
        }
    }
    let at = parts.get(2).copied();
    if let Some(a) = at {
        if !is_attr_token(a) {
            return None;
        }
    }
    let color = |s: &str| -> Option<TagColor> {
        match s {
            "" => None,
            "-" => Some(TagColor::Reset),
            other => Some(TagColor::Color(
                color_by_name(other).unwrap_or(Color::Reset),
            )),
        }
    };
    let attrs = match at {
        None | Some("") => None,
        Some("-") => Some(TagAttrs::Reset),
        Some(a) => {
            let mut m = Modifier::empty();
            for c in a.chars() {
                m |= match c {
                    'l' => Modifier::SLOW_BLINK,
                    'b' => Modifier::BOLD,
                    'i' => Modifier::ITALIC,
                    'd' => Modifier::DIM,
                    'r' => Modifier::REVERSED,
                    'u' => Modifier::UNDERLINED,
                    's' => Modifier::CROSSED_OUT,
                    _ => Modifier::empty(),
                };
            }
            Some(TagAttrs::Set(m))
        }
    };
    Some(Tag {
        fg: color(fg),
        bg: bg.and_then(color),
        attrs,
    })
}

/// Split `s` into (text, tag) segments.
fn segments(s: &str) -> Vec<(String, Option<Tag>)> {
    let mut out: Vec<(String, Option<Tag>)> = Vec::new();
    let mut text = String::new();
    let mut rest = s;
    while let Some(start) = rest.find('[') {
        let (before, after) = rest.split_at(start);
        text.push_str(before);
        let body_end = after[1..].find(']');
        match body_end {
            Some(end) => {
                let body = &after[1..1 + end];
                if let Some(tag) = parse_tag(body) {
                    out.push((std::mem::take(&mut text), Some(tag)));
                    rest = &after[end + 2..];
                    continue;
                }
                text.push('[');
                rest = &after[1..];
            }
            None => {
                text.push_str(after);
                rest = "";
            }
        }
    }
    text.push_str(rest);
    out.push((text, None));
    out
}

/// Remove tview color tags.
pub(crate) fn strip_tags(s: &str) -> String {
    segments(s).into_iter().map(|(t, _)| t).collect()
}

/// Display width of `s` once color tags are removed.
pub(crate) fn tagged_width(s: &str) -> usize {
    UnicodeWidthStr::width(strip_tags(s).as_str())
}

/// Turn tagged text into styled spans starting from `base`.
pub(crate) fn styled_spans(s: &str, base: Style) -> Vec<Span<'static>> {
    let mut style = base;
    let mut spans = Vec::new();
    for (text, tag) in segments(s) {
        if !text.is_empty() {
            spans.push(Span::styled(text, style));
        }
        if let Some(tag) = tag {
            // derailed/tview styleFromTag starts from blank values, so each
            // tag restyles `base`: omitted fields fall back to it.
            style = base;
            if let Some(TagColor::Color(c)) = tag.fg {
                style.fg = Some(c);
            }
            if let Some(TagColor::Color(c)) = tag.bg {
                style.bg = Some(c);
            }
            if let Some(TagAttrs::Set(m)) = tag.attrs {
                style.add_modifier = m;
                style.sub_modifier = base.sub_modifier - m;
            }
        }
    }
    spans
}

/// tcell color names → ratatui colors (tview markup): the 16 basic names
/// are palette colors, W3C names palette fits (palette::named), `#rrggbb`
/// exact RGB.
pub(crate) fn color_by_name(name: &str) -> Option<Color> {
    let n = name.to_ascii_lowercase();
    if let Some(hex) = n.strip_prefix('#') {
        let v = u32::from_str_radix(hex, 16).ok()?;
        return Some(Color::Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8));
    }
    // W3C names are tcell named colors, fitted to the 256 palette.
    let rgb = |v: u32| Some(crate::ui::shell::palette::named(v));
    match n.as_str() {
        "default" | "transparent" => Some(Color::Reset),
        "black" => Some(Color::Black),
        "maroon" => Some(Color::Red),
        "green" => Some(Color::Green),
        "olive" => Some(Color::Yellow),
        "navy" => Some(Color::Blue),
        "purple" => Some(Color::Magenta),
        "teal" => Some(Color::Cyan),
        "silver" => Some(Color::Gray),
        "gray" | "grey" => Some(Color::DarkGray),
        "red" => Some(Color::LightRed),
        "lime" => Some(Color::LightGreen),
        "yellow" => Some(Color::LightYellow),
        "blue" => Some(Color::LightBlue),
        "fuchsia" | "magenta" => Some(Color::LightMagenta),
        "aqua" | "cyan" => Some(Color::LightCyan),
        "white" => Some(Color::White),
        "aliceblue" => rgb(0xF0F8FF),
        "antiquewhite" => rgb(0xFAEBD7),
        "aquamarine" => rgb(0x7FFFD4),
        "azure" => rgb(0xF0FFFF),
        "beige" => rgb(0xF5F5DC),
        "bisque" => rgb(0xFFE4C4),
        "blanchedalmond" => rgb(0xFFEBCD),
        "blueviolet" => rgb(0x8A2BE2),
        "brown" => rgb(0xA52A2A),
        "burlywood" => rgb(0xDEB887),
        "cadetblue" => rgb(0x5F9EA0),
        "chartreuse" => rgb(0x7FFF00),
        "chocolate" => rgb(0xD2691E),
        "coral" => rgb(0xFF7F50),
        "cornflowerblue" => rgb(0x6495ED),
        "cornsilk" => rgb(0xFFF8DC),
        "crimson" => rgb(0xDC143C),
        "darkblue" => rgb(0x00008B),
        "darkcyan" => rgb(0x008B8B),
        "darkgoldenrod" => rgb(0xB8860B),
        "darkgray" | "darkgrey" => rgb(0xA9A9A9),
        "darkgreen" => rgb(0x006400),
        "darkkhaki" => rgb(0xBDB76B),
        "darkmagenta" => rgb(0x8B008B),
        "darkolivegreen" => rgb(0x556B2F),
        "darkorange" => rgb(0xFF8C00),
        "darkorchid" => rgb(0x9932CC),
        "darkred" => rgb(0x8B0000),
        "darksalmon" => rgb(0xE9967A),
        "darkseagreen" => rgb(0x8FBC8F),
        "darkslateblue" => rgb(0x483D8B),
        "darkslategray" | "darkslategrey" => rgb(0x2F4F4F),
        "darkturquoise" => rgb(0x00CED1),
        "darkviolet" => rgb(0x9400D3),
        "deeppink" => rgb(0xFF1493),
        "deepskyblue" => rgb(0x00BFFF),
        "dimgray" | "dimgrey" => rgb(0x696969),
        "dodgerblue" => rgb(0x1E90FF),
        "firebrick" => rgb(0xB22222),
        "floralwhite" => rgb(0xFFFAF0),
        "forestgreen" => rgb(0x228B22),
        "gainsboro" => rgb(0xDCDCDC),
        "ghostwhite" => rgb(0xF8F8FF),
        "gold" => rgb(0xFFD700),
        "goldenrod" => rgb(0xDAA520),
        "greenyellow" => rgb(0xADFF2F),
        "honeydew" => rgb(0xF0FFF0),
        "hotpink" => rgb(0xFF69B4),
        "indianred" => rgb(0xCD5C5C),
        "indigo" => rgb(0x4B0082),
        "ivory" => rgb(0xFFFFF0),
        "khaki" => rgb(0xF0E68C),
        "lavender" => rgb(0xE6E6FA),
        "lavenderblush" => rgb(0xFFF0F5),
        "lawngreen" => rgb(0x7CFC00),
        "lemonchiffon" => rgb(0xFFFACD),
        "lightblue" => rgb(0xADD8E6),
        "lightcoral" => rgb(0xF08080),
        "lightcyan" => rgb(0xE0FFFF),
        "lightgoldenrodyellow" => rgb(0xFAFAD2),
        "lightgray" | "lightgrey" => rgb(0xD3D3D3),
        "lightgreen" => rgb(0x90EE90),
        "lightpink" => rgb(0xFFB6C1),
        "lightsalmon" => rgb(0xFFA07A),
        "lightseagreen" => rgb(0x20B2AA),
        "lightskyblue" => rgb(0x87CEFA),
        "lightslategray" | "lightslategrey" => rgb(0x778899),
        "lightsteelblue" => rgb(0xB0C4DE),
        "lightyellow" => rgb(0xFFFFE0),
        "limegreen" => rgb(0x32CD32),
        "linen" => rgb(0xFAF0E6),
        "mediumaquamarine" => rgb(0x66CDAA),
        "mediumblue" => rgb(0x0000CD),
        "mediumorchid" => rgb(0xBA55D3),
        "mediumpurple" => rgb(0x9370DB),
        "mediumseagreen" => rgb(0x3CB371),
        "mediumslateblue" => rgb(0x7B68EE),
        "mediumspringgreen" => rgb(0x00FA9A),
        "mediumturquoise" => rgb(0x48D1CC),
        "mediumvioletred" => rgb(0xC71585),
        "midnightblue" => rgb(0x191970),
        "mintcream" => rgb(0xF5FFFA),
        "mistyrose" => rgb(0xFFE4E1),
        "moccasin" => rgb(0xFFE4B5),
        "navajowhite" => rgb(0xFFDEAD),
        "oldlace" => rgb(0xFDF5E6),
        "olivedrab" => rgb(0x6B8E23),
        "orange" => rgb(0xFFA500),
        "orangered" => rgb(0xFF4500),
        "orchid" => rgb(0xDA70D6),
        "palegoldenrod" => rgb(0xEEE8AA),
        "palegreen" => rgb(0x98FB98),
        "paleturquoise" => rgb(0xAFEEEE),
        "palevioletred" => rgb(0xDB7093),
        "papayawhip" => rgb(0xFFEFD5),
        "peachpuff" => rgb(0xFFDAB9),
        "peru" => rgb(0xCD853F),
        "pink" => rgb(0xFFC0CB),
        "plum" => rgb(0xDDA0DD),
        "powderblue" => rgb(0xB0E0E6),
        "rebeccapurple" => rgb(0x663399),
        "rosybrown" => rgb(0xBC8F8F),
        "royalblue" => rgb(0x4169E1),
        "saddlebrown" => rgb(0x8B4513),
        "salmon" => rgb(0xFA8072),
        "sandybrown" => rgb(0xF4A460),
        "seagreen" => rgb(0x2E8B57),
        "seashell" => rgb(0xFFF5EE),
        "sienna" => rgb(0xA0522D),
        "skyblue" => rgb(0x87CEEB),
        "slateblue" => rgb(0x6A5ACD),
        "slategray" | "slategrey" => rgb(0x708090),
        "snow" => rgb(0xFFFAFA),
        "springgreen" => rgb(0x00FF7F),
        "steelblue" => rgb(0x4682B4),
        "tan" => rgb(0xD2B48C),
        "thistle" => rgb(0xD8BFD8),
        "tomato" => rgb(0xFF6347),
        "turquoise" => rgb(0x40E0D0),
        "violet" => rgb(0xEE82EE),
        "wheat" => rgb(0xF5DEB3),
        "whitesmoke" => rgb(0xF5F5F5),
        "yellowgreen" => rgb(0x9ACD32),
        _ => None,
    }
}

/// tcell-style name of a color, used when building tagged strings.
pub(crate) fn color_tag(c: Color) -> String {
    match c {
        Color::Reset => "-".into(),
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
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Indexed(_) => "-".into(),
    }
}

/// runewidth.Truncate(s, width, "…").
pub(crate) fn truncate(s: &str, width: usize) -> String {
    if UnicodeWidthStr::width(s) <= width {
        return s.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut w = 0;
    for ch in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw > width.saturating_sub(1) {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push_str(ELLIPSIS);
    out
}

/// ui.IsASCII.
pub(crate) fn is_ascii(s: &str) -> bool {
    s.is_ascii()
}

/// ui.Pad: pad to `width` bytes or truncate.
pub(crate) fn pad(s: &str, width: usize) -> String {
    if s.len() == width {
        return s.to_string();
    }
    if s.len() > width {
        return truncate(s, width);
    }
    format!("{s}{}", " ".repeat(width - s.len()))
}

/// ui.formatCell, measuring the visible text (color tags excluded).
pub(crate) fn format_cell(field: &str, padding: usize) -> String {
    let visible = strip_tags(field);
    if !is_ascii(&visible) {
        return field.to_string();
    }
    if visible.len() == field.len() {
        return pad(field, padding);
    }
    if visible.len() >= padding {
        return field.to_string();
    }
    format!("{field}{}", " ".repeat(padding - visible.len()))
}

/// render.AsThousands.
pub(crate) fn as_thousands(n: i64) -> String {
    let s = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    if n < 0 {
        format!("-{out}")
    } else {
        out
    }
}

/// apimachinery duration.HumanDuration.
pub(crate) fn human_duration(d: chrono::Duration) -> String {
    let seconds = d.num_seconds();
    if seconds < -1 {
        return "<invalid>".into();
    } else if seconds < 0 {
        return "0s".into();
    } else if seconds < 60 * 2 {
        return format!("{seconds}s");
    }
    let minutes = d.num_minutes();
    if minutes < 10 {
        let s = seconds % 60;
        if s == 0 {
            return format!("{minutes}m");
        }
        return format!("{minutes}m{s}s");
    } else if minutes < 60 * 3 {
        return format!("{minutes}m");
    }
    let hours = d.num_hours();
    if hours < 8 {
        let m = minutes % 60;
        if m == 0 {
            return format!("{hours}h");
        }
        return format!("{hours}h{m}m");
    } else if hours < 48 {
        return format!("{hours}h");
    } else if hours < 24 * 8 {
        let h = hours % 24;
        if h == 0 {
            return format!("{}d", hours / 24);
        }
        return format!("{}d{}h", hours / 24, h);
    } else if hours < 24 * 365 * 2 {
        return format!("{}d", hours / 24);
    } else if hours < 24 * 365 * 8 {
        let dy = (hours / 24) % 365;
        if dy == 0 {
            return format!("{}y", hours / 24 / 365);
        }
        return format!("{}y{}d", hours / 24 / 365, dy);
    }
    format!("{}y", hours / 24 / 365)
}

/// AgeDecorator: RFC3339 timestamps become human ages; anything else
/// (already formatted ages, markers) is shown as is.
pub(crate) fn age_decorate(s: &str, now: chrono::DateTime<chrono::Utc>) -> String {
    match chrono::DateTime::parse_from_rfc3339(s.trim()) {
        Ok(t) => human_duration(now.signed_duration_since(t.with_timezone(&chrono::Utc))),
        Err(_) => s.to_string(),
    }
}

/// ui.Deltas: delta indicator appended to a changed cell.
pub(crate) fn deltas(o: &str, n: &str) -> String {
    let (o, n) = (o.trim(), n.trim());
    if o.is_empty() || o == crate::model::NA_VALUE {
        return String::new();
    }
    if let Ok(i) = o.parse::<i64>() {
        let j = n.parse::<i64>().unwrap_or(0);
        return sign(i.cmp(&j));
    }
    if let Some(i) = percentage(o) {
        let j = percentage(n).unwrap_or(0);
        return sign(i.cmp(&j));
    }
    if let Some(q1) = parse_quantity(o) {
        let q2 = parse_quantity(n).unwrap_or(0.0);
        return sign(q1.partial_cmp(&q2).unwrap_or(std::cmp::Ordering::Equal));
    }
    if let Some(d1) = parse_go_duration(o) {
        let d2 = parse_go_duration(n).unwrap_or(0.0);
        return sign(d1.partial_cmp(&d2).unwrap_or(std::cmp::Ordering::Equal));
    }
    if o != n {
        DELTA_SIGN.to_string()
    } else {
        String::new()
    }
}

fn sign(ord: std::cmp::Ordering) -> String {
    match ord {
        std::cmp::Ordering::Less => PLUS_SIGN.to_string(),
        std::cmp::Ordering::Greater => MINUS_SIGN.to_string(),
        std::cmp::Ordering::Equal => String::new(),
    }
}

fn percentage(s: &str) -> Option<i64> {
    let digits = s.strip_suffix('%')?;
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// Minimal resource.ParseQuantity: `<number>[suffix]`.
pub(crate) fn parse_quantity(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let split = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '+' || c == '-'))
        .unwrap_or(s.len());
    let (num, suffix) = s.split_at(split);
    if num.is_empty() || num == "+" || num == "-" {
        return None;
    }
    let base: f64 = num.parse().ok()?;
    let mult = match suffix {
        "" => 1.0,
        "n" => 1e-9,
        "u" => 1e-6,
        "m" => 1e-3,
        "k" => 1e3,
        "M" => 1e6,
        "G" => 1e9,
        "T" => 1e12,
        "P" => 1e15,
        "E" => 1e18,
        "Ki" => 1024.0,
        "Mi" => 1024f64.powi(2),
        "Gi" => 1024f64.powi(3),
        "Ti" => 1024f64.powi(4),
        "Pi" => 1024f64.powi(5),
        "Ei" => 1024f64.powi(6),
        other => {
            let exp = other
                .strip_prefix('e')
                .or_else(|| other.strip_prefix('E'))?;
            let e: i32 = exp.parse().ok()?;
            10f64.powi(e)
        }
    };
    Some(base * mult)
}

/// time.ParseDuration (seconds as f64).
pub(crate) fn parse_go_duration(s: &str) -> Option<f64> {
    let mut s = s.trim();
    if s.is_empty() {
        return None;
    }
    let mut neg = false;
    if let Some(r) = s.strip_prefix('-') {
        neg = true;
        s = r;
    } else if let Some(r) = s.strip_prefix('+') {
        s = r;
    }
    if s == "0" {
        return Some(0.0);
    }
    let mut total = 0.0;
    let mut rest = s;
    while !rest.is_empty() {
        let n_end = rest
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(rest.len());
        if n_end == 0 {
            return None;
        }
        let num: f64 = rest[..n_end].parse().ok()?;
        rest = &rest[n_end..];
        let u_end = rest
            .find(|c: char| c.is_ascii_digit() || c == '.')
            .unwrap_or(rest.len());
        let unit = &rest[..u_end];
        rest = &rest[u_end..];
        let mult = match unit {
            "ns" => 1e-9,
            "us" | "µs" | "μs" => 1e-6,
            "ms" => 1e-3,
            "s" => 1.0,
            "m" => 60.0,
            "h" => 3600.0,
            _ => return None,
        };
        total += num * mult;
    }
    Some(if neg { -total } else { total })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_are_parsed_and_stripped() {
        assert_eq!(strip_tags("[orange::b]Ⓕ"), "Ⓕ");
        assert_eq!(strip_tags("[::b]NAME[::-]↑"), "NAME↑");
        assert_eq!(strip_tags("[1,2]"), "[1,2]");
        assert_eq!(strip_tags("a[[red::b]12[-]]"), "a[12]");
        assert_eq!(tagged_width("[red::b]↑"), 1);
        let spans = styled_spans("a[red::b]b[-::-]c", Style::default().fg(Color::Blue));
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[1].style.fg, Some(Color::LightRed));
        assert!(spans[1].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(spans[2].style.fg, Some(Color::Blue));
        assert!(!spans[2].style.add_modifier.contains(Modifier::BOLD));
        // Each tag restyles the base (derailed/tview styleFromTag).
        let spans = styled_spans("[red]a[::b]b", Style::default().fg(Color::Blue));
        assert_eq!(spans[1].style.fg, Some(Color::Blue));
        assert!(spans[1].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn deltas_like_k9s() {
        assert_eq!(deltas("1", "2"), PLUS_SIGN);
        assert_eq!(deltas("3", "2"), MINUS_SIGN);
        assert_eq!(deltas("10%", "5%"), MINUS_SIGN);
        assert_eq!(deltas("100Mi", "1Gi"), PLUS_SIGN);
        assert_eq!(deltas("1m", "1m"), "");
        assert_eq!(deltas("Running", "Error"), DELTA_SIGN);
        assert_eq!(deltas("", "Error"), "");
        assert_eq!(deltas("n/a", "1"), "");
    }

    #[test]
    fn human_durations() {
        let d = chrono::Duration::seconds;
        assert_eq!(human_duration(d(5)), "5s");
        assert_eq!(human_duration(d(119)), "119s");
        assert_eq!(human_duration(d(125)), "2m5s");
        assert_eq!(human_duration(d(600)), "10m");
        assert_eq!(human_duration(d(3 * 3600 + 120)), "3h2m");
        assert_eq!(human_duration(d(10 * 3600)), "10h");
        assert_eq!(human_duration(d(3 * 86400 + 3600)), "3d1h");
        assert_eq!(human_duration(d(30 * 86400)), "30d");
        assert_eq!(human_duration(d(3 * 365 * 86400)), "3y");
        assert_eq!(human_duration(d(-5)), "<invalid>");
    }

    #[test]
    fn padding_and_numbers() {
        assert_eq!(pad("abc", 5), "abc  ");
        assert_eq!(pad("abcdef", 4), "abc…");
        assert_eq!(format_cell("Ⓕ", 4), "Ⓕ");
        assert_eq!(format_cell("[green::b]ok[::]", 4), "[green::b]ok[::]  ");
        assert_eq!(as_thousands(1234567), "1,234,567");
        assert_eq!(as_thousands(12), "12");
        assert_eq!(parse_go_duration("1h2m3s"), Some(3723.0));
        assert_eq!(parse_quantity("2k"), Some(2000.0));
        let now = chrono::Utc::now();
        let ts = (now - chrono::Duration::seconds(30)).to_rfc3339();
        assert_eq!(age_decorate(&ts, now), "30s");
        assert_eq!(age_decorate("5m", now), "5m");
        assert_eq!(color_tag(Color::Rgb(255, 165, 0)), "#ffa500");
    }
}
