//! Node cordon and drain (k9s dao.Node.ToggleCordon/Drain over kubectl's
//! drain.Helper).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Result};
use k8s_openapi::api::core::v1::Pod;
use kube::api::{DeleteParams, ListParams, Patch, PatchParams};
use serde_json::{json, Value};
use tokio::time::Instant;

use super::{api_status, kerr, patches};
use crate::k8s::DrainOptions;

const DAEMON_SET_FATAL: &str = "DaemonSet-managed Pods (use --ignore-daemonsets to ignore)";
const DAEMON_SET_WARNING: &str = "ignoring DaemonSet-managed Pods";
const LOCAL_STORAGE_FATAL: &str =
    "Pods with local storage (use --delete-emptydir-data to override)";
const LOCAL_STORAGE_WARNING: &str = "deleting Pods with local storage";
const UNMANAGED_FATAL: &str = "Pods declare no controller (use --force to override)";
const UNMANAGED_WARNING: &str = "deleting Pods that declare no controller";
/// corev1.MirrorPodAnnotationKey.
const MIRROR_POD_ANNOTATION: &str = "kubernetes.io/config.mirror";
/// corev1.NamespaceTerminatingCause.
const NAMESPACE_TERMINATING_CAUSE: &str = "NamespaceTerminating";
const POLL_INTERVAL: Duration = Duration::from_secs(1);
const EVICT_RETRY: Duration = Duration::from_secs(5);
/// Go's time.Duration(math.MaxInt64).
const MAX_GO_DURATION: &str = "2562047h47m16.854775807s";

/// drain.PodDeleteStatus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PodStatus {
    Okay,
    Skip,
    Warning { delete: bool, msg: String },
    Error(String),
}

impl PodStatus {
    fn delete(&self) -> bool {
        matches!(
            self,
            PodStatus::Okay | PodStatus::Warning { delete: true, .. }
        )
    }
}

/// Outcome of looking up a pod's controlling DaemonSet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DsLookup {
    Found,
    NotFound(String),
    Failed(String),
}

fn controller_kind(pod: &Pod) -> Option<(&str, &str)> {
    pod.metadata
        .owner_references
        .as_ref()?
        .iter()
        .find(|r| r.controller == Some(true))
        .map(|r| (r.kind.as_str(), r.name.as_str()))
}

fn is_finished(pod: &Pod) -> bool {
    matches!(
        pod.status.as_ref().and_then(|s| s.phase.as_deref()),
        Some("Succeeded") | Some("Failed")
    )
}

/// The DaemonSet name to look up when the daemonSet filter needs it.
pub(crate) fn daemonset_to_check(pod: &Pod) -> Option<String> {
    match controller_kind(pod) {
        Some(("DaemonSet", name)) if !is_finished(pod) => Some(name.to_string()),
        _ => None,
    }
}

fn has_local_storage(pod: &Pod) -> bool {
    pod.spec
        .as_ref()
        .and_then(|s| s.volumes.as_ref())
        .map(|vv| vv.iter().any(|v| v.empty_dir.is_some()))
        .unwrap_or(false)
}

/// Run the drain filters in kubectl order, short-circuiting on the first
/// non-deletable status.
pub(crate) fn filter_pod(pod: &Pod, opts: &DrainOptions, ds: Option<&DsLookup>) -> PodStatus {
    let filters: [&dyn Fn() -> PodStatus; 4] = [
        // daemonSetFilter
        &|| {
            let Some(lookup) = ds else {
                return PodStatus::Okay;
            };
            match lookup {
                DsLookup::NotFound(msg) if opts.force => PodStatus::Warning {
                    delete: true,
                    msg: msg.clone(),
                },
                DsLookup::NotFound(msg) | DsLookup::Failed(msg) => PodStatus::Error(msg.clone()),
                DsLookup::Found if !opts.ignore_all_daemon_sets => {
                    PodStatus::Error(DAEMON_SET_FATAL.into())
                }
                DsLookup::Found => PodStatus::Warning {
                    delete: false,
                    msg: DAEMON_SET_WARNING.into(),
                },
            }
        },
        // mirrorPodFilter
        &|| {
            let mirror = pod
                .metadata
                .annotations
                .as_ref()
                .map(|a| a.contains_key(MIRROR_POD_ANNOTATION))
                .unwrap_or(false);
            if mirror {
                PodStatus::Skip
            } else {
                PodStatus::Okay
            }
        },
        // localStorageFilter
        &|| {
            if !has_local_storage(pod) || is_finished(pod) {
                return PodStatus::Okay;
            }
            if !opts.delete_empty_dir_data {
                return PodStatus::Error(LOCAL_STORAGE_FATAL.into());
            }
            PodStatus::Warning {
                delete: true,
                msg: LOCAL_STORAGE_WARNING.into(),
            }
        },
        // unreplicatedFilter
        &|| {
            if is_finished(pod) || controller_kind(pod).is_some() {
                return PodStatus::Okay;
            }
            if opts.force {
                return PodStatus::Warning {
                    delete: true,
                    msg: UNMANAGED_WARNING.into(),
                };
            }
            PodStatus::Error(UNMANAGED_FATAL.into())
        },
    ];
    // skipDeletedFilter is a no-op: k9s never sets SkipWaitForDeleteTimeoutSeconds.
    let mut status = PodStatus::Okay;
    for f in filters {
        status = f();
        if !status.delete() {
            break;
        }
    }
    status
}

