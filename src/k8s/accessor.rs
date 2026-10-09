//! The `Accessor` trait: everything the UI needs from a cluster. Implemented by
//! `client::KubeAccessor` (real) and `fake::FakeAccessor` (tests).

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use futures::stream::BoxStream;
use kube::api::DynamicObject;

use super::types::*;
use super::Gvr;

pub type SharedAccessor = Arc<dyn Accessor>;
pub type LogStream = BoxStream<'static, Result<LogLine>>;

#[async_trait]
pub trait Accessor: Send + Sync {
    // ---- discovery -------------------------------------------------------
    /// All API resources known to the server (cached, refreshed lazily).
    async fn resources(&self) -> Result<Vec<ResourceMeta>>;
    /// Resolve a GVR to its metadata.
    async fn resource_meta(&self, gvr: &Gvr) -> Result<Option<ResourceMeta>> {
        Ok(self.resources().await?.into_iter().find(|m| &m.gvr == gvr))
    }
    /// Whether the current user may perform `verb` on `gvr` in `ns`
    /// (SelfSubjectAccessReview). Defaults to permissive.
    async fn can(&self, _gvr: &Gvr, _ns: &str, _verb: &str) -> Result<bool> {
        Ok(true)
    }
    /// Namespaces visible to the current user.
    async fn namespaces(&self) -> Result<Vec<String>>;
    /// Header cluster information.
    async fn cluster_info(&self) -> Result<ClusterInfo>;
    /// Whether metrics-server is available.
    async fn has_metrics(&self) -> bool;
    /// Reach the API server (k9s CheckConnectivity). Defaults to OK.
    async fn check_connectivity(&self) -> Result<()> {
        Ok(())
    }

    // ---- contexts --------------------------------------------------------
    async fn contexts(&self) -> Result<Vec<ContextInfo>>;
    fn active_context(&self) -> String;
    /// Switch the active context (reconnects). Returns the new default namespace.
    async fn switch_context(&self, name: &str) -> Result<String>;
    /// Default namespace of the active context.
    fn default_namespace(&self) -> String;

    // ---- CRUD ------------------------------------------------------------
    /// List objects of `gvr` in `ns` (`NAMESPACE_ALL`/"" for all), optionally
    /// filtered by a label selector.
    async fn list(
        &self,
        gvr: &Gvr,
        ns: &str,
        label_selector: Option<&str>,
    ) -> Result<Vec<DynamicObject>>;
    /// List as a server-side table (k9s dao.Table). `Ok(None)` when the
    /// server does not support tables for this resource.
    async fn list_table(
        &self,
        _gvr: &Gvr,
        _ns: &str,
        _label_selector: Option<&str>,
    ) -> Result<Option<ServerTable>> {
        Ok(None)
    }
    /// Get a single object by FQN (`ns/name` or `name`).
    async fn get(&self, gvr: &Gvr, path: &str) -> Result<DynamicObject>;
    async fn delete(&self, gvr: &Gvr, path: &str, opts: DeleteOptions) -> Result<()>;
    /// `kubectl describe`-style text.
    async fn describe(&self, gvr: &Gvr, path: &str) -> Result<String>;
    /// Object as YAML, optionally keeping `metadata.managedFields`.
    async fn to_yaml(&self, gvr: &Gvr, path: &str, show_managed: bool) -> Result<String>;
    /// Replace an object (used after `e`dit). `yaml` is the full manifest.
    async fn apply_yaml(&self, gvr: &Gvr, path: &str, yaml: &str) -> Result<()>;
    /// Scale a scalable resource.
    async fn scale(&self, gvr: &Gvr, path: &str, replicas: i32) -> Result<()>;
    /// Current replica count of a scalable resource.
    async fn replicas(&self, gvr: &Gvr, path: &str) -> Result<i32>;
    /// Rollout restart (patches the pod template restartedAt annotation).
    async fn restart(&self, gvr: &Gvr, path: &str) -> Result<()>;
    /// Set container images on a workload.
    async fn set_images(&self, gvr: &Gvr, path: &str, images: Vec<ImageSpec>) -> Result<()>;
    /// Trigger a job from a cronjob.
    async fn trigger_cronjob(&self, path: &str) -> Result<()>;
    /// Suspend/resume a cronjob.
    async fn toggle_suspend_cronjob(&self, path: &str) -> Result<()>;

    // ---- nodes -----------------------------------------------------------
    async fn toggle_cordon(&self, node: &str, cordon: bool) -> Result<()>;
    /// Drain a node, returning the progress log.
    async fn drain(&self, node: &str, opts: DrainOptions) -> Result<String>;

    // ---- metrics ---------------------------------------------------------
    /// Pod metrics for `ns` ("" for all) keyed by FQN.
    async fn pod_metrics(&self, ns: &str) -> Result<std::collections::HashMap<String, PodMetrics>>;
    /// Node metrics keyed by node name.
    async fn node_metrics(&self) -> Result<std::collections::HashMap<String, NodeMetrics>>;

    // ---- logs / exec / port-forward -------------------------------------
    /// Stream pod logs per `opts`.
    async fn tail_logs(&self, opts: LogOptions) -> Result<LogStream>;
    /// Start a port forward; it runs until the returned handle is dropped.
    async fn port_forward(&self, spec: PortForwardSpec) -> Result<PortForwardHandle>;
    /// Build the argv used to shell out for an interactive exec (k9s shells to
    /// `kubectl exec`).
    fn exec_argv(&self, ns: &str, pod: &str, container: &str, shell: &str) -> Vec<String>;
    /// Build the argv used to shell out for `kubectl edit`.
    fn edit_argv(&self, gvr: &Gvr, path: &str) -> Vec<String>;
    /// Environment overrides (KUBECONFIG, etc.) for shell outs.
    fn shell_env(&self) -> Vec<(String, String)>;
}

/// Running port-forward; dropping it stops the forward.
pub struct PortForwardHandle {
    pub info: PortForwardInfo,
    pub cancel: tokio_util::sync::CancellationToken,
}

impl Drop for PortForwardHandle {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
