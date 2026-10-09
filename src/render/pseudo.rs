//! Pseudo resources (shared contract). k9s renders non-Kubernetes things
//! (contexts, aliases, port forwards, screen dumps, containers, rbac rules,
//! helm releases, ...) as tables. We wrap each one in a `DynamicObject` with
//! `apiVersion: k9s/v1` so every renderer has one input type. The browser
//! builds these with the functions below; renderers read the same fields.
//!
//! Field shapes (all live under the object's top-level data, camelCase):
//! - Context:     name=ctx | cluster, user, namespace, active(bool)
//! - Alias:       name=gvr | gvr, resource, group, aliases([str], sorted)
//! - PortForward: name=id  | namespace, pod, container, address, localPort,
//!   containerPort, active, startedAt(RFC3339), benchConcurrency, benchRequests
//! - ScreenDump:  name=file | dir, path, modified(RFC3339)
//! - Benchmark:   name=file | namespace, resource, path, modified(RFC3339)
//! - DirEntry:    name=file | path, isDir, size, modified(RFC3339)
//! - Container:   name=container, namespace=pod ns | pod(name), isInit,
//!   isEphemeral, spec(Container json), status(ContainerStatus json|null),
//!   podPhase, index (per kind), idx (`I1`/`M1`/`E1`), podCreationTimestamp
//! - Node objects (real v1 nodes) may carry an extra top-level `podCount`
//!   integer, set by the browser with `with_field`, for the PODS column.
//! - PortForward may carry `benchHost` / `benchPath` (benchmark config).
//! - HelmHistory may carry `release` (release name; namespace in metadata).
//! - HelmRelease: name, namespace | revision, status, chart, appVersion, updated
//! - HelmHistory: name=revision | revision, status, chart, appVersion, updated, description
//! - PolicyRule:  name=resource | namespace, group, resource, resourceNames([]),
//!   nonResourceUrls([]), verbs([]), binding, bindingKind
//! - Subject:     name | subjectKind(User|Group|ServiceAccount), firstLocation,
//!   bindingKind (`subjectKind`, not `kind`, so it does not clash with the
//!   object's own `kind: Subject`)
//! - Reference:   name, namespace | gvr

use kube::api::DynamicObject;
use serde_json::{json, Value};

use crate::k8s::{ContextInfo, PortForwardInfo};

/// apiVersion used for every pseudo object.
pub const API_VERSION: &str = "k9s/v1";

fn mk(kind: &str, name: &str, ns: Option<&str>, data: Value) -> DynamicObject {
    let mut v = json!({
        "apiVersion": API_VERSION,
        "kind": kind,
        "metadata": { "name": name },
    });
    if let Some(ns) = ns {
        v["metadata"]["namespace"] = json!(ns);
    }
    if let (Value::Object(m), Value::Object(d)) = (&mut v, data) {
        for (k, val) in d {
            m.insert(k, val);
        }
    }
    serde_json::from_value(v).expect("pseudo object")
}

/// Read a string field from a pseudo object.
pub fn str_field<'a>(o: &'a DynamicObject, key: &str) -> &'a str {
    o.data.get(key).and_then(|v| v.as_str()).unwrap_or("")
}
/// Read a bool field from a pseudo object.
pub fn bool_field(o: &DynamicObject, key: &str) -> bool {
    o.data.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}
