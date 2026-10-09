//! Generic renderers (k9s render/generic.go and render/table.go).

use anyhow::Result;
use serde_json::{json, Value};

use crate::config::ViewSetting;
use crate::k8s::{is_all_namespaces, ServerTable};
use crate::model::{Header, HeaderColumn, Row};
use crate::render::helpers::*;
use crate::render::{RenderInput, Renderer};

const AGE_TABLE_COL: &str = "Age";
const AGE_COLS: [&str; 3] = ["Last Seen", "First Seen", "Age"];

pub(crate) fn header() -> Header {
    Header::new(vec![
        HeaderColumn::new("NAMESPACE"),
        HeaderColumn::new("NAME"),
        HeaderColumn::new("VALID").wide(),
        HeaderColumn::new("AGE").time(),
    ])
}

/// k9s render.Generic: raw objects without a dedicated renderer. Reports
/// `is_generic()` so callers prefer the server-side table when available.
#[derive(Default)]
pub struct Generic {
    base: ViewBase,
}

impl Renderer for Generic {
    fn is_generic(&self) -> bool {
        true
    }

    fn header(&self, ns: &str) -> Header {
        self.base.header(&header(), ns)
    }

    fn render(&self, input: &RenderInput<'_>, ns: &str, row: &mut Row) -> Result<()> {
        let o = input.obj;
        row.id = obj_fqn(o);
        row.fields = vec![
            obj_ns(o).to_string(),
            obj_name(o).to_string(),
            String::new(),
            obj_age(o),
        ];
        let raw = obj_to_value(o);
        self.base.finish(Some(&raw), &header(), ns, row)
    }

    fn set_view_setting(&mut self, vs: Option<ViewSetting>) {
        self.base.set_view_setting(vs.as_ref());
    }
}

