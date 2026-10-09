//! Test helpers: fixtures, rendering and header comparisons.

use kube::api::DynamicObject;
use ratatui::style::Color;

use crate::k8s::Gvr;
use crate::model::{Align, Header, ResEvent, Row, RowEvent};
use crate::render::{Palette, RenderInput, Renderer};

/// Load `testdata/<name>.json` as a DynamicObject.
pub(super) fn load(name: &str) -> DynamicObject {
    let path = format!(
        "{}/src/render/workloads/testdata/{name}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("{path}: {e}"))
}

pub(super) fn obj(v: serde_json::Value) -> DynamicObject {
    serde_json::from_value(v).expect("object")
}

pub(super) fn renderer(gvr: &str) -> Box<dyn Renderer> {
    super::renderer_for(&Gvr::parse(gvr)).expect("renderer")
}

/// Render `o` with the renderer for `gvr` in all namespaces.
pub(super) fn render(gvr: &str, o: &DynamicObject) -> Row {
    let r = renderer(gvr);
    let mut row = Row::new(r.header("all").len());
    r.render(&RenderInput::new(o), "all", &mut row)
        .expect("render");
    assert_eq!(row.fields.len(), r.header("all").len(), "field count");
    row
}

/// Expected age of an object right now.
pub(super) fn age(o: &DynamicObject) -> String {
    crate::render::helpers::obj_age(o)
}

/// Header as (name, wide, time, align-right, capacity, vs) tuples.
pub(super) fn cols(h: &Header) -> Vec<(String, bool, bool, bool)> {
    h.iter()
        .map(|c| {
            assert!(c.decorator.is_none());
            (c.name.clone(), c.wide, c.time, c.align == Align::Right)
        })
        .collect()
}

pub(super) fn names(h: &Header) -> Vec<String> {
    h.iter().map(|c| c.name.clone()).collect()
}

pub(super) fn color(r: &dyn Renderer, ns: &str, kind: ResEvent, fields: &[&str]) -> Color {
    let h = r.header(ns);
    let re = RowEvent::new(
        kind,
        Row::with("x", fields.iter().map(|s| s.to_string()).collect()),
    );
    r.color_for(ns, &h, &re, &Palette::default())
}

pub(super) fn sv(ss: &[&str]) -> Vec<String> {
    ss.iter().map(|s| s.to_string()).collect()
}
