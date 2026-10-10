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
    /// `can` for a named instance (client.CanI with a name).
    async fn can_named(&self, gvr: &Gvr, ns: &str, _name: &str, verb: &str) -> Result<bool> {
        self.can(gvr, ns, verb).await
    }
    /// Namespaces visible to the current user.
    async fn namespaces(&self) -> Result<Vec<String>>;
    /// Header cluster information.
    async fn cluster_info(&self) -> Result<ClusterInfo>;
    /// Whether metrics-server is available.
    async fn has_metrics(&self) -> bool;
    /// client.InitConnection: the metrics-server probe's connection error
    /// (logged as "Fail to locate metrics-server"), if any.
    async fn init_error(&self) -> Option<String> {
        None
    }
    /// Reach the API server (k9s CheckConnectivity). Defaults to OK.
    async fn check_connectivity(&self) -> Result<()> {
        Ok(())
    }

    // ---- contexts --------------------------------------------------------
    async fn contexts(&self) -> Result<Vec<ContextInfo>>;
    fn active_context(&self) -> String;
    /// Switch the active context (reconnects). Returns the new default namespace.
    async fn switch_context(&self, name: &str) -> Result<String>;
    /// client.Config.RenameContext: rewrite the context's kubeconfig file;
    /// renaming the active context switches to the new name.
    async fn rename_context(&self, old: &str, new: &str) -> Result<()>;
    /// client.Config.DelContext.
    async fn delete_context(&self, name: &str) -> Result<()>;
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
    /// `list`, sharing objects with the watch cache where there is one, so
    /// refreshes of big lists do not deep-copy them.
    async fn list_shared(
        &self,
        gvr: &Gvr,
        ns: &str,
        label_selector: Option<&str>,
    ) -> Result<Vec<std::sync::Arc<DynamicObject>>> {
        Ok(self
            .list(gvr, ns, label_selector)
            .await?
            .into_iter()
            .map(std::sync::Arc::new)
            .collect())
    }
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
    /// `list_table`; `include_object` asks for full row objects
    /// (includeObject=Object, k9s does so when views.yaml has columns);
    /// `field_selector` goes to the server (dao/table.go FieldSelector).
    async fn list_table_with(
        &self,
        gvr: &Gvr,
        ns: &str,
        label_selector: Option<&str>,
        _include_object: bool,
        _field_selector: Option<&str>,
    ) -> Result<Option<ServerTable>> {
        self.list_table(gvr, ns, label_selector).await
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
    /// RFC 6902 JSON patch (types.JSONPatchType).
    async fn patch_json(&self, gvr: &Gvr, path: &str, patch: &serde_json::Value) -> Result<()>;
    /// Create an object in `path`'s namespace (`Create`, e.g. the node shell pod).
    async fn create(&self, gvr: &Gvr, path: &str, body: &serde_json::Value) -> Result<()>;
    /// Scale a scalable resource.
    async fn scale(&self, gvr: &Gvr, path: &str, replicas: i32) -> Result<()>;
    /// Current replica count of a scalable resource.
    async fn replicas(&self, gvr: &Gvr, path: &str) -> Result<i32>;
    /// Rollout restart (patches the pod template restartedAt annotation).
    async fn restart(&self, gvr: &Gvr, path: &str, field_manager: &str) -> Result<()>;
    /// Set container images on a workload.
    async fn set_images(&self, gvr: &Gvr, path: &str, images: Vec<ImageSpec>) -> Result<()>;
    /// Trigger a job from a cronjob.
    async fn trigger_cronjob(&self, path: &str) -> Result<()>;
    /// Suspend/resume a cronjob.
    async fn toggle_suspend_cronjob(&self, path: &str) -> Result<()>;

    // ---- nodes -----------------------------------------------------------
    async fn toggle_cordon(&self, node: &str, cordon: bool) -> Result<()>;
    /// Drain a node, returning the progress log.
    /// Drain a node; `progress` receives the Helper Out/ErrOut writes as
    /// they happen. Returns the whole log.
    async fn drain(
        &self,
        node: &str,
        opts: DrainOptions,
        progress: DrainProgress,
    ) -> Result<String>;

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
    /// Route API calls through `proxy` (context config `proxy.address`).
    async fn set_proxy(&self, _proxy: Option<String>) {}
    /// `--as-group` impersonation groups (client.Config.CurrentGroupNames).
    fn impersonate_groups(&self) -> Vec<String> {
        Vec::new()
    }
}

/// Running port-forward; dropping it stops the forward.
/// Live drain output sink (k9s streams it into "Drain Progress").
pub type DrainProgress = Arc<dyn Fn(&str) + Send + Sync>;

pub struct PortForwardHandle {
    pub info: PortForwardInfo,
    pub cancel: tokio_util::sync::CancellationToken,
    /// Fires with the error when the forward dies after starting
    /// (portforward.ForwardPorts returning an error).
    pub died: Option<tokio::sync::oneshot::Receiver<String>>,
}

impl Drop for PortForwardHandle {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
