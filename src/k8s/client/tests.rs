//! Request-level tests against a hand-rolled tower service standing in for
//! the API server.

use std::convert::Infallible;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use bytes::Bytes;
use futures::StreamExt;
use http_body_util::{BodyExt, Full};
use kube::config::Kubeconfig;
use pretty_assertions::assert_eq;
use serde_json::{json, Value};

use super::config::LoadedConfig;
use super::*;

/// A recorded request.
#[derive(Clone, Debug)]
struct Req {
    method: String,
    path: String,
    query: Vec<(String, String)>,
    headers: http::HeaderMap,
    body: String,
}

impl Req {
    fn q(&self, k: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(kk, _)| kk == k)
            .map(|(_, v)| v.as_str())
    }
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }
    fn header(&self, k: &str) -> &str {
        self.headers
            .get(k)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
    }
    fn is(&self, method: &str, path: &str) -> bool {
        self.method == method && self.path == path
    }
}

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                let hex = std::str::from_utf8(&b[i + 1..i + 3]).unwrap();
                out.push(u8::from_str_radix(hex, 16).unwrap());
                i += 2;
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8(out).unwrap()
}

type Reply = (u16, String);
type Handler = Arc<dyn Fn(&Req) -> Reply + Send + Sync>;

#[derive(Clone)]
struct Mock {
    handler: Handler,
    log: Arc<Mutex<Vec<Req>>>,
}

impl tower::Service<http::Request<kube::client::Body>> for Mock {
    type Response = http::Response<Full<Bytes>>;
    type Error = Infallible;
    type Future =
        Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Infallible>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: http::Request<kube::client::Body>) -> Self::Future {
        let me = self.clone();
        Box::pin(async move {
            let (parts, body) = req.into_parts();
            let body = body.collect().await.unwrap().to_bytes();
            let query = parts
                .uri
                .query()
                .unwrap_or_default()
                .split('&')
                .filter(|kv| !kv.is_empty())
                .map(|kv| {
                    let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
                    (pct_decode(k), pct_decode(v))
                })
                .collect();
            let r = Req {
                method: parts.method.to_string(),
                path: parts.uri.path().to_string(),
                query,
                headers: parts.headers,
                body: String::from_utf8_lossy(&body).into_owned(),
            };
            let (code, body) = (me.handler)(&r);
            me.log.lock().unwrap().push(r);
            Ok(http::Response::builder()
                .status(code)
                .header("content-type", "application/json")
                .body(Full::new(Bytes::from(body)))
                .unwrap())
        })
    }
}

fn ok(v: Value) -> Reply {
    (200, v.to_string())
}

fn status(code: u16, reason: &str, msg: &str) -> Reply {
    (
        code,
        json!({"kind": "Status", "apiVersion": "v1", "status": "Failure",
               "message": msg, "reason": reason, "code": code})
        .to_string(),
    )
}

fn not_found(msg: &str) -> Reply {
    status(404, "NotFound", msg)
}

const KC: &str = r#"
apiVersion: v1
kind: Config
current-context: ctx-a
clusters:
- name: cl-a
  cluster:
    server: https://127.0.0.1:1
- name: cl-b
  cluster:
    server: https://127.0.0.1:2
users:
- name: u-a
  user:
    token: t
contexts:
- name: ctx-a
  context:
    cluster: cl-a
    user: u-a
    namespace: team-a
- name: ctx-b
  context:
    cluster: cl-b
    user: u-a
    namespace: team-b
"#;

/// Discovery, metrics probe and allow-all SAR answers.
fn base(r: &Req) -> Reply {
    match (r.method.as_str(), r.path.as_str()) {
        ("GET", "/api") => {
            ok(json!({"kind": "APIVersions", "versions": ["v1"], "serverAddressByClientCIDRs": []}))
        }
        ("GET", "/apis") => ok(
            json!({"kind": "APIGroupList", "apiVersion": "v1", "groups": [
                {"name": "apps", "versions": [{"groupVersion": "apps/v1", "version": "v1"}],
                 "preferredVersion": {"groupVersion": "apps/v1", "version": "v1"}},
                {"name": "batch", "versions": [{"groupVersion": "batch/v1", "version": "v1"}],
                 "preferredVersion": {"groupVersion": "batch/v1", "version": "v1"}},
                {"name": "metrics.k8s.io", "versions": [{"groupVersion": "metrics.k8s.io/v1beta1", "version": "v1beta1"}],
                 "preferredVersion": {"groupVersion": "metrics.k8s.io/v1beta1", "version": "v1beta1"}}
            ]}),
        ),
        ("GET", "/api/v1") => ok(
            json!({"kind": "APIResourceList", "groupVersion": "v1", "resources": [
                {"name": "pods", "singularName": "pod", "namespaced": true, "kind": "Pod", "verbs": ["get", "list"], "shortNames": ["po"]},
                {"name": "pods/eviction", "singularName": "", "namespaced": true, "group": "policy", "version": "v1", "kind": "Eviction", "verbs": ["create"]},
                {"name": "nodes", "singularName": "node", "namespaced": false, "kind": "Node", "verbs": ["get", "list"], "shortNames": ["no"]},
                {"name": "namespaces", "singularName": "namespace", "namespaced": false, "kind": "Namespace", "verbs": ["get", "list"], "shortNames": ["ns"]},
                {"name": "events", "singularName": "event", "namespaced": true, "kind": "Event", "verbs": ["list"]}
            ]}),
        ),
        ("GET", "/apis/apps/v1") => ok(
            json!({"kind": "APIResourceList", "groupVersion": "apps/v1", "resources": [
                {"name": "deployments", "singularName": "deployment", "namespaced": true, "kind": "Deployment", "verbs": ["get", "patch"], "shortNames": ["deploy"]},
                {"name": "deployments/scale", "singularName": "", "namespaced": true, "kind": "Scale", "verbs": ["get"]},
                {"name": "daemonsets", "singularName": "daemonset", "namespaced": true, "kind": "DaemonSet", "verbs": ["get"]},
                {"name": "statefulsets", "singularName": "statefulset", "namespaced": true, "kind": "StatefulSet", "verbs": ["get"]}
            ]}),
        ),
        ("GET", "/apis/batch/v1") => ok(
            json!({"kind": "APIResourceList", "groupVersion": "batch/v1", "resources": [
                {"name": "jobs", "singularName": "job", "namespaced": true, "kind": "Job", "verbs": ["get"]},
                {"name": "cronjobs", "singularName": "cronjob", "namespaced": true, "kind": "CronJob", "verbs": ["get"], "shortNames": ["cj"]}
            ]}),
        ),
        ("GET", "/apis/metrics.k8s.io/v1beta1") => ok(
            json!({"kind": "APIResourceList", "groupVersion": "metrics.k8s.io/v1beta1", "resources": [
                {"name": "pods", "singularName": "", "namespaced": true, "kind": "PodMetrics", "verbs": ["get", "list"]},
                {"name": "nodes", "singularName": "", "namespaced": false, "kind": "NodeMetrics", "verbs": ["get", "list"]}
            ]}),
        ),
        ("POST", "/apis/authorization.k8s.io/v1/selfsubjectaccessreviews") => {
            let mut v = r.json();
            v["status"] = json!({"allowed": true});
            (201, v.to_string())
        }
        _ => not_found(&format!(
            "the server could not find the requested resource ({} {})",
            r.method, r.path
        )),
    }
}

struct Harness {
    acc: KubeAccessor,
    log: Arc<Mutex<Vec<Req>>>,
}

impl Harness {
    fn new(handler: impl Fn(&Req) -> Reply + Send + Sync + 'static) -> Harness {
        Self::with_opts(handler, ConnectOptions::default())
    }

    fn with_opts(
        handler: impl Fn(&Req) -> Reply + Send + Sync + 'static,
        opts: ConnectOptions,
    ) -> Harness {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mock = Mock {
            handler: Arc::new(handler),
            log: log.clone(),
        };
        let client = kube::Client::new(mock, "default");
        let cfg = LoadedConfig::from_kubeconfig(Kubeconfig::from_yaml(KC).unwrap(), opts);
        let acc = KubeAccessor::from_conn(Conn {
            clients: Ok((client.clone(), client)),
            build_failed: false,
            cfg,
            server: "https://k8s.test".into(),
            override_file: None,
        });
        Harness { acc, log }
    }

    /// Requests minus discovery and metrics probing.
    fn calls(&self) -> Vec<Req> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|r| {
                !(r.method == "GET"
                    && (r.path == "/api"
                        || r.path == "/apis"
                        || r.path == "/api/v1"
                        || (r.path.starts_with("/apis/") && r.path.matches('/').count() == 3)))
            })
            .cloned()
            .collect()
    }

    fn sars(&self) -> Vec<Value> {
        self.calls()
            .iter()
            .filter(|r| r.path.ends_with("/selfsubjectaccessreviews"))
            .map(|r| r.json()["spec"]["resourceAttributes"].clone())
            .collect()
    }

    fn non_sar(&self) -> Vec<Req> {
        self.calls()
            .into_iter()
            .filter(|r| !r.path.ends_with("/selfsubjectaccessreviews"))
            .collect()
    }
}

fn deny_all(r: &Req) -> Reply {
    if r.path.ends_with("/selfsubjectaccessreviews") {
        let mut v = r.json();
        v["status"] = json!({"allowed": false});
        return (201, v.to_string());
    }
    base(r)
}

#[tokio::test]
async fn resources_from_discovery() {
    let h = Harness::new(base);
    let rr = h.acc.resources().await.unwrap();
    let names: Vec<String> = rr.iter().map(|m| m.gvr.to_string()).collect();
    assert!(names.contains(&"v1/pods".to_string()));
    assert!(names.contains(&"apps/v1/deployments".to_string()));
    // v1/events is deprecated in k9s and hidden from the registry.
    assert!(!names.contains(&"v1/events".to_string()));
    let dp = rr
        .iter()
        .find(|m| m.gvr.to_string() == "apps/v1/deployments")
        .unwrap();
    assert_eq!(dp.short_names, vec!["deploy"]);
    assert_eq!(dp.categories, vec!["scale"]);
    // Cached: a second call issues no new requests.
    let before = h.log.lock().unwrap().len();
    h.acc.resources().await.unwrap();
    assert_eq!(h.log.lock().unwrap().len(), before);
    let meta = h.acc.resource_meta(&Gvr::parse("v1/events")).await.unwrap();
    assert_eq!(meta.unwrap().kind, "Event");
}

