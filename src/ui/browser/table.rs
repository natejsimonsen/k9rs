//! Table state and drawing (k9s ui/table.go, ui/select_table.go and the
//! tview table layout k9s relies on).

use std::collections::{BTreeSet, HashSet};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Widget};
use ratatui::Frame;
use unicode_width::UnicodeWidthChar;

use crate::config::Styles;
use crate::k8s::{
    is_all_namespaces, is_cluster_scoped, BLANK_NAMESPACE, CLUSTER_SCOPE, NAMESPACE_ALL,
};
use crate::model::{Align, Decorator, Header, HeaderColumn, RowEvent, TableData};

use super::markup::{
    age_decorate, as_thousands, color_tag, deltas, format_cell, styled_spans, tagged_width,
    truncate,
};

/// k9s client.NotNamespaced.
pub(crate) const NOT_NAMESPACED: &str = "*";
/// ui.maxTruncate.
const MAX_TRUNCATE: usize = 50;

/// model1.SortColumn.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SortColumn {
    pub name: String,
    pub asc: bool,
}

/// client.IsClusterWide.
pub(crate) fn is_cluster_wide(ns: &str) -> bool {
    ns == NAMESPACE_ALL || ns == BLANK_NAMESPACE || ns == CLUSTER_SCOPE
}

/// Table view state (ui.Table + ui.SelectTable).
#[derive(Default)]
pub(crate) struct TableState {
    /// Model data (rows in model order with events/deltas).
    pub data: TableData,
    /// True once the first load landed.
    pub loaded: bool,
    /// Decorated, filtered and sorted rows (what is on screen).
    pub rows: Vec<RowEvent>,
    pub sort: SortColumn,
    pub manual_sort: bool,
    pub selected_col: usize,
    pub wide: bool,
    pub toast: bool,
    pub marks: BTreeSet<String>,
    /// Cursor index into `rows`.
    pub cursor: usize,
    pub row_offset: usize,
    pub col_offset: usize,
    /// Rows dimmed after a delete until the next refresh.
    pub deleted: HashSet<String>,
    /// Visible data rows during the last draw (paging).
    pub page: usize,
}

impl TableState {
    pub(crate) fn new() -> Self {
        Self {
            sort: SortColumn {
                name: String::new(),
                asc: true,
            },
            page: 10,
            ..Default::default()
        }
    }

    /// ui.Table.shouldExcludeColumn.
    pub(crate) fn exclude(&self, h: &HeaderColumn, ns: &str, has_metrics: bool) -> bool {
        h.hide
            || (!self.wide && h.wide && !h.show)
            || (h.name == "NAMESPACE" && !is_cluster_wide(ns))
            || (h.mx && !has_metrics)
            || h.vs
    }

    /// Indices of displayed header columns.
    pub(crate) fn visible_cols(&self, header: &Header, ns: &str, has_metrics: bool) -> Vec<usize> {
        header
            .iter()
            .enumerate()
            .filter(|(_, h)| !self.exclude(h, ns, has_metrics))
            .map(|(i, _)| i)
            .collect()
    }

    pub(crate) fn selected(&self) -> Option<&RowEvent> {
        self.rows.get(self.cursor)
    }

    /// GetSelectedItem.
    pub(crate) fn selected_id(&self) -> Option<String> {
        self.selected().map(|r| r.row.id.clone())
    }

    /// GetSelectedItems: marked rows, else the selected one.
    pub(crate) fn selected_items(&self) -> Vec<String> {
        if self.marks.is_empty() {
            return self.selected_id().into_iter().collect();
        }
        self.marks.iter().cloned().collect()
    }

    /// Value of column `name` on the selected row (TrimCell semantics).
    pub(crate) fn selected_cell(&self, name: &str) -> Option<String> {
        let idx = self.data.header.index_of(name)?;
        self.selected()
            .and_then(|r| r.row.fields.get(idx))
            .map(|s| s.trim().to_string())
    }

    pub(crate) fn toggle_mark(&mut self) {
        let Some(id) = self.selected_id() else {
            return;
        };
        if !self.marks.remove(&id) {
            self.marks.insert(id);
        }
    }

