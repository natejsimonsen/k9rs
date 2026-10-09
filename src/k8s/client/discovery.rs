//! API discovery (client-go ServerPreferredResources + k9s dao.loadPreferred).

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::Result;
use futures::StreamExt;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{APIGroup, APIResource, APIResourceList};

use crate::k8s::{Gvr, ResourceMeta};

/// k9s dao.stdGroups.
const STD_GROUPS: &[&str] = &[
    "apps/v1",
    "autoscaling/v1",
    "autoscaling/v2",
    "autoscaling/v2beta1",
    "autoscaling/v2beta2",
    "batch/v1",
    "batch/v1beta1",
    "extensions/v1beta1",
    "policy/v1beta1",
    "policy/v1",
    "v1",
];

/// k9s dao.scalableRes (RcGVR is `apps/v1/replicationcontrollers` upstream).
const SCALABLE: &[&str] = &[
    "apps/v1/deployments",
    "apps/v1/statefulsets",
    "apps/v1/replicasets",
    "apps/v1/replicationcontrollers",
];

/// k9s dao.deprecatedGVRs.
const DEPRECATED: &[&str] = &["v1/events", "extensions/v1beta1/ingresses"];

/// Group with its versions in server order and its preferred version.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Group {
    pub(crate) name: String,
    pub(crate) versions: Vec<String>,
    pub(crate) preferred: String,
}

impl Group {
    fn gv(&self, version: &str) -> String {
        if self.name.is_empty() {
            version.to_string()
        } else {
            format!("{}/{}", self.name, version)
        }
    }
}

/// Cached discovery results.
#[derive(Clone, Debug, Default)]
pub(crate) struct DiscoveryData {
    /// k9s registry metas (preferred versions only).
    pub(crate) preferred: Vec<ResourceMeta>,
    /// Every resource of every served version (no subresources), for lookups.
    pub(crate) all: HashMap<Gvr, ResourceMeta>,
    /// Eviction group/version advertised by `pods/eviction` in core v1.
    pub(crate) eviction_gv: Option<(String, String)>,
}

impl DiscoveryData {
    /// Metadata for a GVR, any version.
    pub(crate) fn lookup(&self, gvr: &Gvr) -> Option<&ResourceMeta> {
        let key = Gvr::new(gvr.g(), gvr.v(), gvr.r());
        self.all.get(&key)
    }
}

pub(crate) fn groups_from(core_versions: Vec<String>, groups: Vec<APIGroup>) -> Vec<Group> {
    let mut out = Vec::with_capacity(groups.len() + 1);
    if !core_versions.is_empty() {
        out.push(Group {
            name: String::new(),
            preferred: core_versions[0].clone(),
            versions: core_versions,
        });
    }
    for g in groups {
        let versions: Vec<String> = g.versions.iter().map(|v| v.version.clone()).collect();
        let preferred = g
            .preferred_version
            .map(|p| p.version)
            .or_else(|| versions.first().cloned())
            .unwrap_or_default();
        out.push(Group {
            name: g.name,
            versions,
            preferred,
        });
    }
    out
}

/// Fetch everything from the server. Individual group/version failures are
/// tolerated (client-go returns partial results), only /api and /apis are fatal.
pub(crate) async fn discover(client: &kube::Client) -> Result<DiscoveryData> {
    let core = client.list_core_api_versions().await.map_err(super::kerr)?;
    let groups = client.list_api_groups().await.map_err(super::kerr)?;
    let groups = groups_from(core.versions, groups.groups);

    let gvs: Vec<String> = groups
        .iter()
        .flat_map(|g| g.versions.iter().map(move |v| g.gv(v)))
        .collect();
    let lists: Vec<(String, Option<APIResourceList>)> = futures::stream::iter(gvs)
        .map(|gv| async move {
            let res = if gv.contains('/') {
                client.list_api_group_resources(&gv).await
            } else {
                client.list_core_api_resources(&gv).await
            };
            match res {
                Ok(l) => (gv, Some(l)),
                Err(e) => {
                    tracing::debug!("discovery of {gv} failed: {e}");
                    (gv, None)
                }
            }
        })
        .buffer_unordered(16)
        .collect()
        .await;
    let lists: HashMap<String, APIResourceList> = lists
        .into_iter()
        .filter_map(|(gv, l)| l.map(|l| (gv, l)))
        .collect();
    Ok(build(&groups, &lists))
}