/// k9s CachedDiscovery: discovery lands in the client-go disk cache and a
/// fresh cache serves the next process without discovery requests.
#[tokio::test]
async fn discovery_disk_cache_shared_across_runs() {
    let dir = tempfile::tempdir().unwrap();
    let disc = |h: &Harness| {
        h.log
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.path == "/api" || r.path.starts_with("/apis") || r.path == "/api/v1")
            .count()
    };
    let mut h = Harness::new(base);
    h.acc.disk_cache = Some(dir.path().to_path_buf());
    let first = h.acc.resources().await.unwrap();
    assert!(disc(&h) > 0);
    let host = dir.path().join("discovery").join("127.0.0.1_1");
    let groups: Value =
        serde_json::from_slice(&std::fs::read(host.join("servergroups.json")).unwrap()).unwrap();
    assert_eq!(groups["kind"], "APIGroupList");
    assert_eq!(groups["groups"][0]["name"], "");
    let core: Value =
        serde_json::from_slice(&std::fs::read(host.join("v1/serverresources.json")).unwrap())
            .unwrap();
    assert_eq!(core["kind"], "APIResourceList");
    assert!(host.join("apps/v1/serverresources.json").exists());

    let mut h2 = Harness::new(base);
    h2.acc.disk_cache = Some(dir.path().to_path_buf());
    let second = h2.acc.resources().await.unwrap();
    assert_eq!(disc(&h2), 0);
    assert_eq!(first, second);
}

#[tokio::test]
async fn list_namespaced_all_and_cluster_scoped() {
    let h = Harness::new(|r| match r.path.as_str() {
        "/api/v1/namespaces/default/pods" | "/api/v1/pods" => ok(json!({
            "kind": "PodList", "apiVersion": "v1", "metadata": {},
            "items": [{"metadata": {"name": "p1", "namespace": "default"}}]
        })),
        "/api/v1/nodes" => ok(
            json!({"kind": "NodeList", "apiVersion": "v1", "items": [{"metadata": {"name": "n1"}}]}),
        ),
        _ => base(r),
    });
    let pods = known::pods();
    let oo = h
        .acc
        .list(&pods, "default", Some("app=nginx,tier!=db"))
        .await
        .unwrap();
    assert_eq!(oo.len(), 1);
    assert_eq!(oo[0].metadata.name.as_deref(), Some("p1"));
    let tm = oo[0].types.clone().unwrap();
    assert_eq!((tm.api_version.as_str(), tm.kind.as_str()), ("v1", "Pod"));
    h.acc.list(&pods, "all", None).await.unwrap();
    h.acc.list(&pods, "", None).await.unwrap();
    h.acc.list(&known::nodes(), "default", None).await.unwrap();
    let calls = h.non_sar();
    assert_eq!(calls[0].path, "/api/v1/namespaces/default/pods");
    assert_eq!(calls[0].q("labelSelector"), Some("app=nginx,tier!=db"));
    assert_eq!(calls[1].path, "/api/v1/pods");
    assert_eq!(calls[1].q("labelSelector"), None);
    assert_eq!(calls[2].path, "/api/v1/pods");
    assert_eq!(calls[3].path, "/api/v1/nodes");
}

#[tokio::test]
async fn list_errors_carry_server_message() {
    let h = Harness::new(|r| {
        if r.path == "/apis/fred.io/v1/namespaces/default/zorgs" {
            return status(
                403,
                "Forbidden",
                "zorgs.fred.io is forbidden: User \"x\" cannot list resource",
            );
        }
        base(r)
    });
    let err = h
        .acc
        .list(&Gvr::parse("fred.io/v1/zorgs"), "default", None)
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "zorgs.fred.io is forbidden: User \"x\" cannot list resource"
    );
}

