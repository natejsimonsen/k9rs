//! Extra lookups kubectl's describers make besides the described object
//! (pods for controllers, endpoint slices for services, ...). A failed
//! lookup leaves its [`Related`] field unset; it never fails the describe.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{anyhow, Result};
use kube::api::{DynamicObject, ListParams};
use kube::core::Request;
use serde_json::Value;

use super::{collection_path, list_items, reqerr, KubeAccessor};
use crate::k8s::describe::Related;
use crate::k8s::gvr::known;
use crate::k8s::Gvr;

/// kubectl's default describe chunk size.
const CHUNK_SIZE: u32 = 500;
/// discoveryv1.LabelServiceName.
const SERVICE_NAME_LABEL: &str = "kubernetes.io/service-name";
/// corev1.NamespaceNodeLease.
const NODE_LEASE_NS: &str = "kube-node-lease";

fn leases() -> Gvr {
    Gvr::new("coordination.k8s.io", "v1", "leases")
}

/// labels.SelectorFromSet(m).String(): sorted `k=v` pairs.
fn set_selector(m: &BTreeMap<String, String>) -> String {
    m.iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// metav1.LabelSelectorAsSelector(sel).String(). Nil and empty selectors
/// render as "" (list everything; the describers filter again).
fn label_selector(sel: &Value) -> Result<String> {
    if sel.is_null() {
        return Ok(String::new());
    }
    // (key, rendered requirement); stable sort by key like Go's ByKey.
    let mut reqs: Vec<(String, String)> = Vec::new();
    if let Some(m) = sel.get("matchLabels").and_then(Value::as_object) {
        for (k, v) in m {
            reqs.push((k.clone(), format!("{k}={}", v.as_str().unwrap_or_default())));
        }
    }
    for e in sel
        .get("matchExpressions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let key = e.get("key").and_then(Value::as_str).unwrap_or_default();
        let mut vals: Vec<&str> = e
            .get("values")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        vals.sort_unstable();
        let vals = vals.join(",");
        let r = match e
            .get("operator")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "In" => format!("{key} in ({vals})"),
            "NotIn" => format!("{key} notin ({vals})"),
            "Exists" => key.to_string(),
            "DoesNotExist" => format!("!{key}"),
            op => return Err(anyhow!("{op:?} is not a valid label selector operator")),
        };
        reqs.push((key.to_string(), r));
    }
    reqs.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(reqs
        .into_iter()
        .map(|(_, r)| r)
        .collect::<Vec<_>>()
        .join(","))
}

/// Service names referenced by an Ingress's default backend and rules.
fn ingress_services(ing: &Value) -> BTreeSet<String> {
    let spec = &ing["spec"];
    let mut backends = vec![&spec["defaultBackend"]];
    for rule in spec["rules"].as_array().into_iter().flatten() {
        for path in rule["http"]["paths"].as_array().into_iter().flatten() {
            backends.push(&path["backend"]);
        }
    }
    backends
        .into_iter()
        .filter_map(|b| b["service"]["name"].as_str())
        .filter(|n| !n.is_empty())
        .map(str::to_string)
        .collect()
}

/// Log a failed lookup and treat it as absent.
fn absent<T>(what: &str, r: Result<T>) -> Option<T> {
    match r {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::debug!("describe {what} lookup failed: {e}");
            None
        }
    }
}

impl KubeAccessor {
    /// List `gvr` in `ns` ("" for all namespaces) in chunks, following
    /// continue tokens like kubectl's FollowContinue.
    async fn list_chunked(
        &self,
        gvr: &Gvr,
        ns: &str,
        labels: &str,
        fields: &str,
    ) -> Result<Vec<DynamicObject>> {
        let mut lp = ListParams::default().limit(CHUNK_SIZE);
        if !labels.is_empty() {
            lp = lp.labels(labels);
        }
        if !fields.is_empty() {
            lp = lp.fields(fields);
        }
        let mut out = Vec::new();
        loop {
            let req = Request::new(collection_path(gvr, ns))
                .list(&lp)
                .map_err(reqerr)?;
            let page = self.send_json(req).await?;
            let next = page["metadata"]["continue"]
                .as_str()
                .filter(|c| !c.is_empty())
                .map(str::to_string);
            out.extend(list_items(page)?);
            match next {
                Some(c) => lp.continue_token = Some(c),
                None => return Ok(out),
            }
        }
    }

    async fn get_in(&self, gvr: &Gvr, ns: &str, name: &str) -> Result<DynamicObject> {
        let req = Request::new(collection_path(gvr, ns))
            .get(name, &Default::default())
            .map_err(reqerr)?;
        let v = self.send_json(req).await?;
        serde_json::from_value(v).map_err(|e| anyhow!("unable to decode object: {e}"))
    }

    /// Pods of a controller: listed by its selector in its namespace.
    async fn controller_pods(&self, ns: &str, sel: Result<String>) -> Option<Vec<DynamicObject>> {
        let sel = absent("selector", sel)?;
        absent(
            "pods",
            self.list_chunked(&known::pods(), ns, &sel, "").await,
        )
    }