/// Assemble discovery data from raw group and resource lists.
pub(crate) fn build(groups: &[Group], lists: &HashMap<String, APIResourceList>) -> DiscoveryData {
    let mut data = DiscoveryData::default();
    // ServerPreferredResources selection.
    let mut selected: BTreeMap<(String, String), (String, APIResource)> = BTreeMap::new();
    for g in groups {
        for v in &g.versions {
            let Some(list) = lists.get(&g.gv(v)) else {
                continue;
            };
            for r in &list.resources {
                if g.name.is_empty()
                    && v == "v1"
                    && r.name == "pods/eviction"
                    && r.kind == "Eviction"
                {
                    if let (Some(gr), Some(ver)) = (&r.group, &r.version) {
                        if !gr.is_empty() && !ver.is_empty() {
                            data.eviction_gv = Some((gr.clone(), ver.clone()));
                        }
                    }
                }
                if r.name.contains('/') {
                    continue;
                }
                let gvr = Gvr::new(&g.name, v, &r.name);
                data.all.insert(gvr, to_meta(&g.gv(v), &g.name, v, r));
                let key = (g.name.clone(), r.name.clone());
                if selected.contains_key(&key) && *v != g.preferred {
                    continue;
                }
                selected.insert(key, (v.clone(), r.clone()));
            }
        }
    }
    for ((group, _), (version, res)) in selected {
        let gvr = Gvr::new(&group, &version, &res.name);
        if is_deprecated(&gvr) {
            continue;
        }
        let gv = if group.is_empty() {
            version.clone()
        } else {
            format!("{group}/{version}")
        };
        data.preferred.push(to_meta(&gv, &group, &version, &res));
    }
    data.preferred.sort_by_key(|m| m.gvr.to_string());
    data
}

fn is_deprecated(gvr: &Gvr) -> bool {
    gvr.v().is_empty() || DEPRECATED.contains(&gvr.to_string().as_str())
}

fn is_standard_group(gv: &str) -> bool {
    STD_GROUPS.contains(&gv) || gv.contains(".k8s.io")
}

