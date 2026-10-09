//! In-memory `Accessor` for tests. Objects are stored per GVR as JSON-backed
//! `DynamicObject`s; mutating calls are recorded in `calls` for assertions.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::StreamExt;
use kube::api::DynamicObject;

use super::accessor::{Accessor, LogStream, PortForwardHandle};
use super::types::*;
use super::{fqn, is_all_namespaces, namespaced, Gvr};

#[derive(Default)]
pub struct FakeAccessor {
    pub objects: Mutex<BTreeMap<Gvr, Vec<DynamicObject>>>,
    pub resources: Mutex<Vec<ResourceMeta>>,
    pub contexts: Mutex<Vec<ContextInfo>>,
    pub active: Mutex<String>,
    pub pod_metrics: Mutex<HashMap<String, PodMetrics>>,
    pub node_metrics: Mutex<HashMap<String, NodeMetrics>>,
    /// Log lines returned by `tail_logs`, keyed by `ns/pod`.
    pub logs: Mutex<HashMap<String, Vec<LogLine>>>,
    /// Recorded mutating calls, e.g. `delete v1/pods default/a`.
    pub calls: Mutex<Vec<String>>,
    pub metrics: bool,
    /// `(gvr, namespace)` pairs `can(.., "list")` refuses.
    pub denied: Mutex<Vec<(String, String)>>,
}

/// Build a DynamicObject from JSON (must contain apiVersion, kind, metadata).
pub fn obj(v: serde_json::Value) -> DynamicObject {
    serde_json::from_value(v).expect("valid DynamicObject json")
}

impl FakeAccessor {
    pub fn new() -> Self {
        let f = FakeAccessor::default();
        *f.active.lock().unwrap() = "fake".into();
        f.contexts.lock().unwrap().push(ContextInfo {
            name: "fake".into(),
            cluster: "fake-cluster".into(),
            user: "fake-user".into(),
            namespace: "default".into(),
            active: true,
        });
        *f.resources.lock().unwrap() = default_resources();
        f
    }
    pub fn with_metrics(mut self) -> Self {
        self.metrics = true;
        self
    }
    /// Add an object under `gvr`.
    pub fn add(&self, gvr: &str, o: DynamicObject) {
        self.objects
            .lock()
            .unwrap()
            .entry(Gvr::parse(gvr))
            .or_default()
            .push(o);
    }
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
    fn record(&self, s: String) {
        self.calls.lock().unwrap().push(s);
    }
    fn find(&self, gvr: &Gvr, path: &str) -> Option<DynamicObject> {
        let (ns, name) = namespaced(path);
        self.objects.lock().unwrap().get(gvr).and_then(|v| {
            v.iter()
                .find(|o| {
                    o.metadata.name.as_deref() == Some(name)
                        && o.metadata.namespace.as_deref().unwrap_or("") == ns
                })
                .cloned()
        })
    }
}