fn pod_fqn(pod: &Pod) -> String {
    format!(
        "{}/{}",
        pod.metadata.namespace.as_deref().unwrap_or_default(),
        pod.metadata.name.as_deref().unwrap_or_default()
    )
}

/// PodDeleteList.errors: one error per distinct message.
pub(crate) fn delete_errors(items: &[(Pod, PodStatus)]) -> Vec<String> {
    let mut failed: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (pod, st) in items {
        if let PodStatus::Error(msg) = st {
            let msg = if msg.is_empty() {
                "unexpected error".to_string()
            } else {
                msg.clone()
            };
            failed.entry(msg).or_default().push(pod_fqn(pod));
        }
    }
    failed
        .into_iter()
        .map(|(msg, pods)| format!("cannot delete {msg}: {}", pods.join(", ")))
        .collect()
}

/// Go time.Duration.String for whole seconds.
pub(crate) fn go_duration(secs: i64) -> String {
    if secs == 0 {
        return "0s".into();
    }
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}h{m}m{s}s")
    } else if m > 0 {
        format!("{m}m{s}s")
    } else {
        format!("{s}s")
    }
}

/// utilerrors.NewAggregate(...).Error().
fn aggregate(errs: Vec<String>) -> Option<String> {
    match errs.len() {
        0 => None,
        1 => errs.into_iter().next(),
        _ => Some(format!("[{}]", errs.join(", "))),
    }
}

/// Shared drain progress log (Helper.Out / ErrOut).
#[derive(Clone, Default)]
pub(crate) struct Out(Arc<Mutex<String>>);

