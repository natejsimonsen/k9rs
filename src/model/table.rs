//! Header / Row / RowEvent / TableData (k9s internal/model1).

use crate::k8s::Gvr;

/// Placeholder for unavailable values (model1.NAValue).
pub const NA_VALUE: &str = "n/a";
/// Missing value marker (model1.MissingValue).
pub const MISSING_VALUE: &str = "<none>";
/// Unknown value marker (render.UnknownValue).
pub const UNKNOWN_VALUE: &str = "<unknown>";
/// Unset value marker (render.UnsetValue).
pub const UNSET_VALUE: &str = "<unset>";
/// Zero value marker (render.ZeroValue).
pub const ZERO_VALUE: &str = "0";

/// Column alignment (tview.AlignLeft/Center/Right).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

/// Column decorator (model1.DecoratorFunc). Only age is used upstream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decorator {
    /// Render the cell as a human duration (AgeDecorator).
    Age,
}

/// Column attributes (model1.Attrs).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HeaderColumn {
    pub name: String,
    pub align: Align,
    pub decorator: Option<Decorator>,
    /// Only shown in wide mode (ctrl-w).
    pub wide: bool,
    /// Forced visible (view settings).
    pub show: bool,
    /// Metrics column (needs metrics-server).
    pub mx: bool,
    /// CPU metrics column.
    pub mxc: bool,
    /// Memory metrics column.
    pub mxm: bool,
    /// Time column (sorted as durations).
    pub time: bool,
    /// Capacity column (sorted as resource quantities).
    pub capacity: bool,
    /// Vulnerability scan column.
    pub vs: bool,
    /// Hidden column.
    pub hide: bool,
}

impl HeaderColumn {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Default::default()
        }
    }
    pub fn align(mut self, a: Align) -> Self {
        self.align = a;
        self
    }
    pub fn wide(mut self) -> Self {
        self.wide = true;
        self
    }
    pub fn mx(mut self) -> Self {
        self.mx = true;
        self
    }
    pub fn mxc(mut self) -> Self {
        self.mx = true;
        self.mxc = true;
        self
    }
    pub fn mxm(mut self) -> Self {
        self.mx = true;
        self.mxm = true;
        self
    }
    /// Time column. Renderers store finished age text (k9s `ToAge`, e.g.
    /// `539d`); sorting treats it as a duration. No draw-time decorator.
    pub fn time(mut self) -> Self {
        self.time = true;
        self
    }
    pub fn capacity(mut self) -> Self {
        self.capacity = true;
        self
    }
    pub fn vs(mut self) -> Self {
        self.vs = true;
        self
    }
    pub fn hide(mut self) -> Self {
        self.hide = true;
        self
    }
}

/// A table header (model1.Header).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Header(pub Vec<HeaderColumn>);

impl Header {
    pub fn new(cols: Vec<HeaderColumn>) -> Self {
        Self(cols)
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn iter(&self) -> std::slice::Iter<'_, HeaderColumn> {
        self.0.iter()
    }
    /// Index of the column named `name` (case-insensitive), if any.
    pub fn index_of(&self, name: &str) -> Option<usize> {
        let up = name.to_uppercase();
        self.0.iter().position(|c| c.name.to_uppercase() == up)
    }
    pub fn has_age(&self) -> bool {
        self.index_of("AGE").is_some()
    }
    pub fn is_metrics_col(&self, idx: usize) -> bool {
        self.0.get(idx).map(|c| c.mx).unwrap_or(false)
    }
    pub fn is_time_col(&self, idx: usize) -> bool {
        self.0.get(idx).map(|c| c.time).unwrap_or(false)
    }
    pub fn is_capacity_col(&self, idx: usize) -> bool {
        self.0.get(idx).map(|c| c.capacity).unwrap_or(false)
    }
    /// Indices of columns to display given wide mode and metrics availability.
    pub fn columns(&self, wide: bool, has_metrics: bool) -> Vec<usize> {
        self.0
            .iter()
            .enumerate()
            .filter(|(_, c)| !c.hide && (wide || !c.wide || c.show) && (has_metrics || !c.mx))
            .map(|(i, _)| i)
            .collect()
    }
}

/// A table row (model1.Row). `id` is the resource FQN (`ns/name`, or `name`
/// for cluster-scoped resources).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Row {
    pub id: String,
    pub fields: Vec<String>,
}

impl Row {
    pub fn new(size: usize) -> Self {
        Self {
            id: String::new(),
            fields: vec![String::new(); size],
        }
    }
    pub fn with(id: impl Into<String>, fields: Vec<String>) -> Self {
        Self {
            id: id.into(),
            fields,
        }
    }
}

/// Resource change kind (model1.ResEvent).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ResEvent {
    #[default]
    Unchanged,
    Add,
    Update,
    Delete,
}