#[tokio::test]
async fn list_table_requests_metadata_tables() {
    let h = Harness::new(|r| match r.path.as_str() {
        "/apis/apps/v1/namespaces/ns1/deployments" => ok(json!({
            "kind": "Table", "apiVersion": "meta.k8s.io/v1",
            "columnDefinitions": [{"name": "Name", "type": "string", "format": "name", "description": "", "priority": 0}],
            "rows": [{"cells": ["d1"], "object": {"metadata": {"name": "d1", "namespace": "ns1"}}}]
        })),
        "/api/v1/pods" => ok(json!({"kind": "PodList", "apiVersion": "v1", "items": []})),
        _ => base(r),
    });
    let t = h
        .acc
        .list_table(&known::deployments(), "ns1", Some("a=b"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(t.rows[0].name, "d1");
    assert_eq!(t.rows[0].namespace, "ns1");
    let c = &h.non_sar()[0];
    assert_eq!(c.path, "/apis/apps/v1/namespaces/ns1/deployments");
    assert_eq!(c.q("includeObject"), Some("Metadata"));
    assert_eq!(c.q("labelSelector"), Some("a=b"));
    assert_eq!(
        c.header("accept"),
        "application/json;as=Table;v=v1;g=meta.k8s.io,application/json;as=Table;v=v1beta1;g=meta.k8s.io,application/json"
    );
    // Custom columns: full row objects (dao/table.go includeObject=Object).
    let t = h
        .acc
        .list_table_with(
            &known::deployments(),
            "ns1",
            None,
            true,
            Some("status.phase=Running"),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        h.non_sar().last().unwrap().q("includeObject"),
        Some("Object")
    );
    // dao/table.go: field selectors go to the server.
    assert_eq!(
        h.non_sar().last().unwrap().q("fieldSelector"),
        Some("status.phase=Running")
    );
    assert_eq!(t.rows[0].object.as_ref().unwrap()["metadata"]["name"], "d1");
    // Plain list answer: no table support.
    assert!(h
        .acc
        .list_table(&known::pods(), "all", None)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn get_and_yaml() {
    let h = Harness::new(|r| match r.path.as_str() {
        "/api/v1/namespaces/default/pods/p1" => ok(json!({
            "kind": "Pod", "apiVersion": "v1",
            "metadata": {"name": "p1", "namespace": "default",
                "managedFields": [{"manager": "kubectl"}], "labels": {"z": "1", "a": "2"}},
            "spec": {"containers": [{"name": "c", "image": "nginx"}]}
        })),
        "/api/v1/nodes/n1" => {
            ok(json!({"kind": "Node", "apiVersion": "v1", "metadata": {"name": "n1"}}))
        }
        _ => base(r),
    });
    let o = h.acc.get(&known::pods(), "default/p1").await.unwrap();
    assert_eq!(o.metadata.name.as_deref(), Some("p1"));
    let n = h.acc.get(&known::nodes(), "n1").await.unwrap();
    assert_eq!(n.metadata.name.as_deref(), Some("n1"));
    let y = h
        .acc
        .to_yaml(&known::pods(), "default/p1", false)
        .await
        .unwrap();
    // yaml.v2 double quotes strings that would read back as numbers.
    assert_eq!(
        y,
        "apiVersion: v1\nkind: Pod\nmetadata:\n  labels:\n    a: \"2\"\n    z: \"1\"\n  name: p1\n  namespace: default\nspec:\n  containers:\n  - image: nginx\n    name: c\n"
    );
    let y = h
        .acc
        .to_yaml(&known::pods(), "default/p1", true)
        .await
        .unwrap();
    assert!(y.contains("managedFields:\n  - manager: kubectl\n"), "{y}");
    let err = h.acc.get(&known::pods(), "default/nope").await.unwrap_err();
    assert!(err.to_string().contains("could not find"), "{err}");
}

#[tokio::test]
async fn delete_checks_access_and_sends_options() {
    let h = Harness::new(|r| {
        if r.method == "DELETE" {
            return ok(json!({"kind": "Status", "apiVersion": "v1", "status": "Success"}));
        }
        base(r)
    });
    let pods = known::pods();
    h.acc
        .delete(
            &pods,
            "default/p1",
            DeleteOptions {
                propagation: Some(Propagation::Foreground),
                grace: Some(-1),
                force: false,
            },
        )
        .await
        .unwrap();
    h.acc
        .delete(
            &pods,
            "default/p2",
            DeleteOptions {
                propagation: Some(Propagation::Background),
                grace: None,
                force: true,
            },
        )
        .await
        .unwrap();
    h.acc
        .delete(
            &known::nodes(),
            "n1",
            DeleteOptions {
                propagation: None,
                grace: Some(10),
                force: false,
            },
        )
        .await
        .unwrap();
    let sars = h.sars();
    assert_eq!(
        sars[0],
        json!({"namespace": "default", "verb": "delete", "version": "v1", "resource": "pods", "name": "p1"})
    );
    assert_eq!(
        sars[2],
        json!({"verb": "delete", "version": "v1", "resource": "nodes", "name": "n1"})
    );
    let dels = h.non_sar();
    assert!(dels[0].is("DELETE", "/api/v1/namespaces/default/pods/p1"));
    assert_eq!(dels[0].json()["propagationPolicy"], json!("Foreground"));
    assert_eq!(dels[0].json().get("gracePeriodSeconds"), None);
    assert_eq!(dels[1].json()["gracePeriodSeconds"], json!(0));
    assert_eq!(dels[1].json()["propagationPolicy"], json!("Background"));
    assert!(dels[2].is("DELETE", "/api/v1/nodes/n1"));
    assert_eq!(dels[2].json()["gracePeriodSeconds"], json!(10));
    assert_eq!(dels[2].json().get("propagationPolicy"), None);

    let h = Harness::new(deny_all);
    let err = h
        .acc
        .delete(&pods, "default/p1", DeleteOptions::default())
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "(delete) access denied for user on resource \"p1\":v1/pods in namespace \"default\""
    );
    assert!(h.non_sar().is_empty());
}

#[tokio::test]
async fn can_uses_cache_and_namespace_rules() {
    let h = Harness::new(base);
    assert!(h.acc.can(&known::pods(), "default", "list").await.unwrap());
    assert!(h.acc.can(&known::pods(), "default", "list").await.unwrap());
    assert!(h.acc.can(&known::pods(), "all", "list").await.unwrap());
    assert!(h.acc.can(&known::nodes(), "-", "get").await.unwrap());
    let sars = h.sars();
    assert_eq!(sars.len(), 3);
    assert_eq!(
        sars[1],
        json!({"verb": "list", "version": "v1", "resource": "pods"})
    );
    assert_eq!(
        sars[2],
        json!({"verb": "get", "version": "v1", "resource": "nodes"})
    );

    // client.CanI: the first denial is an error, the cached one `false`.
    let h = Harness::new(deny_all);
    let err = h
        .acc
        .can(&known::deployments(), "x", "patch")
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "(patch) access denied for user on resource \"\":apps/v1/deployments in namespace \"x\""
    );
    assert!(!h
        .acc
        .can(&known::deployments(), "x", "patch")
        .await
        .unwrap());
    assert_eq!(h.sars().len(), 1);
    assert_eq!(h.sars()[0]["group"], json!("apps"));
}

#[tokio::test]
async fn namespaces_listing() {
    let h = Harness::new(|r| match r.path.as_str() {
        "/api/v1/namespaces" => ok(json!({"kind": "NamespaceList", "apiVersion": "v1",
            "items": [{"metadata": {"name": "zz"}}, {"metadata": {"name": "default"}}]})),
        _ => base(r),
    });
    assert_eq!(h.acc.namespaces().await.unwrap(), vec!["default", "zz"]);
    assert_eq!(h.acc.namespaces().await.unwrap(), vec!["default", "zz"]);
    assert_eq!(h.non_sar().len(), 1);
    let h = Harness::new(deny_all);
    assert_eq!(
        h.acc.namespaces().await.unwrap_err().to_string(),
        "user not authorized to list all namespaces"
    );
}

fn scale_handler(r: &Req) -> Reply {
    match (r.method.as_str(), r.path.as_str()) {
        ("GET", p) if p.ends_with("/scale") => ok(json!({
            "kind": "Scale", "apiVersion": "autoscaling/v1",
            "metadata": {"name": "d1", "namespace": "ns", "resourceVersion": "7"},
            "spec": {"replicas": 3}, "status": {"replicas": 3}
        })),
        ("PUT", p) if p.ends_with("/scale") => (200, r.body.clone()),
        _ => base(r),
    }
}

#[tokio::test]
async fn scale_and_replicas() {
    let h = Harness::new(scale_handler);
    assert_eq!(
        h.acc
            .replicas(&known::deployments(), "ns/d1")
            .await
            .unwrap(),
        3
    );
    h.acc
        .scale(&known::deployments(), "ns/d1", 5)
        .await
        .unwrap();
    let sars = h.sars();
    assert_eq!(
        sars,
        vec![
            json!({"namespace": "ns", "verb": "get", "group": "apps", "version": "v1", "resource": "deployments", "subresource": "scale", "name": "d1"}),
            json!({"namespace": "ns", "verb": "update", "group": "apps", "version": "v1", "resource": "deployments", "subresource": "scale", "name": "d1"}),
        ]
    );
    let calls = h.non_sar();
    assert!(calls[0].is("GET", "/apis/apps/v1/namespaces/ns/deployments/d1/scale"));
    assert!(calls[2].is("PUT", "/apis/apps/v1/namespaces/ns/deployments/d1/scale"));
    let body = calls[2].json();
    assert_eq!(body["spec"]["replicas"], json!(5));
    assert_eq!(body["metadata"]["resourceVersion"], json!("7"));
    // Generic scalers (replicasets, CRDs) skip the access check.
    let h = Harness::new(scale_handler);
    h.acc
        .scale(&known::replicasets(), "ns/r1", 0)
        .await
        .unwrap();
    assert!(h.sars().is_empty());
    assert_eq!(h.non_sar()[1].json()["spec"]["replicas"], json!(0));
}

#[tokio::test]
async fn restart_patches_template() {
    let h = Harness::new(|r| match (r.method.as_str(), r.path.as_str()) {
        ("GET", "/apis/apps/v1/namespaces/ns/deployments/d1") => ok(
            json!({"kind": "Deployment", "apiVersion": "apps/v1", "metadata": {"name": "d1"}, "spec": {}}),
        ),
        ("GET", "/apis/apps/v1/namespaces/ns/deployments/paused") => ok(
            json!({"kind": "Deployment", "apiVersion": "apps/v1", "metadata": {"name": "paused"}, "spec": {"paused": true}}),
        ),
        ("GET", "/apis/apps/v1/namespaces/ns/statefulsets/s1") => ok(
            json!({"kind": "StatefulSet", "apiVersion": "apps/v1", "metadata": {"name": "s1"}, "spec": {}}),
        ),
        ("PATCH", _) => {
            ok(json!({"kind": "Deployment", "apiVersion": "apps/v1", "metadata": {"name": "d1"}}))
        }
        _ => base(r),
    });
    h.acc
        .restart(&known::deployments(), "ns/d1", "kubectl-rollout")
        .await
        .unwrap();
    h.acc
        .restart(&known::statefulsets(), "ns/s1", "")
        .await
        .unwrap();
    let calls = h.non_sar();
    let patch = &calls[1];
    assert!(patch.is("PATCH", "/apis/apps/v1/namespaces/ns/deployments/d1"));
    assert_eq!(
        patch.header("content-type"),
        "application/strategic-merge-patch+json"
    );
    // dialog.ShowRestart FieldManager rides on the PatchOptions.
    assert!(patch
        .query
        .contains(&("fieldManager".to_string(), "kubectl-rollout".to_string())));
    let ts = patch.json()["spec"]["template"]["metadata"]["annotations"]
        ["kubectl.kubernetes.io/restartedAt"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(chrono::DateTime::parse_from_rfc3339(&ts).is_ok());
    assert!(calls[3].is("PATCH", "/apis/apps/v1/namespaces/ns/statefulsets/s1"));
    assert_eq!(h.sars()[0]["verb"], json!("patch"));
    let err = h
        .acc
        .restart(&known::deployments(), "ns/paused", "")
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "can't restart paused deployment (run rollout resume first)"
    );
    let err = h.acc.restart(&known::pods(), "ns/p", "").await.unwrap_err();
    assert_eq!(err.to_string(), "unsupported resource for restart: v1/pods");
}

#[tokio::test]
async fn set_images_patches() {
    let h = Harness::new(|r| match (r.method.as_str(), r.path.as_str()) {
        ("GET", "/api/v1/namespaces/ns/pods/managed") => {
            ok(json!({"kind": "Pod", "apiVersion": "v1",
            "metadata": {"name": "managed", "ownerReferences": [{"apiVersion": "apps/v1", "kind": "ReplicaSet", "name": "rs1", "uid": "1"}]}}))
        }
        ("GET", "/api/v1/namespaces/ns/pods/bare") => {
            ok(json!({"kind": "Pod", "apiVersion": "v1", "metadata": {"name": "bare"}}))
        }
        ("PATCH", _) => ok(json!({"kind": "X", "apiVersion": "v1", "metadata": {"name": "x"}})),
        _ => base(r),
    });
    let imgs = vec![
        ImageSpec {
            name: "c".into(),
            docker_image: "nginx:2".into(),
            init: false,
        },
        ImageSpec {
            name: "i".into(),
            docker_image: "busybox".into(),
            init: true,
        },
    ];
    h.acc
        .set_images(&known::deployments(), "ns/d1", imgs.clone())
        .await
        .unwrap();
    let p = &h.non_sar()[0];
    assert!(p.is("PATCH", "/apis/apps/v1/namespaces/ns/deployments/d1"));
    assert_eq!(
        p.header("content-type"),
        "application/strategic-merge-patch+json"
    );
    assert_eq!(
        p.json(),
        json!({"spec": {"template": {"spec": {
            "$setElementOrder/containers": [{"name": "c"}],
            "$setElementOrder/initContainers": [{"name": "i"}],
            "containers": [{"image": "nginx:2", "name": "c"}],
            "initContainers": [{"image": "busybox", "name": "i"}]
        }}}})
    );
    h.acc
        .set_images(&known::pods(), "ns/bare", imgs[..1].to_vec())
        .await
        .unwrap();
    let p = h.non_sar().last().unwrap().clone();
    assert!(p.is("PATCH", "/api/v1/namespaces/ns/pods/bare"));
    assert_eq!(
        p.json()["spec"]["containers"],
        json!([{"image": "nginx:2", "name": "c"}])
    );
    let err = h
        .acc
        .set_images(&known::pods(), "ns/managed", imgs.clone())
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "unable to set image. This pod is managed by ReplicaSet/rs1. Please set the image on the controller"
    );
    let err = h
        .acc
        .set_images(&known::jobs(), "ns/j", imgs)
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "unsupported resource for setting images: batch/v1/jobs"
    );
}

fn cronjob_handler(r: &Req) -> Reply {
    match (r.method.as_str(), r.path.as_str()) {
        ("GET", "/apis/batch/v1/namespaces/ns/cronjobs/hello") => ok(json!({
            "kind": "CronJob", "apiVersion": "batch/v1",
            "metadata": {"name": "hello", "namespace": "ns", "uid": "cj-uid"},
            "spec": {"schedule": "*/1 * * * *", "suspend": false, "jobTemplate": {
                "metadata": {"labels": {"app": "hello"}},
                "spec": {"template": {"spec": {"containers": [{"name": "c", "image": "busybox"}]}}}}}
        })),
        ("POST", "/apis/batch/v1/namespaces/ns/jobs") | ("PUT", _) => (201, r.body.clone()),
        _ => base(r),
    }
}

