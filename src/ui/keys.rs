//! Key naming (k9s ui/key.go + tcell key names): render a crossterm key as the
//! label k9s shows in menus/help (`ctrl-d`, `shift-f`, `enter`, `esc`, `?`...),
//! and parse hotkeys.yaml / plugins.yaml shortcuts back into key events.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Shifted digit symbols on a US keyboard, indexed by digit (k9s KeyShift0..9).
const SHIFT_DIGITS: [char; 10] = [')', '!', '@', '#', '$', '%', '^', '&', '*', '('];

/// k9s-style label for a key: `a`, `shift-a` (uppercase letters), `ctrl-d`,
/// `enter`, `esc`, `tab`, `space`, `?`, `/`, `:`, `0`..`9`, `shift-0`.. etc.
pub fn key_name(key: &KeyEvent) -> String {
    let m = key.modifiers;
    let ctrl = m.contains(KeyModifiers::CONTROL);
    let alt = m.contains(KeyModifiers::ALT);
    let shift = m.contains(KeyModifiers::SHIFT);
    let base = match key.code {
        KeyCode::Char(c) => {
            if ctrl {
                return format!("ctrl-{}", ctrl_char_name(c));
            }
            let n = char_name(c);
            if alt {
                return format!("alt-{n}");
            }
            return n;
        }
        KeyCode::Enter => "enter".to_string(),
        KeyCode::Esc => "esc".to_string(),
        KeyCode::Tab => {
            if shift {
                return "backtab".into();
            }
            "tab".into()
        }
        KeyCode::BackTab => return "backtab".into(),
        KeyCode::Backspace => "backspace".into(),
        KeyCode::Delete => "delete".into(),
        KeyCode::Insert => "insert".into(),
        KeyCode::Up => "up".into(),
        KeyCode::Down => "down".into(),
        KeyCode::Left => "left".into(),
        KeyCode::Right => "right".into(),
        KeyCode::PageUp => "pgup".into(),
        KeyCode::PageDown => "pgdn".into(),
        KeyCode::Home => "home".into(),
        KeyCode::End => "end".into(),
        KeyCode::F(n) => format!("f{n}"),
        KeyCode::Null => "null".into(),
        other => format!("{other:?}").to_lowercase(),
    };
    let mut out = String::new();
    if ctrl {
        out.push_str("ctrl-");
    }
    if alt {
        out.push_str("alt-");
    }
    if shift {
        out.push_str("shift-");
    }
    out.push_str(&base);
    out
}

fn ctrl_char_name(c: char) -> String {
    match c {
        ' ' => "space".into(),
        c => c.to_lowercase().collect(),
    }
}

fn char_name(c: char) -> String {
    if c == ' ' {
        return "space".into();
    }
    if c.is_ascii_uppercase() {
        return format!("shift-{}", c.to_ascii_lowercase());
    }
    if let Some(d) = SHIFT_DIGITS.iter().position(|s| *s == c) {
        return format!("shift-{d}");
    }
    c.to_string()
}