/// k9s loadPreferred massaging of an APIResource.
fn to_meta(gv: &str, group: &str, version: &str, r: &APIResource) -> ResourceMeta {
    let gvr = Gvr::new(group, version, &r.name);
    let mut categories = r.categories.clone().unwrap_or_default();
    if !is_standard_group(gv) {
        categories.push("crd".into());
    }
    if SCALABLE.contains(&gvr.to_string().as_str()) {
        categories.push("scale".into());
    }
    let singular_name = if r.singular_name.is_empty() {
        r.kind.to_lowercase()
    } else {
        r.singular_name.clone()
    };
    let mut seen = HashSet::new();
    let verbs = r
        .verbs
        .iter()
        .filter(|v| seen.insert(v.as_str()))
        .cloned()
        .collect();
    ResourceMeta {
        gvr,
        kind: r.kind.clone(),
        singular_name,
        short_names: r.short_names.clone().unwrap_or_default(),
        namespaced: r.namespaced,
        verbs,
        categories,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn list(gv: &str, res: serde_json::Value) -> APIResourceList {
        serde_json::from_value(json!({"groupVersion": gv, "resources": res})).unwrap()
    }

    fn fixture() -> (Vec<Group>, HashMap<String, APIResourceList>) {
        let groups: Vec<APIGroup> = serde_json::from_value(json!([
            {"name": "apps", "versions": [{"groupVersion": "apps/v1", "version": "v1"}],
             "preferredVersion": {"groupVersion": "apps/v1", "version": "v1"}},
            {"name": "autoscaling", "versions": [
                {"groupVersion": "autoscaling/v2", "version": "v2"},
                {"groupVersion": "autoscaling/v1", "version": "v1"}],
             "preferredVersion": {"groupVersion": "autoscaling/v2", "version": "v2"}},
            {"name": "fred.io", "versions": [
                {"groupVersion": "fred.io/v1beta1", "version": "v1beta1"},
                {"groupVersion": "fred.io/v1", "version": "v1"}],
             "preferredVersion": {"groupVersion": "fred.io/v1", "version": "v1"}},
            {"name": "events.k8s.io", "versions": [{"groupVersion": "events.k8s.io/v1", "version": "v1"}],
             "preferredVersion": {"groupVersion": "events.k8s.io/v1", "version": "v1"}}
        ]))
        .unwrap();
        let groups = groups_from(vec!["v1".into()], groups);
        let mut lists = HashMap::new();
        lists.insert(
            "v1".into(),
            list(
                "v1",
                json!([
                    {"name": "pods", "singularName": "pod", "namespaced": true, "kind": "Pod",
                     "verbs": ["get", "list", "watch", "delete"], "shortNames": ["po"], "categories": ["all"]},
                    {"name": "pods/log", "singularName": "", "namespaced": true, "kind": "Pod", "verbs": ["get"]},
                    {"name": "pods/eviction", "singularName": "", "namespaced": true, "kind": "Eviction",
                     "group": "policy", "version": "v1", "verbs": ["create"]},
                    {"name": "nodes", "singularName": "", "namespaced": false, "kind": "Node",
                     "verbs": ["get", "list"], "shortNames": ["no"]},
                    {"name": "events", "singularName": "event", "namespaced": true, "kind": "Event",
                     "verbs": ["get", "list"], "shortNames": ["ev"]},
                    {"name": "replicationcontrollers", "singularName": "replicationcontroller", "namespaced": true,
                     "kind": "ReplicationController", "verbs": ["get"], "shortNames": ["rc"]}
                ]),
            ),
        );
        lists.insert(
            "apps/v1".into(),
            list(
                "apps/v1",
                json!([
                    {"name": "deployments", "singularName": "deployment", "namespaced": true, "kind": "Deployment",
                     "verbs": ["get", "patch"], "shortNames": ["deploy"]},
                    {"name": "deployments/scale", "singularName": "", "namespaced": true, "kind": "Scale", "verbs": ["get"]}
                ]),
            ),
        );
        lists.insert(
            "autoscaling/v2".into(),
            list(
                "autoscaling/v2",
                json!([{"name": "horizontalpodautoscalers", "singularName": "horizontalpodautoscaler",
                        "namespaced": true, "kind": "HorizontalPodAutoscaler", "verbs": ["get"], "shortNames": ["hpa"]}]),
            ),
        );
        lists.insert(
            "autoscaling/v1".into(),
            list(
                "autoscaling/v1",
                json!([{"name": "horizontalpodautoscalers", "singularName": "horizontalpodautoscaler",
                        "namespaced": true, "kind": "HorizontalPodAutoscaler", "verbs": ["get"], "shortNames": ["hpa"]}]),
            ),
        );
        lists.insert(
            "fred.io/v1beta1".into(),
            list(
                "fred.io/v1beta1",
                json!([
                    {"name": "blees", "singularName": "", "namespaced": false, "kind": "Blee", "verbs": ["get"]},
                    {"name": "zorgs", "singularName": "zorg", "namespaced": true, "kind": "Zorg", "verbs": ["get"]}
                ]),
            ),
        );
        lists.insert(
            "fred.io/v1".into(),
            list(
                "fred.io/v1",
                json!([{"name": "zorgs", "singularName": "zorg", "namespaced": true, "kind": "Zorg", "verbs": ["get", "get"]}]),
            ),
        );
        lists.insert(
            "events.k8s.io/v1".into(),
            list(
                "events.k8s.io/v1",
                json!([{"name": "events", "singularName": "event", "namespaced": true, "kind": "Event", "verbs": ["get"], "shortNames": ["ev"]}]),
            ),
        );
        (groups, lists)
    }

    #[test]
    fn preferred_versions_and_k9s_massaging() {
        let (groups, lists) = fixture();
        let d = build(&groups, &lists);
        let names: Vec<String> = d.preferred.iter().map(|m| m.gvr.to_string()).collect();
        assert_eq!(
            names,
            vec![
                "apps/v1/deployments",
                "autoscaling/v2/horizontalpodautoscalers",
                "events.k8s.io/v1/events",
                "fred.io/v1/zorgs",
                "fred.io/v1beta1/blees",
                "v1/nodes",
                "v1/pods",
                "v1/replicationcontrollers",
            ]
        );
        let get = |s: &str| d.preferred.iter().find(|m| m.gvr.to_string() == s).unwrap();
        let po = get("v1/pods");
        assert_eq!(po.kind, "Pod");
        assert_eq!(po.short_names, vec!["po"]);
        assert_eq!(po.categories, vec!["all"]);
        assert!(po.namespaced);
        assert!(po.can("delete"));
        // singular name defaults to lowercased kind.
        assert_eq!(get("v1/nodes").singular_name, "node");
        assert!(!get("v1/nodes").namespaced);
        // scale category for deployments; crd category for non std groups.
        assert_eq!(get("apps/v1/deployments").categories, vec!["scale"]);
        assert_eq!(get("fred.io/v1/zorgs").categories, vec!["crd"]);
        assert_eq!(get("fred.io/v1/zorgs").verbs, vec!["get"]);
        assert_eq!(get("fred.io/v1beta1/blees").singular_name, "blee");
        // .k8s.io groups are standard.
        assert!(get("events.k8s.io/v1/events").categories.is_empty());
        // v1 rc is not scalable in k9s (it tracks apps/v1/replicationcontrollers).
        assert!(get("v1/replicationcontrollers").categories.is_empty());
        // all versions are retained for lookups, including deprecated ones.
        assert!(d.lookup(&Gvr::parse("v1/events")).is_some());
        assert!(d
            .lookup(&Gvr::parse("autoscaling/v1/horizontalpodautoscalers"))
            .is_some());
        assert!(d.lookup(&Gvr::parse("fred.io/v1beta1/zorgs")).is_some());
        assert!(d.lookup(&Gvr::parse("v1/pods:log")).is_some());
        assert!(d.lookup(&Gvr::parse("v1/pods/log")).is_none());
        assert_eq!(d.eviction_gv, Some(("policy".into(), "v1".into())));
    }

    #[test]
    fn missing_lists_are_skipped() {
        let (groups, mut lists) = fixture();
        lists.remove("apps/v1");
        let d = build(&groups, &lists);
        assert!(!d.preferred.iter().any(|m| m.gvr.g() == "apps"));
    }

    #[test]
    fn core_group_first_and_preferred_fallback() {
        let groups: Vec<APIGroup> = serde_json::from_value(json!([
            {"name": "x.io", "versions": [{"groupVersion": "x.io/v2", "version": "v2"}]}
        ]))
        .unwrap();
        let g = groups_from(vec!["v1".into()], groups);
        assert_eq!(g[0].name, "");
        assert_eq!(g[0].preferred, "v1");
        assert_eq!(g[1].preferred, "v2");
    }
}
