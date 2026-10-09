//! kube-rs backed `Accessor` (k9s internal/client + internal/dao).
//!
//! Submodules hold the pure pieces (config overrides, discovery selection,
//! patches, drain filters, quantity parsing, argv builders); this file wires
//! them to the API server.

mod config;
mod discovery;
mod drain;
mod informer;
mod logs;
mod metrics;
mod patches;
mod portforward;
mod quantity;
mod related;
mod shell;
mod table;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context as _, Result};
use async_trait::async_trait;
use futures::StreamExt;
use k8s_openapi::api::core::v1::Pod;
use kube::api::{DeleteParams, DynamicObject, ListParams, Patch, PatchParams, PostParams};
use kube::core::Request;
use serde_json::{json, Value};

use self::config::LoadedConfig;
use self::discovery::DiscoveryData;
use super::accessor::{Accessor, LogStream, PortForwardHandle};
use super::gvr::known;
use super::types::*;
use super::{is_all_namespaces, is_cluster_scoped, namespaced, Gvr, SharedAccessor};

/// Connection flags from the CLI (k9s client.Config / genericclioptions).
#[derive(Clone, Debug, Default)]
pub struct ConnectOptions {
    pub kubeconfig: Option<String>,
    pub context: Option<String>,
    pub cluster: Option<String>,
    pub user: Option<String>,
    pub namespace: Option<String>,
    pub request_timeout: Option<std::time::Duration>,
    pub impersonate: Option<String>,
    pub impersonate_groups: Vec<String>,
    pub insecure_skip_tls_verify: bool,
    pub certificate_authority: Option<String>,
    pub client_key: Option<String>,
    pub client_certificate: Option<String>,
    pub token: Option<String>,
}

/// Connect to the cluster described by `opts`. No API request is sent, but
/// kube-rs resolves credentials (e.g. exec plugins) while building the
/// client, so missing credentials fail here; connectivity problems surface
/// on the first API call.
pub async fn connect(opts: ConnectOptions) -> Result<SharedAccessor> {
    let cfg = LoadedConfig::load(opts)?;
    let mut acc = KubeAccessor::from_conn(Conn::open(cfg).await);
    acc.informers = Some(informer::Informers::default());
    Ok(Arc::new(acc))
}

/// k9s client cacheExpiry.
const CACHE_EXPIRY: Duration = Duration::from_secs(5 * 60);
/// k9s mxCacheExpiry / clusterCacheExpiry.
const MX_CACHE_EXPIRY: Duration = Duration::from_secs(60);
/// client.NA.
const NA: &str = "n/a";
/// k9s logChannelBuffer.
const LOG_CHANNEL_BUFFER: usize = 50;
const METRICS_GROUP: &str = "metrics.k8s.io";
const METRICS_VERSION: &str = "v1beta1";

/// Simple TTL cache keyed by string.
struct Cache<V> {
    m: HashMap<String, (Instant, V)>,
}

impl<V: Clone> Cache<V> {
    fn new() -> Self {
        Cache { m: HashMap::new() }
    }
    fn get(&self, k: &str) -> Option<V> {
        self.m
            .get(k)
            .filter(|(exp, _)| Instant::now() < *exp)
            .map(|(_, v)| v.clone())
    }
    fn put(&mut self, k: &str, v: V, ttl: Duration) {
        self.m.insert(k.to_string(), (Instant::now() + ttl, v));
    }
}

struct Caches {
    discovery: Cache<Arc<DiscoveryData>>,
    sar: Cache<bool>,
    flags: Cache<bool>,
    version: Cache<String>,
    namespaces: Cache<Vec<String>>,
    pod_mx: Cache<HashMap<String, PodMetrics>>,
    node_mx: Cache<HashMap<String, Metrics>>,
    nodes: Cache<HashMap<String, Metrics>>,
}

impl Caches {
    fn new() -> Self {
        Caches {
            discovery: Cache::new(),
            sar: Cache::new(),
            flags: Cache::new(),
            version: Cache::new(),
            namespaces: Cache::new(),
            pod_mx: Cache::new(),
            node_mx: Cache::new(),
            nodes: Cache::new(),
        }
    }
}

/// An active connection: API client, a timeout-free client for streams and
/// the configuration it was built from.
struct Conn {
    /// The API and stream clients, or why they could not be built (e.g. an
    /// expired auth plugin token).
    clients: std::result::Result<(kube::Client, kube::Client), String>,
    cfg: LoadedConfig,
    /// Private kubeconfig for kubectl shell-outs; removed when dropped.
    override_file: Option<shell::OverrideFile>,
}

impl Conn {
    /// Build a connection, failing when the clients cannot be built.
    async fn build(cfg: LoadedConfig) -> Result<Conn> {
        let c = Self::open(cfg).await;
        if let Err(e) = &c.clients {
            return Err(anyhow!("{e}"));
        }
        Ok(c)
    }

    /// Build a connection, keeping client errors so k9s can start
    /// disconnected on the context view.
    async fn open(cfg: LoadedConfig) -> Conn {
        let clients = Self::clients(&cfg).await.map_err(|e| format!("{e:#}"));
        let override_file =
            shell::override_kubeconfig(&cfg).and_then(|v| match shell::OverrideFile::write(&v) {
                Ok(f) => Some(f),
                Err(e) => {
                    tracing::error!("unable to write kubectl override kubeconfig: {e}");
                    None
                }
            });
        Conn {
            clients,
            cfg,
            override_file,
        }
    }