    /// SpanMark: mark from the previous (or next) mark to the cursor.
    pub(crate) fn span_mark(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        let sel = self.cursor;
        let prev = (0..sel)
            .rev()
            .find(|i| self.marks.contains(&self.rows[*i].row.id))
            .or_else(|| {
                (sel..self.rows.len()).find(|i| self.marks.contains(&self.rows[*i].row.id))
            });
        let Some(prev) = prev else {
            return;
        };
        let (lo, hi) = if prev > sel { (sel, prev) } else { (prev, sel) };
        // k9s markRange marks (lo, hi].
        for i in lo + 1..=hi {
            let id = self.rows[i].row.id.clone();
            self.marks.insert(id);
        }
    }

    pub(crate) fn clamp_cursor(&mut self) {
        if self.rows.is_empty() {
            self.cursor = 0;
            self.row_offset = 0;
        } else if self.cursor >= self.rows.len() {
            self.cursor = self.rows.len() - 1;
        }
    }

    pub(crate) fn move_cursor(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let max = self.rows.len() as isize - 1;
        let c = (self.cursor as isize + delta).clamp(0, max);
        self.cursor = c as usize;
    }

    pub(crate) fn select_first(&mut self) {
        self.cursor = 0;
        self.row_offset = 0;
    }

    pub(crate) fn select_last(&mut self) {
        if !self.rows.is_empty() {
            self.cursor = self.rows.len() - 1;
        }
    }

    /// SortColCmd.
    pub(crate) fn sort_col_cmd(&mut self, name: &str, asc: bool) {
        let mut sc = self.sort.clone();
        sc.asc = !sc.asc;
        if sc.name != name {
            sc.asc = asc;
        }
        sc.name = name.to_string();
        self.sort = sc;
        self.manual_sort = true;
    }

    /// initSelectedColumn: visible index of the sort column.
    pub(crate) fn init_selected_column(&mut self, ns: &str, has_metrics: bool) {
        if self.data.header.is_empty() {
            return;
        }
        if self.sort.name.is_empty() {
            self.selected_col = 0;
            return;
        }
        let mut visible = 0;
        for h in self.data.header.iter() {
            if self.exclude(h, ns, has_metrics) {
                continue;
            }
            if h.name == self.sort.name {
                self.selected_col = visible;
                return;
            }
            visible += 1;
        }
        self.selected_col = 0;
    }

    /// moveSelectedColumn (wraps around).
    pub(crate) fn move_selected_column(&mut self, delta: isize, ns: &str, has_metrics: bool) {
        let count = self.visible_cols(&self.data.header, ns, has_metrics).len() as isize;
        if count == 0 {
            return;
        }
        let mut idx = self.selected_col as isize + delta;
        if idx >= count {
            idx = 0;
        } else if idx < 0 {
            idx = count - 1;
        }
        self.selected_col = idx as usize;
    }

    /// SortSelectedColumn.
    pub(crate) fn sort_selected_column(&mut self, ns: &str, has_metrics: bool) {
        let cols = self.visible_cols(&self.data.header, ns, has_metrics);
        let Some(&idx) = cols.get(self.selected_col) else {
            return;
        };
        let name = self.data.header.0[idx].name.clone();
        let asc = if self.sort.name == name {
            !self.sort.asc
        } else {
            true
        };
        self.sort = SortColumn { name, asc };
        self.manual_sort = true;
    }
}

/// ui.SkinTitle colors.
pub(crate) struct TitleColors {
    pub fg: Color,
    pub bg: Color,
    pub highlight: Color,
    pub counter: Color,
    pub filter: Color,
}

impl TitleColors {
    pub(crate) fn from_styles(s: &Styles) -> Self {
        Self {
            fg: s.title_fg,
            bg: s.title_bg,
            highlight: s.title_highlight,
            counter: s.title_counter,
            filter: s.title_filter,
        }
    }
}

