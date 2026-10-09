//! Data collection for a browser refresh (k9s model.Table.reconcile + the
//! dao listers for pseudo resources). Runs on the tokio runtime.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{anyhow, Context as _, Result};
use kube::api::DynamicObject;

use crate::k8s::{
    namespaced, Gvr, MetricsSnapshot, PortForwardInfo, SharedAccessor, CLUSTER_SCOPE,
};
use crate::model::{Header, ResEvent, Row, RowEvent, TableData};
use crate::render::{pseudo, render_all, Renderer};

use super::helpers::{
    has_config_map, has_pc, has_pvc, has_secret, helm_time, is_same_subject, obj_fqn,
    parse_helm_json, parse_rules, pod_spec, service_account_matches, PolicyRes,
};
use super::kind::Kind;
use super::table::cleanse_namespace;
use super::Runner;

/// Snapshot of everything a refresh needs.
#[derive(Clone)]
pub(crate) struct LoadSpec {
    pub kind: Kind,
    pub gvr: Gvr,
    /// Model namespace ("" all, "-" cluster scope).
    pub ns: String,
    pub path: String,
    pub instance: String,
    pub label: Option<String>,
    pub field: Option<String>,
    pub uid: Option<String>,
    pub has_metrics: bool,
    pub accessor: SharedAccessor,
    pub renderer: Arc<dyn Renderer>,
    pub server_table: fn(&crate::k8s::ServerTable, &str) -> (Header, Vec<Row>),
    pub run: Runner,
    pub aliases: Vec<(Gvr, Vec<String>)>,
    pub port_forwards: Vec<PortForwardInfo>,
    pub dir: PathBuf,
    pub subject: Option<(String, String)>,
    pub source: Option<(Gvr, String)>,
    pub kube_context: String,
    pub env: Vec<(String, String)>,
    /// Count pods per node (k9s disablePodCounting off).
    pub pod_counting: bool,
    /// Bench settings per port-forward id: (concurrency, requests, host, path).
    pub bench: HashMap<String, (i64, i64, String, String)>,
}

/// dao/workload.go resList.
fn workload_gvrs() -> Vec<Gvr> {
    [
        "v1/pods",
        "v1/services",
        "apps/v1/daemonsets",
        "apps/v1/statefulsets",
        "apps/v1/deployments",
        "apps/v1/replicasets",
    ]
    .iter()
    .map(|s| Gvr::parse(s))
    .collect()
}

fn list_ns(ns: &str) -> String {
    let ns = cleanse_namespace(ns);
    if ns == CLUSTER_SCOPE {
        String::new()
    } else {
        ns
    }
}

fn s<'a>(v: &'a serde_json::Value, p: &str) -> &'a str {
    v.pointer(p).and_then(|x| x.as_str()).unwrap_or("")
}

/// Fetch, render and package the table for `spec`.
pub(crate) async fn load(spec: LoadSpec) -> Result<TableData> {
    let (header, rows) = collect(&spec).await?;
    let mut td = TableData::new(spec.gvr.clone(), header, spec.ns.clone());
    td.rows = rows
        .into_iter()
        .map(|r| RowEvent::new(ResEvent::Add, r))
        .collect();
    Ok(td)
}

fn render(
    spec: &LoadSpec,
    objs: &[DynamicObject],
    mx: Option<&MetricsSnapshot>,
) -> (Header, Vec<Row>) {
    let header = spec.renderer.header(&spec.ns);
    let rows = render_all(spec.renderer.as_ref(), &spec.gvr, objs, &spec.ns, mx);
    (header, rows)
}

