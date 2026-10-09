//! metrics.k8s.io parsing and cluster load (k9s client/metrics.go).

use std::collections::HashMap;

use serde_json::Value;

use super::quantity::{cpu_millis, mem_bytes};
use crate::k8s::{fqn, Metrics, NodeMetrics, PodMetrics};

fn usage(v: &Value) -> Metrics {
    Metrics {
        cpu_millis: v
            .get("cpu")
            .and_then(Value::as_str)
            .map(cpu_millis)
            .unwrap_or(0),
        mem_bytes: v
            .get("memory")
            .and_then(Value::as_str)
            .map(mem_bytes)
            .unwrap_or(0),
    }
}

fn items(list: &Value) -> &[Value] {
    list.get("items")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn meta_str<'a>(o: &'a Value, key: &str) -> &'a str {
    o.pointer(&format!("/metadata/{key}"))
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// PodMetricsList -> FQN keyed container metrics (FetchPodsMetricsMap).
pub(crate) fn pod_metrics(list: &Value) -> HashMap<String, PodMetrics> {
    items(list)
        .iter()
        .map(|o| {
            let containers = o
                .get("containers")
                .and_then(Value::as_array)
                .map(|cc| {
                    cc.iter()
                        .map(|c| {
                            let name = c.get("name").and_then(Value::as_str).unwrap_or_default();
                            (
                                name.to_string(),
                                usage(c.get("usage").unwrap_or(&Value::Null)),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            (
                fqn(meta_str(o, "namespace"), meta_str(o, "name")),
                PodMetrics { containers },
            )
        })
        .collect()
}

/// NodeMetricsList -> usage per node name.
pub(crate) fn node_usage(list: &Value) -> HashMap<String, Metrics> {
    items(list)
        .iter()
        .map(|o| {
            (
                meta_str(o, "name").to_string(),
                usage(o.get("usage").unwrap_or(&Value::Null)),
            )
        })
        .collect()
}

/// NodeList -> allocatable per node name.
pub(crate) fn node_allocatable(list: &Value) -> HashMap<String, Metrics> {
    items(list)
        .iter()
        .map(|o| {
            (
                meta_str(o, "name").to_string(),
                usage(o.pointer("/status/allocatable").unwrap_or(&Value::Null)),
            )
        })
        .collect()
}

/// Node metrics for nodes reported by metrics-server, with allocatable.
pub(crate) fn node_metrics(
    usage: &HashMap<String, Metrics>,
    alloc: &HashMap<String, Metrics>,
) -> HashMap<String, NodeMetrics> {
    usage
        .iter()
        .map(|(n, u)| {
            (
                n.clone(),
                NodeMetrics {
                    usage: *u,
                    allocatable: alloc.get(n).copied().unwrap_or_default(),
                },
            )
        })
        .collect()
}

/// client.ToPercentage: floor(v/dv*100), 0 when dv is 0.
pub(crate) fn to_percentage(v: i64, dv: i64) -> i64 {
    if dv == 0 {
        return 0;
    }
    ((v as f64 / dv as f64) * 100.0).floor() as i64
}

/// MetricsServer.ClusterLoad: usage of known nodes vs total allocatable.
pub(crate) fn cluster_load(
    alloc: &HashMap<String, Metrics>,
    usage: &HashMap<String, Metrics>,
) -> (i64, i64) {
    let (mut ccpu, mut cmem, mut tcpu, mut tmem) = (0, 0, 0, 0);
    for (name, a) in alloc {
        tcpu += a.cpu_millis;
        tmem += a.mem_bytes;
        if let Some(u) = usage.get(name) {
            ccpu += u.cpu_millis;
            cmem += u.mem_bytes;
        }
    }
    (to_percentage(ccpu, tcpu), to_percentage(cmem, tmem))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_pod_metrics() {
        let list = json!({"kind": "PodMetricsList", "items": [
            {"metadata": {"name": "p1", "namespace": "default"}, "containers": [
                {"name": "a", "usage": {"cpu": "1500000n", "memory": "10Mi"}},
                {"name": "b", "usage": {"cpu": "2m", "memory": "1Ki"}}
            ]},
            {"metadata": {"name": "p2", "namespace": "kube-system"}, "containers": []}
        ]});
        let mx = pod_metrics(&list);
        assert_eq!(mx.len(), 2);
        let p1 = &mx["default/p1"];
        assert_eq!(
            p1.containers["a"],
            Metrics {
                cpu_millis: 2,
                mem_bytes: 10 << 20
            }
        );
        assert_eq!(
            p1.total(),
            Metrics {
                cpu_millis: 4,
                mem_bytes: (10 << 20) + 1024
            }
        );
        assert!(mx["kube-system/p2"].containers.is_empty());
        assert!(pod_metrics(&json!({})).is_empty());
    }

    #[test]
    fn node_metrics_and_load() {
        let mx = json!({"items": [
            {"metadata": {"name": "n1"}, "usage": {"cpu": "500m", "memory": "1Gi"}},
            {"metadata": {"name": "ghost"}, "usage": {"cpu": "1", "memory": "1Gi"}}
        ]});
        let nodes = json!({"items": [
            {"metadata": {"name": "n1"}, "status": {"allocatable": {"cpu": "2", "memory": "4Gi"}}},
            {"metadata": {"name": "n2"}, "status": {"allocatable": {"cpu": "2", "memory": "4Gi"}}}
        ]});
        let usage = node_usage(&mx);
        let alloc = node_allocatable(&nodes);
        let nm = node_metrics(&usage, &alloc);
        assert_eq!(
            nm["n1"],
            NodeMetrics {
                usage: Metrics {
                    cpu_millis: 500,
                    mem_bytes: 1 << 30
                },
                allocatable: Metrics {
                    cpu_millis: 2000,
                    mem_bytes: 4 << 30
                },
            }
        );
        assert_eq!(nm["ghost"].allocatable, Metrics::default());
        assert!(!nm.contains_key("n2"));
        // ghost is not a known node so it is not counted.
        assert_eq!(cluster_load(&alloc, &usage), (12, 12));
        assert_eq!(cluster_load(&HashMap::new(), &usage), (0, 0));
    }

    #[test]
    fn percentages() {
        assert_eq!(to_percentage(1, 3), 33);
        assert_eq!(to_percentage(2, 3), 66);
        assert_eq!(to_percentage(5, 0), 0);
        assert_eq!(to_percentage(150, 100), 150);
    }
}