/// Title namespace part (styleTitle).
pub(crate) fn title_ns(model_ns: &str, path: &str, extras: &str) -> String {
    let mut ns = model_ns.to_string();
    if is_cluster_wide(&ns) || ns == NOT_NAMESPACED {
        ns = NAMESPACE_ALL.to_string();
    }
    if !path.is_empty() {
        let (cns, n) = crate::k8s::namespaced(path);
        if cns == CLUSTER_SCOPE {
            ns = n.to_string();
        } else {
            ns = path.to_string();
        }
    }
    if !extras.is_empty() {
        ns = extras.to_string();
    }
    ns
}

/// Build the tagged title (k9s NSTitleFmt/TitleFmt + SearchFmt run through SkinTitle).
pub(crate) fn styled_title(
    resource: &str,
    ns: &str,
    count: usize,
    filter: &str,
    c: &TitleColors,
) -> String {
    let fg = color_tag(c.fg);
    let bg = match c.bg {
        Color::Reset => "-".to_string(),
        other => color_tag(other),
    };
    let hi = color_tag(c.highlight);
    let cnt = color_tag(c.counter);
    let flt = color_tag(c.filter);
    let n = as_thousands(count as i64);
    let mut title = if is_cluster_scoped(ns) {
        format!(
            " [{fg}:{bg}:b]{resource}[{fg}:{bg}:-][[{cnt}:{bg}:b]{n}[{fg}:{bg}:-]][{fg}:{bg}:-] "
        )
    } else {
        format!(
            " [{fg}:{bg}:b]{resource}([{hi}:{bg}:b]{ns}[{fg}:{bg}:-])[{fg}:{bg}:-][[{cnt}:{bg}:b]{n}[{fg}:{bg}:-]][{fg}:{bg}:-] "
        )
    };
    if !filter.is_empty() {
        let f = truncate(filter, MAX_TRUNCATE);
        title.push_str(&format!("<[{flt}:{bg}:r]/{f}[{fg}:{bg}:-]> "));
    }
    title
}

/// Plain title text.
pub(crate) fn plain_title(resource: &str, ns: &str, count: usize, filter: &str) -> String {
    let n = as_thousands(count as i64);
    let mut t = if is_cluster_scoped(ns) {
        format!(" {resource}[{n}] ")
    } else {
        format!(" {resource}({ns})[{n}] ")
    };
    if !filter.is_empty() {
        t.push_str(&format!("</{}> ", truncate(filter, MAX_TRUNCATE)));
    }
    t
}

/// Title filter text (ui/table.go styleTitle): a label selector filter
/// shows the parsed selector, else the model's label selector, else the
/// raw filter.
pub(crate) fn title_filter(filter: &str, label: Option<&str>) -> String {
    use crate::model::filter::{extract_label_selector, is_label_selector};
    if is_label_selector(filter) {
        if let Ok(sel) = extract_label_selector(filter) {
            return sel.to_string();
        }
        return filter.to_string();
    }
    if let Some(sel) = label
        .filter(|l| !l.is_empty())
        .and_then(|l| crate::model::labels::parse_selector(l).ok())
        .filter(|s| !s.is_empty())
    {
        return sel.to_string();
    }
    filter.to_string()
}

/// ui.columnIndicator.
fn column_indicator(sort: bool, selected: bool, asc: bool, sorter: Color, name: &str) -> String {
    let display = if selected {
        format!("[::b]{name}[::-]")
    } else {
        name.to_string()
    };
    let suffix = if sort {
        let order = if asc { "↑" } else { "↓" };
        format!("[{}::b]{order}[::]", color_tag(sorter))
    } else {
        String::new()
    };
    display + &suffix
}

/// Everything the draw pass needs besides the table state.
pub(crate) struct DrawInput<'a> {
    pub styles: &'a Styles,
    /// Model namespace.
    pub ns: &'a str,
    pub has_metrics: bool,
    pub title: String,
    /// Row color for a row event (renderer colorer).
    pub colorer: &'a dyn Fn(&RowEvent) -> Color,
    pub now: chrono::DateTime<chrono::Utc>,
}

struct Cell {
    text: String,
    align: Align,
}