    async fn clients(cfg: &LoadedConfig) -> Result<(kube::Client, kube::Client)> {
        let kcfg = cfg.kube_config().await?;
        let mut scfg = kcfg.clone();
        scfg.read_timeout = None;
        let client = kube::Client::try_from(kcfg).map_err(kerr)?;
        let stream = kube::Client::try_from(scfg).map_err(kerr)?;
        Ok((client, stream))
    }
}

/// The real cluster accessor.
pub struct KubeAccessor {
    conn: RwLock<Arc<Conn>>,
    caches: Mutex<Caches>,
    /// Watch-backed list cache; off unless enabled (unit tests list directly).
    informers: Option<informer::Informers>,
}

/// Error text for a kube error: the API status message like client-go.
fn kerr_msg(e: &kube::Error) -> String {
    match e {
        kube::Error::Api(s) if !s.message.is_empty() => s.message.clone(),
        other => other.to_string(),
    }
}

fn kerr(e: kube::Error) -> anyhow::Error {
    anyhow!(kerr_msg(&e))
}

fn api_status(e: &kube::Error) -> Option<&kube::core::Status> {
    match e {
        kube::Error::Api(s) => Some(s),
        _ => None,
    }
}

fn reqerr(e: kube::core::request::Error) -> anyhow::Error {
    anyhow!("{e}")
}

/// Strip the `-` cluster scope and `all` markers (client.CleanseNamespace).
fn clean_ns(ns: &str) -> &str {
    if is_all_namespaces(ns) || is_cluster_scoped(ns) {
        ""
    } else {
        ns
    }
}

/// Collection URL for a GVR in a namespace ("" = cluster wide).
fn collection_path(gvr: &Gvr, ns: &str) -> String {
    if ns.is_empty() {
        format!("{}/{}", gvr.api_path(), gvr.r())
    } else {
        format!("{}/namespaces/{}/{}", gvr.api_path(), ns, gvr.r())
    }
}

/// Split an FQN into a cleaned namespace and name.
fn split_path(path: &str) -> (String, String) {
    let (ns, n) = namespaced(path);
    (clean_ns(ns).to_string(), n.to_string())
}

fn is_gvr(gvr: &Gvr, other: &Gvr) -> bool {
    gvr.g() == other.g() && gvr.v() == other.v() && gvr.r() == other.r()
}

fn with_header(
    mut req: http::Request<Vec<u8>>,
    k: http::header::HeaderName,
    v: &str,
) -> Result<http::Request<Vec<u8>>> {
    req.headers_mut().insert(
        k,
        http::HeaderValue::from_str(v).map_err(|e| anyhow!("{e}"))?,
    );
    Ok(req)
}

fn append_query(req: &mut http::Request<Vec<u8>>, kv: &str) -> Result<()> {
    let uri = req.uri().to_string();
    let sep = if uri.contains('?') { '&' } else { '?' };
    *req.uri_mut() = format!("{uri}{sep}{kv}")
        .parse()
        .map_err(|e| anyhow!("{e}"))?;
    Ok(())
}

/// Fill apiVersion/kind on list items from the list itself.
fn list_items(list: Value) -> Result<Vec<DynamicObject>> {
    let api_version = list.get("apiVersion").cloned().unwrap_or(Value::Null);
    let kind = list
        .get("kind")
        .and_then(Value::as_str)
        .map(|k| k.strip_suffix("List").unwrap_or(k).to_string());
    let items = match list.get("items") {
        Some(Value::Array(a)) => a.clone(),
        _ => vec![],
    };
    items
        .into_iter()
        .map(|mut it| {
            if let Some(o) = it.as_object_mut() {
                if !o.contains_key("apiVersion") && !api_version.is_null() {
                    o.insert("apiVersion".into(), api_version.clone());
                }
                if let (false, Some(k)) = (o.contains_key("kind"), &kind) {
                    o.insert("kind".into(), json!(k));
                }
            }
            serde_json::from_value(it).map_err(|e| anyhow!("unable to decode object: {e}"))
        })
        .collect()
}

/// client.CanI namespace: the namespace's own name for namespaces, blank
/// for all/cluster scope.
fn sar_namespace(ns: &str, gvr: &Gvr, name: &str) -> String {
    let mut ns = ns.to_string();
    if is_gvr(gvr, &known::namespaces()) {
        ns = name.to_string();
    }
    if ns == super::NAMESPACE_ALL || is_cluster_scoped(&ns) {
        ns.clear();
    }
    ns
}

/// client.makeCacheKey.
fn sar_cache_key(ns: &str, gvr: &Gvr, n: &str, verbs: &[&str]) -> String {
    format!("{ns}:{gvr}:{n}::{}", verbs.join(","))
}

