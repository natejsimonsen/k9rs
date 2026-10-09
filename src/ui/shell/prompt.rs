//! `:` command / `/` filter prompt (k9s ui/prompt.go + model/fish_buff.go).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};

use super::text::{draw_segs, fill, seg, width, Align};
use crate::config::Styles;

/// Prompt height when shown (bordered, view/app.go BufferActive).
pub(crate) const PROMPT_HEIGHT: u16 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PromptKind {
    Command,
    Filter,
}

/// Result of a key press in the prompt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PromptAction {
    /// Key ignored.
    None,
    /// Text changed; `suggest` asks for fresh suggestions (fish buff Notify).
    Edited { suggest: bool },
    /// Suggestion cursor moved (display only).
    Suggested,
    /// Enter: accept the text.
    Submit(String),
    /// Esc: abandon.
    Cancel,
}

#[derive(Clone, Debug)]
pub(crate) struct Prompt {
    pub kind: PromptKind,
    pub text: String,
    suggestion: String,
    suggestions: Vec<String>,
    index: isize,
}

impl Prompt {
    pub fn new(kind: PromptKind, text: impl Into<String>) -> Self {
        Self {
            kind,
            text: text.into(),
            suggestion: String::new(),
            suggestions: Vec::new(),
            index: -1,
        }
    }

    #[cfg(test)]
    pub fn suggestion(&self) -> &str {
        &self.suggestion
    }

    /// fireSuggestionChanged: install suggestions and show the first one.
    pub fn set_suggestions(&mut self, ss: Vec<String>) {
        self.suggestions = ss;
        if self.suggestions.is_empty() {
            self.index = -1;
            self.suggestion.clear();
        } else {
            self.index = 0;
            self.suggestion = self.suggestions[0].clone();
        }
    }

    fn next_suggestion(&mut self) -> Option<String> {
        if self.suggestions.is_empty() {
            return None;
        }
        if self.index < 0 {
            self.index = 0;
        } else {
            self.index += 1;
        }
        if self.index as usize >= self.suggestions.len() {
            self.index = 0;
        }
        Some(self.suggestions[self.index as usize].clone())
    }

    fn prev_suggestion(&mut self) -> Option<String> {
        if self.suggestions.is_empty() {
            return None;
        }
        if self.index < 0 {
            self.index = 0;
        } else {
            self.index -= 1;
        }
        if self.index < 0 {
            self.index = self.suggestions.len() as isize - 1;
        }
        Some(self.suggestions[self.index as usize].clone())
    }

    fn current_suggestion(&self) -> Option<String> {
        if self.index < 0 || self.index as usize >= self.suggestions.len() {
            return None;
        }
        Some(self.suggestions[self.index as usize].clone())
    }

    fn clear_suggestions(&mut self) {
        self.suggestions.clear();
        self.index = -1;
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> PromptAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Backspace | KeyCode::Delete => {
                if self.text.pop().is_none() {
                    return PromptAction::None;
                }
                self.suggestion.clear();
                PromptAction::Edited { suggest: true }
            }
            KeyCode::Char('e') | KeyCode::Char('E') if ctrl => {
                PromptAction::Submit(self.text.clone())
            }
            KeyCode::Char('w') | KeyCode::Char('u') | KeyCode::Char('W') | KeyCode::Char('U')
                if ctrl =>
            {
                self.text.clear();
                self.suggestion.clear();
                PromptAction::Edited { suggest: false }
            }
            KeyCode::Char('f') | KeyCode::Char('F') if ctrl => self.accept(),
            KeyCode::Char('h') if ctrl => {
                if self.text.pop().is_none() {
                    return PromptAction::None;
                }
                self.suggestion.clear();
                PromptAction::Edited { suggest: true }
            }
            KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                if !is_valid_input(c) {
                    return PromptAction::None;
                }
                self.text.push(c);
                PromptAction::Edited { suggest: true }
            }
            KeyCode::Esc => {
                self.text.clear();
                self.suggestion.clear();
                PromptAction::Cancel
            }
            KeyCode::Enter => {
                self.suggestion.clear();
                PromptAction::Submit(self.text.clone())
            }
            KeyCode::Up => match self.next_suggestion() {
                Some(s) => {
                    self.suggestion = s;
                    PromptAction::Suggested
                }
                None => PromptAction::None,
            },
            KeyCode::Down => match self.prev_suggestion() {
                Some(s) => {
                    self.suggestion = s;
                    PromptAction::Suggested
                }
                None => PromptAction::None,
            },
            KeyCode::Tab | KeyCode::Right => self.accept(),
            _ => PromptAction::None,
        }
    }

    fn accept(&mut self) -> PromptAction {
        match self.current_suggestion() {
            Some(s) => {
                self.text.push_str(&s);
                self.suggestion.clear();
                self.clear_suggestions();
                PromptAction::Edited { suggest: false }
            }
            None => PromptAction::None,
        }
    }

    /// Draw the prompt box; returns the cursor position.
    pub fn draw(&self, buf: &mut Buffer, area: Rect, styles: &Styles, no_icons: bool) -> Position {
        let body = Style::default().fg(styles.prompt_fg).bg(styles.prompt_bg);
        fill(buf, area, body);
        let border = match self.kind {
            PromptKind::Command => styles.prompt_border_command,
            PromptKind::Filter => styles.prompt_border_default,
        };
        let bs = Style::default().fg(border).bg(styles.prompt_bg);
        if area.width >= 2 && area.height >= 2 {
            let (l, r, t, b) = (area.left(), area.right() - 1, area.top(), area.bottom() - 1);
            let mut put = |x: u16, y: u16, s: &str| {
                if let Some(c) = buf.cell_mut((x, y)) {
                    c.set_symbol(s);
                    c.set_style(bs);
                }
            };
            for x in l + 1..r {
                put(x, t, "─");
                put(x, b, "─");
            }
            for y in t + 1..b {
                put(l, y, "│");
                put(r, y, "│");
            }
            put(l, t, "┌");
            put(r, t, "┐");
            put(l, b, "└");
            put(r, b, "┘");
        }
        let (icon, prefix) = match self.kind {
            PromptKind::Command => ('🐶', '>'),
            PromptKind::Filter => ('🐩', '/'),
        };
        let icon = if no_icons { ' ' } else { icon };
        let x = area.x + 2;
        let y = area.y + 1;
        let w = area.width.saturating_sub(4);
        let segs = vec![
            seg(format!("{icon}{prefix} "), body),
            seg(self.text.clone(), body.add_modifier(Modifier::BOLD)),
            seg(
                self.suggestion.clone(),
                Style::default()
                    .fg(styles.prompt_suggest)
                    .bg(styles.prompt_bg),
            ),
        ];
        draw_segs(buf, x, y, w, &segs, Align::Left, false);
        let spacer = if no_icons { 3 } else { 4 };
        let cx = (x as usize + spacer + width(&self.text))
            .min((area.right().saturating_sub(2)) as usize);
        Position::new(cx as u16, y)
    }
}

