//! Key codes and key actions (k9s ui/key.go + ui/action.go).
//!
//! Keys are numbered like tcell keys so hints sort the same way k9s sorts
//! them: control keys first (`ctrl-a` = 1 ... `enter` = 13, `esc` = 27),
//! then runes by code point (digits, `?`, `shift-a`..., `a`...), then the
//! special keys (arrows, function keys).

use std::collections::BTreeMap;

use crossterm::event::{KeyCode as CK, KeyEvent, KeyModifiers};

pub(crate) type Key = u32;

pub(crate) const KEY_CTRL_SPACE: Key = 0;
pub(crate) const KEY_BACKSPACE: Key = 8;
pub(crate) const KEY_TAB: Key = 9;
pub(crate) const KEY_ENTER: Key = 13;
pub(crate) const KEY_ESC: Key = 27;
pub(crate) const KEY_CTRL_BACKSLASH: Key = 28;
pub(crate) const KEY_SPACE: Key = 32;
pub(crate) const KEY_BACKSPACE2: Key = 127;
// tcell special keys start after KeyRune (256).
pub(crate) const KEY_UP: Key = 257;
pub(crate) const KEY_DOWN: Key = 258;
pub(crate) const KEY_RIGHT: Key = 259;
pub(crate) const KEY_LEFT: Key = 260;
pub(crate) const KEY_PGUP: Key = 265;
pub(crate) const KEY_PGDN: Key = 266;
pub(crate) const KEY_HOME: Key = 267;
pub(crate) const KEY_END: Key = 268;
pub(crate) const KEY_INSERT: Key = 269;
pub(crate) const KEY_DELETE: Key = 270;
pub(crate) const KEY_BACKTAB: Key = 273;
pub(crate) const KEY_F1: Key = 279;

/// `ctrl-<letter>`.
pub(crate) const fn ctrl(c: char) -> Key {
    (c as u32) - ('a' as u32) + 1
}

/// A plain rune key (`a`, `A` = shift-a, `0`, `?`).
pub(crate) const fn rune(c: char) -> Key {
    c as u32
}

/// Map a crossterm key event to a tcell-like key code.
pub(crate) fn key_of(ev: &KeyEvent) -> Option<Key> {
    let ctrl_mod = ev.modifiers.contains(KeyModifiers::CONTROL);
    match ev.code {
        CK::Char(c) if ctrl_mod => {
            let l = c.to_ascii_lowercase();
            match l {
                'a'..='z' => Some(ctrl(l)),
                ' ' | '@' | '2' => Some(KEY_CTRL_SPACE),
                '\\' | '4' => Some(KEY_CTRL_BACKSLASH),
                ']' | '5' => Some(29),
                '^' | '6' => Some(30),
                '_' | '7' => Some(31),
                _ => None,
            }
        }
        CK::Char(c) => {
            let c = if ev.modifiers.contains(KeyModifiers::SHIFT) && c.is_ascii_lowercase() {
                c.to_ascii_uppercase()
            } else {
                c
            };
            Some(c as u32)
        }
        CK::Null => Some(KEY_CTRL_SPACE),
        CK::Enter => Some(KEY_ENTER),
        CK::Esc => Some(KEY_ESC),
        CK::Tab => Some(KEY_TAB),
        CK::BackTab => Some(KEY_BACKTAB),
        CK::Backspace => Some(KEY_BACKSPACE2),
        CK::Up => Some(KEY_UP),
        CK::Down => Some(KEY_DOWN),
        CK::Left => Some(KEY_LEFT),
        CK::Right => Some(KEY_RIGHT),
        CK::PageUp => Some(KEY_PGUP),
        CK::PageDown => Some(KEY_PGDN),
        CK::Home => Some(KEY_HOME),
        CK::End => Some(KEY_END),
        CK::Insert => Some(KEY_INSERT),
        CK::Delete => Some(KEY_DELETE),
        CK::F(n) if (1..=64).contains(&n) => Some(KEY_F1 + n as u32 - 1),
        _ => None,
    }
}