/// client.makeSAR body.
fn sar_body(ns: &str, gvr: &Gvr, name: &str, verb: &str) -> Value {
    let mut attrs = serde_json::Map::new();
    let mut put = |k: &str, v: &str| {
        if !v.is_empty() {
            attrs.insert(k.into(), json!(v));
        }
    };
    put("namespace", ns);
    put("verb", verb);
    put("group", gvr.g());
    put("version", gvr.v());
    put("resource", gvr.r());
    put("subresource", &gvr.sub_resource);
    put("name", name);
    json!({
        "apiVersion": "authorization.k8s.io/v1",
        "kind": "SelfSubjectAccessReview",
        "spec": {"resourceAttributes": Value::Object(attrs)},
    })
}

/// kubectl's event search field selector (sorted like fields.Set.String()).
fn events_selector(obj: &DynamicObject, gvr: &Gvr) -> String {
    let name = obj.metadata.name.clone().unwrap_or_default();
    let ns = obj.metadata.namespace.clone().unwrap_or_default();
    let kind = obj
        .types
        .as_ref()
        .map(|t| t.kind.clone())
        .unwrap_or_default();
    // kubectl's node describer matches node events by name as the uid.
    let uid = if is_gvr(gvr, &known::nodes()) {
        name.clone()
    } else {
        obj.metadata.uid.clone().unwrap_or_default()
    };
    let mut parts = vec![];
    if !kind.is_empty() {
        parts.push(format!("involvedObject.kind={kind}"));
    }
    parts.push(format!("involvedObject.name={name}"));
    parts.push(format!("involvedObject.namespace={ns}"));
    if !uid.is_empty() {
        parts.push(format!("involvedObject.uid={uid}"));
    }
    parts.join(",")
}

impl KubeAccessor {
    #[cfg(test)]
    async fn new(cfg: LoadedConfig) -> Result<KubeAccessor> {
        Ok(Self::from_conn(Conn::build(cfg).await?))
    }

    fn from_conn(conn: Conn) -> KubeAccessor {
        KubeAccessor {
            conn: RwLock::new(Arc::new(conn)),
            caches: Mutex::new(Caches::new()),
            informers: None,
        }
    }

    fn conn(&self) -> Arc<Conn> {
        self.conn.read().unwrap().clone()
    }

    fn client(&self) -> Result<kube::Client> {
        match &self.conn().clients {
            Ok((c, _)) => Ok(c.clone()),
            Err(e) => Err(anyhow!("{e}")),
        }
    }

    fn stream_client(&self) -> Result<kube::Client> {
        match &self.conn().clients {
            Ok((_, s)) => Ok(s.clone()),
            Err(e) => Err(anyhow!("{e}")),
        }
    }

    /// Bound a call by the configured call timeout.
    async fn timed<T>(&self, fut: impl Future<Output = Result<T>>) -> Result<T> {
        let d = self.conn().cfg.call_timeout();
        match tokio::time::timeout(d, fut).await {
            Ok(r) => r,
            Err(_) => Err(anyhow!("context deadline exceeded")),
        }
    }

    async fn send_json(&self, req: http::Request<Vec<u8>>) -> Result<Value> {
        let c = self.client()?;
        self.timed(async move { c.request::<Value>(req).await.map_err(kerr) })
            .await
    }

    async fn discovery(&self) -> Result<Arc<DiscoveryData>> {
        if let Some(d) = self.caches.lock().unwrap().discovery.get("") {
            return Ok(d);
        }
        let c = self.client()?;
        let d = Arc::new(
            self.timed(async move { discovery::discover(&c).await })
                .await?,
        );
        self.caches
            .lock()
            .unwrap()
            .discovery
            .put("", d.clone(), CACHE_EXPIRY);
        Ok(d)
    }

    /// Namespace to use in URLs: cleaned, and dropped for cluster scoped resources.
    async fn url_ns(&self, gvr: &Gvr, ns: &str) -> String {
        let ns = clean_ns(ns);
        if ns.is_empty() {
            return String::new();
        }
        match self.discovery().await {
            Ok(d) => match d.lookup(gvr) {
                Some(m) if !m.namespaced => String::new(),
                _ => ns.to_string(),
            },
            Err(_) => ns.to_string(),
        }
    }

    /// watch.Factory.List: serve lists of watchable resources from an
    /// informer. `None` means list directly.
    async fn cached_list(
        &self,
        gvr: &Gvr,
        ns: &str,
        label_selector: Option<&str>,
    ) -> Option<Vec<DynamicObject>> {
        let informers = self.informers.as_ref()?;
        let selector = match label_selector.filter(|s| !s.is_empty()) {
            Some(s) => crate::model::labels::parse_selector(s).ok()?,
            None => Default::default(),
        };
        let d = self.discovery().await.ok()?;
        let meta = d.lookup(gvr)?;
        if !meta.can("list") || !meta.can("watch") || !gvr.sub_resource.is_empty() {
            return None;
        }
        let client = self.client().ok()?;
        let wait = self.conn().cfg.call_timeout();
        informers
            .list(&client, gvr, &meta.kind, ns, &selector, wait)
            .await
    }

    async fn object_path(&self, gvr: &Gvr, path: &str) -> Result<(Request, String, String)> {
        let (ns, n) = split_path(path);
        if n.is_empty() {
            return Err(anyhow!("missing resource name in path {path:?}"));
        }
        let ns = self.url_ns(gvr, &ns).await;
        Ok((Request::new(collection_path(gvr, &ns)), ns, n))
    }