/// prompt.go isValidInputRune.
fn is_valid_input(c: char) -> bool {
    if c.is_control() && c != '\t' && c != '\n' && c != '\r' {
        return false;
    }
    !c.is_control() || c.is_whitespace()
}

#[cfg(test)]
mod tests {
    use super::super::text::testutil::*;
    use super::*;

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    #[test]
    fn typing_and_suggestions() {
        let mut p = Prompt::new(PromptKind::Command, "");
        assert_eq!(
            p.handle_key(key(KeyCode::Char('p'))),
            PromptAction::Edited { suggest: true }
        );
        p.set_suggestions(vec!["od".into(), "odsecuritypolicies".into()]);
        assert_eq!(p.suggestion(), "od");
        assert_eq!(p.handle_key(key(KeyCode::Up)), PromptAction::Suggested);
        assert_eq!(p.suggestion(), "odsecuritypolicies");
        assert_eq!(p.handle_key(key(KeyCode::Up)), PromptAction::Suggested);
        assert_eq!(p.suggestion(), "od");
        assert_eq!(p.handle_key(key(KeyCode::Down)), PromptAction::Suggested);
        assert_eq!(p.suggestion(), "odsecuritypolicies");
        assert_eq!(
            p.handle_key(key(KeyCode::Tab)),
            PromptAction::Edited { suggest: false }
        );
        assert_eq!(p.text, "podsecuritypolicies");
        assert_eq!(p.suggestion(), "");
        assert_eq!(p.handle_key(key(KeyCode::Tab)), PromptAction::None);
        p.handle_key(key(KeyCode::Backspace));
        assert_eq!(p.text, "podsecuritypolicie");
        assert_eq!(
            p.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL)),
            PromptAction::Edited { suggest: false }
        );
        assert_eq!(p.text, "");
        p.text = "po".into();
        assert_eq!(
            p.handle_key(key(KeyCode::Enter)),
            PromptAction::Submit("po".into())
        );
        assert_eq!(p.handle_key(key(KeyCode::Esc)), PromptAction::Cancel);
        assert_eq!(p.text, "");
        assert_eq!(
            p.handle_key(key(KeyCode::Char('\u{7}'))),
            PromptAction::None
        );
    }

    #[test]
    fn renders_command_and_filter() {
        let st = Styles::default();
        let mut p = Prompt::new(PromptKind::Command, "po");
        p.set_suggestions(vec!["ds".into()]);
        let mut buf = Buffer::empty(Rect::new(0, 0, 30, 3));
        let cur = {
            let a = buf.area;
            p.draw(&mut buf, a, &st, false)
        };
        assert_eq!(row(&buf, 1), "│ 🐶> pods                   │");
        assert_eq!(fg(&buf, 0, 0), st.prompt_border_command);
        let (x, y) = find(&buf, "po").unwrap();
        assert_eq!(fg(&buf, x, y), st.prompt_fg);
        assert!(buf[(x, y)].modifier.contains(Modifier::BOLD));
        assert_eq!(fg(&buf, x + 2, y), st.prompt_suggest);
        assert_eq!(cur, Position::new(8, 1));

        let p = Prompt::new(PromptKind::Filter, "ng");
        let mut buf = Buffer::empty(Rect::new(0, 0, 30, 3));
        {
            let a = buf.area;
            p.draw(&mut buf, a, &st, true)
        };
        assert_eq!(row(&buf, 1), "│  / ng                      │");
        assert_eq!(fg(&buf, 0, 0), st.prompt_border_default);
        let mut buf = Buffer::empty(Rect::new(0, 0, 30, 3));
        {
            let a = buf.area;
            p.draw(&mut buf, a, &st, false)
        };
        assert!(row(&buf, 1).starts_with("│ 🐩/ ng"));
    }
}
