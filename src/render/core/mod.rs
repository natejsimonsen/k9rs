//! Core renderers (owned by the `render-core` slice): pod, container, node,
//! namespace, event, generic/server table, context, alias, port-forward,
//! screen dump, benchmark, dir and helm, plus custom columns (`custcol`) and
//! the JSONPath evaluator behind them.
//!
//! Conventions shared by every renderer here:
//! - Headers and rows follow k9s exactly (names, order, attributes, values).
//! - Ages are human durations computed at render time (k9s `ToAge`), using
//!   `helpers::now()` so tests can pin the clock.
//! - Resource NAMESPACE columns are only present when the namespace is
//!   cluster wide (all, blank or `-`), which is what the k9s table view
//!   shows. Context's NAMESPACE column (a context attribute) is always kept.
//! - Row ids are `crate::k8s::fqn(ns, name)` (`ns/name` or `name`).

use ratatui::style::Color;

use crate::k8s::Gvr;
use crate::model::{Header, RowEvent};

use super::Renderer;

pub mod custcol;
pub mod jsonpath;

mod alias;
mod benchmark;
mod container;
mod context;
mod dir;
mod event;
mod generic;
mod helm;
mod namespace;
mod node;
mod pod;
mod portforward;
mod screendump;

pub use container::{container_ports, container_state};
pub use generic::render_server_table_with;
pub use pod::{pod_phase, pod_status};

/// tcell.ColorSkyblue.
pub(crate) const SKY_BLUE: Color = Color::Rgb(0x87, 0xCE, 0xEB);
/// tcell.ColorNavajoWhite.
pub(crate) const NAVAJO_WHITE: Color = Color::Rgb(0xFF, 0xDE, 0xAD);
/// tcell.ColorPaleGreen.
pub(crate) const PALE_GREEN: Color = Color::Rgb(0x98, 0xFB, 0x98);
/// tcell.ColorCadetBlue.
pub(crate) const CADET_BLUE: Color = Color::Rgb(0x5F, 0x9E, 0xA0);

/// Trimmed field of the column named exactly `name` (k9s IndexOf).
pub(crate) fn field_of<'a>(h: &Header, re: &'a RowEvent, name: &str) -> Option<&'a str> {
    let idx = h.iter().position(|c| c.name == name)?;
    re.row.fields.get(idx).map(|s| s.trim())
}

/// Renderer for `gvr` if this module owns it.
pub fn renderer_for(gvr: &Gvr) -> Option<Box<dyn Renderer>> {
    let r: Box<dyn Renderer> = match (gvr.g(), gvr.v(), gvr.r()) {
        ("", "v1", "pods") => Box::new(pod::Pod::default()),
        ("", "v1", "nodes") => Box::new(node::Node::default()),
        ("", "v1", "namespaces") => Box::new(namespace::Namespace::default()),
        ("", "v1", "events") | ("events.k8s.io", _, "events") => Box::new(event::Event::default()),
        ("", "", "containers") => Box::new(container::Container),
        ("", "", "contexts") => Box::new(context::Context),
        ("", "", "aliases") => Box::new(alias::Alias),
        ("", "", "portforwards") => Box::new(portforward::PortForward),
        ("", "", "screendumps") => Box::new(screendump::ScreenDump),
        ("", "", "benchmarks") => Box::new(benchmark::Benchmark),
        ("", "", "dir") | ("", "", "dirs") => Box::new(dir::Dir),
        ("", "", "helm") => Box::new(helm::Chart),
        ("", "", "helm-history") => Box::new(helm::History),
        _ => return None,
    };
    Some(r)
}

/// The generic fallback renderer (k9s render.Generic; `is_generic()` is true
/// so callers prefer server-side tables). Header: NAMESPACE, NAME,
/// VALID (wide), AGE.
pub fn generic() -> Box<dyn Renderer> {
    Box::new(generic::Generic::default())
}

/// Render a server-side table (k9s render.Table): column names upper cased,
/// "Last Seen"/"First Seen" become time columns, the "Age" column moves to
/// the end as AGE, and NAMESPACE is prepended when `ns` is all-namespaces
/// and the rows are namespaced. Column priority is ignored, like k9s.
pub fn render_server_table(
    table: &crate::k8s::ServerTable,
    ns: &str,
) -> (crate::model::Header, Vec<crate::model::Row>) {
    generic::render_server_table_with(table, ns, None)
}

#[cfg(test)]
pub(crate) mod testutil {
    use kube::api::DynamicObject;

    /// Load a JSON fixture from `src/render/core/testdata`.
    pub fn load(name: &str) -> DynamicObject {
        let path = format!(
            "{}/src/render/core/testdata/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let raw = std::fs::read_to_string(&path).expect("fixture");
        serde_json::from_str(&raw).expect("fixture json")
    }

    pub fn obj(v: serde_json::Value) -> DynamicObject {
        serde_json::from_value(v).expect("object")
    }

    pub fn names(h: &crate::model::Header) -> Vec<String> {
        h.iter().map(|c| c.name.clone()).collect()
    }

    pub fn palette() -> crate::render::Palette {
        use ratatui::style::Color;
        crate::render::Palette {
            std: Color::White,
            add: Color::Blue,
            modify: Color::Green,
            err: Color::Red,
            highlight: Color::Yellow,
            kill: Color::Gray,
            completed: Color::DarkGray,
            pending: Color::Magenta,
        }
    }

    pub fn t0() -> chrono::DateTime<chrono::Utc> {
        crate::render::helpers::parse_time("2020-01-01T00:00:00Z").unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry() {
        for g in [
            "v1/pods",
            "v1/nodes",
            "v1/namespaces",
            "v1/events",
            "events.k8s.io/v1/events",
            "containers",
            "contexts",
            "aliases",
            "portforwards",
            "screendumps",
            "benchmarks",
            "dir",
            "dirs",
            "helm",
            "helm-history",
        ] {
            assert!(renderer_for(&Gvr::parse(g)).is_some(), "{g}");
        }
        assert!(renderer_for(&Gvr::parse("apps/v1/deployments")).is_none());
        assert!(renderer_for(&Gvr::parse("v1/events")).unwrap().is_generic());
        assert!(!renderer_for(&Gvr::parse("v1/pods")).unwrap().is_generic());
        assert!(generic().is_generic());
    }
}
