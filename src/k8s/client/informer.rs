//! Watch-backed list cache (k9s watch.Factory over client-go informers).
//!
//! The first list of a resource in a namespace starts a list+watch that
//! keeps a local store up to date. Later lists read the store, so views
//! open instantly and refreshes cost no API calls. Informers live until the
//! context changes, as k9s factories do.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use kube::api::{ApiResource, DynamicObject};
use kube::core::TypeMeta;
use kube::runtime::reflector::store::{Store, Writer};
use kube::runtime::{watcher, WatchStreamExt};
use kube::Api;

use crate::k8s::Gvr;
use crate::model::labels::Selector;

struct Informer {
    store: Store<DynamicObject>,
    types: TypeMeta,
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
    ) -> Arc<Informer> {
        let mut map = self.map.lock().expect("informers");
        if let Some(i) = map.get(&key(gvr, ns)) {
            return i.clone();
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
            let stream = watcher(api, watcher::Config::default())
                .default_backoff()
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
        let inf = Arc::new(Informer { store, types, task });
        map.insert(key(gvr, ns), inf.clone());
        inf
    }

    fn forget(&self, gvr: &Gvr, ns: &str) {
        self.map.lock().expect("informers").remove(&key(gvr, ns));
    }

    /// Cached objects of `gvr` in `ns`, sorted like an API list
    /// (namespace, name). `None` when the informer could not sync within
    /// `wait`; the caller then lists directly.
    pub(super) async fn list(
        &self,
        client: &kube::Client,
        gvr: &Gvr,
        kind: &str,
        ns: &str,
        selector: &Selector,
        wait: Duration,
    ) -> Option<Vec<DynamicObject>> {
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
        let inf = match wide {
            Some(i) => i,
            None => self.get_or_start(client, gvr, kind, ns),
        };
        match tokio::time::timeout(wait, inf.store.wait_until_ready()).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => {
                self.forget(gvr, ns);
                return None;
            }
            Err(_) => return None,
        }
        let mut out: Vec<DynamicObject> = inf
            .store
            .state()
            .into_iter()
            .filter(|o| ns.is_empty() || o.metadata.namespace.as_deref() == Some(ns))
            .filter(|o| {
                selector.is_empty()
                    || selector.matches(&o.metadata.labels.clone().unwrap_or_default())
            })
            .map(|o| {
                let mut o = (*o).clone();
                if o.types.is_none() {
                    o.types = Some(inf.types.clone());
                }
                o
            })
            .collect();
        out.sort_by(|a, b| {
            (&a.metadata.namespace, &a.metadata.name)
                .cmp(&(&b.metadata.namespace, &b.metadata.name))
        });
        Some(out)
    }
}