fn meta(gvr: &str, kind: &str, short: &[&str], namespaced: bool) -> ResourceMeta {
    let g = Gvr::parse(gvr);
    ResourceMeta {
        singular_name: kind.to_lowercase(),
        kind: kind.into(),
        short_names: short.iter().map(|s| s.to_string()).collect(),
        namespaced,
        verbs: [
            "get", "list", "watch", "create", "update", "patch", "delete",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect(),
        categories: vec![],
        gvr: g,
    }
}

/// Discovery list for a typical cluster.
pub fn default_resources() -> Vec<ResourceMeta> {
    vec![
        meta("v1/pods", "Pod", &["po"], true),
        meta("v1/namespaces", "Namespace", &["ns"], false),
        meta("v1/nodes", "Node", &["no"], false),
        meta("v1/services", "Service", &["svc"], true),
        meta("v1/configmaps", "ConfigMap", &["cm"], true),
        meta("v1/secrets", "Secret", &[], true),
        meta("v1/events", "Event", &["ev"], true),
        meta("v1/serviceaccounts", "ServiceAccount", &["sa"], true),
        meta("v1/persistentvolumes", "PersistentVolume", &["pv"], false),
        meta(
            "v1/persistentvolumeclaims",
            "PersistentVolumeClaim",
            &["pvc"],
            true,
        ),
        meta("apps/v1/deployments", "Deployment", &["deploy"], true),
        meta("apps/v1/statefulsets", "StatefulSet", &["sts"], true),
        meta("apps/v1/daemonsets", "DaemonSet", &["ds"], true),
        meta("apps/v1/replicasets", "ReplicaSet", &["rs"], true),
        meta("batch/v1/jobs", "Job", &[], true),
        meta("batch/v1/cronjobs", "CronJob", &["cj"], true),
        meta("networking.k8s.io/v1/ingresses", "Ingress", &["ing"], true),
        meta(
            "rbac.authorization.k8s.io/v1/clusterroles",
            "ClusterRole",
            &[],
            false,
        ),
        meta(
            "apiextensions.k8s.io/v1/customresourcedefinitions",
            "CustomResourceDefinition",
            &["crd", "crds"],
            false,
        ),
    ]
}

#[async_trait]
impl Accessor for FakeAccessor {
    async fn can(&self, gvr: &Gvr, ns: &str, verb: &str) -> Result<bool> {
        let key = (gvr.to_string(), ns.to_string());
        Ok(verb != "list" || !self.denied.lock().unwrap().contains(&key))
    }
    async fn resources(&self) -> Result<Vec<ResourceMeta>> {
        Ok(self.resources.lock().unwrap().clone())
    }
    async fn namespaces(&self) -> Result<Vec<String>> {
        let mut out: Vec<String> = self
            .objects
            .lock()
            .unwrap()
            .get(&Gvr::parse("v1/namespaces"))
            .map(|v| v.iter().filter_map(|o| o.metadata.name.clone()).collect())
            .unwrap_or_default();
        if out.is_empty() {
            out.push("default".into());
        }
        out.sort();
        Ok(out)
    }
    async fn cluster_info(&self) -> Result<ClusterInfo> {
        Ok(ClusterInfo {
            context: self.active_context(),
            cluster: "fake-cluster".into(),
            user: "fake-user".into(),
            k9s_version: crate::version::VERSION.into(),
            k9s_latest: String::new(),
            k8s_version: "v1.34.0".into(),
            cpu_pct: self.metrics.then_some(10),
            mem_pct: self.metrics.then_some(20),
        })
    }
    async fn has_metrics(&self) -> bool {
        self.metrics
    }
    async fn contexts(&self) -> Result<Vec<ContextInfo>> {
        let active = self.active_context();
        Ok(self
            .contexts
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .map(|mut c| {
                c.active = c.name == active;
                c
            })
            .collect())
    }
    fn active_context(&self) -> String {
        self.active.lock().unwrap().clone()
    }
    async fn switch_context(&self, name: &str) -> Result<String> {
        let ns = self
            .contexts
            .lock()
            .unwrap()
            .iter()
            .find(|c| c.name == name)
            .map(|c| c.namespace.clone())
            .ok_or_else(|| anyhow!("no context named {name}"))?;
        *self.active.lock().unwrap() = name.into();
        self.record(format!("switch_context {name}"));
        Ok(ns)
    }
    fn default_namespace(&self) -> String {
        "default".into()
    }
    async fn list(
        &self,
        gvr: &Gvr,
        ns: &str,
        label_selector: Option<&str>,
    ) -> Result<Vec<DynamicObject>> {
        let all = self
            .objects
            .lock()
            .unwrap()
            .get(gvr)
            .cloned()
            .unwrap_or_default();
        let sel: Vec<(String, String)> = label_selector
            .unwrap_or("")
            .split(',')
            .filter_map(|kv| {
                kv.split_once('=')
                    .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
            })
            .collect();
        Ok(all
            .into_iter()
            .filter(|o| {
                is_all_namespaces(ns)
                    || o.metadata.namespace.as_deref().unwrap_or("") == ns
                    || o.metadata.namespace.is_none()
            })
            .filter(|o| {
                sel.iter().all(|(k, v)| {
                    o.metadata
                        .labels
                        .as_ref()
                        .and_then(|l| l.get(k))
                        .map(|x| x == v)
                        .unwrap_or(false)
                })
            })
            .collect())
    }
    async fn list_table(
        &self,
        gvr: &Gvr,
        ns: &str,
        label_selector: Option<&str>,
    ) -> Result<Option<ServerTable>> {
        let objs = self.list(gvr, ns, label_selector).await?;
        let col = |n: &str, t: &str, f: &str| ServerTableColumn {
            name: n.into(),
            type_: t.into(),
            format: f.into(),
            description: String::new(),
            priority: 0,
        };
        Ok(Some(ServerTable {
            columns: vec![col("Name", "string", "name"), col("Age", "date", "")],
            rows: objs
                .into_iter()
                .map(|o| {
                    let ts = o
                        .metadata
                        .creation_timestamp
                        .as_ref()
                        .map(|t| t.0.to_string());
                    ServerTableRow {
                        cells: vec![
                            serde_json::json!(o.metadata.name.clone().unwrap_or_default()),
                            serde_json::json!(ts.clone().unwrap_or_default()),
                        ],
                        name: o.metadata.name.clone().unwrap_or_default(),
                        namespace: o.metadata.namespace.clone().unwrap_or_default(),
                        creation_timestamp: ts,
                        labels: o.metadata.labels.clone().unwrap_or_default(),
                    }
                })
                .collect(),
        }))
    }
    async fn get(&self, gvr: &Gvr, path: &str) -> Result<DynamicObject> {
        self.find(gvr, path)
            .ok_or_else(|| anyhow!("{gvr} {path} not found"))
    }
    async fn delete(&self, gvr: &Gvr, path: &str, opts: DeleteOptions) -> Result<()> {
        let (ns, name) = namespaced(path);
        let mut objs = self.objects.lock().unwrap();
        let v = objs
            .get_mut(gvr)
            .ok_or_else(|| anyhow!("{gvr} {path} not found"))?;
        let before = v.len();
        v.retain(|o| {
            !(o.metadata.name.as_deref() == Some(name)
                && o.metadata.namespace.as_deref().unwrap_or("") == ns)
        });
        if v.len() == before {
            return Err(anyhow!("{gvr} {path} not found"));
        }
        drop(objs);
        self.record(format!("delete {gvr} {path} force={}", opts.force));
        Ok(())
    }
    async fn describe(&self, gvr: &Gvr, path: &str) -> Result<String> {
        let o = self.get(gvr, path).await?;
        Ok(format!(
            "Name:         {}\nNamespace:    {}\n",
            o.metadata.name.unwrap_or_default(),
            o.metadata.namespace.unwrap_or_default()
        ))
    }
    async fn to_yaml(&self, gvr: &Gvr, path: &str, _show_managed: bool) -> Result<String> {
        let v = serde_json::to_value(self.get(gvr, path).await?)?;
        Ok(super::goyaml::to_yaml(&v))
    }
    async fn apply_yaml(&self, gvr: &Gvr, path: &str, _yaml: &str) -> Result<()> {
        self.record(format!("apply {gvr} {path}"));
        Ok(())
    }
    async fn scale(&self, gvr: &Gvr, path: &str, replicas: i32) -> Result<()> {
        self.record(format!("scale {gvr} {path} {replicas}"));
        Ok(())
    }
    async fn replicas(&self, gvr: &Gvr, path: &str) -> Result<i32> {
        let o = self.get(gvr, path).await?;
        Ok(o.data
            .pointer("/spec/replicas")
            .and_then(|v| v.as_i64())
            .unwrap_or(1) as i32)
    }
    async fn restart(&self, gvr: &Gvr, path: &str) -> Result<()> {
        self.record(format!("restart {gvr} {path}"));
        Ok(())
    }
    async fn set_images(&self, gvr: &Gvr, path: &str, images: Vec<ImageSpec>) -> Result<()> {
        let s: Vec<String> = images
            .iter()
            .map(|i| format!("{}={}", i.name, i.docker_image))
            .collect();
        self.record(format!("set_images {gvr} {path} {}", s.join(",")));
        Ok(())
    }
    async fn trigger_cronjob(&self, path: &str) -> Result<()> {
        self.record(format!("trigger {path}"));
        Ok(())
    }
    async fn toggle_suspend_cronjob(&self, path: &str) -> Result<()> {
        self.record(format!("suspend {path}"));
        Ok(())
    }
    async fn toggle_cordon(&self, node: &str, cordon: bool) -> Result<()> {
        self.record(format!("cordon {node} {cordon}"));
        Ok(())
    }
    async fn drain(&self, node: &str, _opts: DrainOptions) -> Result<String> {
        self.record(format!("drain {node}"));
        Ok(format!("node/{node} drained\n"))
    }
    async fn pod_metrics(&self, ns: &str) -> Result<HashMap<String, PodMetrics>> {
        Ok(self
            .pod_metrics
            .lock()
            .unwrap()
            .iter()
            .filter(|(k, _)| is_all_namespaces(ns) || namespaced(k).0 == ns)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect())
    }
    async fn node_metrics(&self) -> Result<HashMap<String, NodeMetrics>> {
        Ok(self.node_metrics.lock().unwrap().clone())
    }
    async fn tail_logs(&self, opts: LogOptions) -> Result<LogStream> {
        let key = fqn(&opts.namespace, &opts.path);
        let lines = self
            .logs
            .lock()
            .unwrap()
            .get(&key)
            .cloned()
            .unwrap_or_default();
        let lines: Vec<LogLine> = lines
            .into_iter()
            .filter(|l| {
                opts.container
                    .as_deref()
                    .map(|c| c == l.container)
                    .unwrap_or(true)
            })
            .collect();
        Ok(futures::stream::iter(lines.into_iter().map(Ok)).boxed())
    }
    async fn port_forward(&self, spec: PortForwardSpec) -> Result<PortForwardHandle> {
        self.record(format!("port_forward {}", spec.id()));
        Ok(PortForwardHandle {
            info: PortForwardInfo {
                spec,
                started_at: chrono::Utc::now(),
                active: true,
            },
            cancel: tokio_util::sync::CancellationToken::new(),
        })
    }
    fn exec_argv(&self, ns: &str, pod: &str, container: &str, shell: &str) -> Vec<String> {
        [
            "kubectl", "exec", "-it", "-n", ns, pod, "-c", container, "--", shell,
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }
    fn edit_argv(&self, gvr: &Gvr, path: &str) -> Vec<String> {
        let (ns, name) = namespaced(path);
        let mut v: Vec<String> = vec![
            "kubectl".into(),
            "edit".into(),
            format!("{}/{}", gvr.r(), name),
        ];
        if !ns.is_empty() {
            v.extend(["-n".into(), ns.into()]);
        }
        v
    }
    fn shell_env(&self) -> Vec<(String, String)> {
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn list_filters_by_namespace_and_labels() {
        let f = FakeAccessor::new();
        f.add("v1/pods", obj(json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"a","namespace":"default","labels":{"app":"x"}}})));
        f.add("v1/pods", obj(json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"b","namespace":"kube-system"}})));
        let g = Gvr::parse("v1/pods");
        assert_eq!(f.list(&g, "default", None).await.unwrap().len(), 1);
        assert_eq!(f.list(&g, "all", None).await.unwrap().len(), 2);
        assert_eq!(f.list(&g, "", Some("app=x")).await.unwrap().len(), 1);
        f.delete(&g, "default/a", DeleteOptions::default())
            .await
            .unwrap();
        assert_eq!(f.list(&g, "all", None).await.unwrap().len(), 1);
        assert_eq!(f.calls(), vec!["delete v1/pods default/a force=false"]);
    }
}
