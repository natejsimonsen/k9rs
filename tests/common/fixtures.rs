//! A small but realistic cluster for the fake accessor.

use std::collections::HashMap;

use chrono::{Duration, Utc};
use k9rs::k8s::fake::{obj, FakeAccessor};
use k9rs::k8s::{ContextInfo, Gvr, LogLine, Metrics, NodeMetrics, PodMetrics, ResourceMeta};
use serde_json::{json, Value};

/// RFC3339 timestamp `mins` minutes ago.
pub fn ago(mins: i64) -> String {
    (Utc::now() - Duration::minutes(mins))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

fn meta(name: &str, ns: Option<&str>, mins: i64) -> Value {
    let mut m = json!({"name": name, "creationTimestamp": ago(mins), "uid": format!("uid-{name}")});
    if let Some(ns) = ns {
        m["namespace"] = json!(ns);
    }
    m
}

fn with(mut v: Value, f: impl FnOnce(&mut Value)) -> Value {
    f(&mut v);
    v
}

fn owner(kind: &str, name: &str, api: &str) -> Value {
    json!([{"apiVersion": api, "kind": kind, "name": name, "uid": format!("uid-{name}"), "controller": true}])
}

struct PodSpec<'a> {
    ns: &'a str,
    name: &'a str,
    labels: Value,
    owner: Option<Value>,
    phase: &'a str,
    ready: bool,
    restarts: i64,
    state: Value,
    terminating: bool,
    age_mins: i64,
}

fn container_status(name: &str, image: &str, p: &PodSpec<'_>) -> Value {
    json!({
        "name": name,
        "image": image,
        "imageID": "",
        "ready": p.ready,
        "started": p.ready,
        "restartCount": p.restarts,
        "state": p.state,
    })
}

fn pod(p: PodSpec<'_>) -> Value {
    let mut m = meta(p.name, Some(p.ns), p.age_mins);
    m["labels"] = p.labels.clone();
    if let Some(o) = &p.owner {
        m["ownerReferences"] = o.clone();
    }
    if p.terminating {
        m["deletionTimestamp"] = json!(ago(1));
    }
    let image = if p.name.starts_with("api") {
        "acme/api:1.2"
    } else {
        "nginx:1.27"
    };
    let main = p.name.split('-').next().unwrap_or(p.name);
    let mut st = json!({
        "phase": p.phase,
        "podIP": "10.244.0.12",
        "hostIP": "192.168.1.10",
        "qosClass": "Burstable",
        "conditions": [{"type": "Ready", "status": if p.ready {"True"} else {"False"}}],
    });
    if p.phase != "Pending" {
        st["containerStatuses"] = json!([container_status(main, image, &p)]);
    }
    json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": m,
        "spec": {
            "nodeName": if p.phase == "Pending" { Value::Null } else { json!("node-1") },
            "serviceAccountName": "default",
            "containers": [{
                "name": main,
                "image": image,
                "ports": [{"name": "http", "containerPort": 80, "protocol": "TCP"}],
                "resources": {
                    "requests": {"cpu": "100m", "memory": "64Mi"},
                    "limits": {"cpu": "500m", "memory": "128Mi"}
                }
            }]
        },
        "status": st,
    })
}

fn running() -> Value {
    json!({"running": {"startedAt": ago(110)}})
}

fn deployment(ns: &str, name: &str, replicas: i64, ready: i64) -> Value {
    json!({
        "apiVersion": "apps/v1",
        "kind": "Deployment",
        "metadata": with(meta(name, Some(ns), 600), |m| m["labels"] = json!({"app": name})),
        "spec": {
            "replicas": replicas,
            "selector": {"matchLabels": {"app": name}},
            "template": {"metadata": {"labels": {"app": name}}, "spec": {"containers": [{"name": name, "image": "nginx:1.27"}]}}
        },
        "status": {"replicas": replicas, "readyReplicas": ready, "updatedReplicas": replicas, "availableReplicas": ready}
    })
}

fn replicaset(ns: &str, name: &str, deploy: &str, replicas: i64, ready: i64) -> Value {
    json!({
        "apiVersion": "apps/v1",
        "kind": "ReplicaSet",
        "metadata": with(meta(name, Some(ns), 600), |m| {
            m["labels"] = json!({"app": deploy});
            m["ownerReferences"] = owner("Deployment", deploy, "apps/v1");
        }),
        "spec": {"replicas": replicas, "selector": {"matchLabels": {"app": deploy}}},
        "status": {"replicas": replicas, "readyReplicas": ready, "availableReplicas": ready}
    })
}

