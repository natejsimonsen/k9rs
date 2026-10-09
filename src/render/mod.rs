//! Resource renderers (k9s internal/render): turn k8s objects into table rows
//! and pick row colors. Implemented by the `render-core` and
//! `render-workloads` slices; this file is the shared contract.

use anyhow::Result;
use kube::api::DynamicObject;
use ratatui::style::Color;

use crate::k8s::{Gvr, MetricsSnapshot};
use crate::model::{Header, ResEvent, Row, RowEvent};

pub mod core;
pub mod helpers;
pub mod pseudo;
pub mod registry;
pub mod workloads;

pub use registry::renderer_for;

/// Row colorer palette (k9s model1 StdColor/AddColor/...), taken from the
/// skin's `frame.status` section.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub std: Color,
    pub add: Color,
    pub modify: Color,
    pub err: Color,
    pub highlight: Color,
    pub kill: Color,
    pub completed: Color,
    pub pending: Color,
}

impl Default for Palette {
    /// k9s default skin `frame.status` colors (StdColor = newColor, etc.).
    fn default() -> Self {
        crate::config::styles::default_palette()
    }
}

/// Input to a renderer: the object plus optional metrics.
pub struct RenderInput<'a> {
    pub obj: &'a DynamicObject,
    pub metrics: Option<&'a MetricsSnapshot>,
}

impl<'a> RenderInput<'a> {
    pub fn new(obj: &'a DynamicObject) -> Self {
        Self { obj, metrics: None }
    }
    pub fn with_metrics(obj: &'a DynamicObject, metrics: &'a MetricsSnapshot) -> Self {
        Self {
            obj,
            metrics: Some(metrics),
        }
    }
}

/// k9s model1.Renderer.
pub trait Renderer: Send + Sync {
    /// True for the generic (server-side Table / dynamic) renderer.
    fn is_generic(&self) -> bool {
        false
    }
    /// Table header for namespace `ns` (NAMESPACE column present when `ns`
    /// is all-namespaces).
    fn header(&self, ns: &str) -> Header;
    /// Render one object into `row` (sets `row.id` to the FQN).
    fn render(&self, input: &RenderInput<'_>, ns: &str, row: &mut Row) -> Result<()>;
    /// Row color for `re` (k9s ColorerFunc). Default: `default_colorer`.
    fn color_for(&self, ns: &str, header: &Header, re: &RowEvent, palette: &Palette) -> Color {
        default_colorer(ns, header, re, palette)
    }
    /// Apply custom column settings from views.yaml.
    fn set_view_setting(&mut self, _vs: Option<crate::config::ViewSetting>) {}
}

/// k9s model1.IsValid: a row is invalid when its VALID column is set and not "true".
pub fn is_valid(h: &Header, r: &Row) -> bool {
    if r.fields.is_empty() {
        return true;
    }
    match h.index_of("VALID") {
        Some(i) if i < r.fields.len() => {
            let v = r.fields[i].trim();
            v.is_empty() || v.eq_ignore_ascii_case("true")
        }
        _ => true,
    }
}

/// k9s model1.DefaultColorer: invalid → err, add → add, update → modify,
/// delete → kill, else std.
pub fn default_colorer(_ns: &str, h: &Header, re: &RowEvent, p: &Palette) -> Color {
    if !is_valid(h, &re.row) {
        return p.err;
    }
    match re.kind {
        ResEvent::Add => p.add,
        ResEvent::Update => p.modify,
        ResEvent::Delete => p.kill,
        ResEvent::Unchanged => p.std,
    }
}

/// Convenience: render a batch of objects with a renderer into rows.
pub fn render_all(
    r: &dyn Renderer,
    gvr: &Gvr,
    objs: &[DynamicObject],
    ns: &str,
    metrics: Option<&MetricsSnapshot>,
) -> Vec<Row> {
    let size = r.header(ns).len();
    let mut out = Vec::with_capacity(objs.len());
    for o in objs {
        let mut row = Row::new(size);
        let input = RenderInput { obj: o, metrics };
        match r.render(&input, ns, &mut row) {
            Ok(()) => out.push(row),
            Err(e) => tracing::warn!(gvr = %gvr, error = %e, "render failed"),
        }
    }
    out
}
