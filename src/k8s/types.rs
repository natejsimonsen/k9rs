//! Plain data types shared between the cluster layer, renderers and views.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::Gvr;

/// Discovery metadata for one API resource (k9s dao.ResourceMeta / metav1.APIResource).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceMeta {
    pub gvr: Gvr,
    pub kind: String,
    pub singular_name: String,
    pub short_names: Vec<String>,
    pub namespaced: bool,
    pub verbs: Vec<String>,
    pub categories: Vec<String>,
}

impl ResourceMeta {
    pub fn can(&self, verb: &str) -> bool {
        self.verbs.iter().any(|v| v == verb)
    }
}

/// A kubeconfig context (k9s render.Context input).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextInfo {
    pub name: String,
    pub cluster: String,
    pub user: String,
    pub namespace: String,
    pub active: bool,
}

/// Header cluster info (k9s view.ClusterInfo).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ClusterInfo {
    pub context: String,
    pub cluster: String,
    pub user: String,
    pub k9s_version: String,
    /// Newer k9s release, empty when current or unknown.
    #[serde(default)]
    pub k9s_latest: String,
    pub k8s_version: String,
    /// Cluster CPU usage percent (n/a when metrics-server is missing).
    pub cpu_pct: Option<i64>,
    /// Cluster memory usage percent.
    pub mem_pct: Option<i64>,
}

/// CPU in millicores + memory in bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Metrics {
    pub cpu_millis: i64,
    pub mem_bytes: i64,
}

/// metrics.k8s.io PodMetrics flattened by container.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PodMetrics {
    /// Keyed by container name.
    pub containers: HashMap<String, Metrics>,
}

impl PodMetrics {
    pub fn total(&self) -> Metrics {
        self.containers
            .values()
            .fold(Metrics::default(), |acc, m| Metrics {
                cpu_millis: acc.cpu_millis + m.cpu_millis,
                mem_bytes: acc.mem_bytes + m.mem_bytes,
            })
    }
}

/// Node metrics with the node's allocatable capacity for percentages.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeMetrics {
    pub usage: Metrics,
    pub allocatable: Metrics,
}

/// Snapshot of metrics available to renderers. Keys are FQNs (`ns/pod`, `node`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MetricsSnapshot {
    pub pods: HashMap<String, PodMetrics>,
    pub nodes: HashMap<String, NodeMetrics>,
}

/// Deletion propagation (metav1.DeletionPropagation).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Propagation {
    #[default]
    Background,
    Foreground,
    Orphan,
}

/// Delete options from the k9s delete dialog.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeleteOptions {
    pub propagation: Option<Propagation>,
    /// Grace period seconds; `Some(0)` with `force` is a force delete.
    pub grace: Option<i64>,
    pub force: bool,
}

/// Node drain options (k9s dao.DrainOptions).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DrainOptions {
    pub grace_period_seconds: i64,
    pub timeout_secs: i64,
    pub ignore_all_daemon_sets: bool,
    pub delete_empty_dir_data: bool,
    pub force: bool,
    pub disable_eviction: bool,
}

impl Default for DrainOptions {
    fn default() -> Self {
        Self {
            grace_period_seconds: -1,
            timeout_secs: 5 * 60,
            ignore_all_daemon_sets: true,
            delete_empty_dir_data: true,
            force: false,
            disable_eviction: false,
        }
    }
}

/// Log tailing options (k9s dao.LogOptions).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogOptions {
    pub namespace: String,
    /// Pod name (or a label selector when `is_selector`).
    pub path: String,
    /// Container name; `None` tails all containers of the pod.
    pub container: Option<String>,
    /// The pod's `kubectl.kubernetes.io/default-container`, if any.
    pub default_container: Option<String>,
    pub all_containers: bool,
    /// Max lines to fetch initially (config logger.tail).
    pub lines: i64,
    /// `None` = full history (head), `Some(n)` = last n seconds.
    pub since_seconds: Option<i64>,
    pub previous: bool,
    pub show_timestamp: bool,
    pub follow: bool,
}

impl Default for LogOptions {
    fn default() -> Self {
        Self {
            namespace: String::new(),
            path: String::new(),
            container: None,
            default_container: None,
            all_containers: false,
            lines: 100,
            since_seconds: None,
            previous: false,
            show_timestamp: false,
            follow: true,
        }
    }
}

/// One streamed log line (k9s dao.LogItem).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LogLine {
    pub pod: String,
    pub container: String,
    /// RFC3339 timestamp if `show_timestamp` was requested.
    pub timestamp: Option<String>,
    pub text: String,
}

/// Port-forward request (k9s port.PortTunnel).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PortForwardSpec {
    pub namespace: String,
    pub pod: String,
    pub container: String,
    pub address: String,
    pub local_port: u16,
    pub container_port: u16,
}

impl PortForwardSpec {
    /// k9s PF id: `ns/pod|container:local:container`.
    pub fn id(&self) -> String {
        format!(
            "{}/{}|{}:{}:{}",
            self.namespace, self.pod, self.container, self.local_port, self.container_port
        )
    }
}

/// An active port forward.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortForwardInfo {
    pub spec: PortForwardSpec,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub active: bool,
}

/// Image spec for `set image` (k9s dao.ImageSpec).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImageSpec {
    pub name: String,
    pub docker_image: String,
    pub init: bool,
}

/// Column of a server-side table (metav1.TableColumnDefinition).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerTableColumn {
    pub name: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub format: String,
    pub description: String,
    /// 0 = shown by default, >0 = wide only.
    pub priority: i32,
}

/// One row of a server-side table.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ServerTableRow {
    pub cells: Vec<serde_json::Value>,
    pub name: String,
    pub namespace: String,
    /// metadata.creationTimestamp (RFC3339), if present.
    pub creation_timestamp: Option<String>,
    pub labels: std::collections::BTreeMap<String, String>,
}

/// Server-side table (`Accept: application/json;as=Table;v=v1;g=meta.k8s.io`).
/// k9s uses this for every resource without a dedicated renderer (CRDs etc).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ServerTable {
    pub columns: Vec<ServerTableColumn>,
    pub rows: Vec<ServerTableRow>,
}