/// Default discovery plus the CRD-backed widgets resource.
pub fn resources() -> Vec<ResourceMeta> {
    let mut r = k9rs::k8s::fake::default_resources();
    // k9s hides the deprecated v1 events; real clusters serve events.k8s.io.
    r.push(ResourceMeta {
        gvr: Gvr::parse("events.k8s.io/v1/events"),
        kind: "Event".into(),
        singular_name: "event".into(),
        short_names: vec!["ev".into()],
        namespaced: true,
        verbs: ["get", "list", "watch", "delete"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        categories: vec![],
    });
    r.push(ResourceMeta {
        gvr: Gvr::parse("example.com/v1/widgets"),
        kind: "Widget".into(),
        singular_name: "widget".into(),
        short_names: vec!["wdg".into()],
        namespaced: true,
        verbs: [
            "get", "list", "watch", "create", "update", "patch", "delete",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect(),
        categories: vec![],
    });
    r
}

/// Build the seeded fake cluster.
pub fn cluster() -> FakeAccessor {
    let f = FakeAccessor::new().with_metrics();
    *f.resources.lock().unwrap() = resources();
    f.contexts.lock().unwrap().push(ContextInfo {
        name: "staging".into(),
        cluster: "staging-cluster".into(),
        user: "staging-user".into(),
        namespace: "kube-system".into(),
        active: false,
    });

    for (n, mins) in [
        ("default", 9000),
        ("kube-system", 9000),
        ("monitoring", 3000),
    ] {
        f.add(
            "v1/namespaces",
            obj(json!({
                "apiVersion": "v1", "kind": "Namespace",
                "metadata": meta(n, None, mins),
                "status": {"phase": "Active"}
            })),
        );
    }

    let rs_owner = || Some(owner("ReplicaSet", "nginx-7d9c5b8f4", "apps/v1"));
    let pods = [
        PodSpec {
            ns: "default",
            name: "nginx-7d9c5b8f4-abcde",
            labels: json!({"app": "nginx", "tier": "web"}),
            owner: rs_owner(),
            phase: "Running",
            ready: true,
            restarts: 0,
            state: running(),
            age_mins: 120,
            terminating: false,
        },
        PodSpec {
            ns: "default",
            name: "nginx-7d9c5b8f4-fghij",
            labels: json!({"app": "nginx", "tier": "web"}),
            owner: rs_owner(),
            phase: "Running",
            ready: true,
            restarts: 1,
            state: running(),
            age_mins: 120,
            terminating: false,
        },
        PodSpec {
            ns: "default",
            name: "api-6f5d8c7b9-xyz12",
            labels: json!({"app": "api", "tier": "backend"}),
            owner: Some(owner("ReplicaSet", "api-6f5d8c7b9", "apps/v1")),
            phase: "Running",
            ready: false,
            restarts: 7,
            state: json!({"waiting": {"reason": "CrashLoopBackOff", "message": "back-off 5m0s"}}),
            age_mins: 120,
            terminating: false,
        },
        PodSpec {
            ns: "default",
            name: "migrate-28xkq",
            labels: json!({"job-name": "migrate"}),
            owner: Some(owner("Job", "migrate", "batch/v1")),
            phase: "Succeeded",
            ready: false,
            restarts: 0,
            state: json!({"terminated": {"reason": "Completed", "exitCode": 0}}),
            age_mins: 120,
            terminating: false,
        },
        PodSpec {
            ns: "default",
            name: "pending-pod",
            labels: json!({"app": "pending"}),
            owner: None,
            phase: "Pending",
            ready: false,
            restarts: 0,
            state: json!({}),
            age_mins: 5,
            terminating: false,
        },
        PodSpec {
            ns: "default",
            name: "old-worker-0",
            labels: json!({"app": "worker"}),
            owner: None,
            phase: "Running",
            ready: true,
            restarts: 0,
            state: running(),
            age_mins: 120,
            terminating: true,
        },
        PodSpec {
            ns: "default",
            name: "web-0",
            labels: json!({"app": "web"}),
            owner: Some(owner("StatefulSet", "web", "apps/v1")),
            phase: "Running",
            ready: true,
            restarts: 0,
            state: running(),
            age_mins: 3000,
            terminating: false,
        },
        PodSpec {
            ns: "kube-system",
            name: "coredns-5d78c9869d-q2w3e",
            labels: json!({"k8s-app": "kube-dns"}),
            owner: Some(owner("ReplicaSet", "coredns-5d78c9869d", "apps/v1")),
            phase: "Running",
            ready: true,
            restarts: 0,
            state: running(),
            age_mins: 120,
            terminating: false,
        },
        PodSpec {
            ns: "kube-system",
            name: "fluentd-x7k2p",
            labels: json!({"app": "fluentd"}),
            owner: Some(owner("DaemonSet", "fluentd", "apps/v1")),
            phase: "Running",
            ready: true,
            restarts: 0,
            state: running(),
            age_mins: 120,
            terminating: false,
        },
    ];
    for p in pods {
        f.add("v1/pods", obj(pod(p)));
    }

    f.add(
        "apps/v1/deployments",
        obj(deployment("default", "nginx", 2, 2)),
    );
    f.add(
        "apps/v1/deployments",
        obj(deployment("default", "api", 1, 0)),
    );
    f.add(
        "apps/v1/deployments",
        obj(deployment("kube-system", "coredns", 1, 1)),
    );
    f.add(
        "apps/v1/replicasets",
        obj(replicaset("default", "nginx-7d9c5b8f4", "nginx", 2, 2)),
    );
    f.add(
        "apps/v1/replicasets",
        obj(replicaset("default", "api-6f5d8c7b9", "api", 1, 0)),
    );
    f.add(
        "apps/v1/statefulsets",
        obj(json!({
            "apiVersion": "apps/v1", "kind": "StatefulSet",
            "metadata": meta("web", Some("default"), 600),
            "spec": {"replicas": 1, "serviceName": "web", "selector": {"matchLabels": {"app": "web"}},
                     "template": {"spec": {"containers": [{"name": "web", "image": "nginx:1.27"}]}}},
            "status": {"replicas": 1, "readyReplicas": 1, "currentReplicas": 1, "updatedReplicas": 1}
        })),
    );
    f.add(
        "apps/v1/daemonsets",
        obj(json!({
            "apiVersion": "apps/v1", "kind": "DaemonSet",
            "metadata": meta("fluentd", Some("kube-system"), 600),
            "spec": {"selector": {"matchLabels": {"app": "fluentd"}},
                     "template": {"spec": {"containers": [{"name": "fluentd", "image": "fluentd:1.16"}]}}},
            "status": {"desiredNumberScheduled": 2, "currentNumberScheduled": 2, "numberReady": 2,
                       "updatedNumberScheduled": 2, "numberAvailable": 2}
        })),
    );
    f.add(
        "v1/services",
        obj(json!({
            "apiVersion": "v1", "kind": "Service",
            "metadata": meta("kubernetes", Some("default"), 9000),
            "spec": {"type": "ClusterIP", "clusterIP": "10.96.0.1",
                     "ports": [{"name": "https", "port": 443, "protocol": "TCP", "targetPort": 6443}]}
        })),
    );
    f.add(
        "v1/services",
        obj(json!({
            "apiVersion": "v1", "kind": "Service",
            "metadata": meta("nginx", Some("default"), 600),
            "spec": {"type": "ClusterIP", "clusterIP": "10.96.12.34", "selector": {"app": "nginx"},
                     "ports": [{"name": "http", "port": 80, "protocol": "TCP", "targetPort": 80}]}
        })),
    );
    f.add(
        "v1/configmaps",
        obj(json!({
            "apiVersion": "v1", "kind": "ConfigMap",
            "metadata": meta("app-config", Some("default"), 600),
            "data": {"LOG_LEVEL": "info", "PORT": "8080"}
        })),
    );
    f.add(
        "v1/secrets",
        obj(json!({
            "apiVersion": "v1", "kind": "Secret",
            "metadata": meta("db-creds", Some("default"), 600),
            "type": "Opaque",
            "data": {"password": "aHVudGVyMg==", "user": "YWRtaW4="}
        })),
    );
    for n in ["node-1", "node-2"] {
        f.add(
            "v1/nodes",
            obj(json!({
                "apiVersion": "v1", "kind": "Node",
                "metadata": with(meta(n, None, 9000), |m| m["labels"] = json!({"kubernetes.io/hostname": n, "node-role.kubernetes.io/worker": ""})),
                "spec": {},
                "status": {
                    "conditions": [{"type": "Ready", "status": "True"}],
                    "addresses": [{"type": "InternalIP", "address": "192.168.1.10"}],
                    "capacity": {"cpu": "4", "memory": "16Gi", "pods": "110"},
                    "allocatable": {"cpu": "4", "memory": "16Gi", "pods": "110"},
                    "nodeInfo": {"kubeletVersion": "v1.34.0", "kernelVersion": "6.8.0", "architecture": "amd64",
                                 "osImage": "Ubuntu 24.04", "containerRuntimeVersion": "containerd://1.7"}
                }
            })),
        );
    }
    f.add(
        "events.k8s.io/v1/events",
        obj(json!({
            "apiVersion": "events.k8s.io/v1", "kind": "Event",
            "metadata": meta("api-6f5d8c7b9-xyz12.17a", Some("default"), 5),
            "regarding": {"kind": "Pod", "name": "api-6f5d8c7b9-xyz12", "namespace": "default"},
            "reason": "BackOff", "type": "Warning",
            "note": "Back-off restarting failed container api",
            "reportingController": "kubelet", "deprecatedCount": 12,
            "deprecatedFirstTimestamp": ago(60), "deprecatedLastTimestamp": ago(2)
        })),
    );
    f.add(
        "batch/v1/jobs",
        obj(json!({
            "apiVersion": "batch/v1", "kind": "Job",
            "metadata": meta("migrate", Some("default"), 300),
            "spec": {"completions": 1, "parallelism": 1, "selector": {"matchLabels": {"job-name": "migrate"}},
                     "template": {"spec": {"containers": [{"name": "migrate", "image": "acme/migrate:1"}]}}},
            "status": {"succeeded": 1, "startTime": ago(300), "completionTime": ago(298)}
        })),
    );
    f.add(
        "batch/v1/cronjobs",
        obj(json!({
            "apiVersion": "batch/v1", "kind": "CronJob",
            "metadata": meta("backup", Some("default"), 3000),
            "spec": {"schedule": "*/5 * * * *", "suspend": false,
                     "jobTemplate": {"spec": {"template": {"spec": {"containers": [{"name": "backup", "image": "acme/backup:1"}]}}}}},
            "status": {"lastScheduleTime": ago(3)}
        })),
    );
    f.add(
        "apiextensions.k8s.io/v1/customresourcedefinitions",
        obj(json!({
            "apiVersion": "apiextensions.k8s.io/v1", "kind": "CustomResourceDefinition",
            "metadata": meta("widgets.example.com", None, 3000),
            "spec": {"group": "example.com", "scope": "Namespaced",
                     "names": {"plural": "widgets", "singular": "widget", "kind": "Widget", "shortNames": ["wdg"]},
                     "versions": [{"name": "v1", "served": true, "storage": true}]}
        })),
    );
    f.add(
        "example.com/v1/widgets",
        obj(json!({
            "apiVersion": "example.com/v1", "kind": "Widget",
            "metadata": meta("my-widget", Some("default"), 30),
            "spec": {"size": 3}
        })),
    );

    let mut pm = HashMap::new();
    for (name, cpu, mem) in [
        ("default/nginx-7d9c5b8f4-abcde", 50, 40 << 20),
        ("default/nginx-7d9c5b8f4-fghij", 25, 30 << 20),
        ("default/web-0", 10, 20 << 20),
    ] {
        let c = name.rsplit('/').next().unwrap().split('-').next().unwrap();
        pm.insert(
            name.to_string(),
            PodMetrics {
                containers: HashMap::from([(
                    c.to_string(),
                    Metrics {
                        cpu_millis: cpu,
                        mem_bytes: mem,
                    },
                )]),
            },
        );
    }
    *f.pod_metrics.lock().unwrap() = pm;
    let mut nm = HashMap::new();
    for (n, cpu) in [("node-1", 1200), ("node-2", 300)] {
        nm.insert(
            n.to_string(),
            NodeMetrics {
                usage: Metrics {
                    cpu_millis: cpu,
                    mem_bytes: 4 << 30,
                },
                allocatable: Metrics {
                    cpu_millis: 4000,
                    mem_bytes: 16 << 30,
                },
            },
        );
    }
    *f.node_metrics.lock().unwrap() = nm;

    let lines = (1..=5)
        .map(|i| LogLine {
            pod: "nginx-7d9c5b8f4-abcde".into(),
            container: "nginx".into(),
            timestamp: Some(format!("2026-10-09T10:00:0{i}Z")),
            text: format!("GET /healthz 200 request-{i}"),
            ..Default::default()
        })
        .collect();
    f.logs
        .lock()
        .unwrap()
        .insert("default/nginx-7d9c5b8f4-abcde".into(), lines);
    f
}