    /// The related objects kubectl's describer for `gvr` looks up.
    pub(super) async fn related(&self, gvr: &Gvr, obj: &DynamicObject) -> Result<Related> {
        let v = serde_json::to_value(obj).map_err(|e| anyhow!("serializing object: {e}"))?;
        let name = obj.metadata.name.clone().unwrap_or_default();
        let ns = obj.metadata.namespace.clone().unwrap_or_default();
        let mut rel = Related::default();
        match (gvr.g(), gvr.r()) {
            ("", "nodes") => {
                let fields =
                    format!("spec.nodeName={name},status.phase!=Succeeded,status.phase!=Failed");
                rel.pods = absent(
                    "node pods",
                    self.list_chunked(&known::pods(), "", "", &fields).await,
                );
                rel.lease = absent(
                    "node lease",
                    self.get_in(&leases(), NODE_LEASE_NS, &name).await,
                );
            }
            ("", "replicationcontrollers") => {
                let sel: BTreeMap<String, String> =
                    serde_json::from_value(v["spec"]["selector"].clone()).unwrap_or_default();
                rel.pods = self.controller_pods(&ns, Ok(set_selector(&sel))).await;
            }
            ("apps", "replicasets") | ("apps", "statefulsets") | ("apps", "daemonsets") => {
                let sel = label_selector(&v["spec"]["selector"]);
                rel.pods = self.controller_pods(&ns, sel).await;
            }
            ("apps", "deployments") => {
                if let Some(sel) = absent("selector", label_selector(&v["spec"]["selector"])) {
                    rel.replica_sets = absent(
                        "replicasets",
                        self.list_chunked(&known::replicasets(), &ns, &sel, "")
                            .await,
                    );
                }
            }
            ("", "persistentvolumeclaims") => {
                rel.pods = absent("pods", self.list_chunked(&known::pods(), &ns, "", "").await);
            }
            ("", "services") => {
                let sel = format!("{SERVICE_NAME_LABEL}={name}");
                rel.endpoint_slices = absent(
                    "endpointslices",
                    self.list_chunked(&known::endpointslices(), &ns, &sel, "")
                        .await,
                );
            }
            ("networking.k8s.io", "ingresses") => {
                let mut slices = Vec::new();
                let mut services = Vec::new();
                for svc in ingress_services(&v) {
                    let sel = format!("{SERVICE_NAME_LABEL}={svc}");
                    if let Some(l) = absent(
                        "endpointslices",
                        self.list_chunked(&known::endpointslices(), &ns, &sel, "")
                            .await,
                    ) {
                        slices.extend(l);
                    }
                    if let Some(s) =
                        absent("service", self.get_in(&known::services(), &ns, &svc).await)
                    {
                        services.push(s);
                    }
                }
                rel.endpoint_slices = Some(slices);
                rel.services = Some(services);
            }
            ("", "namespaces") => {
                rel.resource_quotas = absent(
                    "resourcequotas",
                    self.list_chunked(&Gvr::new("", "v1", "resourcequotas"), &name, "", "")
                        .await,
                );
                rel.limit_ranges = absent(
                    "limitranges",
                    self.list_chunked(&Gvr::new("", "v1", "limitranges"), &name, "", "")
                        .await,
                );
            }
            ("", "serviceaccounts") => {
                rel.secrets = absent(
                    "secrets",
                    self.list_chunked(&known::secrets(), &ns, "", "").await,
                );
            }
            _ => {}
        }
        Ok(rel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn selectors() {
        assert_eq!(label_selector(&Value::Null).unwrap(), "");
        assert_eq!(label_selector(&json!({})).unwrap(), "");
        let sel = json!({
            "matchLabels": {"b": "2", "a": "1"},
            "matchExpressions": [
                {"key": "z", "operator": "In", "values": ["y", "x"]},
                {"key": "c", "operator": "NotIn", "values": ["q"]},
                {"key": "d", "operator": "Exists"},
                {"key": "e", "operator": "DoesNotExist"}
            ]
        });
        assert_eq!(
            label_selector(&sel).unwrap(),
            "a=1,b=2,c notin (q),d,!e,z in (x,y)"
        );
        assert!(
            label_selector(&json!({"matchExpressions": [{"key": "a", "operator": "Bad"}]}))
                .is_err()
        );
        let m: BTreeMap<String, String> =
            [("b".to_string(), "2".to_string()), ("a".into(), "1".into())].into();
        assert_eq!(set_selector(&m), "a=1,b=2");
    }

    #[test]
    fn ingress_backend_services() {
        let ing = json!({"spec": {
            "defaultBackend": {"service": {"name": "def", "port": {"number": 80}}},
            "rules": [
                {"http": {"paths": [
                    {"backend": {"service": {"name": "web"}}},
                    {"backend": {"resource": {"kind": "Bucket", "name": "b"}}},
                    {"backend": {"service": {"name": "web"}}}
                ]}},
                {"host": "no-http"}
            ]
        }});
        let got: Vec<String> = ingress_services(&ing).into_iter().collect();
        assert_eq!(got, vec!["def", "web"]);
    }
}