/// A row plus its change kind and per-column deltas (model1.RowEvent).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RowEvent {
    pub kind: ResEvent,
    pub row: Row,
    /// Previous value of each field when `kind == Update`, else empty.
    pub deltas: Vec<String>,
}

impl RowEvent {
    pub fn new(kind: ResEvent, row: Row) -> Self {
        Self {
            kind,
            row,
            deltas: Vec::new(),
        }
    }
    pub fn with_deltas(kind: ResEvent, row: Row, deltas: Vec<String>) -> Self {
        Self { kind, row, deltas }
    }
}

/// Table data: header + rows for a resource (model1.TableData).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TableData {
    pub header: Header,
    pub rows: Vec<RowEvent>,
    pub namespace: String,
    pub gvr: Gvr,
}

impl TableData {
    pub fn new(gvr: Gvr, header: Header, namespace: impl Into<String>) -> Self {
        Self {
            header,
            rows: Vec::new(),
            namespace: namespace.into(),
            gvr,
        }
    }
    pub fn len(&self) -> usize {
        self.rows.len()
    }
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
    /// Compute row events by diffing `new_rows` against this table's rows:
    /// rows present only in `new_rows` are `Add`, changed rows are `Update`
    /// with deltas, unchanged rows are `Unchanged`. Rows that disappeared are
    /// dropped (k9s only shows deletes transiently; it does the same on refresh).
    pub fn update(&mut self, new_rows: Vec<Row>) {
        let old: std::collections::HashMap<&str, &RowEvent> = self
            .rows
            .iter()
            .map(|re| (re.row.id.as_str(), re))
            .collect();
        let mut out = Vec::with_capacity(new_rows.len());
        for row in new_rows {
            match old.get(row.id.as_str()) {
                None => out.push(RowEvent::new(ResEvent::Add, row)),
                Some(prev) if prev.row.fields == row.fields => {
                    out.push(RowEvent::new(ResEvent::Unchanged, row))
                }
                Some(prev) => {
                    let deltas = new_delta_row(&prev.row, &row, &self.header);
                    out.push(RowEvent::with_deltas(ResEvent::Update, row, deltas))
                }
            }
        }
        self.rows = out;
    }
}

/// Compute the delta between two rows (model1.NewDeltaRow): the old value for
/// each changed, non-age column, else "".
pub fn new_delta_row(o: &Row, n: &Row, h: &Header) -> Vec<String> {
    let mut deltas = vec![String::new(); o.fields.len()];
    for (i, old) in o.fields.iter().enumerate() {
        if i >= n.fields.len() {
            continue;
        }
        if h.is_time_col(i) {
            continue;
        }
        if !old.is_empty() && *old != n.fields[i] {
            deltas[i] = old.clone();
        }
    }
    deltas
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delta_row_skips_blank_old_values() {
        let h = Header::new(vec![HeaderColumn::new("A"), HeaderColumn::new("B")]);
        let o = Row::with("x", vec!["".into(), "1".into()]);
        let n = Row::with("x", vec!["v".into(), "2".into()]);
        assert_eq!(new_delta_row(&o, &n, &h), vec!["".to_string(), "1".into()]);
    }

    #[test]
    fn header_columns_respects_wide_and_metrics() {
        let h = Header::new(vec![
            HeaderColumn::new("NAME"),
            HeaderColumn::new("CPU").mxc(),
            HeaderColumn::new("IP").wide(),
            HeaderColumn::new("AGE").time(),
        ]);
        assert_eq!(h.columns(false, false), vec![0, 3]);
        assert_eq!(h.columns(true, false), vec![0, 2, 3]);
        assert_eq!(h.columns(true, true), vec![0, 1, 2, 3]);
        assert_eq!(h.index_of("age"), Some(3));
    }

    #[test]
    fn update_computes_events_and_deltas() {
        let h = Header::new(vec![
            HeaderColumn::new("NAME"),
            HeaderColumn::new("STATUS"),
            HeaderColumn::new("AGE").time(),
        ]);
        let mut td = TableData::new(Gvr::parse("v1/pods"), h, "default");
        td.update(vec![Row::with(
            "default/a",
            vec!["a".into(), "Running".into(), "1m".into()],
        )]);
        assert_eq!(td.rows[0].kind, ResEvent::Add);
        td.update(vec![
            Row::with("default/a", vec!["a".into(), "Error".into(), "2m".into()]),
            Row::with("default/b", vec!["b".into(), "Running".into(), "1s".into()]),
        ]);
        assert_eq!(td.rows[0].kind, ResEvent::Update);
        assert_eq!(td.rows[0].deltas, vec!["", "Running", ""]);
        assert_eq!(td.rows[1].kind, ResEvent::Add);
    }
}