    /// SAR check; `Ok(Some(msg))` when denied.
    async fn access_review(
        &self,
        ns: &str,
        gvr: &Gvr,
        name: &str,
        verbs: &[&str],
    ) -> Result<Option<String>> {
        let ns = sar_namespace(ns, gvr, name);
        let key = sar_cache_key(&ns, gvr, name, verbs);
        if let Some(ok) = self.caches.lock().unwrap().sar.get(&key) {
            return Ok(if ok {
                None
            } else {
                Some(denied_msg(
                    verbs.first().copied().unwrap_or_default(),
                    name,
                    gvr,
                    &ns,
                ))
            });
        }
        for verb in verbs {
            let body = sar_body(&ns, gvr, name, verb);
            let req = http::Request::post("/apis/authorization.k8s.io/v1/selfsubjectaccessreviews")
                .header(http::header::CONTENT_TYPE, "application/json")
                .body(serde_json::to_vec(&body)?)?;
            let resp = match self.send_json(req).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!("Auth request failed: {e}");
                    self.caches
                        .lock()
                        .unwrap()
                        .sar
                        .put(&key, false, CACHE_EXPIRY);
                    return Err(e);
                }
            };
            let allowed = resp
                .pointer("/status/allowed")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if !allowed {
                self.caches
                    .lock()
                    .unwrap()
                    .sar
                    .put(&key, false, CACHE_EXPIRY);
                return Ok(Some(denied_msg(verb, name, gvr, &ns)));
            }
        }
        self.caches
            .lock()
            .unwrap()
            .sar
            .put(&key, true, CACHE_EXPIRY);
        Ok(None)
    }

    /// client.CanI: errors on denial with k9s' message.
    async fn can_i(&self, ns: &str, gvr: &Gvr, name: &str, verbs: &[&str]) -> Result<()> {
        match self.access_review(ns, gvr, name, verbs).await? {
            None => Ok(()),
            Some(msg) => Err(anyhow!(msg)),
        }
    }

    async fn get_raw(&self, gvr: &Gvr, path: &str) -> Result<Value> {
        let (r, _, n) = self.object_path(gvr, path).await?;
        self.send_json(r.get(&n, &Default::default()).map_err(reqerr)?)
            .await
    }

    async fn server_version(&self) -> Result<String> {
        if let Some(v) = self.caches.lock().unwrap().version.get("") {
            return Ok(v);
        }
        let c = self.client()?;
        let info = self
            .timed(async move { c.apiserver_version().await.map_err(kerr) })
            .await?;
        self.caches
            .lock()
            .unwrap()
            .version
            .put("", info.git_version.clone(), CACHE_EXPIRY);
        Ok(info.git_version)
    }

    async fn metrics_check(&self, ns: &str, gvr: &Gvr) -> Result<()> {
        if !self.has_metrics().await {
            return Err(anyhow!("no metrics-server detected on cluster"));
        }
        self.can_i(ns, gvr, "", &["list"]).await
    }

    async fn node_usage(&self) -> Result<HashMap<String, Metrics>> {
        self.metrics_check(super::CLUSTER_SCOPE, &known::node_metrics())
            .await?;
        if let Some(v) = self.caches.lock().unwrap().node_mx.get("nodes") {
            return Ok(v);
        }
        let req = Request::new(collection_path(&known::node_metrics(), ""))
            .list(&ListParams::default())
            .map_err(reqerr)?;
        let mx = metrics::node_usage(&self.send_json(req).await?);
        self.caches
            .lock()
            .unwrap()
            .node_mx
            .put("nodes", mx.clone(), MX_CACHE_EXPIRY);
        Ok(mx)
    }

    async fn nodes_allocatable(&self) -> Result<HashMap<String, Metrics>> {
        if let Some(v) = self.caches.lock().unwrap().nodes.get("nodes") {
            return Ok(v);
        }
        let req = Request::new(collection_path(&known::nodes(), ""))
            .list(&ListParams::default())
            .map_err(reqerr)?;
        let alloc = metrics::node_allocatable(&self.send_json(req).await?);
        if !alloc.is_empty() {
            self.caches
                .lock()
                .unwrap()
                .nodes
                .put("nodes", alloc.clone(), MX_CACHE_EXPIRY);
        }
        Ok(alloc)
    }

    async fn get_pod(&self, ns: &str, name: &str) -> Result<Pod> {
        let v = self.get_raw(&known::pods(), &super::fqn(ns, name)).await?;
        serde_json::from_value(v).map_err(|e| anyhow!("expecting Pod resource: {e}"))
    }

    async fn patch_strategic(&self, gvr: &Gvr, path: &str, patch: Value) -> Result<()> {
        let (r, _, n) = self.object_path(gvr, path).await?;
        let req = r
            .patch(&n, &PatchParams::default(), &Patch::Strategic(patch))
            .map_err(reqerr)?;
        self.send_json(req).await.map(|_| ())
    }

    async fn replace_raw(&self, gvr: &Gvr, path: &str, body: &Value) -> Result<()> {
        let (r, _, n) = self.object_path(gvr, path).await?;
        let req = r
            .replace(&n, &PostParams::default(), serde_json::to_vec(body)?)
            .map_err(reqerr)?;
        self.send_json(req).await.map(|_| ())
    }

    async fn scale_raw(&self, gvr: &Gvr, path: &str) -> Result<(Request, String, Value)> {
        let (r, _, n) = self.object_path(gvr, path).await?;
        let req = r.get_subresource("scale", &n).map_err(reqerr)?;
        let scale = self.send_json(req).await?;
        Ok((r, n, scale))
    }

    /// Pod names a log request targets: one pod, or all pods matching a selector.
    async fn log_pods(&self, ns: &str, path: &str) -> Result<Vec<Pod>> {
        if path.contains('=') {
            let objs = self.list(&known::pods(), ns, Some(path)).await?;
            return objs
                .into_iter()
                .map(|o| {
                    serde_json::to_value(o)
                        .and_then(serde_json::from_value)
                        .map_err(|e| anyhow!("expecting Pod resource: {e}"))
                })
                .collect();
        }
        Ok(vec![self.get_pod(ns, path).await?])
    }
}