async fn collect(spec: &LoadSpec) -> Result<(Header, Vec<Row>)> {
    let acc = &spec.accessor;
    match spec.kind {
        Kind::Container => {
            let pod = acc
                .get(&Gvr::parse("v1/pods"), &spec.path)
                .await
                .with_context(|| format!("failed to locate pod {:?}", spec.path))?;
            let objs = containers_of(&pod);
            let mx = if spec.has_metrics {
                let (ns, _) = namespaced(&spec.path);
                let pods = acc.pod_metrics(ns).await.unwrap_or_default();
                Some(MetricsSnapshot {
                    pods,
                    nodes: HashMap::new(),
                })
            } else {
                None
            };
            Ok(render(spec, &objs, mx.as_ref()))
        }
        Kind::Context => {
            let objs: Vec<_> = acc.contexts().await?.iter().map(pseudo::context).collect();
            Ok(render(spec, &objs, None))
        }
        Kind::Alias => {
            let objs: Vec<_> = spec
                .aliases
                .iter()
                .map(|(g, a)| pseudo::alias(g, a.clone()))
                .collect();
            Ok(render(spec, &objs, None))
        }
        Kind::PortForward => {
            let ns = cleanse_namespace(&spec.ns);
            let objs: Vec<_> =
                spec.port_forwards
                    .iter()
                    .filter(|pf| ns.is_empty() || ns == CLUSTER_SCOPE || pf.spec.namespace == ns)
                    .filter(|pf| {
                        spec.path.is_empty() || {
                            let pod = crate::k8s::fqn(&pf.spec.namespace, &pf.spec.pod);
                            spec.path == pod
                                || spec.path == format!("{pod}|{}", pf.spec.container)
                                || spec.path == pf.spec.id()
                        }
                    })
                    .map(|pf| {
                        let (c, n, host, path) = spec
                            .bench
                            .get(&pf.spec.id())
                            .cloned()
                            .unwrap_or((1, 200, String::new(), "/".into()));
                        let o = pseudo::port_forward(pf, c, n);
                        let o = pseudo::with_field(o, "benchHost", serde_json::json!(host));
                        pseudo::with_field(o, "benchPath", serde_json::json!(path))
                    })
                    .collect();
            Ok(render(spec, &objs, None))
        }
        Kind::ScreenDump => {
            std::fs::create_dir_all(&spec.dir).ok();
            let dir = spec.dir.to_string_lossy().to_string();
            let mut objs = Vec::new();
            for e in std::fs::read_dir(&spec.dir).with_context(|| format!("reading {dir}"))? {
                let e = e?;
                let md = e.metadata()?;
                let modified: chrono::DateTime<chrono::Utc> = md
                    .modified()
                    .map(Into::into)
                    .unwrap_or_else(|_| chrono::Utc::now());
                objs.push(pseudo::screen_dump(
                    &dir,
                    &e.file_name().to_string_lossy(),
                    modified,
                ));
            }
            Ok(render(spec, &objs, None))
        }
        Kind::Benchmark => {
            let mut objs = Vec::new();
            let prefix = if spec.path.is_empty() {
                String::new()
            } else {
                let rx = regex::Regex::new(r"[:|]+").expect("rx");
                rx.replace_all(&spec.path.replacen('/', "_", 1), "_")
                    .to_string()
            };
            if let Ok(rd) = std::fs::read_dir(&spec.dir) {
                for e in rd {
                    let e = e?;
                    let file = e.file_name().to_string_lossy().to_string();
                    if !file.starts_with(&prefix) {
                        continue;
                    }
                    let md = e.metadata()?;
                    let modified: chrono::DateTime<chrono::Utc> = md
                        .modified()
                        .map(Into::into)
                        .unwrap_or_else(|_| chrono::Utc::now());
                    let tt: Vec<&str> = file.split('_').collect();
                    let ns = tt.first().copied().unwrap_or("");
                    let res = tt.get(1).copied().unwrap_or("");
                    let path = e.path().to_string_lossy().to_string();
                    objs.push(pseudo::benchmark(ns, res, &path, &file, modified));
                }
            }
            Ok(render(spec, &objs, None))
        }
        Kind::Dir => {
            let rx = regex::Regex::new(r".*\.(yml|yaml|json)").expect("rx");
            let mut objs = Vec::new();
            for e in std::fs::read_dir(&spec.dir)
                .with_context(|| format!("reading {}", spec.dir.display()))?
            {
                let e = e?;
                let name = e.file_name().to_string_lossy().to_string();
                let md = e.metadata()?;
                if name.starts_with('.') || (!md.is_dir() && !rx.is_match(&name)) {
                    continue;
                }
                let modified: chrono::DateTime<chrono::Utc> = md
                    .modified()
                    .map(Into::into)
                    .unwrap_or_else(|_| chrono::Utc::now());
                let path = e.path().to_string_lossy().to_string();
                objs.push(pseudo::dir_entry(
                    &path,
                    &name,
                    md.is_dir(),
                    md.len(),
                    modified,
                ));
            }
            Ok(render(spec, &objs, None))
        }
        Kind::Helm => {
            let mut argv = vec![
                "helm".to_string(),
                "list".into(),
                "-o".into(),
                "json".into(),
            ];
            let ns = list_ns(&spec.ns);
            if ns.is_empty() {
                argv.push("-A".into());
            } else {
                argv.extend(["-n".into(), ns]);
            }
            argv.extend(helm_ctx(spec));
            let raw = (spec.run)(argv, spec.env.clone()).await?;
            let objs: Vec<_> = parse_helm_json(&raw)?
                .iter()
                .map(|r| {
                    pseudo::helm_release(
                        &r.name,
                        &r.namespace,
                        &r.revision,
                        &r.status,
                        &r.chart,
                        &r.app_version,
                        &helm_time(&r.updated),
                    )
                })
                .collect();
            Ok(render(spec, &objs, None))
        }
        Kind::HelmHistory => {
            let (ns, name) = namespaced(&spec.path);
            let mut argv = vec![
                "helm".to_string(),
                "history".into(),
                name.to_string(),
                "-o".into(),
                "json".into(),
            ];
            if !ns.is_empty() {
                argv.extend(["-n".into(), ns.to_string()]);
            }
            argv.extend(helm_ctx(spec));
            let raw = (spec.run)(argv, spec.env.clone()).await?;
            let objs: Vec<_> = parse_helm_json(&raw)?
                .iter()
                .map(|r| {
                    let mut o = pseudo::helm_history(
                        &r.revision,
                        &r.status,
                        &r.chart,
                        &r.app_version,
                        &r.updated,
                        &r.description,
                    );
                    if !ns.is_empty() {
                        o.metadata.namespace = Some(ns.to_string());
                    }
                    pseudo::with_field(o, "release", serde_json::json!(name))
                })
                .collect();
            Ok(render(spec, &objs, None))
        }
        Kind::Rules => {
            let (gvr, path) = spec
                .source
                .clone()
                .ok_or_else(|| anyhow!("expecting a context gvr"))?;
            let pp = rbac_rules(acc, &gvr, &path).await?;
            let objs = policies_to_objs(&pp);
            Ok(render(spec, &objs, None))
        }
        Kind::Policy => {
            let (kind, name) = spec
                .subject
                .clone()
                .ok_or_else(|| anyhow!("expecting a context subject kind"))?;
            let pp = subject_policies(acc, &kind, &name).await?;
            let objs = policies_to_objs(&pp);
            Ok(render(spec, &objs, None))
        }
        Kind::User | Kind::Group => {
            let kind = if spec.kind == Kind::User {
                "User"
            } else {
                "Group"
            };
            let objs = subjects(acc, kind).await?;
            Ok(render(spec, &objs, None))
        }
        Kind::Reference => {
            let (gvr, path) = spec
                .source
                .clone()
                .ok_or_else(|| anyhow!("no context for gvr found"))?;
            let refs = scan_refs(acc, &gvr, &path).await?;
            let (ns, _) = namespaced(&path);
            let objs: Vec<_> = refs
                .iter()
                .map(|(g, fqn)| pseudo::reference(ns, namespaced(fqn).1, g))
                .collect();
            Ok(render(spec, &objs, None))
        }
        Kind::Workload => {
            let ns = list_ns(&spec.ns);
            let mut objs = Vec::new();
            for g in workload_gvrs() {
                objs.extend(acc.list(&g, &ns, spec.label.as_deref()).await?);
            }
            Ok(render(spec, &objs, None))
        }
        _ => k8s_rows(spec).await,
    }
}

