//! Selection list (k9s view.Picker, also used for dialog.ShowSelection):
//! items get `a`, `b`, ... shortcuts; enter or the shortcut picks.

use std::any::Any;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::ui::{Ctx, Hint, View, ViewCmd, ViewId};

/// Callback run with the chosen item.
pub(crate) type PickFn = Box<dyn FnOnce(&mut Ctx, usize, String) -> Vec<ViewCmd> + Send>;

pub(crate) struct Picker {
    id: ViewId,
    title: String,
    items: Vec<String>,
    cursor: usize,
    on_pick: Option<PickFn>,
}

impl Picker {
    pub(crate) fn new(ctx: &mut Ctx, title: &str, items: Vec<String>, on_pick: PickFn) -> Self {
        Self {
            id: ctx.new_view_id(),
            title: title.into(),
            items,
            cursor: 0,
            on_pick: Some(on_pick),
        }
    }

    fn pick(&mut self, idx: usize, ctx: &mut Ctx) -> Vec<ViewCmd> {
        let Some(item) = self.items.get(idx).cloned() else {
            return Vec::new();
        };
        let mut out = vec![ViewCmd::Pop];
        if let Some(f) = self.on_pick.take() {
            out.extend(f(ctx, idx, item));
        }
        out
    }

    #[cfg(test)]
    pub(crate) fn items(&self) -> &[String] {
        &self.items
    }
}

impl View for Picker {
    fn owns_frame(&self) -> bool {
        true
    }
    fn id(&self) -> ViewId {
        self.id
    }
    fn name(&self) -> String {
        "picker".into()
    }
    fn title(&self) -> String {
        format!(" {} ", self.title)
    }
    fn hints(&self) -> Vec<Hint> {
        vec![Hint::new("esc", "Back")]
    }
    fn handle_key(&mut self, key: KeyEvent, ctx: &mut Ctx) -> Vec<ViewCmd> {
        match key.code {
            KeyCode::Esc => vec![ViewCmd::Pop],
            KeyCode::Up => {
                self.cursor = self.cursor.saturating_sub(1);
                Vec::new()
            }
            KeyCode::Down | KeyCode::Tab => {
                if self.cursor + 1 < self.items.len() {
                    self.cursor += 1;
                }
                Vec::new()
            }
            KeyCode::Enter => self.pick(self.cursor, ctx),
            KeyCode::Char(c) if c.is_ascii_lowercase() => {
                let idx = (c as u8 - b'a') as usize;
                self.pick(idx, ctx)
            }
            _ => Vec::new(),
        }
    }
    fn draw(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &Ctx) {
        let s = &ctx.styles;
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(s.border_focus).bg(s.bg))
            .style(Style::default().bg(s.bg))
            .title(Line::from(Span::styled(
                format!(" {} ", self.title),
                Style::default().fg(s.title_fg).add_modifier(Modifier::BOLD),
            )));
        let mut lines = Vec::new();
        for (i, it) in self.items.iter().enumerate() {
            let sc = char::from(b'a' + (i as u8 % 26));
            let main = if i == self.cursor {
                Style::default().fg(s.bg).bg(s.picker_focus)
            } else {
                Style::default().fg(s.picker_main)
            };
            lines.push(Line::from(vec![
                Span::styled(format!("({sc}) "), Style::default().fg(s.picker_shortcut)),
                Span::styled(it.clone(), main),
            ]));
        }
        frame.render_widget(Paragraph::new(lines).block(block), area);
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