/// ui.ComputeMaxColumns over the on-screen rows.
fn compute_pads(header: &Header, rows: &[RowEvent], sort_name: &str) -> Vec<usize> {
    let mut pads: Vec<usize> = header
        .iter()
        .map(|h| h.name.len() + if h.name == sort_name { 2 } else { 0 })
        .collect();
    for re in rows {
        for (i, f) in re.row.fields.iter().enumerate() {
            let w = tagged_width(f) + 1;
            if i < pads.len() && w > pads[i] {
                pads[i] = w;
            }
        }
    }
    pads
}

/// Cell text for column `c` of `re` (buildRow).
fn cell_text(
    re: &RowEvent,
    c: usize,
    h: &Header,
    pads: &[usize],
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    let mut field = re.row.fields.get(c).cloned().unwrap_or_default();
    let blank = re.deltas.iter().all(|d| d.is_empty());
    if !blank && !h.is_time_col(c) {
        let old = re.deltas.get(c).cloned().unwrap_or_default();
        field.push_str(&deltas(&old, &field));
    }
    let col = &h.0[c];
    if col.decorator == Some(Decorator::Age) {
        field = age_decorate(&field, now);
    }
    if col.align == Align::Left {
        field = format_cell(&field, pads.get(c).copied().unwrap_or(0));
    }
    field
}

/// Print tagged `text` at (x, y) clipped to `width`, as tview
/// printWithStyle: right aligned text loses its start, centered text both
/// ends. No ellipsis: derailed/tview's table draws it at a bogus position,
/// so k9s never shows one.
fn print_cell(buf: &mut Buffer, x: u16, y: u16, width: usize, cell: &Cell, base: Style) {
    if width == 0 {
        return;
    }
    let total = tagged_width(&cell.text);
    let (start, skip) = match cell.align {
        _ if total <= width => match cell.align {
            Align::Left => (0, 0),
            Align::Right => (width - total, 0),
            Align::Center => ((width - total) / 2, 0),
        },
        Align::Left => (0, 0),
        Align::Right => (0, total - width),
        Align::Center => (0, (total - width) / 2),
    };
    let spans = styled_spans(&cell.text, base);
    let mut pos = 0usize;
    let mut col = start;
    'outer: for sp in &spans {
        for ch in sp.content.chars() {
            let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
            if cw == 0 {
                continue;
            }
            if pos < skip {
                pos += cw;
                continue;
            }
            if col + cw > width {
                break 'outer;
            }
            if let Some(c) = buf.cell_mut((x + col as u16, y)) {
                c.set_char(ch).set_style(sp.style);
            }
            col += cw;
        }
    }
}

fn fill(buf: &mut Buffer, area: Rect, style: Style) {
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if let Some(c) = buf.cell_mut((x, y)) {
                c.set_style(style);
            }
        }
    }
}