fn denied_msg(verb: &str, name: &str, gvr: &Gvr, ns: &str) -> String {
    format!("({verb}) access denied for user on resource {name:?}:{gvr} in namespace {ns:?}")
}

#[async_trait]
impl Accessor for KubeAccessor {
    async fn resources(&self) -> Result<Vec<ResourceMeta>> {
        Ok(self.discovery().await?.preferred.clone())
    }

    async fn resource_meta(&self, gvr: &Gvr) -> Result<Option<ResourceMeta>> {
        Ok(self.discovery().await?.lookup(gvr).cloned())
    }

    /// client.CanI: a fresh denial is an error carrying k9s' message; a
    /// cached one is just `false`.
    async fn can(&self, gvr: &Gvr, ns: &str, verb: &str) -> Result<bool> {
        let key = sar_cache_key(&sar_namespace(ns, gvr, ""), gvr, "", &[verb]);
        if let Some(ok) = self.caches.lock().unwrap().sar.get(&key) {
            return Ok(ok);
        }
        match self.access_review(ns, gvr, "", &[verb]).await? {
            None => Ok(true),
            Some(msg) => Err(anyhow!(msg)),
        }
    }

    async fn namespaces(&self) -> Result<Vec<String>> {
        if let Some(v) = self
            .caches
            .lock()
            .unwrap()
            .namespaces
            .get("validNamespaces")
        {
            return Ok(v);
        }
        if self
            .can_i(super::CLUSTER_SCOPE, &known::namespaces(), "", &["list"])
            .await
            .is_err()
        {
            return Err(anyhow!("user not authorized to list all namespaces"));
        }
        let req = Request::new(collection_path(&known::namespaces(), ""))
            .list(&ListParams::default())
            .map_err(reqerr)?;
        let list = self.send_json(req).await?;
        let mut names: Vec<String> = list
            .get("items")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|o| o.pointer("/metadata/name").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        self.caches
            .lock()
            .unwrap()
            .namespaces
            .put("validNamespaces", names.clone(), CACHE_EXPIRY);
        Ok(names)
    }

    async fn cluster_info(&self) -> Result<ClusterInfo> {
        let conn = self.conn();
        let or_na = |r: Result<String>| r.unwrap_or_else(|_| NA.to_string());
        let mut info = ClusterInfo {
            context: or_na(conn.cfg.current_context_name()),
            cluster: or_na(conn.cfg.current_cluster_name()),
            user: or_na(conn.cfg.current_user_name()),
            k9s_version: crate::version::VERSION.to_string(),
            k9s_latest: String::new(),
            k8s_version: or_na(self.server_version().await),
            cpu_pct: None,
            mem_pct: None,
        };
        if self.has_metrics().await {
            let load = async {
                let alloc = self.nodes_allocatable().await?;
                if alloc.is_empty() {
                    return Err(anyhow!("unable to fetch nodes list"));
                }
                let usage = self.node_usage().await?;
                Ok::<_, anyhow::Error>(metrics::cluster_load(&alloc, &usage))
            };
            match load.await {
                Ok((cpu, mem)) => {
                    info.cpu_pct = Some(cpu);
                    info.mem_pct = Some(mem);
                }
                Err(e) => tracing::warn!("cluster metrics failed: {e}"),
            }
        }
        Ok(info)
    }

    async fn check_connectivity(&self) -> Result<()> {
        self.server_version().await.map(|_| ())
    }

    async fn has_metrics(&self) -> bool {
        if let Some(v) = self.caches.lock().unwrap().flags.get("metricsAPI") {
            return v;
        }
        let supported = match self
            .timed(async move { self.client()?.list_api_groups().await.map_err(kerr) })
            .await
        {
            Ok(groups) => groups.groups.iter().any(|g| {
                g.name == METRICS_GROUP && g.versions.iter().any(|v| v.version == METRICS_VERSION)
            }),
            Err(e) => {
                tracing::warn!("Fail to locate metrics-server: {e}");
                false
            }
        };
        self.caches
            .lock()
            .unwrap()
            .flags
            .put("metricsAPI", supported, CACHE_EXPIRY);
        supported
    }

