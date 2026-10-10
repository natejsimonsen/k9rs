//! Watch-backed list cache (k9s watch.Factory over client-go informers).
//!
//! The first list of a resource in a namespace starts a list+watch that
//! keeps a local store up to date. Later lists read the store, so views
//! open instantly and refreshes cost no API calls. Informers live until the
//! context changes, as k9s factories do.
//!
//! Like k9s listers, reads never block on a sync: a new informer gets
//! `SYNC_WAIT` (k9s model.Table initRefreshRate) to finish its first list,
//! after that the store is returned as is (empty until synced) and later
//! refreshes pick up the data.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::{StreamExt, TryStreamExt};
use kube::api::{ApiResource, DynamicObject};
use kube::core::TypeMeta;
use kube::runtime::reflector::store::{Store, Writer};
use kube::runtime::{watcher, WatchStreamExt};
use kube::Api;

use crate::k8s::Gvr;
use crate::model::labels::Selector;

/// How long a fresh informer may take before its (empty) store is served.
pub(super) const SYNC_WAIT: Duration = Duration::from_millis(300);

struct Informer {
    store: Store<DynamicObject>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Informer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Informers keyed by `gvr|namespace` ("" namespace = all/cluster wide).
#[derive(Default)]
pub(super) struct Informers {
    map: Mutex<HashMap<String, Arc<Informer>>>,
}

fn key(gvr: &Gvr, ns: &str) -> String {
    format!("{gvr}|{ns}")
}

impl Informers {
    /// Drop every informer (watch.Factory.Terminate).
    pub(super) fn clear(&self) {
        self.map.lock().expect("informers").clear();
    }

    fn get_or_start(
        &self,
        client: &kube::Client,
        gvr: &Gvr,
        kind: &str,
        ns: &str,
    ) -> (Arc<Informer>, bool) {
        let mut map = self.map.lock().expect("informers");
        if let Some(i) = map.get(&key(gvr, ns)) {
            return (i.clone(), false);
        }
        let ar = ApiResource {
            group: gvr.g().to_string(),
            version: gvr.v().to_string(),
            api_version: if gvr.g().is_empty() {
                gvr.v().to_string()
            } else {
                format!("{}/{}", gvr.g(), gvr.v())
            },
            kind: kind.to_string(),
            plural: gvr.r().to_string(),
        };
        let api: Api<DynamicObject> = if ns.is_empty() {
            Api::all_with(client.clone(), &ar)
        } else {
            Api::namespaced_with(client.clone(), ns, &ar)
        };
        let types = TypeMeta {
            api_version: ar.api_version.clone(),
            kind: ar.kind.clone(),
        };
        let writer = Writer::new(ar);
        let store = writer.as_reader();
        let what = key(gvr, ns);
        let task = tokio::spawn(async move {
            // List items carry no apiVersion/kind: stamp them once here so
            // shared store objects are complete (renderers read the kind).
            // managedFields are dropped: only lists read the store, and they
            // never show them (YAML/describe fetch the object fresh).
            let stamp = types.clone();
            let stream = watcher(api, watcher::Config::default())
                .default_backoff()
                .map_ok(move |ev| {
                    ev.modify(|o| {
                        if o.types.is_none() {
                            o.types = Some(stamp.clone());
                        }
                        o.metadata.managed_fields = None;
                    })
                })
                .reflect(writer);
            futures::pin_mut!(stream);
            let mut synced = false;
            while let Some(ev) = stream.next().await {
                match ev {
                    Ok(watcher::Event::InitDone) => synced = true,
                    // Never synced (no list/watch access, unknown resource):
                    // dropping the writer fails the waiting lists.
                    Err(e) if !synced => {
                        tracing::warn!("informer {what} failed: {e}");
                        return;
                    }
                    Err(e) => tracing::debug!("informer {what} watch error: {e}"),
                    _ => {}
                }
            }
        });
        let inf = Arc::new(Informer { store, task });
        map.insert(key(gvr, ns), inf.clone());
        (inf, true)
    }

    fn forget(&self, gvr: &Gvr, ns: &str) {
        self.map.lock().expect("informers").remove(&key(gvr, ns));
    }