fn helm_ctx(spec: &LoadSpec) -> Vec<String> {
    if spec.kube_context.is_empty() {
        Vec::new()
    } else {
        vec!["--kube-context".into(), spec.kube_context.clone()]
    }
}

/// Regular API resources (generic server tables or typed renderers).
async fn k8s_rows(spec: &LoadSpec) -> Result<(Header, Vec<Row>)> {
    let acc = &spec.accessor;
    let ns = list_ns(&spec.ns);
    if spec.renderer.is_generic() && spec.instance.is_empty() {
        if let Some(t) = acc
            .list_table(&spec.gvr, &ns, spec.label.as_deref())
            .await?
        {
            return Ok((spec.server_table)(&t, &spec.ns));
        }
    }
    let mut objs = if spec.instance.is_empty() {
        acc.list(&spec.gvr, &ns, spec.label.as_deref()).await?
    } else {
        vec![acc.get(&spec.gvr, &spec.instance).await?]
    };
    if let Some(field) = &spec.field {
        if let Some((k, v)) = field.split_once('=') {
            let ptr = format!("/{}", k.replace('.', "/"));
            objs.retain(|o| o.data.pointer(&ptr).and_then(|x| x.as_str()) == Some(v));
        }
    }
    if let Some(uid) = &spec.uid {
        objs.retain(|o| {
            o.metadata
                .owner_references
                .as_ref()
                .map(|oo| oo.iter().any(|r| &r.uid == uid))
                .unwrap_or(false)
        });
    }
    let mx = if spec.has_metrics {
        match spec.gvr.r() {
            "pods" => Some(MetricsSnapshot {
                pods: acc.pod_metrics(&ns).await.unwrap_or_default(),
                nodes: HashMap::new(),
            }),
            "nodes" => Some(MetricsSnapshot {
                pods: HashMap::new(),
                nodes: acc.node_metrics().await.unwrap_or_default(),
            }),
            _ => None,
        }
    } else {
        None
    };
    if spec.gvr.r() == "nodes" && spec.pod_counting {
        let pods = acc
            .list(&Gvr::parse("v1/pods"), "", None)
            .await
            .unwrap_or_default();
        let mut counts: HashMap<String, i64> = HashMap::new();
        for p in &pods {
            let node = s(&p.data, "/spec/nodeName");
            if !node.is_empty() {
                *counts.entry(node.to_string()).or_default() += 1;
            }
        }
        objs = objs
            .into_iter()
            .map(|o| {
                let n = counts
                    .get(o.metadata.name.as_deref().unwrap_or(""))
                    .copied()
                    .unwrap_or(0);
                pseudo::with_field(o, "podCount", serde_json::json!(n))
            })
            .collect();
    }
    Ok(render(spec, &objs, mx.as_ref()))
}