impl Out {
    pub(crate) fn write(&self, s: &str) {
        self.0.lock().unwrap().push_str(s);
    }
    pub(crate) fn take(&self) -> String {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

/// Node.ToggleCordon.
pub(crate) async fn toggle_cordon(client: &kube::Client, node: &str, cordon: bool) -> Result<()> {
    let api: kube::Api<k8s_openapi::api::core::v1::Node> = kube::Api::all(client.clone());
    let n = api.get(node).await.map_err(kerr)?;
    let unschedulable = n.spec.and_then(|s| s.unschedulable).unwrap_or(false);
    if unschedulable == cordon {
        if cordon {
            return Err(anyhow!("node is already cordoned"));
        }
        return Err(anyhow!("node is already uncordoned"));
    }
    api.patch(
        node,
        &PatchParams::default(),
        &Patch::Strategic(patches::cordon_patch(cordon)),
    )
    .await
    .map_err(kerr)?;
    Ok(())
}

/// Drain helper bound to one client and option set.
pub(crate) struct Drainer {
    pub(crate) client: kube::Client,
    pub(crate) opts: DrainOptions,
    pub(crate) out: Out,
}

impl Drainer {
    fn pods_api(&self, ns: &str) -> kube::Api<Pod> {
        kube::Api::namespaced(self.client.clone(), ns)
    }

    fn global_timeout(&self) -> (Option<Duration>, String) {
        if self.opts.timeout_secs <= 0 {
            (None, MAX_GO_DURATION.into())
        } else {
            (
                Some(Duration::from_secs(self.opts.timeout_secs as u64)),
                go_duration(self.opts.timeout_secs),
            )
        }
    }

    fn delete_options(&self) -> Value {
        if self.opts.grace_period_seconds >= 0 {
            json!({"gracePeriodSeconds": self.opts.grace_period_seconds})
        } else {
            json!({})
        }
    }

    /// GetPodsForDeletion.
    pub(crate) async fn pods_for_deletion(&self, node: &str) -> Result<Vec<Pod>, Vec<String>> {
        let api: kube::Api<Pod> = kube::Api::all(self.client.clone());
        let mut pods = Vec::new();
        let mut lp = ListParams::default()
            .fields(&format!("spec.nodeName={node}"))
            .limit(500);
        loop {
            let page = api.list(&lp).await.map_err(|e| vec![super::kerr_msg(&e)])?;
            pods.extend(page.items);
            match page.metadata.continue_.filter(|c| !c.is_empty()) {
                Some(c) => lp.continue_token = Some(c),
                None => break,
            }
        }
        let mut items = Vec::with_capacity(pods.len());
        for pod in pods {
            let ds = match daemonset_to_check(&pod) {
                Some(name) => {
                    let ns = pod.metadata.namespace.clone().unwrap_or_default();
                    let dsapi: kube::Api<k8s_openapi::api::apps::v1::DaemonSet> =
                        kube::Api::namespaced(self.client.clone(), &ns);
                    Some(match dsapi.get(&name).await {
                        Ok(_) => DsLookup::Found,
                        Err(e) => match api_status(&e) {
                            Some(s) if s.is_not_found() => DsLookup::NotFound(s.message.clone()),
                            _ => DsLookup::Failed(super::kerr_msg(&e)),
                        },
                    })
                }
                None => None,
            };
            let st = filter_pod(&pod, &self.opts, ds.as_ref());
            items.push((pod, st));
        }
        let errs = delete_errors(&items);
        if !errs.is_empty() {
            return Err(errs);
        }
        Ok(items
            .into_iter()
            .filter(|(_, st)| st.delete())
            .map(|(p, _)| p)
            .collect())
    }

    /// DeleteOrEvictPods.
    pub(crate) async fn delete_or_evict(
        &self,
        pods: Vec<Pod>,
        eviction_gv: Option<(String, String)>,
    ) -> Result<()> {
        if pods.is_empty() {
            return Ok(());
        }
        if !self.opts.disable_eviction {
            if let Some(gv) = eviction_gv {
                return self.evict_pods(pods, gv).await;
            }
        }
        self.delete_pods(pods).await
    }

    async fn evict_pod(&self, pod: &Pod, gv: &(String, String)) -> Result<(), kube::Error> {
        let ns = pod.metadata.namespace.clone().unwrap_or_default();
        let name = pod.metadata.name.clone().unwrap_or_default();
        let body = json!({
            "apiVersion": format!("{}/{}", gv.0, gv.1),
            "kind": "Eviction",
            "metadata": {"name": name, "namespace": ns},
            "deleteOptions": self.delete_options(),
        });
        let req = http::Request::post(format!("/api/v1/namespaces/{ns}/pods/{name}/eviction"))
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(serde_json::to_vec(&body).unwrap_or_default())
            .map_err(kube::Error::HttpError)?;
        self.client.request::<Value>(req).await.map(|_| ())
    }

    /// Poll until the pod is gone or replaced (waitForDelete for one pod).
    async fn gone(&self, pod: &Pod) -> Result<bool> {
        let ns = pod.metadata.namespace.clone().unwrap_or_default();
        let name = pod.metadata.name.clone().unwrap_or_default();
        match self.pods_api(&ns).get(&name).await {
            Ok(p) => Ok(p.metadata.uid != pod.metadata.uid),
            Err(e) => match api_status(&e) {
                Some(s) if s.is_not_found() => Ok(true),
                _ => Err(kerr(e)),
            },
        }
    }

    async fn evict_one(
        &self,
        pod: Pod,
        gv: &(String, String),
        deadline: Option<Instant>,
        timeout_str: &str,
    ) -> Result<(), String> {
        let ns = pod.metadata.namespace.clone().unwrap_or_default();
        let name = pod.metadata.name.clone().unwrap_or_default();
        let expired = || deadline.map(|d| Instant::now() >= d).unwrap_or(false);
        loop {
            self.out.write(&format!("evicting pod {ns}/{name}\n"));
            if expired() {
                return Err(format!(
                    "error when evicting pods/{name:?} -n {ns:?}: global timeout reached: {timeout_str}"
                ));
            }
            match self.evict_pod(&pod, gv).await {
                Ok(()) => break,
                Err(e) => {
                    let status = api_status(&e).cloned();
                    let msg = super::kerr_msg(&e);
                    match status {
                        Some(s) if s.is_not_found() => return Ok(()),
                        Some(s) if s.code == 429 => {
                            self.out.write(&format!(
                                "error when evicting pods/{name:?} -n {ns:?} (will retry after 5s): {msg}\n"
                            ));
                            tokio::time::sleep(EVICT_RETRY).await;
                        }
                        Some(s)
                            if s.is_forbidden() && has_cause(&s, NAMESPACE_TERMINATING_CAUSE) =>
                        {
                            if pod.metadata.deletion_timestamp.is_some() {
                                break;
                            }
                            self.out.write(&format!(
                                "error when evicting pod {name:?} (will retry after 5s): {msg}\n"
                            ));
                            tokio::time::sleep(EVICT_RETRY).await;
                        }
                        _ => {
                            return Err(format!(
                                "error when evicting pods/{name:?} -n {ns:?}: {msg}"
                            ))
                        }
                    }
                }
            }
        }
        // waitForDelete with an infinite poll timeout bounded by the global context.
        loop {
            match self.gone(&pod).await {
                Ok(true) => return Ok(()),
                Ok(false) => {}
                Err(e) => {
                    return Err(format!(
                        "error when waiting for pod {name:?} terminating: {e}"
                    ))
                }
            }
            if expired() {
                return Err(format!(
                    "error when waiting for pod {name:?} terminating: global timeout reached: {timeout_str}"
                ));
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    async fn evict_pods(&self, pods: Vec<Pod>, gv: (String, String)) -> Result<()> {
        let (timeout, timeout_str) = self.global_timeout();
        let deadline = timeout.map(|t| Instant::now() + t);
        let results = futures::future::join_all(
            pods.into_iter()
                .map(|p| self.evict_one(p, &gv, deadline, &timeout_str)),
        )
        .await;
        let errs: Vec<String> = results.into_iter().filter_map(Result::err).collect();
        match aggregate(errs) {
            Some(e) => Err(anyhow!(e)),
            None => Ok(()),
        }
    }

    async fn delete_pods(&self, pods: Vec<Pod>) -> Result<()> {
        let (timeout, _) = self.global_timeout();
        let mut dp = DeleteParams::default();
        if self.opts.grace_period_seconds >= 0 {
            dp.grace_period_seconds = Some(self.opts.grace_period_seconds as u32);
        }
        for pod in &pods {
            let ns = pod.metadata.namespace.clone().unwrap_or_default();
            let name = pod.metadata.name.clone().unwrap_or_default();
            if let Err(e) = self.pods_api(&ns).delete(&name, &dp).await {
                match api_status(&e) {
                    Some(s) if s.is_not_found() => {}
                    _ => return Err(kerr(e)),
                }
            }
        }
        let deadline = timeout.map(|t| Instant::now() + t);
        let mut pending = pods;
        loop {
            let mut still = Vec::new();
            for p in pending {
                if !self.gone(&p).await? {
                    still.push(p);
                }
            }
            pending = still;
            if pending.is_empty() {
                return Ok(());
            }
            if deadline.map(|d| Instant::now() >= d).unwrap_or(false) {
                return Err(anyhow!("timed out waiting for the condition"));
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

fn has_cause(s: &kube::core::Status, cause: &str) -> bool {
    s.details
        .as_ref()
        .map(|d| d.causes.iter().any(|c| c.reason == cause))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pod(v: Value) -> Pod {
        serde_json::from_value(v).unwrap()
    }

    fn owned(kind: &str) -> Value {
        json!([{"apiVersion": "apps/v1", "kind": kind, "name": "owner", "uid": "1", "controller": true}])
    }

    #[test]
    fn plain_pods() {
        let o = DrainOptions::default();
        let managed = pod(
            json!({"metadata": {"name": "a", "namespace": "ns", "ownerReferences": owned("ReplicaSet")}}),
        );
        assert_eq!(filter_pod(&managed, &o, None), PodStatus::Okay);
        let bare = pod(json!({"metadata": {"name": "a", "namespace": "ns"}}));
        assert_eq!(
            filter_pod(&bare, &o, None),
            PodStatus::Error(UNMANAGED_FATAL.into())
        );
        let forced = DrainOptions {
            force: true,
            ..o.clone()
        };
        assert_eq!(
            filter_pod(&bare, &forced, None),
            PodStatus::Warning {
                delete: true,
                msg: UNMANAGED_WARNING.into()
            }
        );
        let done = pod(json!({"metadata": {"name": "a"}, "status": {"phase": "Succeeded"}}));
        assert_eq!(filter_pod(&done, &o, None), PodStatus::Okay);
        // Non controller owner refs do not count.
        let weak = pod(
            json!({"metadata": {"name": "a", "ownerReferences": [{"apiVersion": "v1", "kind": "X", "name": "x", "uid": "1"}]}}),
        );
        assert_eq!(
            filter_pod(&weak, &o, None),
            PodStatus::Error(UNMANAGED_FATAL.into())
        );
    }

    #[test]
    fn daemonsets() {
        let ds = pod(
            json!({"metadata": {"name": "d", "namespace": "ns", "ownerReferences": owned("DaemonSet")}}),
        );
        assert_eq!(daemonset_to_check(&ds).as_deref(), Some("owner"));
        let o = DrainOptions::default();
        assert_eq!(
            filter_pod(&ds, &o, Some(&DsLookup::Found)),
            PodStatus::Warning {
                delete: false,
                msg: DAEMON_SET_WARNING.into()
            }
        );
        let strict = DrainOptions {
            ignore_all_daemon_sets: false,
            ..o.clone()
        };
        assert_eq!(
            filter_pod(&ds, &strict, Some(&DsLookup::Found)),
            PodStatus::Error(DAEMON_SET_FATAL.into())
        );
        let nf = DsLookup::NotFound("daemonsets.apps \"owner\" not found".into());
        assert_eq!(
            filter_pod(&ds, &o, Some(&nf)),
            PodStatus::Error("daemonsets.apps \"owner\" not found".into())
        );
        let forced = DrainOptions {
            force: true,
            ..o.clone()
        };
        assert!(filter_pod(&ds, &forced, Some(&nf)).delete());
        let done = pod(
            json!({"metadata": {"name": "d", "ownerReferences": owned("DaemonSet")}, "status": {"phase": "Failed"}}),
        );
        assert_eq!(daemonset_to_check(&done), None);
    }

    #[test]
    fn mirror_and_local_storage() {
        let o = DrainOptions::default();
        let mirror =
            pod(json!({"metadata": {"name": "m", "annotations": {MIRROR_POD_ANNOTATION: "x"}}}));
        assert_eq!(filter_pod(&mirror, &o, None), PodStatus::Skip);
        let ed = pod(json!({
            "metadata": {"name": "e", "ownerReferences": owned("ReplicaSet")},
            "spec": {"containers": [], "volumes": [{"name": "v", "emptyDir": {}}]}
        }));
        // Warning from localStorage is overwritten by the later Okay (kubectl TODO).
        assert_eq!(filter_pod(&ed, &o, None), PodStatus::Okay);
        let keep = DrainOptions {
            delete_empty_dir_data: false,
            ..o
        };
        assert_eq!(
            filter_pod(&ed, &keep, None),
            PodStatus::Error(LOCAL_STORAGE_FATAL.into())
        );
    }

    #[test]
    fn errors_grouped() {
        let p = |n: &str| pod(json!({"metadata": {"name": n, "namespace": "ns"}}));
        let items = vec![
            (p("a"), PodStatus::Error(UNMANAGED_FATAL.into())),
            (p("b"), PodStatus::Okay),
            (p("c"), PodStatus::Error(UNMANAGED_FATAL.into())),
            (p("d"), PodStatus::Error(String::new())),
        ];
        assert_eq!(
            delete_errors(&items),
            vec![
                format!("cannot delete {UNMANAGED_FATAL}: ns/a, ns/c"),
                "cannot delete unexpected error: ns/d".to_string(),
            ]
        );
    }

    #[test]
    fn durations_and_aggregates() {
        assert_eq!(go_duration(0), "0s");
        assert_eq!(go_duration(30), "30s");
        assert_eq!(go_duration(300), "5m0s");
        assert_eq!(go_duration(3661), "1h1m1s");
        assert_eq!(aggregate(vec![]), None);
        assert_eq!(aggregate(vec!["a".into()]), Some("a".into()));
        assert_eq!(
            aggregate(vec!["a".into(), "b".into()]),
            Some("[a, b]".into())
        );
        let out = Out::default();
        out.write("x");
        out.write("y");
        assert_eq!(out.take(), "xy");
        assert_eq!(out.take(), "");
    }
}