/// Parse a hotkeys.yaml / plugins.yaml shortcut (`Shift-F`, `Ctrl-X`, `F1`,
/// `a`) into a key event. Names follow tcell's key names (k9s `asKey`);
/// matching is case-insensitive for named keys.
pub fn parse_key(s: &str) -> Option<KeyEvent> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let mut chars = s.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c.is_ascii_uppercase() {
            return Some(KeyEvent::new(KeyCode::Char(c), KeyModifiers::SHIFT));
        }
        return Some(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    let lower = s.to_lowercase();
    if let Some(rest) = strip_prefix_ci(s, "shift-") {
        let mut it = rest.chars();
        if let (Some(c), None) = (it.next(), it.next()) {
            if let Some(d) = c.to_digit(10) {
                return Some(KeyEvent::new(
                    KeyCode::Char(SHIFT_DIGITS[d as usize]),
                    KeyModifiers::NONE,
                ));
            }
            if c.is_ascii_alphabetic() {
                return Some(KeyEvent::new(
                    KeyCode::Char(c.to_ascii_uppercase()),
                    KeyModifiers::SHIFT,
                ));
            }
            return None;
        }
        let inner = named_key(&rest.to_lowercase())?;
        return Some(KeyEvent::new(inner, KeyModifiers::SHIFT));
    }
    if let Some(rest) = strip_prefix_ci(s, "ctrl-") {
        let mut it = rest.chars();
        if let (Some(c), None) = (it.next(), it.next()) {
            return Some(KeyEvent::new(
                KeyCode::Char(c.to_ascii_lowercase()),
                KeyModifiers::CONTROL,
            ));
        }
        let r = rest.to_lowercase();
        if r == "space" {
            return Some(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL));
        }
        let inner = named_key(&r)?;
        return Some(KeyEvent::new(inner, KeyModifiers::CONTROL));
    }
    if let Some(rest) = strip_prefix_ci(s, "alt-") {
        let mut it = rest.chars();
        if let (Some(c), None) = (it.next(), it.next()) {
            return Some(KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT));
        }
        let inner = named_key(&rest.to_lowercase())?;
        return Some(KeyEvent::new(inner, KeyModifiers::ALT));
    }
    if lower == "space" {
        return Some(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
    }
    if lower == "backtab" {
        return Some(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
    }
    named_key(&lower).map(|c| KeyEvent::new(c, KeyModifiers::NONE))
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    // `get` keeps a multibyte char at the cut (user hotkeys) from panicking.
    match (s.get(..prefix.len()), s.get(prefix.len()..)) {
        (Some(h), Some(t)) if !t.is_empty() && h.eq_ignore_ascii_case(prefix) => Some(t),
        _ => None,
    }
}

fn named_key(lower: &str) -> Option<KeyCode> {
    Some(match lower {
        "enter" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Esc,
        "tab" => KeyCode::Tab,
        "backspace" | "backspace2" => KeyCode::Backspace,
        "delete" | "del" => KeyCode::Delete,
        "insert" => KeyCode::Insert,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "pgup" => KeyCode::PageUp,
        "pgdn" => KeyCode::PageDown,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        f if f.starts_with('f') && f.len() > 1 => {
            let n: u8 = f[1..].parse().ok()?;
            if !(1..=64).contains(&n) {
                return None;
            }
            KeyCode::F(n)
        }
        _ => return None,
    })
}

/// True when two key events name the same k9s key.
pub(crate) fn same_key(a: &KeyEvent, b: &KeyEvent) -> bool {
    key_name(a) == key_name(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hotkey files are user input: odd names must not panic.
    #[test]
    fn parse_key_never_panics() {
        for s in [
            "",
            "ü",
            "ctrlü-x",
            "shifü",
            "alt-",
            "ctrl-",
            "shift-",
            "ctrl-🐶",
            "shift-9",
            "shiftéé",
            "alt-ü",
            "ctr",
            "F",
            "f99999999999",
            "f0",
            "🐶🐶🐶🐶🐶🐶🐶",
        ] {
            let _ = parse_key(s);
        }
        assert_eq!(parse_key("ctrlü-x"), None);
    }

    fn k(code: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, m)
    }

    #[test]
    fn key_names() {
        let cases = [
            (k(KeyCode::Char('a'), KeyModifiers::NONE), "a"),
            (k(KeyCode::Char('A'), KeyModifiers::SHIFT), "shift-a"),
            (k(KeyCode::Char('F'), KeyModifiers::NONE), "shift-f"),
            (k(KeyCode::Char('d'), KeyModifiers::CONTROL), "ctrl-d"),
            (k(KeyCode::Char('D'), KeyModifiers::CONTROL), "ctrl-d"),
            (k(KeyCode::Char(' '), KeyModifiers::CONTROL), "ctrl-space"),
            (k(KeyCode::Char('\\'), KeyModifiers::CONTROL), "ctrl-\\"),
            (k(KeyCode::Enter, KeyModifiers::NONE), "enter"),
            (k(KeyCode::Esc, KeyModifiers::NONE), "esc"),
            (k(KeyCode::Tab, KeyModifiers::NONE), "tab"),
            (k(KeyCode::BackTab, KeyModifiers::SHIFT), "backtab"),
            (k(KeyCode::Char(' '), KeyModifiers::NONE), "space"),
            (k(KeyCode::Char('?'), KeyModifiers::NONE), "?"),
            (k(KeyCode::Char('/'), KeyModifiers::NONE), "/"),
            (k(KeyCode::Char(':'), KeyModifiers::NONE), ":"),
            (k(KeyCode::Char('0'), KeyModifiers::NONE), "0"),
            (k(KeyCode::Char('9'), KeyModifiers::NONE), "9"),
            (k(KeyCode::Char(')'), KeyModifiers::SHIFT), "shift-0"),
            (k(KeyCode::Char('!'), KeyModifiers::NONE), "shift-1"),
            (k(KeyCode::Up, KeyModifiers::NONE), "up"),
            (k(KeyCode::Up, KeyModifiers::SHIFT), "shift-up"),
            (k(KeyCode::PageUp, KeyModifiers::NONE), "pgup"),
            (k(KeyCode::PageDown, KeyModifiers::NONE), "pgdn"),
            (k(KeyCode::F(5), KeyModifiers::NONE), "f5"),
            (k(KeyCode::Backspace, KeyModifiers::NONE), "backspace"),
            (k(KeyCode::Delete, KeyModifiers::NONE), "delete"),
            (k(KeyCode::Char('x'), KeyModifiers::ALT), "alt-x"),
        ];
        for (ev, want) in cases {
            assert_eq!(key_name(&ev), want, "{ev:?}");
        }
    }

    #[test]
    fn parse_keys() {
        let cases = [
            ("a", "a"),
            ("Shift-F", "shift-f"),
            ("shift-f", "shift-f"),
            ("Ctrl-X", "ctrl-x"),
            ("ctrl-x", "ctrl-x"),
            ("Ctrl-Space", "ctrl-space"),
            ("F1", "f1"),
            ("F12", "f12"),
            ("Shift-1", "shift-1"),
            ("Shift-0", "shift-0"),
            ("Enter", "enter"),
            ("Esc", "esc"),
            ("Tab", "tab"),
            ("Backtab", "backtab"),
            ("PgUp", "pgup"),
            ("PgDn", "pgdn"),
            ("space", "space"),
            ("?", "?"),
            ("/", "/"),
            ("5", "5"),
            ("Up", "up"),
            ("Shift-Up", "shift-up"),
            ("Alt-x", "alt-x"),
        ];
        for (s, want) in cases {
            let ev = parse_key(s).unwrap_or_else(|| panic!("parse {s}"));
            assert_eq!(key_name(&ev), want, "{s}");
        }
        for bad in ["", "Bogus", "Ctrl-", "F0", "F99", "Shift-Bogus"] {
            assert!(parse_key(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn same_key_normalizes_shift() {
        let a = parse_key("Shift-F").unwrap();
        let b = KeyEvent::new(KeyCode::Char('F'), KeyModifiers::NONE);
        assert!(same_key(&a, &b));
        assert!(!same_key(&a, &parse_key("f").unwrap()));
    }
}