/// Read an integer field from a pseudo object.
pub fn i64_field(o: &DynamicObject, key: &str) -> i64 {
    o.data.get(key).and_then(|v| v.as_i64()).unwrap_or(0)
}
/// Read a string-array field from a pseudo object.
pub fn strs_field(o: &DynamicObject, key: &str) -> Vec<String> {
    o.data
        .get(key)
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

pub fn context(c: &ContextInfo) -> DynamicObject {
    mk(
        "Context",
        &c.name,
        None,
        json!({"cluster": c.cluster, "user": c.user, "namespace": c.namespace, "active": c.active}),
    )
}

pub fn alias(gvr: &crate::k8s::Gvr, mut aliases: Vec<String>) -> DynamicObject {
    aliases.sort();
    mk(
        "Alias",
        &gvr.to_string(),
        None,
        json!({"gvr": gvr.to_string(), "resource": gvr.r(), "group": gvr.g(), "aliases": aliases}),
    )
}

pub fn port_forward(
    pf: &PortForwardInfo,
    bench_concurrency: i64,
    bench_requests: i64,
) -> DynamicObject {
    let s = &pf.spec;
    mk(
        "PortForward",
        &s.id(),
        Some(&s.namespace),
        json!({
            "namespace": s.namespace, "pod": s.pod, "container": s.container,
            "address": s.address, "localPort": s.local_port, "containerPort": s.container_port,
            "active": pf.active, "startedAt": pf.started_at.to_rfc3339(),
            "benchConcurrency": bench_concurrency, "benchRequests": bench_requests,
        }),
    )
}

pub fn screen_dump(
    dir: &str,
    file: &str,
    modified: chrono::DateTime<chrono::Utc>,
) -> DynamicObject {
    let path = std::path::Path::new(dir).join(file);
    mk(
        "ScreenDump",
        file,
        None,
        json!({"dir": dir, "path": path.to_string_lossy(), "modified": modified.to_rfc3339()}),
    )
}

pub fn benchmark(
    namespace: &str,
    resource: &str,
    path: &str,
    file: &str,
    modified: chrono::DateTime<chrono::Utc>,
) -> DynamicObject {
    mk(
        "Benchmark",
        file,
        Some(namespace),
        json!({"namespace": namespace, "resource": resource, "path": path, "modified": modified.to_rfc3339()}),
    )
}

pub fn dir_entry(
    path: &str,
    name: &str,
    is_dir: bool,
    size: u64,
    modified: chrono::DateTime<chrono::Utc>,
) -> DynamicObject {
    mk(
        "DirEntry",
        name,
        None,
        json!({"path": path, "isDir": is_dir, "size": size, "modified": modified.to_rfc3339()}),
    )
}

/// Set an extra top-level field on any object (pseudo or real).
pub fn with_field(mut o: DynamicObject, key: &str, value: Value) -> DynamicObject {
    if let Value::Object(m) = &mut o.data {
        m.insert(key.to_string(), value);
    }
    o
}

/// One container of `pod`. `index` is its position within its own kind
/// (init, main or ephemeral), as k9s numbers them (I1, M1, E1).
pub fn container(
    pod: &DynamicObject,
    spec: &Value,
    status: Option<&Value>,
    is_init: bool,
    is_ephemeral: bool,
    index: usize,
) -> DynamicObject {
    let name = spec.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let ns = pod.metadata.namespace.clone().unwrap_or_default();
    let phase = pod
        .data
        .pointer("/status/phase")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    mk(
        "Container",
        name,
        Some(&ns),
        json!({
            "pod": pod.metadata.name.clone().unwrap_or_default(),
            "isInit": is_init, "isEphemeral": is_ephemeral,
            "spec": spec, "status": status.cloned().unwrap_or(Value::Null),
            "podPhase": phase, "index": index,
            "idx": format!("{}{}", if is_init { "I" } else if is_ephemeral { "E" } else { "M" }, index + 1),
            "podCreationTimestamp": pod.metadata.creation_timestamp.as_ref().map(|t| t.0.to_string()),
        }),
    )
}

pub fn helm_release(
    name: &str,
    namespace: &str,
    revision: &str,
    status: &str,
    chart: &str,
    app_version: &str,
    updated: &str,
) -> DynamicObject {
    mk(
        "HelmRelease",
        name,
        Some(namespace),
        json!({"revision": revision, "status": status, "chart": chart, "appVersion": app_version, "updated": updated}),
    )
}

pub fn helm_history(
    revision: &str,
    status: &str,
    chart: &str,
    app_version: &str,
    updated: &str,
    description: &str,
) -> DynamicObject {
    mk(
        "HelmHistory",
        revision,
        None,
        json!({"revision": revision, "status": status, "chart": chart, "appVersion": app_version, "updated": updated, "description": description}),
    )
}

#[allow(clippy::too_many_arguments)]
pub fn policy_rule(
    namespace: &str,
    group: &str,
    resource: &str,
    resource_names: &[String],
    non_resource_urls: &[String],
    verbs: &[String],
    binding: &str,
    binding_kind: &str,
) -> DynamicObject {
    mk(
        "PolicyRule",
        resource,
        if namespace.is_empty() {
            None
        } else {
            Some(namespace)
        },
        json!({
            "namespace": namespace, "group": group, "resource": resource,
            "resourceNames": resource_names, "nonResourceUrls": non_resource_urls,
            "verbs": verbs, "binding": binding, "bindingKind": binding_kind,
        }),
    )
}

pub fn subject(name: &str, kind: &str, first_location: &str, binding_kind: &str) -> DynamicObject {
    mk(
        "Subject",
        name,
        None,
        json!({"subjectKind": kind, "firstLocation": first_location, "bindingKind": binding_kind}),
    )
}

pub fn reference(namespace: &str, name: &str, gvr: &str) -> DynamicObject {
    mk(
        "Reference",
        name,
        if namespace.is_empty() {
            None
        } else {
            Some(namespace)
        },
        json!({"gvr": gvr}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_roundtrip() {
        let o = context(&ContextInfo {
            name: "c1".into(),
            cluster: "k".into(),
            user: "u".into(),
            namespace: "ns".into(),
            active: true,
        });
        assert_eq!(o.metadata.name.as_deref(), Some("c1"));
        assert_eq!(str_field(&o, "cluster"), "k");
        assert!(bool_field(&o, "active"));
        let a = alias(
            &crate::k8s::Gvr::parse("v1/pods"),
            vec!["po".into(), "pod".into()],
        );
        assert_eq!(strs_field(&a, "aliases"), vec!["po", "pod"]);
        assert_eq!(str_field(&a, "resource"), "pods");
    }

    #[test]
    fn subject_kind_does_not_clash() {
        let o = subject(
            "fernand",
            "User",
            "ClusterRoleBinding:blee",
            "ClusterRoleBinding",
        );
        assert_eq!(o.types.as_ref().map(|t| t.kind.as_str()), Some("Subject"));
        assert_eq!(str_field(&o, "subjectKind"), "User");
        assert_eq!(str_field(&o, "firstLocation"), "ClusterRoleBinding:blee");
        assert_eq!(str_field(&o, "bindingKind"), "ClusterRoleBinding");
    }
}