#[tokio::test]
async fn trigger_cronjob_creates_manual_job() {
    let h = Harness::new(cronjob_handler);
    h.acc.trigger_cronjob("ns/hello").await.unwrap();
    let sars = h.sars();
    assert_eq!(sars[0]["resource"], json!("jobs"));
    assert_eq!(sars[0]["verb"], json!("get"));
    assert_eq!(sars[1]["verb"], json!("create"));
    assert_eq!(sars[0]["name"], json!("hello"));
    let post = h
        .non_sar()
        .into_iter()
        .find(|r| r.method == "POST")
        .unwrap();
    let job = post.json();
    let name = job["metadata"]["name"].as_str().unwrap();
    assert!(
        name.starts_with("hello-manual-") && name.len() == "hello-manual-".len() + 3,
        "{name}"
    );
    assert_eq!(job["metadata"]["labels"], json!({"app": "hello"}));
    assert_eq!(
        job["metadata"]["ownerReferences"],
        json!([{"apiVersion": "batch/v1", "kind": "CronJob", "name": "hello", "uid": "cj-uid", "controller": true, "blockOwnerDeletion": true}])
    );
    assert_eq!(
        job["spec"]["template"]["spec"]["containers"][0]["image"],
        json!("busybox")
    );
}

#[tokio::test]
async fn toggle_suspend_replaces_cronjob() {
    let h = Harness::new(cronjob_handler);
    h.acc.toggle_suspend_cronjob("ns/hello").await.unwrap();
    let put = h.non_sar().into_iter().find(|r| r.method == "PUT").unwrap();
    assert_eq!(put.path, "/apis/batch/v1/namespaces/ns/cronjobs/hello");
    assert_eq!(put.json()["spec"]["suspend"], json!(true));
    assert_eq!(h.sars()[1]["verb"], json!("update"));
}

fn node_handler(unschedulable: bool) -> impl Fn(&Req) -> Reply + Send + Sync {
    move |r: &Req| match (r.method.as_str(), r.path.as_str()) {
        ("GET", "/api/v1/nodes/n1") => ok(
            json!({"kind": "Node", "apiVersion": "v1", "metadata": {"name": "n1"},
            "spec": {"unschedulable": unschedulable}}),
        ),
        ("PATCH", "/api/v1/nodes/n1") => {
            ok(json!({"kind": "Node", "apiVersion": "v1", "metadata": {"name": "n1"}}))
        }
        _ => base(r),
    }
}

#[tokio::test]
async fn cordon_and_uncordon() {
    let h = Harness::new(node_handler(false));
    h.acc.toggle_cordon("n1", true).await.unwrap();
    let p = h
        .non_sar()
        .into_iter()
        .find(|r| r.method == "PATCH")
        .unwrap();
    assert_eq!(
        p.header("content-type"),
        "application/strategic-merge-patch+json"
    );
    assert_eq!(p.json(), json!({"spec": {"unschedulable": true}}));
    let err = h.acc.toggle_cordon("n1", false).await.unwrap_err();
    assert_eq!(err.to_string(), "node is already uncordoned");

    let h = Harness::new(node_handler(true));
    h.acc.toggle_cordon("n1", false).await.unwrap();
    let p = h
        .non_sar()
        .into_iter()
        .find(|r| r.method == "PATCH")
        .unwrap();
    assert_eq!(p.json(), json!({"spec": {"unschedulable": null}}));
    assert_eq!(
        h.acc
            .toggle_cordon("n1", true)
            .await
            .unwrap_err()
            .to_string(),
        "node is already cordoned"
    );
}

fn drain_handler(with_bare_pod: bool) -> impl Fn(&Req) -> Reply + Send + Sync {
    move |r: &Req| match (r.method.as_str(), r.path.as_str()) {
        ("GET", "/api/v1/pods") => {
            let mut items = vec![
                json!({"metadata": {"name": "web", "namespace": "ns", "uid": "u1",
                    "ownerReferences": [{"apiVersion": "apps/v1", "kind": "ReplicaSet", "name": "rs", "uid": "r", "controller": true}]},
                    "spec": {"containers": [], "volumes": [{"name": "tmp", "emptyDir": {}}]}}),
                json!({"metadata": {"name": "agent", "namespace": "kube-system", "uid": "u2",
                    "ownerReferences": [{"apiVersion": "apps/v1", "kind": "DaemonSet", "name": "ds", "uid": "d", "controller": true}]},
                    "spec": {"containers": []}}),
                json!({"metadata": {"name": "mirror", "namespace": "kube-system", "uid": "u3",
                    "annotations": {"kubernetes.io/config.mirror": "x"}}, "spec": {"containers": []}}),
            ];
            if with_bare_pod {
                items.push(json!({"metadata": {"name": "bare", "namespace": "ns", "uid": "u4"}, "spec": {"containers": []}}));
            }
            ok(json!({"kind": "PodList", "apiVersion": "v1", "metadata": {}, "items": items}))
        }
        ("GET", "/apis/apps/v1/namespaces/kube-system/daemonsets/ds") => {
            ok(json!({"kind": "DaemonSet", "apiVersion": "apps/v1", "metadata": {"name": "ds"}}))
        }
        ("POST", "/api/v1/namespaces/ns/pods/web/eviction") => (
            201,
            json!({"kind": "Status", "apiVersion": "v1", "status": "Success"}).to_string(),
        ),
        ("GET", "/api/v1/namespaces/ns/pods/web") => not_found("pods \"web\" not found"),
        _ => node_handler(false)(r),
    }
}

#[tokio::test]
async fn drain_cordons_and_evicts() {
    let h = Harness::new(drain_handler(false));
    let out = h
        .acc
        .drain(
            "n1",
            DrainOptions {
                grace_period_seconds: 30,
                ..Default::default()
            },
            noop(),
        )
        .await
        .unwrap();
    assert_eq!(out, "evicting pod ns/web\nNode n1 drained!");
    let calls = h.non_sar();
    assert!(calls.iter().any(|r| r.is("PATCH", "/api/v1/nodes/n1")));
    let list = calls.iter().find(|r| r.is("GET", "/api/v1/pods")).unwrap();
    assert_eq!(list.q("fieldSelector"), Some("spec.nodeName=n1"));
    assert_eq!(list.q("limit"), Some("500"));
    let ev = calls.iter().find(|r| r.method == "POST").unwrap();
    assert_eq!(
        ev.json(),
        json!({"apiVersion": "policy/v1", "kind": "Eviction", "metadata": {"name": "web", "namespace": "ns"},
               "deleteOptions": {"gracePeriodSeconds": 30}})
    );
    // daemonset and mirror pods are left alone.
    assert!(!calls
        .iter()
        .any(|r| r.path.contains("agent") || r.path.contains("mirror")));
}

fn noop() -> crate::k8s::DrainProgress {
    Arc::new(|_: &str| {})
}

/// kubectl v0.35 evictPods: a pod that outlives the global timeout reports
/// the context error, and the progress written so far is kept.
#[tokio::test]
async fn drain_eviction_wait_times_out() {
    let h = Harness::new(|r: &Req| match (r.method.as_str(), r.path.as_str()) {
        ("GET", "/api/v1/namespaces/ns/pods/web") => ok(
            json!({"kind": "Pod", "apiVersion": "v1", "metadata": {"name": "web", "namespace": "ns", "uid": "u1"}}),
        ),
        _ => drain_handler(false)(r),
    });
    let err = h
        .acc
        .drain(
            "n1",
            DrainOptions {
                timeout_secs: 1,
                ..Default::default()
            },
            noop(),
        )
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "error when waiting for pod \"web\" in namespace \"ns\" to terminate: context deadline exceeded"
    );
    let f = err
        .downcast_ref::<crate::k8s::DrainFailed>()
        .expect("drain output");
    assert_eq!(f.out, "evicting pod ns/web\n");
}

#[tokio::test]
async fn drain_without_eviction_deletes() {
    let h = Harness::new(|r: &Req| match (r.method.as_str(), r.path.as_str()) {
        ("DELETE", "/api/v1/namespaces/ns/pods/web") => {
            ok(json!({"kind": "Pod", "apiVersion": "v1", "metadata": {"name": "web"}}))
        }
        _ => drain_handler(false)(r),
    });
    let out = h
        .acc
        .drain(
            "n1",
            DrainOptions {
                disable_eviction: true,
                ..Default::default()
            },
            noop(),
        )
        .await
        .unwrap();
    assert_eq!(out, "Node n1 drained!");
    let del = h
        .non_sar()
        .into_iter()
        .find(|r| r.method == "DELETE")
        .unwrap();
    assert_eq!(del.json().get("gracePeriodSeconds"), None);
}

#[tokio::test]
async fn drain_reports_filter_errors() {
    let h = Harness::new(drain_handler(true));
    let err = h
        .acc
        .drain("n1", DrainOptions::default(), noop())
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "cannot delete Pods declare no controller (use --force to override): ns/bare"
    );
    // dao.Node.Drain writes each filter error to the progress writer.
    let f = err
        .downcast_ref::<crate::k8s::DrainFailed>()
        .expect("drain output");
    assert_eq!(
        f.out,
        "[n1] cannot delete Pods declare no controller (use --force to override): ns/bare\n"
    );
    let h = Harness::new(drain_handler(true));
    let err = h
        .acc
        .drain(
            "n1",
            DrainOptions {
                ignore_all_daemon_sets: false,
                force: true,
                ..Default::default()
            },
            noop(),
        )
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "cannot delete DaemonSet-managed Pods (use --ignore-daemonsets to ignore): kube-system/agent"
    );
}

fn metrics_handler(r: &Req) -> Reply {
    match r.path.as_str() {
        "/apis/metrics.k8s.io/v1beta1/namespaces/default/pods"
        | "/apis/metrics.k8s.io/v1beta1/pods" => ok(json!({
            "kind": "PodMetricsList", "apiVersion": "metrics.k8s.io/v1beta1",
            "items": [{"metadata": {"name": "p1", "namespace": "default"},
                "containers": [{"name": "c", "usage": {"cpu": "250m", "memory": "64Mi"}}]}]
        })),
        "/apis/metrics.k8s.io/v1beta1/nodes" => ok(json!({
            "kind": "NodeMetricsList", "apiVersion": "metrics.k8s.io/v1beta1",
            "items": [{"metadata": {"name": "n1"}, "usage": {"cpu": "1", "memory": "2Gi"}}]
        })),
        "/api/v1/nodes" => ok(json!({"kind": "NodeList", "apiVersion": "v1", "items": [
            {"metadata": {"name": "n1"}, "status": {"allocatable": {"cpu": "4", "memory": "8Gi"}}},
            {"metadata": {"name": "n2"}, "status": {"allocatable": {"cpu": "4", "memory": "8Gi"}}}
        ]})),
        "/version" => ok(
            json!({"major": "1", "minor": "34", "gitVersion": "v1.34.1", "gitCommit": "", "gitTreeState": "",
            "buildDate": "", "goVersion": "", "compiler": "", "platform": ""}),
        ),
        _ => base(r),
    }
}

