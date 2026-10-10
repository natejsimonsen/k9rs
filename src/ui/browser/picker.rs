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
    /// tview List itemOffset (first shown item).
    offset: usize,
    /// Inner height of the last draw.
    height: usize,
    on_pick: Option<PickFn>,
}

impl Picker {
    pub(crate) fn new(ctx: &mut Ctx, title: &str, items: Vec<String>, on_pick: PickFn) -> Self {
        Self {
            id: ctx.new_view_id(),
            title: title.into(),
            items,
            cursor: 0,
            offset: 0,
            height: 0,
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
    /// tview List.MouseHandler: a click picks the item under the pointer,
    /// the wheel scrolls the list.
    fn on_mouse(&mut self, m: crate::ui::Mouse, area: Rect, ctx: &mut Ctx) -> Vec<ViewCmd> {
        use crate::ui::Mouse;
        match m {
            Mouse::Click { y, .. } => {
                let inner_y = area.y + 1;
                let h = area.height.saturating_sub(2);
                if y < inner_y || y >= inner_y + h {
                    return Vec::new();
                }
                let idx = (y - inner_y) as usize + self.offset;
                if idx >= self.items.len() {
                    return Vec::new();
                }
                self.cursor = idx;
                self.pick(idx, ctx)
            }
            Mouse::ScrollUp { .. } => {
                self.offset = self.offset.saturating_sub(1);
                Vec::new()
            }
            Mouse::ScrollDown { .. } => {
                if self.items.len().saturating_sub(self.offset) > self.height {
                    self.offset += 1;
                }
                Vec::new()
            }
        }
    }
    fn draw(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &Ctx) {
        let s = &ctx.styles;
        // List.Draw keeps the current item in view.
        self.height = area.height.saturating_sub(2) as usize;
        if self.cursor < self.offset {
            self.offset = self.cursor;
        } else if self.height > 0 && self.cursor >= self.offset + self.height {
            self.offset = self.cursor + 1 - self.height;
        }
        let block = Block::default()
            .borders(Borders::ALL)
            // tview.Styles.BorderColor (frame border fg); the title is centered.
            .border_style(Style::default().fg(s.border).bg(s.bg))
            .title_alignment(ratatui::layout::Alignment::Center)
            .style(Style::default().bg(s.bg))
            .title(Line::from(Span::styled(
                format!(" {} ", self.title),
                Style::default().fg(s.title_fg).add_modifier(Modifier::BOLD),
            )));
        let mut lines = Vec::new();
        for (i, it) in self.items.iter().enumerate().skip(self.offset) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// tview List.MouseHandler: click picks the item under the pointer
    /// (inside the border), the wheel scrolls within bounds.
    #[test]
    fn picker_mouse() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut ctx = Ctx::new(
            Arc::new(crate::k8s::fake::FakeAccessor::new()),
            Default::default(),
            Default::default(),
            "default".into(),
            tx,
        );
        let got: Arc<Mutex<Option<(usize, String)>>> = Default::default();
        let g = got.clone();
        let mut p = Picker::new(
            &mut ctx,
            "Containers Picker",
            vec!["a".into(), "b".into(), "c".into()],
            Box::new(move |_, i, s| {
                *g.lock().unwrap() = Some((i, s));
                Vec::new()
            }),
        );
        let area = Rect::new(0, 0, 20, 4);
        p.height = 2;
        use crate::ui::Mouse;
        assert!(
            p.on_mouse(Mouse::Click { x: 1, y: 0 }, area, &mut ctx)
                .is_empty(),
            "border"
        );
        p.on_mouse(Mouse::ScrollDown { x: 1, y: 1 }, area, &mut ctx);
        assert_eq!(p.offset, 1);
        p.on_mouse(Mouse::ScrollDown { x: 1, y: 1 }, area, &mut ctx);
        assert_eq!(p.offset, 1, "no scrolling past the end");
        let out = p.on_mouse(Mouse::Click { x: 3, y: 2 }, area, &mut ctx);
        assert!(matches!(out.as_slice(), [ViewCmd::Pop]));
        assert_eq!(*got.lock().unwrap(), Some((2, "c".into())));
    }
}
