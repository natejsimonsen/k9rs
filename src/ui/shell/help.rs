//! Help view (k9s view/help.go).

use std::any::Any;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::Frame;

use super::header::{sort_hints, to_mnemonic};
use super::text::{draw_segs, seg, Align};
use crate::config::{HotKeys, Styles};
use crate::ui::{Ctx, Hint, View, ViewCmd, ViewId};

pub(crate) const HELP_NAME: &str = "help";

fn h(m: &str, d: &str) -> Hint {
    Hint::new(m, d)
}

fn general() -> Vec<Hint> {
    vec![
        h("?", "Help"),
        h("Ctrl-a", "Aliases"),
        h(":cmd", "Command mode"),
        h("/term", "Filter mode"),
        h("esc", "Back/Clear"),
        h("q", "Back"),
        h("tab", "Field Next"),
        h("backtab", "Field Previous"),
        h("Ctrl-r", "Reload"),
        h("Ctrl-u", "Command Clear"),
        h("Ctrl-e", "Toggle Header"),
        h("Ctrl-g", "Toggle Crumbs"),
        h(":q", "Quit"),
        h("space", "Mark"),
        h("Ctrl-space", "Mark Range"),
        h("Ctrl-\\", "Mark Clear"),
        h("Ctrl-s", "Save"),
    ]
}

fn navigation() -> Vec<Hint> {
    vec![
        h("g", "Goto Top"),
        h("Shift-g", "Goto Bottom"),
        h("Ctrl-b", "Page Up"),
        h("Ctrl-f", "Page Down"),
        h("h", "Left"),
        h("l", "Right"),
        h("k", "Up"),
        h("j", "Down"),
        h("[", "History Back"),
        h("]", "History Forward"),
        h("-", "Last Used Command"),
    ]
}