/// dao.Container.List: init, main then ephemeral containers.
pub(crate) fn containers_of(pod: &DynamicObject) -> Vec<DynamicObject> {
    let mut out = Vec::new();
    let status = |key: &str, name: &str| -> Option<serde_json::Value> {
        pod.data
            .pointer(&format!("/status/{key}"))
            .and_then(|v| v.as_array())
            .and_then(|a| {
                a.iter()
                    .find(|c| c.get("name").and_then(|n| n.as_str()) == Some(name))
                    .cloned()
            })
    };
    let groups = [
        ("initContainers", "initContainerStatuses", true, false),
        ("containers", "containerStatuses", false, false),
        (
            "ephemeralContainers",
            "ephemeralContainerStatuses",
            false,
            true,
        ),
    ];
    for (spec_key, status_key, init, eph) in groups {
        let mut idx = 0;
        if let Some(cc) = pod
            .data
            .pointer(&format!("/spec/{spec_key}"))
            .and_then(|v| v.as_array())
        {
            for c in cc {
                let name = c.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let st = status(status_key, name);
                out.push(pseudo::container(pod, c, st.as_ref(), init, eph, idx));
                idx += 1;
            }
        }
    }
    out
}

fn policies_to_objs(pp: &[PolicyRes]) -> Vec<DynamicObject> {
    pp.iter()
        .map(|p| {
            let kind = match p.binding.split_once(':') {
                Some(("CR", _)) => "ClusterRole",
                Some(("RO", _)) => "Role",
                _ => "",
            };
            let ns = if p.namespace == CLUSTER_SCOPE || p.namespace == super::table::NOT_NAMESPACED
            {
                ""
            } else {
                p.namespace.as_str()
            };
            pseudo::policy_rule(
                ns,
                &p.group,
                &p.resource,
                &p.resource_names,
                &p.non_resource_urls,
                &p.verbs,
                &p.binding,
                kind,
            )
        })
        .collect()
}