/// Draw the table in `area` (border, title, header, rows).
pub(crate) fn draw_table(
    frame: &mut Frame<'_>,
    area: Rect,
    st: &mut TableState,
    input: &DrawInput<'_>,
) {
    let styles = input.styles;
    // `[fg:bg:-]` resets attributes: the bold border must not leak in.
    let title_spans: Vec<Span<'static>> = styled_spans(
        &input.title,
        Style::default()
            .fg(styles.title_fg)
            .bg(styles.title_bg)
            .remove_modifier(Modifier::BOLD),
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(
            Style::default()
                .fg(styles.border_focus)
                .bg(styles.table_bg)
                .add_modifier(Modifier::BOLD),
        )
        .style(Style::default().bg(styles.table_bg))
        .title(Line::from(title_spans).centered());
    let inner = block.inner(area);
    block.render(area, frame.buffer_mut());
    if inner.width <= 2 || inner.height == 0 {
        return;
    }
    // Border padding (0, 0, 1, 1).
    let body = Rect {
        x: inner.x + 1,
        y: inner.y,
        width: inner.width - 2,
        height: inner.height,
    };
    let buf = frame.buffer_mut();
    let width = body.width as usize;
    let header = st.data.header.clone();
    let cols = st.visible_cols(&header, input.ns, input.has_metrics);

    // Scroll so the cursor stays visible.
    let visible_rows = (body.height as usize).saturating_sub(1);
    st.page = visible_rows.max(1);
    st.clamp_cursor();
    if st.cursor < st.row_offset {
        st.row_offset = st.cursor;
    }
    if visible_rows > 0 && st.cursor >= st.row_offset + visible_rows {
        st.row_offset = st.cursor + 1 - visible_rows;
    }
    if st.row_offset > st.rows.len() {
        st.row_offset = 0;
    }
    let end = (st.row_offset + visible_rows).min(st.rows.len());
    let shown = &st.rows[st.row_offset..end];

    let pads = compute_pads(&header, &st.rows, &st.sort.name);
    let header_cells: Vec<Cell> = cols
        .iter()
        .enumerate()
        .map(|(vi, &c)| {
            let h = &header.0[c];
            Cell {
                text: column_indicator(
                    h.name == st.sort.name,
                    vi == st.selected_col,
                    st.sort.asc,
                    styles.table_sorter,
                    &h.name,
                ),
                align: h.align,
            }
        })
        .collect();
    let row_cells: Vec<Vec<Cell>> = shown
        .iter()
        .map(|re| {
            cols.iter()
                .map(|&c| Cell {
                    text: cell_text(re, c, &header, &pads, input.now),
                    align: header.0[c].align,
                })
                .collect()
        })
        .collect();

    // Column widths (tview: max cell width; columns from the offset while they fit).
    if st.col_offset >= cols.len() {
        st.col_offset = cols.len().saturating_sub(1);
    }
    let mut widths: Vec<usize> = Vec::new();
    let mut table_width = 0;
    for ci in st.col_offset..cols.len() {
        let mut w = tagged_width(&header_cells[ci].text);
        for r in &row_cells {
            w = w.max(tagged_width(&r[ci].text));
        }
        widths.push(w);
        table_width += w + 1;
        if table_width >= width {
            break;
        }
    }
    if table_width < width && !widths.is_empty() {
        let mut to_distribute = width - table_width;
        let mut total = widths.len();
        for w in widths.iter_mut() {
            if total == 0 {
                break;
            }
            let e = to_distribute / total;
            *w += e;
            to_distribute -= e;
            total -= 1;
        }
    }

    let header_style = Style::default()
        .fg(styles.table_header_fg)
        .bg(styles.table_header_bg);
    let mark_color = styles.table_mark;
    let draw_row = |buf: &mut Buffer,
                    y: u16,
                    cells: &[Cell],
                    style_for: &dyn Fn(usize) -> Style,
                    fill_style: Option<Style>| {
        let mut x = 0usize;
        for (i, w) in widths.iter().enumerate() {
            if x >= width {
                break;
            }
            let ci = st.col_offset + i;
            let final_w = (*w).min(width - x);
            if let Some(fs) = fill_style {
                let span = (final_w + 1).min(width - x);
                fill(
                    buf,
                    Rect {
                        x: body.x + x as u16,
                        y,
                        width: span as u16,
                        height: 1,
                    },
                    fs,
                );
            }
            print_cell(
                buf,
                body.x + x as u16,
                y,
                final_w,
                &cells[ci],
                style_for(ci),
            );
            x += w + 1;
        }
    };

    // Header.
    draw_row(
        buf,
        body.y,
        &header_cells,
        &|_| header_style,
        Some(header_style),
    );

    for (i, re) in shown.iter().enumerate() {
        let y = body.y + 1 + i as u16;
        let marked = st.marks.contains(&re.row.id);
        let color = if marked {
            mark_color
        } else {
            (input.colorer)(re)
        };
        let mut base = Style::default().fg(color).bg(styles.table_bg);
        if st.deleted.contains(&re.row.id) {
            base = base.add_modifier(Modifier::DIM);
        }
        draw_row(buf, y, &row_cells[i], &|_| base, None);
        if st.row_offset + i == st.cursor {
            // Selected style: cursor fg on the row color, bold, whole row.
            let sel = Style::default()
                .fg(styles.table_cursor_fg)
                .bg(color)
                .add_modifier(Modifier::BOLD);
            let used = widths.iter().map(|w| w + 1).sum::<usize>().min(width);
            for x in 0..used {
                if let Some(c) = buf.cell_mut((body.x + x as u16, y)) {
                    c.set_style(Style::reset()).set_style(sel);
                    if st.deleted.contains(&re.row.id) {
                        c.modifier.insert(Modifier::DIM);
                    }
                }
            }
        }
    }
}