/// [`super::render_server_table`] with views.yaml custom columns. JSONPath
/// specs see the row's metadata (name, namespace, labels,
/// creationTimestamp), like k9s's default `includeObject=Metadata` tables.
pub fn render_server_table_with(
    table: &ServerTable,
    ns: &str,
    vs: Option<&ViewSetting>,
) -> (Header, Vec<Row>) {
    let prepend = is_all_namespaces(ns)
        && (table.rows.is_empty() || table.rows.iter().any(|r| !r.namespace.is_empty()));
    let mut names: Vec<&str> = Vec::with_capacity(table.columns.len() + 1);
    if prepend {
        names.push("Namespace");
    }
    names.extend(table.columns.iter().map(|c| c.name.as_str()));
    let age_idx = names.iter().position(|n| *n == AGE_TABLE_COL);

    let mut cols = Vec::with_capacity(names.len());
    for (i, n) in names.iter().enumerate() {
        if Some(i) == age_idx {
            continue;
        }
        let mut c = HeaderColumn::new(n.to_uppercase());
        if AGE_COLS.contains(n) {
            c = c.time();
        }
        cols.push(c);
    }
    if age_idx.is_some() {
        cols.push(HeaderColumn::new("AGE").time());
    }
    let default = Header::new(cols);

    let mut base = ViewBase::default();
    base.set_view_setting(vs);
    let header = base.header(&default, ns);

    let name_idx = default.iter().position(|c| c.name == "NAME");
    let ns_idx = default.iter().position(|c| c.name == "NAMESPACE");
    let mut rows = Vec::with_capacity(table.rows.len());
    for r in &table.rows {
        let mut cells: Vec<Value> = Vec::with_capacity(r.cells.len() + 1);
        if prepend {
            cells.push(Value::String(r.namespace.clone()));
        }
        cells.extend(r.cells.iter().cloned());

        let mut fields = Vec::with_capacity(default.len());
        let mut age: Option<&Value> = None;
        for (i, c) in cells.iter().enumerate() {
            if Some(i) == age_idx {
                age = Some(c);
                continue;
            }
            fields.push(if c.is_null() {
                String::new()
            } else {
                go_fmt(c)
            });
        }
        if age_idx.is_some() {
            match age.and_then(Value::as_str) {
                Some(a) => fields.push(a.to_string()),
                None => {
                    tracing::warn!("No Duration detected on age field");
                    fields.push(NA_VALUE.into());
                }
            }
        }

        let mut name = r.name.clone();
        if name.is_empty() {
            name = name_idx
                .and_then(|i| fields.get(i).cloned())
                .unwrap_or_else(|| UNKNOWN_VALUE.into());
        }
        let mut rns = r.namespace.clone();
        if rns.is_empty() {
            rns = ns_idx
                .and_then(|i| fields.get(i).cloned())
                .unwrap_or_default();
        }
        let mut row = Row::with(crate::k8s::fqn(&rns, &name), fields);
        let meta = json!({"metadata": {
            "name": r.name,
            "namespace": r.namespace,
            "labels": r.labels,
            "creationTimestamp": r.creation_timestamp,
        }});
        match base.finish(Some(&meta), &default, ns, &mut row) {
            Ok(()) => rows.push(row),
            Err(e) => tracing::warn!(error = %e, "server table row render failed"),
        }
    }
    (header, rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::k8s::{ServerTableColumn, ServerTableRow};
    use crate::render::core::testutil::*;

    fn col(n: &str) -> ServerTableColumn {
        ServerTableColumn {
            name: n.into(),
            type_: "string".into(),
            ..Default::default()
        }
    }

    fn row(ns: &str, name: &str, cells: Vec<Value>) -> ServerTableRow {
        ServerTableRow {
            cells,
            name: name.into(),
            namespace: ns.into(),
            ..Default::default()
        }
    }

    #[test]
    fn with_ns() {
        let t = ServerTable {
            columns: vec![col("a"), col("b"), col("c")],
            rows: vec![row(
                "ns1",
                "fred",
                vec![json!("c1"), json!("c2"), json!("c3")],
            )],
        };
        let (h, rr) = crate::render::core::render_server_table(&t, "all");
        assert_eq!(names(&h), ["NAMESPACE", "A", "B", "C"]);
        assert_eq!(rr[0].id, "ns1/fred");
        assert_eq!(rr[0].fields, ["ns1", "c1", "c2", "c3"]);
        let (h, rr) = crate::render::core::render_server_table(&t, "ns1");
        assert_eq!(names(&h), ["A", "B", "C"]);
        assert_eq!(rr[0].id, "ns1/fred");
        assert_eq!(rr[0].fields, ["c1", "c2", "c3"]);
    }

    #[test]
    fn cluster_wide() {
        let t = ServerTable {
            columns: vec![col("a"), col("b"), col("c")],
            rows: vec![row("", "fred", vec![json!("c1"), json!(2), Value::Null])],
        };
        let (h, rr) = crate::render::core::render_server_table(&t, "-");
        assert_eq!(names(&h), ["A", "B", "C"]);
        assert_eq!(rr[0].id, "fred");
        assert_eq!(rr[0].fields, ["c1", "2", ""]);
        // all-namespaces but cluster scoped rows: no NAMESPACE column.
        let (h, _) = crate::render::core::render_server_table(&t, "all");
        assert_eq!(names(&h), ["A", "B", "C"]);
    }

    #[test]
    fn age_and_time_cols() {
        let t = ServerTable {
            columns: vec![col("a"), col("Age"), col("Last Seen"), col("c")],
            rows: vec![
                row(
                    "",
                    "fred",
                    vec![json!("c1"), json!("2d"), json!("5m"), json!("c2")],
                ),
                row(
                    "",
                    "blee",
                    vec![json!("c1"), json!(3), json!("5m"), json!("c2")],
                ),
            ],
        };
        let (h, rr) = crate::render::core::render_server_table(&t, "-");
        assert_eq!(names(&h), ["A", "LAST SEEN", "C", "AGE"]);
        assert!(h.0[1].time && h.0[3].time && !h.0[0].time);
        assert_eq!(rr[0].fields, ["c1", "5m", "c2", "2d"]);
        assert_eq!(rr[1].fields, ["c1", "5m", "c2", "n/a"]);
    }

    #[test]
    fn name_from_cells() {
        let t = ServerTable {
            columns: vec![col("Name"), col("x")],
            rows: vec![row("", "", vec![json!("bob"), json!("y")])],
        };
        let (_, rr) = crate::render::core::render_server_table(&t, "-");
        assert_eq!(rr[0].id, "bob");
    }

    #[test]
    fn custom_columns() {
        let t = ServerTable {
            columns: vec![col("a"), col("b"), col("c")],
            rows: vec![row(
                "ns1",
                "fred",
                vec![json!("c1"), json!("c2"), json!("c3")],
            )],
        };
        let vs = ViewSetting {
            columns: vec![
                "NAMESPACE".into(),
                "BLEE:.metadata.name".into(),
                "ZORG:.metadata.namespace".into(),
            ],
            sort_column: String::new(),
        };
        let (h, rr) = render_server_table_with(&t, "all", Some(&vs));
        assert_eq!(names(&h), ["NAMESPACE", "BLEE", "ZORG", "A", "B", "C"]);
        assert_eq!(rr[0].id, "ns1/fred");
        assert_eq!(rr[0].fields, ["ns1", "fred", "ns1", "c1", "c2", "c3"]);
    }

    #[test]
    fn generic_render() {
        let o = load("p1");
        let g = Generic::default();
        assert!(g.is_generic());
        let mut r = Row::default();
        with_now(t0(), || g.render(&RenderInput::new(&o), "all", &mut r)).unwrap();
        assert_eq!(
            names(&g.header("all")),
            ["NAMESPACE", "NAME", "VALID", "AGE"]
        );
        assert_eq!(r.fields[1..], ["nginx-7fb78fb6d8-2w75j", "", "4h32m"]);
        g.render(&RenderInput::new(&o), "default", &mut r).unwrap();
        assert_eq!(r.fields.len(), 3);
        assert_eq!(g.header("default").len(), 3);
    }
}
