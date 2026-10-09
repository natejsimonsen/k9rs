//! Server-side tables (k9s dao/table.go).

use serde_json::Value;

use crate::k8s::{ServerTable, ServerTableColumn, ServerTableRow};

/// Accept header k9s sends for table lists (v1, then v1beta1, then plain JSON).
pub(crate) const TABLE_ACCEPT: &str = "application/json;as=Table;v=v1;g=meta.k8s.io,application/json;as=Table;v=v1beta1;g=meta.k8s.io,application/json";

fn s(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Decode a `meta.k8s.io` Table. Returns `None` when the server answered with
/// something else (no table support for the resource).
pub(crate) fn parse_table(v: &Value) -> Option<ServerTable> {
    if v.get("kind").and_then(Value::as_str) != Some("Table") {
        return None;
    }
    let api = v
        .get("apiVersion")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !api.starts_with("meta.k8s.io/") {
        return None;
    }
    let columns = v
        .get("columnDefinitions")
        .and_then(Value::as_array)
        .map(|cc| {
            cc.iter()
                .map(|c| ServerTableColumn {
                    name: s(c, "name"),
                    type_: s(c, "type"),
                    format: s(c, "format"),
                    description: s(c, "description"),
                    priority: c.get("priority").and_then(Value::as_i64).unwrap_or(0) as i32,
                })
                .collect()
        })
        .unwrap_or_default();
    let rows = v
        .get("rows")
        .and_then(Value::as_array)
        .map(|rr| {
            rr.iter()
                .map(|r| {
                    let meta = r
                        .pointer("/object/metadata")
                        .cloned()
                        .unwrap_or(Value::Null);
                    let labels = meta
                        .get("labels")
                        .and_then(Value::as_object)
                        .map(|m| {
                            m.iter()
                                .map(|(k, v)| {
                                    (k.clone(), v.as_str().unwrap_or_default().to_string())
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    ServerTableRow {
                        cells: r
                            .get("cells")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default(),
                        name: s(&meta, "name"),
                        namespace: s(&meta, "namespace"),
                        creation_timestamp: meta
                            .get("creationTimestamp")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        labels,
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    Some(ServerTable { columns, rows })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_tables() {
        let v = json!({
            "kind": "Table", "apiVersion": "meta.k8s.io/v1",
            "columnDefinitions": [
                {"name": "Name", "type": "string", "format": "name", "description": "the name", "priority": 0},
                {"name": "Age", "type": "date", "format": "", "description": "", "priority": 1}
            ],
            "rows": [{
                "cells": ["fred", "3d"],
                "object": {"kind": "PartialObjectMetadata", "apiVersion": "meta.k8s.io/v1",
                    "metadata": {"name": "fred", "namespace": "ns1",
                        "creationTimestamp": "2024-01-01T00:00:00Z", "labels": {"a": "b"}}}
            }, {"cells": [1, null]}]
        });
        let t = parse_table(&v).unwrap();
        assert_eq!(t.columns.len(), 2);
        assert_eq!(t.columns[0].name, "Name");
        assert_eq!(t.columns[0].format, "name");
        assert_eq!(t.columns[0].description, "the name");
        assert_eq!(t.columns[1].type_, "date");
        assert_eq!(t.columns[1].priority, 1);
        assert_eq!(t.rows[0].cells, vec![json!("fred"), json!("3d")]);
        assert_eq!(t.rows[0].name, "fred");
        assert_eq!(t.rows[0].namespace, "ns1");
        assert_eq!(
            t.rows[0].creation_timestamp.as_deref(),
            Some("2024-01-01T00:00:00Z")
        );
        assert_eq!(t.rows[0].labels.get("a").map(String::as_str), Some("b"));
        assert_eq!(t.rows[1].cells, vec![json!(1), Value::Null]);
        assert_eq!(t.rows[1].name, "");

        let beta = json!({"kind": "Table", "apiVersion": "meta.k8s.io/v1beta1", "rows": []});
        assert!(parse_table(&beta).unwrap().rows.is_empty());
        assert!(parse_table(&json!({"kind": "PodList", "items": []})).is_none());
        assert!(parse_table(&json!({"kind": "Table", "apiVersion": "fred/v1"})).is_none());
    }
}