    /// Cached objects of `gvr` in `ns`, shared with the store (no copies),
    /// sorted like an API list
    /// (namespace, name). A new informer gets `wait` to sync; otherwise the
    /// current store is served, synced or not. `None` when the informer
    /// failed before syncing; the caller then lists directly.
    pub(super) async fn list(
        &self,
        client: &kube::Client,
        gvr: &Gvr,
        kind: &str,
        ns: &str,
        selector: &Selector,
        wait: Duration,
    ) -> Option<Vec<Arc<DynamicObject>>> {
        // A synced cluster wide informer also serves namespaced lists.
        let wide = (!ns.is_empty())
            .then(|| {
                self.map
                    .lock()
                    .expect("informers")
                    .get(&key(gvr, ""))
                    .cloned()
            })
            .flatten()
            .filter(|i| {
                matches!(
                    futures::FutureExt::now_or_never(i.store.wait_until_ready()),
                    Some(Ok(()))
                )
            });
        let (inf, fresh) = match wide {
            Some(i) => (i, false),
            None => self.get_or_start(client, gvr, kind, ns),
        };
        let wait = if fresh { wait } else { Duration::ZERO };
        // Timing out leaves an unsynced (empty) store, like a k9s lister.
        if let Ok(Err(_)) = tokio::time::timeout(wait, inf.store.wait_until_ready()).await {
            self.forget(gvr, ns);
            return None;
        }
        let mut out: Vec<Arc<DynamicObject>> = inf
            .store
            .state()
            .into_iter()
            .filter(|o| ns.is_empty() || o.metadata.namespace.as_deref() == Some(ns))
            .filter(|o| {
                selector.is_empty()
                    || selector.matches(&o.metadata.labels.clone().unwrap_or_default())
            })
            .collect();
        out.sort_by(|a, b| {
            (&a.metadata.namespace, &a.metadata.name)
                .cmp(&(&b.metadata.namespace, &b.metadata.name))
        });
        Some(out)
    }