#[tokio::test]
async fn metrics_and_cluster_info() {
    let h = Harness::new(metrics_handler);
    assert!(h.acc.has_metrics().await);
    let pm = h.acc.pod_metrics("default").await.unwrap();
    assert_eq!(
        pm["default/p1"].containers["c"],
        Metrics {
            cpu_millis: 250,
            mem_bytes: 64 << 20
        }
    );
    let all = h.acc.pod_metrics("all").await.unwrap();
    assert_eq!(all.len(), 1);
    let nm = h.acc.node_metrics().await.unwrap();
    assert_eq!(
        nm["n1"].usage,
        Metrics {
            cpu_millis: 1000,
            mem_bytes: 2 << 30
        }
    );
    assert_eq!(
        nm["n1"].allocatable,
        Metrics {
            cpu_millis: 4000,
            mem_bytes: 8 << 30
        }
    );
    let info = h.acc.cluster_info().await.unwrap();
    assert_eq!(
        info,
        ClusterInfo {
            context: "ctx-a".into(),
            cluster: "cl-a".into(),
            user: "u-a".into(),
            k9s_version: crate::version::VERSION.into(),
            k9s_latest: String::new(),
            k8s_version: "v1.34.1".into(),
            cpu_pct: Some(12),
            mem_pct: Some(12),
        }
    );
    let sars = h.sars();
    assert_eq!(
        sars[0],
        json!({"namespace": "default", "verb": "list", "group": "metrics.k8s.io", "version": "v1beta1", "resource": "pods"})
    );
    // pod metrics are cached per namespace for a minute.
    h.acc.pod_metrics("default").await.unwrap();
    let n = h
        .non_sar()
        .iter()
        .filter(|r| r.path.ends_with("/namespaces/default/pods"))
        .count();
    assert_eq!(n, 1);
}

#[tokio::test]
async fn no_metrics_server() {
    let h = Harness::new(|r: &Req| match r.path.as_str() {
        "/apis" => ok(json!({"kind": "APIGroupList", "apiVersion": "v1", "groups": []})),
        "/version" => status(500, "InternalError", "boom"),
        _ => base(r),
    });
    assert!(!h.acc.has_metrics().await);
    assert_eq!(
        h.acc.pod_metrics("").await.unwrap_err().to_string(),
        "no metrics-server detected on cluster"
    );
    assert!(h.acc.node_metrics().await.is_err());
    let info = h.acc.cluster_info().await.unwrap();
    assert_eq!(info.k8s_version, "n/a");
    assert_eq!((info.cpu_pct, info.mem_pct), (None, None));
    // The probe is cached.
    let probes = h
        .log
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.path == "/apis")
        .count();
    assert_eq!(probes, 1);
}

fn logs_handler(r: &Req) -> Reply {
    match (r.method.as_str(), r.path.as_str()) {
        ("GET", "/api/v1/namespaces/ns/pods/p1") => ok(json!({"kind": "Pod", "apiVersion": "v1",
            "metadata": {"name": "p1", "namespace": "ns"},
            "spec": {"initContainers": [{"name": "init"}], "containers": [{"name": "app"}]},
            "status": {"phase": "Running"}})),
        ("GET", "/api/v1/namespaces/ns/pods/p1/log") => {
            let co = r.q("container").unwrap_or_default().to_string();
            (
                200,
                format!("2024-05-01T10:00:00Z {co} one\n2024-05-01T10:00:01Z {co} two\n"),
            )
        }
        ("GET", "/api/v1/namespaces/ns/pods") => {
            ok(json!({"kind": "PodList", "apiVersion": "v1", "items": [
                {"metadata": {"name": "p1", "namespace": "ns"}, "spec": {"containers": [{"name": "app"}]}}
            ]}))
        }
        _ => base(r),
    }
}

async fn collect(s: LogStream) -> (Vec<LogLine>, Vec<String>) {
    let items: Vec<Result<LogLine>> = s.collect().await;
    let mut lines = vec![];
    let mut errs = vec![];
    for i in items {
        match i {
            // EOF/retry errors arrive as error items (k9s ToErrLogItem).
            Ok(l) if l.is_error => errs.push(l.text),
            Ok(l) => lines.push(l),
            Err(e) => errs.push(e.to_string()),
        }
    }
    (lines, errs)
}

#[tokio::test]
async fn tail_single_container() {
    let h = Harness::new(logs_handler);
    let s = h
        .acc
        .tail_logs(LogOptions {
            namespace: "ns".into(),
            path: "p1".into(),
            container: Some("app".into()),
            lines: 50,
            since_seconds: Some(60),
            show_timestamp: true,
            follow: true,
            ..Default::default()
        })
        .await
        .unwrap();
    let (lines, errs) = collect(s).await;
    assert_eq!(
        lines,
        vec![
            LogLine {
                pod: "p1".into(),
                container: "app".into(),
                timestamp: Some("2024-05-01T10:00:00Z".into()),
                text: "app one".into(),
                ..Default::default()
            },
            LogLine {
                pod: "p1".into(),
                container: "app".into(),
                timestamp: Some("2024-05-01T10:00:01Z".into()),
                text: "app two".into(),
                ..Default::default()
            },
        ]
    );
    assert_eq!(errs, vec!["stream closed: EOF for ns/p1 (app)"]);
    let log = h
        .non_sar()
        .into_iter()
        .find(|r| r.path.ends_with("/log"))
        .unwrap();
    assert_eq!(log.q("container"), Some("app"));
    assert_eq!(log.q("follow"), Some("true"));
    assert_eq!(log.q("timestamps"), Some("true"));
    assert_eq!(log.q("tailLines"), Some("50"));
    assert_eq!(log.q("sinceSeconds"), Some("60"));
    assert_eq!(log.q("previous"), None);
    let sar = &h.sars()[0];
    assert_eq!(sar["subresource"], json!("log"));
    assert_eq!(sar["name"], json!("p1"));
}

#[tokio::test]
async fn tail_all_containers_head_mode() {
    let h = Harness::new(logs_handler);
    let s = h
        .acc
        .tail_logs(LogOptions {
            namespace: "ns".into(),
            path: "p1".into(),
            since_seconds: None,
            previous: true,
            ..Default::default()
        })
        .await
        .unwrap();
    let (lines, errs) = collect(s).await;
    assert_eq!(lines.len(), 4);
    assert!(lines.iter().all(|l| l.timestamp.is_none()));
    let mut cos: Vec<String> = lines
        .iter()
        .map(|l| format!("{}:{}", l.container, l.text))
        .collect();
    cos.sort();
    assert_eq!(
        cos,
        vec![
            "app:app one",
            "app:app two",
            "init:init one",
            "init:init two"
        ]
    );
    assert_eq!(errs.len(), 2);
    let reqs: Vec<Req> = h
        .non_sar()
        .into_iter()
        .filter(|r| r.path.ends_with("/log"))
        .collect();
    assert_eq!(reqs.len(), 2);
    for r in reqs {
        assert_eq!(r.q("limitBytes"), Some("5000"));
        assert_eq!(r.q("tailLines"), None);
        assert_eq!(r.q("follow"), None);
        assert_eq!(r.q("previous"), Some("true"));
    }
}

#[tokio::test]
async fn tail_by_selector() {
    let h = Harness::new(logs_handler);
    let s = h
        .acc
        .tail_logs(LogOptions {
            namespace: "ns".into(),
            path: "app=web".into(),
            since_seconds: Some(-1),
            ..Default::default()
        })
        .await
        .unwrap();
    let (lines, _) = collect(s).await;
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].pod, "p1");
    let list = h
        .non_sar()
        .into_iter()
        .find(|r| r.path == "/api/v1/namespaces/ns/pods")
        .unwrap();
    assert_eq!(list.q("labelSelector"), Some("app=web"));
}

#[tokio::test]
async fn tail_denied() {
    let h = Harness::new(|r: &Req| {
        if r.path.ends_with("/selfsubjectaccessreviews") {
            return deny_all(r);
        }
        logs_handler(r)
    });
    let err = h
        .acc
        .tail_logs(LogOptions {
            namespace: "ns".into(),
            path: "p1".into(),
            ..Default::default()
        })
        .await
        .err()
        .unwrap();
    assert_eq!(
        err.to_string(),
        "(get) access denied for user on resource \"p1\":v1/pods:log in namespace \"ns\""
    );
}

#[tokio::test]
async fn port_forward_requires_running_pod() {
    let h = Harness::new(|r: &Req| match r.path.as_str() {
        "/api/v1/namespaces/ns/pods/p1" => ok(
            json!({"kind": "Pod", "apiVersion": "v1", "metadata": {"name": "p1"},
            "status": {"phase": "Pending"}}),
        ),
        _ => base(r),
    });
    let err = h
        .acc
        .port_forward(PortForwardSpec {
            namespace: "ns".into(),
            pod: "p1".into(),
            container: "c".into(),
            address: "localhost".into(),
            local_port: 0,
            container_port: 80,
        })
        .await
        .err()
        .unwrap();
    assert_eq!(
        err.to_string(),
        "unable to forward port because pod is not running. Current status=Pending"
    );
}

#[tokio::test]
async fn apply_yaml_replaces() {
    let h = Harness::new(|r: &Req| match r.method.as_str() {
        "PUT" => ok(r.json()),
        _ => base(r),
    });
    let yaml =
        "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: cm\n  namespace: ns\ndata:\n  a: b\n";
    h.acc
        .apply_yaml(&known::configmaps(), "ns/cm", yaml)
        .await
        .unwrap();
    let put = &h.non_sar()[0];
    assert_eq!(put.path, "/api/v1/namespaces/ns/configmaps/cm");
    assert_eq!(put.json()["data"], json!({"a": "b"}));
    assert!(h
        .acc
        .apply_yaml(&known::configmaps(), "ns/cm", "- a\n- b\n")
        .await
        .is_err());
    assert!(h
        .acc
        .apply_yaml(&known::configmaps(), "ns/cm", "a: [")
        .await
        .is_err());
}