/// k9s-style (lowercase) key label.
pub(crate) fn key_name(k: Key) -> String {
    match k {
        KEY_CTRL_SPACE => "ctrl-space".into(),
        KEY_BACKSPACE => "backspace".into(),
        KEY_TAB => "tab".into(),
        KEY_ENTER => "enter".into(),
        KEY_ESC => "esc".into(),
        KEY_CTRL_BACKSLASH => "ctrl-\\".into(),
        29 => "ctrl-]".into(),
        30 => "ctrl-^".into(),
        31 => "ctrl-_".into(),
        KEY_SPACE => "space".into(),
        KEY_BACKSPACE2 => "backspace2".into(),
        1..=26 => format!("ctrl-{}", char::from_u32(k - 1 + 'a' as u32).unwrap_or('?')),
        KEY_UP => "up".into(),
        KEY_DOWN => "down".into(),
        KEY_RIGHT => "right".into(),
        KEY_LEFT => "left".into(),
        KEY_PGUP => "pgup".into(),
        KEY_PGDN => "pgdn".into(),
        KEY_HOME => "home".into(),
        KEY_END => "end".into(),
        KEY_INSERT => "insert".into(),
        KEY_DELETE => "delete".into(),
        KEY_BACKTAB => "backtab".into(),
        k if (KEY_F1..KEY_F1 + 64).contains(&k) => format!("f{}", k - KEY_F1 + 1),
        k => match char::from_u32(k) {
            Some(c) if c.is_ascii_uppercase() => format!("shift-{}", c.to_ascii_lowercase()),
            Some(c) => c.to_string(),
            None => String::new(),
        },
    }
}

/// What a key does. Payloads carry the data the handler needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Act {
    // table (view/table.go)
    Help,
    Mark,
    MarkRange,
    ClearMarks,
    Save,
    FilterMode,
    ToggleFaults,
    ToggleWide,
    SortCol(String, bool),
    SortSelected,
    // browser (view/browser.go)
    FilterReset,
    Copy,
    CopyNs,
    Enter,
    Refresh,
    Edit,
    Delete,
    Yaml,
    Describe,
    Warp,
    SwitchNs(String),
    // extenders
    Logs(bool),
    SetImage,
    JumpOwner,
    ShowPortForward,
    PortForward,
    Scale,
    Restart,
    // pods / containers
    Kill,
    Shell,
    Attach,
    Transfer,
    Sanitize,
    ShowNode,
    // workloads
    ReplicaSets,
    Rollback,
    Trigger,
    SuspendResume,
    // nodes
    Cordon(bool),
    Drain,
    NodeShell,
    // misc resources
    UseNamespace,
    Bench,
    Decode,
    UsedBy,
    Rules,
    EventFaults,
    // pseudo resources
    PfDelete,
    DirApply,
    DirDelete,
    DirEdit,
    HelmValues,
    HelmReleases,
    HelmRollback,
    // config driven
    Plugin(String),
    HotKey(String),
}

/// A bound key action (ui.KeyAction).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Action {
    pub desc: String,
    pub act: Act,
    pub visible: bool,
    pub shared: bool,
    pub dangerous: bool,
}

impl Action {
    pub(crate) fn new(desc: &str, act: Act, visible: bool) -> Self {
        Self {
            desc: desc.into(),
            act,
            visible,
            shared: false,
            dangerous: false,
        }
    }
    pub(crate) fn shared(desc: &str, act: Act, visible: bool) -> Self {
        Self {
            shared: true,
            ..Self::new(desc, act, visible)
        }
    }
    pub(crate) fn danger(desc: &str, act: Act) -> Self {
        Self {
            dangerous: true,
            ..Self::new(desc, act, true)
        }
    }
}

pub(crate) type Actions = BTreeMap<Key, Action>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_codes() {
        let ev = |c, m| KeyEvent::new(c, m);
        assert_eq!(
            key_of(&ev(CK::Char('d'), KeyModifiers::CONTROL)),
            Some(ctrl('d'))
        );
        assert_eq!(
            key_of(&ev(CK::Char('F'), KeyModifiers::SHIFT)),
            Some(rune('F'))
        );
        assert_eq!(
            key_of(&ev(CK::Char('f'), KeyModifiers::SHIFT)),
            Some(rune('F'))
        );
        assert_eq!(
            key_of(&ev(CK::Char(' '), KeyModifiers::CONTROL)),
            Some(KEY_CTRL_SPACE)
        );
        assert_eq!(key_of(&ev(CK::Enter, KeyModifiers::NONE)), Some(KEY_ENTER));
        assert_eq!(key_name(ctrl('d')), "ctrl-d");
        assert_eq!(key_name(rune('F')), "shift-f");
        assert_eq!(key_name(rune('0')), "0");
        assert_eq!(key_name(KEY_ENTER), "enter");
        assert_eq!(key_name(KEY_CTRL_BACKSLASH), "ctrl-\\");
        assert_eq!(key_name(KEY_F1 + 1), "f2");
        assert_eq!(key_name(rune('?')), "?");
    }
}