    async fn contexts(&self) -> Result<Vec<ContextInfo>> {
        let conn = self.conn();
        let fresh = conn.cfg.reload().unwrap_or_else(|_| conn.cfg.clone());
        Ok(fresh.contexts())
    }

    fn active_context(&self) -> String {
        self.conn().cfg.current_context_name().unwrap_or_default()
    }

    async fn switch_context(&self, name: &str) -> Result<String> {
        let current = self.conn();
        let fresh = current.cfg.reload().unwrap_or_else(|_| current.cfg.clone());
        let next = fresh.switched(name)?;
        let conn = Conn::build(next).await?;
        let ns = conn.cfg.default_namespace();
        *self.conn.write().unwrap() = Arc::new(conn);
        *self.caches.lock().unwrap() = Caches::new();
        if let Some(i) = &self.informers {
            i.clear();
        }
        Ok(ns)
    }

    fn default_namespace(&self) -> String {
        self.conn().cfg.default_namespace()
    }

    async fn list(
        &self,
        gvr: &Gvr,
        ns: &str,
        label_selector: Option<&str>,
    ) -> Result<Vec<DynamicObject>> {
        let ns = self.url_ns(gvr, ns).await;
        if let Some(objs) = self.cached_list(gvr, &ns, label_selector).await {
            return Ok(objs);
        }
        let mut lp = ListParams::default();
        if let Some(sel) = label_selector.filter(|s| !s.is_empty()) {
            lp = lp.labels(sel);
        }
        let req = Request::new(collection_path(gvr, &ns))
            .list(&lp)
            .map_err(reqerr)?;
        list_items(self.send_json(req).await?)
    }

    async fn list_table(
        &self,
        gvr: &Gvr,
        ns: &str,
        label_selector: Option<&str>,
    ) -> Result<Option<ServerTable>> {
        let ns = self.url_ns(gvr, ns).await;
        let mut lp = ListParams::default();
        if let Some(sel) = label_selector.filter(|s| !s.is_empty()) {
            lp = lp.labels(sel);
        }
        let mut req = Request::new(collection_path(gvr, &ns))
            .list(&lp)
            .map_err(reqerr)?;
        append_query(&mut req, "includeObject=Metadata")?;
        let req = with_header(req, http::header::ACCEPT, table::TABLE_ACCEPT)?;
        Ok(table::parse_table(&self.send_json(req).await?))
    }

    async fn get(&self, gvr: &Gvr, path: &str) -> Result<DynamicObject> {
        let v = self.get_raw(gvr, path).await?;
        serde_json::from_value(v).map_err(|e| anyhow!("unable to decode object: {e}"))
    }

    async fn delete(&self, gvr: &Gvr, path: &str, opts: DeleteOptions) -> Result<()> {
        let (ns, n) = split_path(path);
        self.can_i(&ns, gvr, &n, &["delete"]).await?;
        let grace = if opts.force {
            Some(opts.grace.unwrap_or(0).max(0))
        } else {
            opts.grace.filter(|g| *g >= 0)
        };
        let dp = DeleteParams {
            propagation_policy: opts.propagation.map(|p| match p {
                Propagation::Background => kube::api::PropagationPolicy::Background,
                Propagation::Foreground => kube::api::PropagationPolicy::Foreground,
                Propagation::Orphan => kube::api::PropagationPolicy::Orphan,
            }),
            grace_period_seconds: grace.map(|g| g as u32),
            ..Default::default()
        };
        let (r, _, n) = self.object_path(gvr, path).await?;
        let req = r.delete(&n, &dp).map_err(reqerr)?;
        self.send_json(req).await.map(|_| ())
    }

    async fn describe(&self, gvr: &Gvr, path: &str) -> Result<String> {
        let obj = self.get(gvr, path).await?;
        let ns = obj.metadata.namespace.clone().unwrap_or_default();
        let lp = ListParams::default().fields(&events_selector(&obj, gvr));
        let events = match Request::new(collection_path(&known::events(), &ns)).list(&lp) {
            Ok(req) => match self.send_json(req).await {
                Ok(list) => list_items(list).unwrap_or_default(),
                Err(e) => {
                    tracing::debug!("describe events lookup failed: {e}");
                    vec![]
                }
            },
            Err(_) => vec![],
        };
        let related = self.related(gvr, &obj).await?;
        super::describe::describe_related(gvr, &obj, &events, &related, chrono::Utc::now())
    }

    async fn to_yaml(&self, gvr: &Gvr, path: &str, show_managed: bool) -> Result<String> {
        let mut v = self.get_raw(gvr, path).await?;
        if !show_managed {
            if let Some(m) = v.get_mut("metadata").and_then(Value::as_object_mut) {
                m.remove("managedFields");
            }
        }
        Ok(super::goyaml::to_yaml(&v))
    }

    async fn apply_yaml(&self, gvr: &Gvr, path: &str, yaml: &str) -> Result<()> {
        let body: Value = serde_yaml::from_str(yaml).context("invalid yaml")?;
        if !body.is_object() {
            return Err(anyhow!("invalid yaml: expecting a resource manifest"));
        }
        self.replace_raw(gvr, path, &body).await
    }