#[tokio::test]
async fn client_errors_are_kept_until_used() {
    let cfg = LoadedConfig::from_kubeconfig(Kubeconfig::from_yaml(KC).unwrap(), Default::default());
    let acc = KubeAccessor::from_conn(Conn {
        clients: Err("auth error: token expired".into()),
        build_failed: false,
        cfg,
        server: String::new(),
        override_file: None,
    });
    // Kubeconfig-only calls still work, so the context view can load.
    assert_eq!(acc.active_context(), "ctx-a");
    assert!(!acc.contexts().await.unwrap().is_empty());
    assert_eq!(
        acc.check_connectivity().await.unwrap_err().to_string(),
        "auth error: token expired"
    );
    assert!(acc.list(&known::pods(), "default", None).await.is_err());
}

#[tokio::test]
async fn contexts_shell_and_switch() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config");
    std::fs::write(&path, KC).unwrap();
    let kc = path.display().to_string();
    let acc = KubeAccessor::new(
        LoadedConfig::load(ConnectOptions {
            kubeconfig: Some(kc.clone()),
            ..Default::default()
        })
        .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(acc.active_context(), "ctx-a");
    assert_eq!(acc.default_namespace(), "team-a");
    let cc = acc.contexts().await.unwrap();
    assert_eq!(cc.len(), 2);
    assert!(cc[0].active && cc[0].name == "ctx-a" && cc[0].namespace == "team-a");
    // Defaults match the kubeconfig: no override file, explicit path exported.
    assert!(acc.conn().override_file.is_none());
    assert_eq!(
        acc.shell_env(),
        vec![("KUBECONFIG".to_string(), kc.clone())]
    );
    assert_eq!(
        acc.exec_argv("ns", "p", "c", ""),
        vec![
            "kubectl",
            "exec",
            "-it",
            "-n",
            "ns",
            "p",
            "-c",
            "c",
            "--",
            "sh",
            "-c",
            shell::SHELL_CHECK
        ]
    );

    assert_eq!(acc.switch_context("ctx-b").await.unwrap(), "team-b");
    assert_eq!(acc.active_context(), "ctx-b");
    assert_eq!(acc.default_namespace(), "team-b");
    assert!(acc
        .contexts()
        .await
        .unwrap()
        .iter()
        .any(|c| c.name == "ctx-b" && c.active));
    // After a switch kubectl is pointed at the active context through an
    // override kubeconfig placed first in KUBECONFIG; argv never carries it.
    let ov = acc
        .conn()
        .override_file
        .as_ref()
        .unwrap()
        .path()
        .to_path_buf();
    assert_eq!(
        acc.shell_env(),
        vec![("KUBECONFIG".to_string(), format!("{}:{kc}", ov.display()))]
    );
    let merged = Kubeconfig::read_from(&ov).unwrap();
    assert_eq!(merged.current_context.as_deref(), Some("ctx-b"));
    let argv = acc.edit_argv(&known::deployments(), "ns/d");
    assert_eq!(
        argv,
        vec!["kubectl", "edit", "deployments.v1.apps/d", "-n", "ns"]
    );
    assert!(!acc
        .exec_argv("ns", "p", "", "")
        .iter()
        .any(|a| a.starts_with("--context") || a.starts_with("--kubeconfig")));
    assert_eq!(
        acc.switch_context("nope").await.unwrap_err().to_string(),
        "context \"nope\" does not exist"
    );
    assert_eq!(acc.active_context(), "ctx-b");
    // Switching back removes the old override file.
    acc.switch_context("ctx-a").await.unwrap();
    assert!(!ov.exists());
    assert!(acc.conn().override_file.is_none());
}

#[tokio::test]
async fn token_override_stays_out_of_argv() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config");
    std::fs::write(&path, KC).unwrap();
    let kc = path.display().to_string();
    let acc = KubeAccessor::new(
        LoadedConfig::load(ConnectOptions {
            kubeconfig: Some(kc.clone()),
            token: Some("s3cr3t".into()),
            insecure_skip_tls_verify: true,
            ..Default::default()
        })
        .unwrap(),
    )
    .await
    .unwrap();
    let argv = acc.exec_argv("ns", "p", "c", "");
    assert!(!argv
        .iter()
        .any(|a| a.contains("s3cr3t") || a.starts_with("--token")));
    assert_eq!(argv[..3], ["kubectl", "exec", "--insecure-skip-tls-verify"]);
    let ov = acc
        .conn()
        .override_file
        .as_ref()
        .unwrap()
        .path()
        .to_path_buf();
    let k = Kubeconfig::read_from(&ov).unwrap();
    assert_eq!(k.current_context.as_deref(), Some(shell::OVERRIDE_NAME));
    assert_eq!(k.contexts[0].context.as_ref().unwrap().cluster, "cl-a");
    assert_eq!(
        k.contexts[0].context.as_ref().unwrap().namespace.as_deref(),
        Some("team-a")
    );
    assert!(std::fs::read_to_string(&ov).unwrap().contains("s3cr3t"));
    assert!(acc.shell_env()[0].1.starts_with(&ov.display().to_string()));
    drop(acc);
    assert!(!ov.exists());
}

#[test]
fn event_selectors() {
    let pod: DynamicObject = serde_json::from_value(json!({
        "apiVersion": "v1", "kind": "Pod", "metadata": {"name": "p", "namespace": "ns", "uid": "u1"}
    }))
    .unwrap();
    assert_eq!(
        events_selectors(&pod, &known::pods()),
        vec!["involvedObject.kind=Pod,involvedObject.name=p,involvedObject.namespace=ns,involvedObject.uid=u1"]
    );
    let node: DynamicObject = serde_json::from_value(json!({
        "apiVersion": "v1", "kind": "Node", "metadata": {"name": "n1", "uid": "real-uid"}
    }))
    .unwrap();
    assert_eq!(
        // kubectl's node describer merges uid (controllers) and name (kubelet) matches.
        events_selectors(&node, &known::nodes()),
        vec![
            "involvedObject.kind=Node,involvedObject.name=n1,involvedObject.namespace=,involvedObject.uid=real-uid",
            "involvedObject.kind=Node,involvedObject.name=n1,involvedObject.namespace=,involvedObject.uid=n1"
        ]
    );
}

#[test]
fn helpers() {
    assert_eq!(clean_ns("all"), "");
    assert_eq!(clean_ns("-"), "");
    assert_eq!(clean_ns("x"), "x");
    assert_eq!(collection_path(&known::pods(), ""), "/api/v1/pods");
    assert_eq!(
        collection_path(&known::jobs(), "ns"),
        "/apis/batch/v1/namespaces/ns/jobs"
    );
    assert_eq!(split_path("-/n1"), ("".to_string(), "n1".to_string()));
    assert_eq!(
        sar_cache_key("ns", &known::pods(), "p", &["get", "list"]),
        "ns:v1/pods:p::get,list"
    );
    let items = list_items(json!({"apiVersion": "apps/v1", "kind": "DeploymentList", "items": [
        {"metadata": {"name": "d"}}, {"apiVersion": "apps/v1", "kind": "Deployment", "metadata": {"name": "e"}}
    ]}))
    .unwrap();
    assert_eq!(items[0].types.as_ref().unwrap().kind, "Deployment");
    assert_eq!(items[1].metadata.name.as_deref(), Some("e"));
    assert!(list_items(json!({})).unwrap().is_empty());
}

/// Read-only smoke test against the cluster in `K9RS_LIVE_KUBECONFIG`.
#[tokio::test]
#[ignore]
async fn live_smoke_read_only() {
    let Some(kc) = std::env::var_os("K9RS_LIVE_KUBECONFIG").map(std::path::PathBuf::from) else {
        eprintln!("skipping: K9RS_LIVE_KUBECONFIG not set");
        return;
    };
    let acc = match connect(ConnectOptions {
        kubeconfig: Some(kc.display().to_string()),
        request_timeout: Some(std::time::Duration::from_secs(20)),
        ..Default::default()
    })
    .await
    {
        Ok(a) => a,
        Err(e) => {
            eprintln!("skipping: no credentials for the test cluster: {e}");
            return;
        }
    };
    let rr = match acc.resources().await {
        Ok(rr) => rr,
        Err(e) => {
            eprintln!("skipping: cluster unreachable or no credentials: {e}");
            return;
        }
    };
    assert!(rr.iter().any(|m| m.gvr.to_string() == "v1/pods"));
    let info = acc.cluster_info().await.unwrap();
    assert!(info.k8s_version.starts_with('v'), "{info:?}");
    let ns = acc.default_namespace();
    let pods = acc.list(&known::pods(), &ns, None).await.unwrap();
    let table = acc.list_table(&known::pods(), &ns, None).await.unwrap();
    assert!(table.is_some());
    eprintln!(
        "{} pods in {ns}; metrics={}",
        pods.len(),
        acc.has_metrics().await
    );
}

/// Requests a describe sent besides discovery, SARs and the events lookup.
fn related_calls(h: &Harness) -> Vec<Req> {
    h.non_sar()
        .into_iter()
        .filter(|r| !r.path.ends_with("/events"))
        .collect()
}

fn list(kind: &str, api_version: &str, items: Vec<Value>) -> Reply {
    ok(
        json!({"kind": format!("{kind}List"), "apiVersion": api_version,
        "metadata": {}, "items": items}),
    )
}

fn pod_json(name: &str, ns: &str, phase: &str, owner_uid: Option<&str>) -> Value {
    let mut meta = json!({"name": name, "namespace": ns,
        "creationTimestamp": "2024-01-01T11:00:00Z", "labels": {"app": "web"}});
    if let Some(uid) = owner_uid {
        meta["ownerReferences"] = json!([{"apiVersion": "apps/v1", "kind": "ReplicaSet",
            "name": "owner", "uid": uid, "controller": true}]);
    }
    json!({"metadata": meta,
        "spec": {"nodeName": "n1", "containers": [{"name": "c", "image": "nginx",
            "resources": {"requests": {"cpu": "100m", "memory": "64Mi"}}}],
            "volumes": [{"name": "data", "persistentVolumeClaim": {"claimName": "data"}}]},
        "status": {"phase": phase}})
}

