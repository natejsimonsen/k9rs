//! Crash audit: every renderer must survive hostile cluster data (missing
//! fields, wrong types, huge or negative quantities, unicode) without
//! panicking; k9s shows n/a or skips instead.

use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};

use kube::api::DynamicObject;
use serde_json::{json, Value};

use super::*;
use crate::k8s::{Metrics, MetricsSnapshot, NodeMetrics, PodMetrics};
use crate::model::{ResEvent, RowEvent};

const GVRS: &[&str] = &[
    "v1/pods",
    "v1/nodes",
    "v1/namespaces",
    "v1/events",
    "events.k8s.io/v1/events",
    "v1/services",
    "v1/configmaps",
    "v1/secrets",
    "v1/serviceaccounts",
    "v1/endpoints",
    "v1/persistentvolumes",
    "v1/persistentvolumeclaims",
    "apps/v1/deployments",
    "apps/v1/daemonsets",
    "apps/v1/statefulsets",
    "apps/v1/replicasets",
    "batch/v1/jobs",
    "batch/v1/cronjobs",
    "networking.k8s.io/v1/networkpolicies",
    "networking.k8s.io/v1/ingresses",
    "discovery.k8s.io/v1/endpointslices",
    "policy/v1/poddisruptionbudgets",
    "storage.k8s.io/v1/storageclasses",
    "apiextensions.k8s.io/v1/customresourcedefinitions",
    "rbac.authorization.k8s.io/v1/clusterroles",
    "rbac.authorization.k8s.io/v1/clusterrolebindings",
    "rbac.authorization.k8s.io/v1/roles",
    "rbac.authorization.k8s.io/v1/rolebindings",
    "containers",
    "contexts",
    "aliases",
    "portforwards",
    "benchmarks",
    "screendumps",
    "dirs",
    "references",
    "workloads",
    "helm",
    "helm-history",
    "policy",
    "users",
    "groups",
    "example.com/v1/widgets",
];

/// Every fixture JSON shipped with the renderer tests.
fn fixtures() -> Vec<Value> {
    let mut out = Vec::new();
    for dir in ["src/render/core/testdata", "src/render/workloads/testdata"] {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(dir);
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.extension().is_some_and(|x| x == "json") {
                out.push(serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap());
            }
        }
    }
    out
}

/// Rewrite every leaf (and optionally containers) with `f`.
fn map_leaves(v: &Value, f: &dyn Fn(&Value) -> Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, v)| {
                    // Keep the object identity fields parseable.
                    if k == "apiVersion" || k == "kind" {
                        (k.clone(), v.clone())
                    } else {
                        (k.clone(), map_leaves(v, f))
                    }
                })
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(|v| map_leaves(v, f)).collect()),
        leaf => f(leaf),
    }
}

/// Hostile variants of one object.
fn variants(base: &Value) -> Vec<Value> {
    let huge = "9".repeat(400);
    let mut out = vec![
        base.clone(),
        json!({"apiVersion": base["apiVersion"], "kind": base["kind"], "metadata": {}}),
        json!({"apiVersion": base["apiVersion"], "kind": base["kind"], "metadata": {"name": "x"},
               "spec": null, "status": null}),
        json!({"apiVersion": base["apiVersion"], "kind": base["kind"], "metadata": {"name": "x"},
               "spec": [], "status": "garbage", "data": 42}),
    ];
    let fns: Vec<Box<dyn Fn(&Value) -> Value>> = vec![
        Box::new(|_| Value::Null),
        Box::new(|v| match v {
            Value::String(_) => json!(-1),
            Value::Number(_) => json!("x"),
            Value::Bool(_) => json!("true"),
            _ => json!({}),
        }),
        Box::new(|v| match v {
            Value::String(_) => json!("🐶👩‍👩‍👧‍👦⚡️漢字\u{0}\t\n\u{202e}"),
            other => other.clone(),
        }),
        Box::new(|v| match v {
            Value::Number(_) => json!(i64::MAX),
            Value::String(_) => json!("-9999999999999999999Ei"),
            other => other.clone(),
        }),
        Box::new(|v| match v {
            Value::Number(_) => json!(i64::MIN),
            Value::String(_) => json!("1e400"),
            other => other.clone(),
        }),
        Box::new(|v| match v {
            Value::Number(_) => json!(-1.5e300),
            Value::String(_) => json!("-5m"),
            other => other.clone(),
        }),
        Box::new(move |v| match v {
            Value::String(_) => json!(huge.clone()),
            other => other.clone(),
        }),
        Box::new(|v| match v {
            Value::String(_) => json!(""),
            Value::Number(_) => json!(0),
            other => other.clone(),
        }),
        Box::new(|v| match v {
            Value::String(_) => json!("2026-13-45T99:99:99Z"),
            other => other.clone(),
        }),
    ];
    for f in &fns {
        let mut v = map_leaves(base, f.as_ref());
        // Metadata must still deserialize into ObjectMeta; mangle names only.
        v["metadata"] = json!({
            "name": base["metadata"]["name"].as_str().map(|_| "🐶-ü-名前").unwrap_or(""),
            "namespace": "ns-⚡️",
            "creationTimestamp": "1970-01-01T00:00:00Z",
            "labels": {"a": "", "": "b"},
            "ownerReferences": [{"apiVersion": "", "kind": "", "name": "", "uid": ""}],
        });
        out.push(v);
    }
    out
}

fn metrics() -> MetricsSnapshot {
    let big = Metrics {
        cpu_millis: i64::MAX,
        mem_bytes: i64::MAX,
    };
    let neg = Metrics {
        cpu_millis: -5,
        mem_bytes: i64::MIN,
    };
    let mut pods = HashMap::new();
    for n in ["", "🐶-ü-名前", "ns-⚡️/🐶-ü-名前"] {
        pods.insert(
            n.to_string(),
            PodMetrics {
                containers: [
                    ("a".to_string(), big),
                    ("b".to_string(), neg),
                    ("c".into(), big),
                ]
                .into_iter()
                .collect(),
            },
        );
    }
    let mut nodes = HashMap::new();
    for n in ["", "🐶-ü-名前"] {
        nodes.insert(
            n.to_string(),
            NodeMetrics {
                usage: big,
                allocatable: neg,
            },
        );
    }
    MetricsSnapshot { pods, nodes }
}

#[test]
fn renderers_survive_hostile_objects() {
    let mut objs: Vec<Value> = Vec::new();
    for f in fixtures() {
        objs.extend(variants(&f));
    }
    let mx = metrics();
    let palette = Palette::default();
    let mut failures = Vec::new();
    for g in GVRS {
        let gvr = Gvr::parse(g);
        let r = registry::renderer_for(&gvr);
        for ns in ["", "default", "-", "all"] {
            let h = r.header(ns);
            for (i, v) in objs.iter().enumerate() {
                let Ok(o) = serde_json::from_value::<DynamicObject>(v.clone()) else {
                    continue;
                };
                for m in [None, Some(&mx)] {
                    let res = catch_unwind(AssertUnwindSafe(|| {
                        let rows = render_all(r.as_ref(), &gvr, std::slice::from_ref(&o), ns, m);
                        for row in rows {
                            for kind in [ResEvent::Add, ResEvent::Update, ResEvent::Delete] {
                                let mut re = RowEvent::new(kind, row.clone());
                                re.deltas = vec!["-1".into(); row.fields.len() + 3];
                                let _ = r.color_for(ns, &h, &re, &palette);
                            }
                        }
                    }));
                    if res.is_err() {
                        failures.push(format!("{g} ns={ns:?} obj#{i} metrics={}", m.is_some()));
                    }
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} panics:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