    async fn scale(&self, gvr: &Gvr, path: &str, replicas: i32) -> Result<()> {
        let (ns, n) = split_path(path);
        if is_gvr(gvr, &known::deployments()) || is_gvr(gvr, &known::statefulsets()) {
            let mut sgvr = gvr.clone();
            sgvr.sub_resource = "scale".into();
            self.can_i(&ns, &sgvr, &n, &["get", "update"]).await?;
        }
        let (r, n, mut scale) = self.scale_raw(gvr, path).await?;
        match scale.get_mut("spec").and_then(Value::as_object_mut) {
            Some(spec) => {
                spec.insert("replicas".into(), json!(replicas));
            }
            None => {
                scale["spec"] = json!({ "replicas": replicas });
            }
        }
        let req = r
            .replace_subresource(
                "scale",
                &n,
                &PostParams::default(),
                serde_json::to_vec(&scale)?,
            )
            .map_err(reqerr)?;
        self.send_json(req).await.map(|_| ())
    }

    async fn replicas(&self, gvr: &Gvr, path: &str) -> Result<i32> {
        let (_, _, scale) = self.scale_raw(gvr, path).await?;
        Ok(scale
            .pointer("/spec/replicas")
            .and_then(Value::as_i64)
            .unwrap_or(0) as i32)
    }

    async fn restart(&self, gvr: &Gvr, path: &str) -> Result<()> {
        let is_dp = is_gvr(gvr, &known::deployments());
        if !is_dp && !is_gvr(gvr, &known::daemonsets()) && !is_gvr(gvr, &known::statefulsets()) {
            return Err(anyhow!("unsupported resource for restart: {gvr}"));
        }
        let obj = self.get_raw(gvr, path).await?;
        let (ns, n) = split_path(path);
        self.can_i(&ns, gvr, &n, &["patch"]).await?;
        let patch = patches::restart_patch(&obj, is_dp, &patches::rfc3339_now())?;
        self.patch_strategic(gvr, path, patch).await
    }

    async fn set_images(&self, gvr: &Gvr, path: &str, images: Vec<ImageSpec>) -> Result<()> {
        let (ns, n) = split_path(path);
        let supported = [
            known::pods(),
            known::deployments(),
            known::statefulsets(),
            known::daemonsets(),
        ];
        if !supported.iter().any(|g| is_gvr(gvr, g)) {
            return Err(anyhow!("unsupported resource for setting images: {gvr}"));
        }
        self.can_i(&ns, gvr, &n, &["patch"]).await?;
        let patch = if is_gvr(gvr, &known::pods()) {
            let pod = self.get_pod(&ns, &n).await?;
            if let Some(r) = pod
                .metadata
                .owner_references
                .as_ref()
                .and_then(|r| r.first())
            {
                return Err(anyhow!(
                    "unable to set image. This pod is managed by {}/{}. Please set the image on the controller",
                    r.kind,
                    r.name
                ));
            }
            patches::pod_images_patch(&images)
        } else {
            patches::template_images_patch(&images)
        };
        self.patch_strategic(gvr, path, patch).await
    }

    async fn trigger_cronjob(&self, path: &str) -> Result<()> {
        let (ns, n) = split_path(path);
        self.can_i(&ns, &known::jobs(), &n, &["get", "create"])
            .await?;
        let cj_gvr = known::cronjobs();
        let cj = self.get_raw(&cj_gvr, path).await?;
        let job = patches::job_from_cronjob(&cj, &cj_gvr.gv(), &patches::rand_string(3))?;
        let req = Request::new(collection_path(&known::jobs(), &ns))
            .create(&PostParams::default(), serde_json::to_vec(&job)?)
            .map_err(reqerr)?;
        self.send_json(req).await.map(|_| ())
    }

    async fn toggle_suspend_cronjob(&self, path: &str) -> Result<()> {
        let (ns, n) = split_path(path);
        let gvr = known::cronjobs();
        self.can_i(&ns, &gvr, &n, &["get", "update"]).await?;
        let mut cj = self.get_raw(&gvr, path).await?;
        patches::toggle_suspend(&mut cj)?;
        self.replace_raw(&gvr, path, &cj).await
    }

    async fn toggle_cordon(&self, node: &str, cordon: bool) -> Result<()> {
        let c = self.client()?;
        let node = node.to_string();
        self.timed(async move { drain::toggle_cordon(&c, &node, cordon).await })
            .await
    }

    async fn drain(&self, node: &str, opts: DrainOptions) -> Result<String> {
        let c = self.client()?;
        let n: k8s_openapi::api::core::v1::Node =
            kube::Api::all(c.clone()).get(node).await.map_err(kerr)?;
        let cordoned = n.spec.and_then(|s| s.unschedulable).unwrap_or(false);
        if !cordoned {
            self.toggle_cordon(node, true).await?;
        }
        let d = drain::Drainer {
            client: c.clone(),
            opts,
            out: drain::Out::default(),
        };
        let pods = match d.pods_for_deletion(node).await {
            Ok(p) => p,
            Err(errs) => {
                for e in &errs {
                    d.out.write(&format!("[{node}] {e}\n"));
                }
                return Err(anyhow!(errs.join("\n")));
            }
        };
        let eviction_gv = if d.opts.disable_eviction || pods.is_empty() {
            None
        } else {
            let core = c.list_core_api_resources("v1").await.map_err(kerr)?;
            core.resources
                .iter()
                .find(|r| r.name == "pods/eviction" && r.kind == "Eviction")
                .and_then(|r| match (&r.group, &r.version) {
                    (Some(g), Some(v)) if !g.is_empty() && !v.is_empty() => {
                        Some((g.clone(), v.clone()))
                    }
                    _ => None,
                })
        };
        d.delete_or_evict(pods, eviction_gv).await?;
        d.out.write(&format!("Node {node} drained!"));
        Ok(d.out.take())
    }