#[tokio::test]
async fn describe_node_fetches_pods_in_chunks_and_lease() {
    let h = Harness::new(|r| match r.path.as_str() {
        "/api/v1/nodes/n1" => ok(json!({"kind": "Node", "apiVersion": "v1",
            "metadata": {"name": "n1", "uid": "nu", "creationTimestamp": "2024-01-01T00:00:00Z"},
            "status": {"capacity": {"cpu": "4", "memory": "8Gi", "pods": "110"},
                "allocatable": {"cpu": "4", "memory": "8Gi", "pods": "110"}}})),
        "/api/v1/pods" if r.q("continue").is_none() => ok(json!({
            "kind": "PodList", "apiVersion": "v1", "metadata": {"continue": "page2"},
            "items": [pod_json("p-one", "a", "Running", None)]})),
        "/api/v1/pods" => list("Pod", "v1", vec![pod_json("p-two", "b", "Pending", None)]),
        "/apis/coordination.k8s.io/v1/namespaces/kube-node-lease/leases/n1" => ok(json!({
            "kind": "Lease", "apiVersion": "coordination.k8s.io/v1",
            "metadata": {"name": "n1", "namespace": "kube-node-lease"},
            "spec": {"holderIdentity": "n1-holder", "renewTime": "2024-01-01T11:59:50.000000Z"}})),
        _ => base(r),
    });
    let out = h.acc.describe(&known::nodes(), "n1").await.unwrap();
    let calls = related_calls(&h);
    let pods: Vec<&Req> = calls.iter().filter(|r| r.path == "/api/v1/pods").collect();
    assert_eq!(pods.len(), 2);
    for p in &pods {
        assert_eq!(
            p.q("fieldSelector"),
            Some("spec.nodeName=n1,status.phase!=Succeeded,status.phase!=Failed")
        );
        assert_eq!(p.q("limit"), Some("500"));
        assert_eq!(p.q("labelSelector"), None);
    }
    assert_eq!(pods[1].q("continue"), Some("page2"));
    assert!(calls.iter().any(|r| r.is(
        "GET",
        "/apis/coordination.k8s.io/v1/namespaces/kube-node-lease/leases/n1"
    )));
    assert!(out.contains("Non-terminated Pods:"), "{out}");
    assert!(out.contains("(2 in total)"), "{out}");
    assert!(out.contains("p-one") && out.contains("p-two"), "{out}");
    assert!(out.contains("Lease:\n"), "{out}");
    assert!(out.contains("n1-holder"), "{out}");
}

#[tokio::test]
async fn describe_node_tolerates_forbidden_lookups() {
    let h = Harness::new(|r| match r.path.as_str() {
        "/api/v1/nodes/n1" => ok(json!({"kind": "Node", "apiVersion": "v1",
            "metadata": {"name": "n1"}})),
        "/api/v1/pods" => status(403, "Forbidden", "pods is forbidden"),
        "/apis/coordination.k8s.io/v1/namespaces/kube-node-lease/leases/n1" => {
            not_found("leases.coordination.k8s.io \"n1\" not found")
        }
        "/api/v1/events" => status(403, "Forbidden", "events is forbidden"),
        _ => base(r),
    });
    let out = h.acc.describe(&known::nodes(), "n1").await.unwrap();
    assert!(out.starts_with("Name:"), "{out}");
    assert!(!out.contains("Non-terminated Pods:"), "{out}");
    assert!(!out.contains("Lease:"), "{out}");
}

#[tokio::test]
async fn describe_service_fetches_endpoint_slices() {
    let h = Harness::new(|r| match r.path.as_str() {
        "/api/v1/namespaces/default/services/web" => ok(json!({"kind": "Service",
            "apiVersion": "v1", "metadata": {"name": "web", "namespace": "default"},
            "spec": {"type": "ClusterIP", "clusterIP": "10.0.0.1",
                "ports": [{"name": "http", "port": 80, "protocol": "TCP", "targetPort": 8080}]}})),
        "/apis/discovery.k8s.io/v1/namespaces/default/endpointslices" => list(
            "EndpointSlice",
            "discovery.k8s.io/v1",
            vec![json!({"metadata": {"name": "web-x", "namespace": "default",
                "labels": {"kubernetes.io/service-name": "web"}},
            "endpoints": [{"addresses": ["10.1.0.7"], "conditions": {"ready": true}}],
            "ports": [{"name": "http", "port": 8080, "protocol": "TCP"}]})],
        ),
        _ => base(r),
    });
    let out = h
        .acc
        .describe(&known::services(), "default/web")
        .await
        .unwrap();
    let calls = related_calls(&h);
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(calls[0].is("GET", "/api/v1/namespaces/default/services/web"));
    assert!(calls[1].is(
        "GET",
        "/apis/discovery.k8s.io/v1/namespaces/default/endpointslices"
    ));
    assert_eq!(
        calls[1].q("labelSelector"),
        Some("kubernetes.io/service-name=web")
    );
    assert!(out.contains("Endpoints:         10.1.0.7:8080\n"), "{out}");
}

#[tokio::test]
async fn describe_ingress_fetches_backends() {
    let h = Harness::new(|r| match r.path.as_str() {
        "/apis/networking.k8s.io/v1/namespaces/default/ingresses/ing" => ok(json!({
        "kind": "Ingress", "apiVersion": "networking.k8s.io/v1",
        "metadata": {"name": "ing", "namespace": "default"},
        "spec": {"rules": [{"host": "a.example.com", "http": {"paths": [
            {"path": "/", "pathType": "Prefix",
             "backend": {"service": {"name": "web", "port": {"number": 80}}}},
            {"path": "/api", "pathType": "Prefix",
             "backend": {"service": {"name": "api", "port": {"name": "http"}}}}
        ]}}]}})),
        "/apis/discovery.k8s.io/v1/namespaces/default/endpointslices"
            if r.q("labelSelector") == Some("kubernetes.io/service-name=web") =>
        {
            list(
                "EndpointSlice",
                "discovery.k8s.io/v1",
                vec![json!({"metadata": {"name": "web-x", "namespace": "default",
                    "labels": {"kubernetes.io/service-name": "web"}},
                "endpoints": [{"addresses": ["10.1.0.1"], "conditions": {"ready": true}}],
                "ports": [{"name": "", "port": 8080, "protocol": "TCP"}]})],
            )
        }
        "/apis/discovery.k8s.io/v1/namespaces/default/endpointslices" => {
            status(403, "Forbidden", "endpointslices is forbidden")
        }
        "/api/v1/namespaces/default/services/web" => ok(json!({"kind": "Service",
            "apiVersion": "v1", "metadata": {"name": "web", "namespace": "default"},
            "spec": {"ports": [{"port": 80, "protocol": "TCP"}]}})),
        _ => base(r),
    });
    let out = h
        .acc
        .describe(&known::ingresses(), "default/ing")
        .await
        .unwrap();
    let calls = related_calls(&h);
    let sels: Vec<&str> = calls
        .iter()
        .filter(|r| r.path.ends_with("/endpointslices"))
        .filter_map(|r| r.q("labelSelector"))
        .collect();
    assert_eq!(
        sels,
        vec![
            "kubernetes.io/service-name=api",
            "kubernetes.io/service-name=web"
        ]
    );
    assert!(calls
        .iter()
        .any(|r| r.is("GET", "/api/v1/namespaces/default/services/api")));
    assert!(calls
        .iter()
        .any(|r| r.is("GET", "/api/v1/namespaces/default/services/web")));
    assert!(out.contains("web:80 (10.1.0.1:8080)"), "{out}");
    assert!(
        out.contains("api:http (<error: services \"api\" not found>)"),
        "{out}"
    );
}

#[tokio::test]
async fn describe_deployment_fetches_replica_sets() {
    let template = json!({"metadata": {"labels": {"app": "web"}},
        "spec": {"containers": [{"name": "c", "image": "nginx"}]}});
    let tpl = template.clone();
    let h = Harness::new(move |r| match r.path.as_str() {
        "/apis/apps/v1/namespaces/default/deployments/web" => ok(json!({
            "kind": "Deployment", "apiVersion": "apps/v1",
            "metadata": {"name": "web", "namespace": "default", "uid": "dep-uid"},
            "spec": {"replicas": 1, "selector": {"matchLabels": {"app": "web"},
                "matchExpressions": [{"key": "tier", "operator": "NotIn", "values": ["db"]}]},
                "template": tpl.clone(), "strategy": {"type": "Recreate"}},
            "status": {"replicas": 1}})),
        "/apis/apps/v1/namespaces/default/replicasets" => {
            let mut rs_tpl = tpl.clone();
            rs_tpl["metadata"]["labels"]["pod-template-hash"] = json!("abc");
            list(
                "ReplicaSet",
                "apps/v1",
                vec![
                    json!({"metadata": {"name": "web-abc", "namespace": "default",
                    "labels": {"app": "web", "pod-template-hash": "abc"},
                    "creationTimestamp": "2024-01-01T00:00:00Z",
                    "ownerReferences": [{"apiVersion": "apps/v1", "kind": "Deployment",
                        "name": "web", "uid": "dep-uid", "controller": true}]},
                "spec": {"replicas": 1, "selector": {"matchLabels": {"app": "web"}},
                    "template": rs_tpl},
                "status": {"replicas": 1}}),
                ],
            )
        }
        _ => base(r),
    });
    let out = h
        .acc
        .describe(&known::deployments(), "default/web")
        .await
        .unwrap();
    let calls = related_calls(&h);
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(calls[1].is("GET", "/apis/apps/v1/namespaces/default/replicasets"));
    assert_eq!(calls[1].q("labelSelector"), Some("app=web,tier notin (db)"));
    assert_eq!(calls[1].q("limit"), Some("500"));
    assert!(
        out.contains(
            "OldReplicaSets:    <none>\nNewReplicaSet:     web-abc (1/1 replicas created)\n"
        ),
        "{out}"
    );
}