fn gvr(s: &str) -> Gvr {
    Gvr::parse(s)
}

const CR: &str = "rbac.authorization.k8s.io/v1/clusterroles";
const CRB: &str = "rbac.authorization.k8s.io/v1/clusterrolebindings";
const RO: &str = "rbac.authorization.k8s.io/v1/roles";
const ROB: &str = "rbac.authorization.k8s.io/v1/rolebindings";

/// dao.Rbac.List for a role/binding path.
async fn rbac_rules(acc: &SharedAccessor, src: &Gvr, path: &str) -> Result<Vec<PolicyRes>> {
    let rules_of = |o: &DynamicObject| parse_rules(CLUSTER_SCOPE, "-", o.data.get("rules"));
    match src.r() {
        "clusterrolebindings" => {
            let crb = acc.get(&gvr(CRB), path).await?;
            let cr = acc.get(&gvr(CR), s(&crb.data, "/roleRef/name")).await?;
            Ok(rules_of(&cr))
        }
        "rolebindings" => {
            let rb = acc.get(&gvr(ROB), path).await?;
            let role = s(&rb.data, "/roleRef/name");
            if s(&rb.data, "/roleRef/kind") == "ClusterRole" {
                return Ok(rules_of(&acc.get(&gvr(CR), role).await?));
            }
            let ns = rb.metadata.namespace.clone().unwrap_or_default();
            Ok(rules_of(
                &acc.get(&gvr(RO), &crate::k8s::fqn(&ns, role)).await?,
            ))
        }
        "clusterroles" => Ok(rules_of(&acc.get(&gvr(CR), path).await?)),
        "roles" => Ok(rules_of(&acc.get(&gvr(RO), path).await?)),
        other => Err(anyhow!("expecting clusterrole/role but found {other}")),
    }
}

fn subjects_of(o: &DynamicObject) -> Vec<serde_json::Value> {
    o.data
        .get("subjects")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
}

/// dao.Policy.List: rules granted to a subject through bindings.
async fn subject_policies(acc: &SharedAccessor, kind: &str, name: &str) -> Result<Vec<PolicyRes>> {
    let (ns, n) = namespaced(name);
    let crbs = acc.list(&gvr(CRB), "", None).await?;
    let crs = acc.list(&gvr(CR), "", None).await?;
    let mut role_names = Vec::new();
    for crb in &crbs {
        let bns = crb.metadata.namespace.clone().unwrap_or_default();
        for sub in subjects_of(crb) {
            if is_same_subject(kind, ns, &bns, n, &sub) {
                role_names.push(s(&crb.data, "/roleRef/name").to_string());
            }
        }
    }
    let mut rows = Vec::new();
    for cr in &crs {
        let cname = cr.metadata.name.clone().unwrap_or_default();
        if role_names.contains(&cname) {
            rows.extend(parse_rules(
                super::table::NOT_NAMESPACED,
                &format!("CR:{cname}"),
                cr.data.get("rules"),
            ));
        }
    }
    let rbs = acc.list(&gvr(ROB), "", None).await?;
    let mut rb_map: HashMap<String, String> = HashMap::new();
    for rb in &rbs {
        let bns = rb.metadata.namespace.clone().unwrap_or_default();
        for sub in subjects_of(rb) {
            if is_same_subject(kind, ns, &bns, n, &sub) {
                rb_map.insert(
                    format!(
                        "{}:{}",
                        s(&rb.data, "/roleRef/kind"),
                        s(&rb.data, "/roleRef/name")
                    ),
                    bns.clone(),
                );
            }
        }
    }
    for cr in &crs {
        let cname = cr.metadata.name.clone().unwrap_or_default();
        if let Some(rb_ns) = rb_map.get(&format!("ClusterRole:{cname}")) {
            rows.extend(parse_rules(
                rb_ns,
                &format!("CR:{cname}"),
                cr.data.get("rules"),
            ));
        }
    }
    for ro in acc.list(&gvr(RO), "", None).await? {
        let rname = ro.metadata.name.clone().unwrap_or_default();
        if rb_map.contains_key(&format!("Role:{rname}")) {
            let rns = ro.metadata.namespace.clone().unwrap_or_default();
            rows.extend(parse_rules(
                &rns,
                &format!("RO:{rname}"),
                ro.data.get("rules"),
            ));
        }
    }
    Ok(rows)
}