    async fn pod_metrics(&self, ns: &str) -> Result<HashMap<String, PodMetrics>> {
        let ns = if ns == super::NAMESPACE_ALL { "" } else { ns };
        self.metrics_check(ns, &known::pod_metrics()).await?;
        let key = super::fqn(ns, "pods");
        if let Some(v) = self.caches.lock().unwrap().pod_mx.get(&key) {
            return Ok(v);
        }
        let req = Request::new(collection_path(&known::pod_metrics(), ns))
            .list(&ListParams::default())
            .map_err(reqerr)?;
        let mx = metrics::pod_metrics(&self.send_json(req).await?);
        self.caches
            .lock()
            .unwrap()
            .pod_mx
            .put(&key, mx.clone(), MX_CACHE_EXPIRY);
        Ok(mx)
    }

    async fn node_metrics(&self) -> Result<HashMap<String, NodeMetrics>> {
        let usage = self.node_usage().await?;
        let alloc = self.nodes_allocatable().await?;
        Ok(metrics::node_metrics(&usage, &alloc))
    }

    async fn tail_logs(&self, opts: LogOptions) -> Result<LogStream> {
        let (ns, path) = match opts.path.split_once('/') {
            Some((ns, p)) if !p.contains('=') => (ns.to_string(), p.to_string()),
            _ => (opts.namespace.clone(), opts.path.clone()),
        };
        let ns = clean_ns(&ns).to_string();
        let pods = self.log_pods(&ns, &path).await?;
        let mut log_gvr = known::pods();
        log_gvr.sub_resource = "log".into();
        let stream_client = self.stream_client()?;
        let (tx, rx) = tokio::sync::mpsc::channel(LOG_CHANNEL_BUFFER);
        let mut tails = Vec::new();
        for pod in pods {
            let name = pod.metadata.name.clone().unwrap_or_default();
            let pns = pod.metadata.namespace.clone().unwrap_or_else(|| ns.clone());
            self.can_i(&pns, &log_gvr, &name, &["get"]).await?;
            for co in logs::containers_to_tail(&pod, &opts) {
                tails.push(logs::Tail {
                    client: stream_client.clone(),
                    namespace: pns.clone(),
                    pod: name.clone(),
                    container: co,
                    opts: opts.clone(),
                });
            }
        }
        for t in tails {
            tokio::spawn(t.run(tx.clone()));
        }
        drop(tx);
        Ok(tokio_stream::wrappers::ReceiverStream::new(rx).boxed())
    }

    async fn port_forward(&self, spec: PortForwardSpec) -> Result<PortForwardHandle> {
        let ns = clean_ns(&spec.namespace).to_string();
        let pods = known::pods();
        self.can_i(&ns, &pods, &spec.pod, &["get"]).await?;
        let pod = self.get_pod(&ns, &spec.pod).await?;
        let phase = pod.status.and_then(|s| s.phase).unwrap_or_default();
        if phase != "Running" {
            return Err(anyhow!(
                "unable to forward port because pod is not running. Current status={phase}"
            ));
        }
        let mut pf_gvr = pods.clone();
        pf_gvr.sub_resource = "portforward".into();
        self.can_i(&ns, &pf_gvr, "", &["create"]).await?;
        let cancel = tokio_util::sync::CancellationToken::new();
        let api: kube::Api<Pod> = kube::Api::namespaced(self.stream_client()?, &ns);
        portforward::start(api, spec.clone(), cancel.clone()).await?;
        Ok(PortForwardHandle {
            info: PortForwardInfo {
                spec,
                started_at: chrono::Utc::now(),
                active: true,
            },
            cancel,
        })
    }

    fn exec_argv(&self, ns: &str, pod: &str, container: &str, shell: &str) -> Vec<String> {
        let insecure = self.conn().cfg.opts.insecure_skip_tls_verify;
        shell::exec_argv(insecure, clean_ns(ns), pod, container, shell)
    }

    fn edit_argv(&self, gvr: &Gvr, path: &str) -> Vec<String> {
        shell::edit_argv(self.conn().cfg.opts.insecure_skip_tls_verify, gvr, path)
    }

    fn shell_env(&self) -> Vec<(String, String)> {
        let conn = self.conn();
        let explicit = conn
            .cfg
            .opts
            .kubeconfig
            .as_deref()
            .is_some_and(|k| !k.is_empty());
        shell::shell_env(
            conn.override_file.as_ref().map(|f| f.path()),
            &conn.cfg.kubeconfig_paths(),
            explicit,
            |k| std::env::var(k).ok(),
        )
    }
}