/// Namespace helper: k9s CleanseNamespace.
pub(crate) fn cleanse_namespace(ns: &str) -> String {
    if ns == NAMESPACE_ALL {
        BLANK_NAMESPACE.to_string()
    } else {
        ns.to_string()
    }
}

/// client.PrintNamespace.
pub(crate) fn print_namespace(ns: &str) -> String {
    if is_all_namespaces(ns) {
        NAMESPACE_ALL.to_string()
    } else {
        ns.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Row;

    fn state() -> TableState {
        let mut st = TableState::new();
        st.data.header = Header::new(vec![
            HeaderColumn::new("NAME"),
            HeaderColumn::new("IP").wide(),
        ]);
        st.rows = ["a", "b", "c", "d"]
            .iter()
            .map(|n| {
                RowEvent::new(
                    crate::model::ResEvent::Unchanged,
                    Row::with(*n, vec![n.to_string(), "1".into()]),
                )
            })
            .collect();
        st
    }

    #[test]
    fn marks_and_spans() {
        let mut st = state();
        st.toggle_mark();
        assert_eq!(st.selected_items(), vec!["a"]);
        st.cursor = 2;
        st.span_mark();
        assert_eq!(st.selected_items(), vec!["a", "b", "c"]);
        st.cursor = 0;
        st.toggle_mark();
        assert_eq!(st.selected_items(), vec!["b", "c"]);
        st.marks.clear();
        st.cursor = 3;
        assert_eq!(st.selected_items(), vec!["d"]);
    }

    #[test]
    fn sort_cmd_toggles() {
        let mut st = state();
        st.sort_col_cmd("NAME", true);
        assert_eq!(
            st.sort,
            SortColumn {
                name: "NAME".into(),
                asc: true
            }
        );
        st.sort_col_cmd("NAME", true);
        assert!(!st.sort.asc);
        st.sort_col_cmd("AGE", true);
        assert_eq!(
            st.sort,
            SortColumn {
                name: "AGE".into(),
                asc: true
            }
        );
        st.sort.name = "NAME".into();
        st.init_selected_column("default", false);
        assert_eq!(st.selected_col, 0);
        st.move_selected_column(1, "default", false);
        assert_eq!(st.selected_col, 0, "single visible column wraps");
        st.wide = true;
        st.move_selected_column(1, "default", false);
        assert_eq!(st.selected_col, 1);
        st.sort_selected_column("default", false);
        assert_eq!(
            st.sort,
            SortColumn {
                name: "IP".into(),
                asc: true
            }
        );
        st.sort_selected_column("default", false);
        assert!(!st.sort.asc);
    }

    #[test]
    fn titles() {
        assert_eq!(title_ns("", "", ""), "all");
        assert_eq!(title_ns("-", "", ""), "all");
        assert_eq!(title_ns("default", "default/nginx", ""), "default/nginx");
        assert_eq!(title_ns("default", "-/node1", ""), "node1");
        assert_eq!(
            plain_title("pods", "default", 1234, ""),
            " pods(default)[1,234] "
        );
        assert_eq!(
            plain_title("pods", "default", 2, "fred"),
            " pods(default)[2] </fred> "
        );
        assert_eq!(plain_title("nodes", "-", 2, ""), " nodes[2] ");
        let c = TitleColors::from_styles(&Styles::default());
        let t = styled_title("pods", "default", 3, "x", &c);
        assert_eq!(
            super::super::markup::strip_tags(&t),
            " pods(default)[3] </x> "
        );
    }
}