/// dao.Subject.List.
async fn subjects(acc: &SharedAccessor, kind: &str) -> Result<Vec<DynamicObject>> {
    let mut out: Vec<(String, String, String)> = Vec::new();
    let mut upsert = |name: String, loc: String, bkind: &str| {
        if let Some(e) = out.iter_mut().find(|(n, _, _)| *n == name) {
            *e = (name, loc, bkind.to_string());
        } else {
            out.push((name, loc, bkind.to_string()));
        }
    };
    for (g, bkind) in [(CRB, "ClusterRoleBinding"), (ROB, "RoleBinding")] {
        for b in acc.list(&gvr(g), "", None).await? {
            for sub in subjects_of(&b) {
                if s(&sub, "/kind") != kind {
                    continue;
                }
                upsert(
                    s(&sub, "/name").to_string(),
                    b.metadata.name.clone().unwrap_or_default(),
                    bkind,
                );
            }
        }
    }
    Ok(out
        .into_iter()
        .map(|(name, loc, bkind)| pseudo::subject(&name, kind, &loc, &bkind))
        .collect())
}

/// dao.ScanForRefs / ScanForSARefs over deployments, daemonsets,
/// statefulsets, cronjobs and jobs.
pub(crate) async fn scan_refs(
    acc: &SharedAccessor,
    src: &Gvr,
    path: &str,
) -> Result<Vec<(String, String)>> {
    let (ns, n) = namespaced(path);
    let mut refs = Vec::new();
    let scanners = [
        "apps/v1/deployments",
        "apps/v1/daemonsets",
        "apps/v1/statefulsets",
        "batch/v1/cronjobs",
        "batch/v1/jobs",
    ];
    for g in scanners {
        let g = gvr(g);
        let objs = match acc.list(&g, ns, None).await {
            Ok(o) => o,
            Err(e) => {
                tracing::error!(gvr = %g, error = %e, "reference scan failed");
                continue;
            }
        };
        for o in objs {
            let Some(spec) = pod_spec(&o) else {
                continue;
            };
            let hit = match src.r() {
                "configmaps" => has_config_map(spec, n),
                "secrets" => {
                    has_secret(spec, n) || {
                        let sa = spec
                            .get("serviceAccountName")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let ons = o.metadata.namespace.clone().unwrap_or_default();
                        if sa.is_empty() {
                            false
                        } else {
                            match acc
                                .get(&gvr("v1/serviceaccounts"), &crate::k8s::fqn(&ons, sa))
                                .await
                            {
                                Ok(sa) => sa
                                    .data
                                    .get("secrets")
                                    .and_then(|v| v.as_array())
                                    .map(|a| {
                                        a.iter().any(|r| {
                                            s(r, "/name") == n
                                                && (s(r, "/namespace").is_empty()
                                                    || s(r, "/namespace") == ons)
                                        })
                                    })
                                    .unwrap_or(false),
                                Err(_) => false,
                            }
                        }
                    }
                }
                "persistentvolumeclaims" => has_pvc(spec, n),
                "priorityclasses" => has_pc(spec, n),
                "serviceaccounts" => service_account_matches(spec, n),
                _ => false,
            };
            if hit {
                refs.push((g.to_string(), obj_fqn(&o)));
            }
        }
    }
    Ok(refs)
}