#[tokio::test]
async fn describe_controllers_fetch_pods_by_selector() {
    let cases = [
        (
            known::replicasets(),
            "/apis/apps/v1/namespaces/default/replicasets/rs",
            json!({"kind": "ReplicaSet", "apiVersion": "apps/v1"}),
        ),
        (
            known::statefulsets(),
            "/apis/apps/v1/namespaces/default/statefulsets/rs",
            json!({"kind": "StatefulSet", "apiVersion": "apps/v1"}),
        ),
        (
            known::daemonsets(),
            "/apis/apps/v1/namespaces/default/daemonsets/rs",
            json!({"kind": "DaemonSet", "apiVersion": "apps/v1"}),
        ),
        (
            known::replicationcontrollers(),
            "/api/v1/namespaces/default/replicationcontrollers/rs",
            json!({"kind": "ReplicationController", "apiVersion": "v1"}),
        ),
    ];
    for (gvr, path, head) in cases {
        let rc = gvr.r() == "replicationcontrollers";
        let selector = if rc {
            json!({"app": "web"})
        } else {
            json!({"matchLabels": {"app": "web"}})
        };
        let mut obj = head;
        obj["metadata"] = json!({"name": "rs", "namespace": "default", "uid": "ctl-uid"});
        obj["spec"] = json!({"replicas": 3, "selector": selector,
            "template": {"metadata": {"labels": {"app": "web"}},
                "spec": {"containers": [{"name": "c", "image": "nginx"}]}}});
        obj["status"] = json!({"replicas": 3, "readyReplicas": 3});
        let p = path.to_string();
        let h = Harness::new(move |r| {
            if r.path == p {
                return ok(obj.clone());
            }
            match r.path.as_str() {
                "/api/v1/namespaces/default/pods" => list(
                    "Pod",
                    "v1",
                    vec![
                        pod_json("a", "default", "Running", Some("ctl-uid")),
                        pod_json("b", "default", "Pending", Some("ctl-uid")),
                        pod_json("c", "default", "Failed", Some("ctl-uid")),
                        pod_json("other", "default", "Running", Some("someone-else")),
                    ],
                ),
                _ => base(r),
            }
        });
        let out = h.acc.describe(&gvr, "default/rs").await.unwrap();
        let calls = related_calls(&h);
        assert_eq!(calls.len(), 2, "{gvr}: {calls:?}");
        assert!(
            calls[1].is("GET", "/api/v1/namespaces/default/pods"),
            "{gvr}"
        );
        assert_eq!(calls[1].q("labelSelector"), Some("app=web"), "{gvr}");
        assert!(
            out.contains("1 Running / 1 Waiting / 0 Succeeded / 1 Failed"),
            "{gvr}: {out}"
        );
    }
}

#[tokio::test]
async fn describe_pvc_fetches_namespace_pods() {
    let h = Harness::new(|r| match r.path.as_str() {
        "/api/v1/namespaces/default/persistentvolumeclaims/data" => ok(json!({
            "kind": "PersistentVolumeClaim", "apiVersion": "v1",
            "metadata": {"name": "data", "namespace": "default"},
            "spec": {"volumeName": "pv-1"}, "status": {"phase": "Bound"}})),
        "/api/v1/namespaces/default/pods" => list(
            "Pod",
            "v1",
            vec![pod_json("user-pod", "default", "Running", None)],
        ),
        _ => base(r),
    });
    let out = h
        .acc
        .describe(&known::persistentvolumeclaims(), "default/data")
        .await
        .unwrap();
    let calls = related_calls(&h);
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(calls[1].is("GET", "/api/v1/namespaces/default/pods"));
    assert_eq!(calls[1].q("labelSelector"), None);
    assert_eq!(calls[1].q("fieldSelector"), None);
    assert!(out.contains("Used By:"), "{out}");
    assert!(out.contains("user-pod"), "{out}");
}

#[tokio::test]
async fn describe_namespace_fetches_quotas_and_limits() {
    let h = Harness::new(|r| match r.path.as_str() {
        "/api/v1/namespaces/team" => ok(json!({"kind": "Namespace", "apiVersion": "v1",
            "metadata": {"name": "team"}, "status": {"phase": "Active"}})),
        "/api/v1/namespaces/team/resourcequotas" => list(
            "ResourceQuota",
            "v1",
            vec![json!({"metadata": {"name": "q1", "namespace": "team"},
                "spec": {"hard": {"pods": "10"}}, "status": {"hard": {"pods": "10"}, "used": {"pods": "2"}}})],
        ),
        "/api/v1/namespaces/team/limitranges" => list(
            "LimitRange",
            "v1",
            vec![json!({"metadata": {"name": "lr1", "namespace": "team"},
                "spec": {"limits": [{"type": "Container", "max": {"cpu": "2"}}]}})],
        ),
        _ => base(r),
    });
    let out = h.acc.describe(&known::namespaces(), "team").await.unwrap();
    let calls = related_calls(&h);
    assert!(calls
        .iter()
        .any(|r| r.is("GET", "/api/v1/namespaces/team/resourcequotas")));
    assert!(calls
        .iter()
        .any(|r| r.is("GET", "/api/v1/namespaces/team/limitranges")));
    assert!(out.contains("Resource Quotas\n"), "{out}");
    assert!(out.contains("q1"), "{out}");
    assert!(out.contains("Resource Limits\n"), "{out}");
}

// kubectl v0.35 no longer lists the namespace's secrets for a ServiceAccount.
#[tokio::test]
async fn describe_service_account_makes_no_secret_lookups() {
    let h = Harness::new(|r| match r.path.as_str() {
        "/api/v1/namespaces/default/serviceaccounts/sa" => ok(json!({
            "kind": "ServiceAccount", "apiVersion": "v1",
            "metadata": {"name": "sa", "namespace": "default", "uid": "sa-uid"},
            "secrets": [{"name": "gone"}]})),
        _ => base(r),
    });
    let out = h
        .acc
        .describe(&known::serviceaccounts(), "default/sa")
        .await
        .unwrap();
    let calls = related_calls(&h);
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert!(!out.contains("Tokens:"), "{out}");
}

#[tokio::test]
async fn describe_pod_makes_no_extra_lookups() {
    let h = Harness::new(|r| match r.path.as_str() {
        "/api/v1/namespaces/default/pods/p1" => ok(json!({"kind": "Pod", "apiVersion": "v1",
            "metadata": {"name": "p1", "namespace": "default"},
            "spec": {"containers": [{"name": "c", "image": "nginx"}]}})),
        _ => base(r),
    });
    let out = h.acc.describe(&known::pods(), "default/p1").await.unwrap();
    assert!(out.starts_with("Name:"), "{out}");
    let calls = related_calls(&h);
    assert_eq!(calls.len(), 1, "{calls:?}");
}

#[test]
fn user_agent_matches_client_go_default() {
    // client-go's form for the k9rs binary (field manager "k9rs").
    let ua = super::default_user_agent();
    assert!(
        ua.starts_with(&format!("k9rs/{} (", crate::version::VERSION)),
        "{ua}"
    );
    assert!(ua.ends_with(") kubernetes/$Format"), "{ua}");
    assert!(!ua.contains("aarch64") && !ua.contains("x86_64"), "{ua}");
}

/// APIClient.CheckConnectivity: every check hits /version (no 5 minute
/// cache), so a lost API server is noticed on the next cluster refresh.
#[tokio::test]
async fn connectivity_check_is_uncached() {
    let down = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let d = down.clone();
    let h = Harness::new(move |r| {
        if r.path == "/version" {
            if d.load(std::sync::atomic::Ordering::SeqCst) {
                return status(503, "ServiceUnavailable", "down");
            }
            return ok(json!({"gitVersion": "v1.30.0", "major": "1", "minor": "30"}));
        }
        base(r)
    });
    let versions = |h: &Harness| {
        h.log
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.path == "/version")
            .count()
    };
    h.acc.check_connectivity().await.unwrap();
    h.acc.check_connectivity().await.unwrap();
    assert_eq!(versions(&h), 2);
    down.store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(h.acc.check_connectivity().await.is_err());
    down.store(false, std::sync::atomic::Ordering::SeqCst);
    h.acc.check_connectivity().await.unwrap();
    assert_eq!(versions(&h), 4);
}

/// SetImages: a denied patch says "user is not authorized to patch a <kind>"
/// (dao pod.go also says "deployment"). The pod owner check matches k9s's
/// isControlled: any owner reference refuses, named after the first one.
#[tokio::test]
async fn set_images_denied_patch_messages() {
    let h = Harness::new(deny_all);
    let imgs = vec![ImageSpec {
        name: "c".into(),
        docker_image: "nginx:2".into(),
        init: false,
    }];
    for (g, what) in [
        (known::pods(), "deployment"),
        (known::deployments(), "deployment"),
        (known::statefulsets(), "statefulset"),
        (known::daemonsets(), "daemonset"),
    ] {
        let e = h
            .acc
            .set_images(&g, "ns/x", imgs.clone())
            .await
            .unwrap_err();
        assert_eq!(
            e.to_string(),
            format!("user is not authorized to patch a {what}")
        );
    }
}

/// watch.Factory.CanForResource: watched lists check RBAC first, so a
/// denial reads like k9s (fresh CanI error, then the cached
/// `[list] access denied on resource ...`) instead of a raw 403.
#[tokio::test]
async fn watched_list_checks_access_first() {
    let mut h = Harness::new(|r| match (r.method.as_str(), r.path.as_str()) {
        ("GET", "/api/v1") => ok(
            json!({"kind": "APIResourceList", "groupVersion": "v1", "resources": [
                {"name": "pods", "singularName": "pod", "namespaced": true, "kind": "Pod", "verbs": ["get", "list", "watch"]}
            ]}),
        ),
        _ => deny_all(r),
    });
    h.acc.informers = Some(informer::Informers::default());
    let e1 = h
        .acc
        .list(&known::pods(), "ns1", None)
        .await
        .unwrap_err()
        .to_string();
    assert!(e1.contains("access denied"), "{e1}");
    let e2 = h
        .acc
        .list(&known::pods(), "ns1", None)
        .await
        .unwrap_err()
        .to_string();
    assert_eq!(e2, "[list] access denied on resource \"ns1\":\"v1/pods\"");
    assert!(!h
        .calls()
        .iter()
        .any(|c| c.path == "/api/v1/namespaces/ns1/pods"));
}