fn hotkey_hints(hk: &HotKeys) -> Vec<Hint> {
    hk.hot_keys
        .values()
        .map(|k| h(&k.short_cut, &k.description))
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CellKind {
    Heading,
    Key,
    NumKey,
    Info,
}

#[derive(Clone, Debug)]
struct Cell {
    text: String,
    kind: CellKind,
    expand: bool,
}

/// render.Pad: right pad to `w`, truncating with an ellipsis when longer.
fn pad(s: &str, w: usize) -> String {
    let l = s.chars().count();
    if l == w {
        return s.to_string();
    }
    if l > w {
        return super::text::truncate(s, w);
    }
    format!("{s}{}", " ".repeat(w - l))
}

/// The help table: `[row][col]` cells (None = no cell).
struct HelpTable {
    rows: Vec<Vec<Option<Cell>>>,
}

impl HelpTable {
    fn set(&mut self, r: usize, c: usize, cell: Cell) {
        if self.rows.len() <= r {
            self.rows.resize(r + 1, Vec::new());
        }
        let row = &mut self.rows[r];
        if row.len() <= c {
            row.resize(c + 1, None);
        }
        row[c] = Some(cell);
    }

    fn cols(&self) -> usize {
        self.rows.iter().map(|r| r.len()).max().unwrap_or(0)
    }
}

fn build(resource: &[Hint], hotkeys: &[Hint]) -> HelpTable {
    let mut t = HelpTable { rows: Vec::new() };
    let sections: [(&str, Vec<Hint>); 3] = [
        ("RESOURCE", resource.to_vec()),
        ("GENERAL", general()),
        ("NAVIGATION", navigation()),
    ];
    let mut max_rows = general().len();
    let mut col = 0;
    for (title, mut hh) in sections {
        sort_hints(&mut hh);
        add_section(&mut t, col, title, &hh, &mut max_rows);
        col += 2;
    }
    // k9s shows HOTKEYS whenever hotkeys.yaml loads, and a missing file
    // loads fine, so the section is always present (possibly empty).
    let hh = hotkeys.to_vec();
    add_section(&mut t, col, "HOTKEYS", &hh, &mut max_rows);
    t
}

fn add_section(t: &mut HelpTable, c: usize, title: &str, hh: &[Hint], max_rows: &mut usize) {
    let max_key = hh.iter().map(|h| h.mnemonic.len()).max().unwrap_or(0) + 2;
    let max_desc = hh.iter().map(|h| h.description.len()).max().unwrap_or(0);
    if hh.len() > *max_rows {
        *max_rows = hh.len();
    }
    t.set(
        0,
        c,
        Cell {
            text: title.to_string(),
            kind: CellKind::Heading,
            expand: true,
        },
    );
    t.set(
        0,
        c + 1,
        Cell {
            text: pad("", max_key),
            kind: CellKind::Heading,
            expand: true,
        },
    );
    let mut row = 1;
    for hint in hh {
        let kind = if hint.mnemonic.parse::<i64>().is_ok() {
            CellKind::NumKey
        } else {
            CellKind::Key
        };
        t.set(
            row,
            c,
            Cell {
                text: pad(&to_mnemonic(&hint.mnemonic), max_key),
                kind,
                expand: false,
            },
        );
        t.set(
            row,
            c + 1,
            Cell {
                text: pad(&hint.description, max_desc),
                kind: CellKind::Info,
                expand: false,
            },
        );
        row += 1;
    }
    if hh.len() >= *max_rows {
        return;
    }
    for _ in 0..(*max_rows - hh.len()) {
        t.set(
            row,
            c,
            Cell {
                text: pad("", max_key),
                kind: CellKind::Key,
                expand: false,
            },
        );
        t.set(
            row,
            c + 1,
            Cell {
                text: pad("", max_desc),
                kind: CellKind::Info,
                expand: false,
            },
        );
        row += 1;
    }
}

/// tview column widths with expansion distribution.
fn column_widths(t: &HelpTable, avail: usize) -> Vec<usize> {
    let cols = t.cols();
    let mut widths = vec![0usize; cols];
    let mut expansions = vec![0usize; cols];
    for row in &t.rows {
        for (c, cell) in row.iter().enumerate() {
            if let Some(cell) = cell {
                widths[c] = widths[c].max(super::text::width(&cell.text));
                if cell.expand {
                    expansions[c] = 1;
                }
            }
        }
    }
    let used: usize = widths.iter().sum::<usize>() + cols.saturating_sub(1);
    if used < avail {
        let mut to_distribute = avail - used;
        let mut total: usize = expansions.iter().sum();
        for c in 0..cols {
            if total == 0 {
                break;
            }
            let e = expansions[c];
            let w = to_distribute * e / total;
            widths[c] += w;
            to_distribute -= w;
            total -= e;
        }
    }
    widths
}

struct Help {
    id: ViewId,
    resource: Vec<Hint>,
    hotkeys: Vec<Hint>,
    offset: usize,
    page: usize,
}

/// The help view (`?`): general keys, resource keys (`hints`), navigation, hotkeys.
pub(crate) fn new_help(ctx: &mut Ctx, hints: Vec<Hint>) -> Box<dyn View> {
    Box::new(Help {
        id: ctx.new_view_id(),
        resource: hints,
        hotkeys: hotkey_hints(&ctx.hotkeys),
        offset: 0,
        page: 1,
    })
}

impl Help {
    fn row_count(&self) -> usize {
        build(&self.resource, &self.hotkeys).rows.len()
    }

    fn scroll(&mut self, delta: isize) {
        let max = self.row_count().saturating_sub(1) as isize;
        let o = (self.offset as isize + delta).clamp(0, max.max(0));
        self.offset = o as usize;
    }
}

impl View for Help {
    fn id(&self) -> ViewId {
        self.id
    }
    fn name(&self) -> String {
        HELP_NAME.into()
    }
    fn title(&self) -> String {
        "Help".into()
    }
    fn hints(&self) -> Vec<Hint> {
        vec![
            Hint::new("esc", "Back"),
            Hint::new("?", "Back").hidden(),
            Hint::new("enter", "Back").hidden(),
            Hint::new("q", "Back").hidden(),
        ]
    }
    fn handle_key(&mut self, key: KeyEvent, _ctx: &mut Ctx) -> Vec<ViewCmd> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Char('?') => {
                return vec![ViewCmd::Pop]
            }
            KeyCode::Char('j') | KeyCode::Down => self.scroll(1),
            KeyCode::Char('k') | KeyCode::Up => self.scroll(-1),
            KeyCode::Char('f') if ctrl => self.scroll(self.page as isize),
            KeyCode::Char('b') if ctrl => self.scroll(-(self.page as isize)),
            KeyCode::PageDown => self.scroll(self.page as isize),
            KeyCode::PageUp => self.scroll(-(self.page as isize)),
            KeyCode::Char('g') => self.offset = 0,
            KeyCode::Char('G') => self.offset = self.row_count().saturating_sub(1),
            _ => {}
        }
        Vec::new()
    }
    fn draw(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &Ctx) {
        self.page = area.height.max(1) as usize;
        draw_help(
            frame.buffer_mut(),
            area,
            &self.resource,
            &self.hotkeys,
            self.offset,
            &ctx.styles,
        );
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

fn draw_help(
    buf: &mut Buffer,
    area: Rect,
    resource: &[Hint],
    hotkeys: &[Hint],
    offset: usize,
    styles: &Styles,
) {
    let t = build(resource, hotkeys);
    let widths = column_widths(&t, area.width as usize);
    let base = Style::default().bg(styles.help_bg);
    super::text::fill(buf, area, base);
    // Row 0 is the fixed heading row in tview tables? No: help rows scroll as a whole.
    for (vis, r) in (offset..t.rows.len()).enumerate() {
        let y = area.y + vis as u16;
        if y >= area.bottom() {
            break;
        }
        let mut x = area.x as usize;
        let right = (area.x + area.width) as usize;
        for (c, w) in widths.iter().enumerate() {
            if x >= right {
                break;
            }
            if let Some(Some(cell)) = t.rows[r].get(c) {
                let style = match cell.kind {
                    _ if r == 0 => base.fg(styles.help_section),
                    CellKind::Heading => base.fg(styles.help_section),
                    CellKind::Info => base.fg(styles.help_fg),
                    CellKind::NumKey => base.fg(styles.help_num_key).add_modifier(Modifier::BOLD),
                    CellKind::Key => base.fg(styles.help_key).add_modifier(Modifier::BOLD),
                };
                let cw = (*w).min(right - x) as u16;
                draw_segs(
                    buf,
                    x as u16,
                    y,
                    cw,
                    &[seg(cell.text.clone(), style)],
                    Align::Left,
                    false,
                );
            }
            x += w + 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::text::testutil::*;
    use super::*;
    use crate::config::HotKey;

    #[test]
    fn sections_and_colors() {
        let st = Styles::default();
        let res = vec![
            Hint::new("d", "Describe"),
            Hint::new("0", "all"),
            Hint::new("ctrl-d", "Delete").hidden(),
        ];
        let hk = vec![Hint::new("Shift-0", "Pods")];
        let mut buf = Buffer::empty(Rect::new(0, 0, 160, 20));
        {
            let a = buf.area;
            draw_help(&mut buf, a, &res, &hk, 0, &st)
        };
        let r0 = row(&buf, 0);
        assert!(r0.starts_with("RESOURCE"));
        for s in ["GENERAL", "NAVIGATION", "HOTKEYS"] {
            assert!(r0.contains(s), "{s} in {r0}");
        }
        assert_eq!(fg(&buf, 0, 0), st.help_section);
        // Numeric hints sort first and use the num key color.
        // Columns widen to fill the width (tview expansion), so check positions.
        assert_eq!(find(&buf, "<0>"), Some((0, 1)));
        let (ax, ay) = find(&buf, "all").unwrap();
        assert_eq!(ay, 1);
        let (dx, dy) = find(&buf, "Delete").unwrap();
        assert_eq!((dx, dy), (ax, 2), "descriptions share a column");
        assert_eq!(fg(&buf, 0, 1), st.help_num_key);
        assert_eq!(find(&buf, "<ctrl-d>"), Some((0, 2)));
        assert_eq!(fg(&buf, 0, 2), st.help_key);
        assert!(buf[(0, 2)].modifier.contains(Modifier::BOLD));
        let (x, y) = find(&buf, "Delete").unwrap();
        assert_eq!(fg(&buf, x, y), st.help_fg);
        let (x, y) = find(&buf, "<ctrl-a>").unwrap();
        // GENERAL is sorted by description: Aliases first.
        assert_eq!((y, fg(&buf, x, y)), (1, st.help_key));
        assert!(find(&buf, "<:cmd>").is_some());
        assert!(find(&buf, "<shift-g>").is_some());
        assert!(find(&buf, "<shift-0>").is_some());
        // General has 17 entries: rows 1..=17, all other sections padded to it.
        assert!(!row(&buf, 17).trim_end().is_empty());
        assert!(row(&buf, 18).trim().is_empty());
    }

    #[test]
    fn hotkeys_section_shown_without_hotkeys() {
        let t = build(&[Hint::new("d", "Describe")], &[]);
        let titles: Vec<String> = t
            .rows
            .first()
            .map(|r| r.iter().flatten().map(|c| c.text.clone()).collect())
            .unwrap_or_default();
        assert!(titles.iter().any(|c| c == "HOTKEYS"), "{titles:?}");
    }

    #[test]
    fn view_keys_and_hotkeys() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let acc: crate::k8s::SharedAccessor =
            std::sync::Arc::new(crate::k8s::fake::FakeAccessor::new());
        let mut ctx = Ctx::new(
            acc,
            Default::default(),
            Styles::default(),
            "default".into(),
            tx,
        );
        ctx.hotkeys.hot_keys.insert(
            "pods".into(),
            HotKey {
                short_cut: "Shift-1".into(),
                description: "Goto pods".into(),
                command: "pods".into(),
                ..Default::default()
            },
        );
        let mut v = new_help(&mut ctx, vec![Hint::new("d", "Describe")]);
        assert_eq!(v.name(), "help");
        let hk = v.as_any().downcast_ref::<Help>().unwrap();
        assert_eq!(hk.hotkeys, vec![Hint::new("Shift-1", "Goto pods")]);
        assert!(v
            .handle_key(
                KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
                &mut ctx
            )
            .is_empty());
        assert_eq!(v.as_any().downcast_ref::<Help>().unwrap().offset, 1);
        let out = v.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut ctx);
        assert!(matches!(out.as_slice(), [ViewCmd::Pop]));
        let out = v.handle_key(
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
            &mut ctx,
        );
        assert!(matches!(out.as_slice(), [ViewCmd::Pop]));
    }

    #[test]
    fn expansion_spreads_extra_width() {
        let t = build(&[Hint::new("a", "A")], &[]);
        let natural = column_widths(&t, 0);
        let wide = column_widths(&t, 200);
        assert_eq!(wide.iter().sum::<usize>() + wide.len() - 1, 200);
        assert!(wide.iter().zip(natural.iter()).all(|(w, n)| w >= n));
    }
}