    /// watch.Factory.Get: one object from an already synced informer of
    /// `gvr` in `ns` (or cluster wide). Outer `None`: no such informer, ask
    /// the API; inner `None`: the store does not have it.
    pub(super) fn get(
        &self,
        gvr: &Gvr,
        ns: &str,
        name: &str,
    ) -> Option<Option<Arc<DynamicObject>>> {
        let map = self.map.lock().expect("informers");
        let inf = [key(gvr, ns), key(gvr, "")]
            .iter()
            .filter_map(|k| map.get(k))
            .find(|i| {
                matches!(
                    futures::FutureExt::now_or_never(i.store.wait_until_ready()),
                    Some(Ok(()))
                )
            })?
            .clone();
        drop(map);
        Some(inf.store.state().into_iter().find(|o| {
            o.metadata.name.as_deref() == Some(name)
                && (ns.is_empty() || o.metadata.namespace.as_deref() == Some(ns))
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use bytes::Bytes;
    use http_body_util::{combinators::BoxBody, BodyExt, Full, StreamBody};
    use hyper::body::Frame;
    use serde_json::json;

    use super::*;

    type Body = BoxBody<Bytes, Infallible>;

    /// Fake API server: pod lists answer after `delay` with `status`; watches hang.
    fn client(delay: Duration, status: u16, lists: Arc<AtomicUsize>) -> kube::Client {
        let svc = tower::service_fn(move |req: http::Request<kube::client::Body>| {
            let lists = lists.clone();
            async move {
                let q = req.uri().query().unwrap_or_default().to_string();
                let path = req.uri().path().to_string();
                let body: Body = if q.contains("watch=true") {
                    let hang = futures::stream::pending::<Result<Frame<Bytes>, Infallible>>();
                    BodyExt::boxed(StreamBody::new(hang))
                } else {
                    lists.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(delay).await;
                    let pod = |ns: &str, n: &str, app: &str| {
                        json!({"metadata": {"namespace": ns, "name": n,
                               "labels": {"app": app}, "resourceVersion": "1",
                               "managedFields": [{"manager": "kubectl"}]}})
                    };
                    let items = if path.starts_with("/api/v1/namespaces/b/") {
                        vec![pod("b", "z", "web")]
                    } else {
                        vec![
                            pod("b", "z", "web"),
                            pod("a", "y", "db"),
                            pod("a", "x", "web"),
                        ]
                    };
                    let v = if status == 200 {
                        json!({"apiVersion": "v1", "kind": "PodList",
                               "metadata": {"resourceVersion": "1"}, "items": items})
                    } else {
                        json!({"kind": "Status", "apiVersion": "v1", "status": "Failure",
                               "reason": "Forbidden", "code": status, "message": "nope"})
                    };
                    Full::new(Bytes::from(v.to_string())).boxed()
                };
                let code = if q.contains("watch=true") {
                    200
                } else {
                    status
                };
                Ok::<_, Infallible>(
                    http::Response::builder()
                        .status(code)
                        .header("content-type", "application/json")
                        .body(body)
                        .unwrap(),
                )
            }
        });
        kube::Client::new(svc, "default")
    }

    fn pods() -> Gvr {
        Gvr::parse("v1/pods")
    }

    fn names(oo: &[Arc<DynamicObject>]) -> Vec<String> {
        oo.iter()
            .map(|o| {
                format!(
                    "{}/{}",
                    o.metadata.namespace.as_deref().unwrap_or_default(),
                    o.metadata.name.as_deref().unwrap_or_default()
                )
            })
            .collect()
    }

    #[tokio::test(start_paused = true)]
    async fn synced_store_is_sorted_typed_and_cached() {
        let lists = Arc::new(AtomicUsize::new(0));
        let c = client(Duration::ZERO, 200, lists.clone());
        let inf = Informers::default();
        let all = Selector::default();
        let oo = inf
            .list(&c, &pods(), "Pod", "", &all, SYNC_WAIT)
            .await
            .unwrap();
        assert_eq!(names(&oo), vec!["a/x", "a/y", "b/z"]);
        assert_eq!(oo[0].types.as_ref().unwrap().kind, "Pod");
        assert!(oo[0].metadata.managed_fields.is_none());
        let web = crate::model::labels::parse_selector("app=web").unwrap();
        let oo = inf
            .list(&c, &pods(), "Pod", "", &web, SYNC_WAIT)
            .await
            .unwrap();
        assert_eq!(names(&oo), vec!["a/x", "b/z"]);
        // The synced cluster wide informer serves namespaced lists too.
        let oo = inf
            .list(&c, &pods(), "Pod", "a", &all, SYNC_WAIT)
            .await
            .unwrap();
        assert_eq!(names(&oo), vec!["a/x", "a/y"]);
        assert_eq!(lists.load(Ordering::SeqCst), 1);
        // Refreshes share the store's objects instead of copying them.
        let again = inf
            .list(&c, &pods(), "Pod", "a", &all, SYNC_WAIT)
            .await
            .unwrap();
        assert!(Arc::ptr_eq(&oo[0], &again[0]));
        // Single gets come from the synced store too (watch.Factory.Get).
        let one = inf.get(&pods(), "a", "x").unwrap().unwrap();
        assert!(Arc::ptr_eq(&one, &oo[0]));
        assert!(inf.get(&pods(), "a", "nope").unwrap().is_none());
        assert!(inf.get(&Gvr::parse("v1/nodes"), "", "x").is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn slow_sync_serves_empty_store_then_data() {
        let lists = Arc::new(AtomicUsize::new(0));
        let c = client(Duration::from_secs(2), 200, lists.clone());
        let inf = Informers::default();
        let all = Selector::default();
        let t0 = tokio::time::Instant::now();
        let oo = inf.list(&c, &pods(), "Pod", "b", &all, SYNC_WAIT).await;
        assert_eq!(oo.map(|o| o.len()), Some(0));
        assert_eq!(t0.elapsed(), SYNC_WAIT);
        // Not fresh anymore: no second wait, still unsynced.
        let t1 = tokio::time::Instant::now();
        let oo = inf.list(&c, &pods(), "Pod", "b", &all, SYNC_WAIT).await;
        assert_eq!(oo.map(|o| o.len()), Some(0));
        assert_eq!(t1.elapsed(), Duration::ZERO);
        tokio::time::sleep(Duration::from_secs(2)).await;
        let oo = inf
            .list(&c, &pods(), "Pod", "b", &all, SYNC_WAIT)
            .await
            .unwrap();
        assert_eq!(names(&oo), vec!["b/z"]);
        assert_eq!(lists.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn failed_sync_falls_back_and_restarts() {
        let lists = Arc::new(AtomicUsize::new(0));
        let c = client(Duration::ZERO, 403, lists.clone());
        let inf = Informers::default();
        let all = Selector::default();
        assert!(inf
            .list(&c, &pods(), "Pod", "a", &all, SYNC_WAIT)
            .await
            .is_none());
        assert!(inf
            .list(&c, &pods(), "Pod", "a", &all, SYNC_WAIT)
            .await
            .is_none());
        assert_eq!(lists.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn clear_drops_informers() {
        let lists = Arc::new(AtomicUsize::new(0));
        let c = client(Duration::ZERO, 200, lists.clone());
        let inf = Informers::default();
        let all = Selector::default();
        inf.list(&c, &pods(), "Pod", "", &all, SYNC_WAIT).await;
        inf.clear();
        inf.list(&c, &pods(), "Pod", "", &all, SYNC_WAIT).await;
        assert_eq!(lists.load(Ordering::SeqCst), 2);
    }
}
