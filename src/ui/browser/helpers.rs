//! Object helpers mirroring k9s view/dao helpers: pod status, containers,
//! selectors, plugin env substitution, rbac rules, reference scans, helm
//! output parsing, CSV dumps.

use std::collections::BTreeMap;

use kube::api::DynamicObject;
use serde_json::Value;

use crate::model::{Header, Row};

/// k8s annotation selecting the default container.
pub(crate) const DEFAULT_CONTAINER_ANNOTATION: &str = "kubectl.kubernetes.io/default-container";

/// `ns/name` of an object (client.FQN).
pub(crate) fn obj_fqn(o: &DynamicObject) -> String {
    crate::k8s::fqn(
        o.metadata.namespace.as_deref().unwrap_or(""),
        o.metadata.name.as_deref().unwrap_or(""),
    )
}

/// Pod spec of a pod or of a workload's pod template.
pub(crate) fn pod_spec(o: &DynamicObject) -> Option<&Value> {
    let kind = o.types.as_ref().map(|t| t.kind.as_str()).unwrap_or("");
    match kind {
        "Pod" => o.data.get("spec"),
        "CronJob" => o.data.pointer("/spec/jobTemplate/spec/template/spec"),
        _ => o
            .data
            .pointer("/spec/template/spec")
            .or_else(|| o.data.get("spec").filter(|s| s.get("containers").is_some())),
    }
}

fn names(v: Option<&Value>) -> Vec<String> {
    v.and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|c| c.get("name").and_then(|n| n.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// dao.GetDefaultContainer.
pub(crate) fn default_container(o: &DynamicObject, spec: &Value) -> Option<String> {
    let want = o
        .metadata
        .annotations
        .as_ref()?
        .get(DEFAULT_CONTAINER_ANNOTATION)?;
    names(spec.get("containers"))
        .into_iter()
        .find(|n| n == want)
}

/// view.fetchContainers: default container first, then containers, sidecar
/// (or all) init containers, ephemeral containers.
pub(crate) fn fetch_containers(o: &DynamicObject, spec: &Value, all: bool) -> Vec<String> {
    let mut out = Vec::new();
    let def = default_container(o, spec);
    if let Some(d) = &def {
        out.push(d.clone());
    }
    for n in names(spec.get("containers")) {
        if Some(&n) != def.as_ref() {
            out.push(n);
        }
    }
    if let Some(inits) = spec.get("initContainers").and_then(|v| v.as_array()) {
        for c in inits {
            let sidecar = c.get("restartPolicy").and_then(|v| v.as_str()) == Some("Always");
            if all || sidecar {
                if let Some(n) = c.get("name").and_then(|v| v.as_str()) {
                    out.push(n.to_string());
                }
            }
        }
    }
    out.extend(names(spec.get("ephemeralContainers")));
    out
}

fn has_ready_condition(o: &DynamicObject) -> bool {
    o.data
        .pointer("/status/conditions")
        .and_then(|v| v.as_array())
        .map(|cc| {
            cc.iter().any(|c| {
                c.get("type").and_then(|v| v.as_str()) == Some("Ready")
                    && c.get("status").and_then(|v| v.as_str()) == Some("True")
            })
        })
        .unwrap_or(false)
}

fn s<'a>(v: &'a Value, p: &str) -> &'a str {
    v.pointer(p).and_then(|x| x.as_str()).unwrap_or("")
}

fn terminated_reason(t: &Value, prefix: &str) -> String {
    let reason = s(t, "/reason");
    if !reason.is_empty() {
        return format!("{prefix}{reason}");
    }
    let signal = t.get("signal").and_then(|v| v.as_i64()).unwrap_or(0);
    if signal != 0 {
        format!("{prefix}Signal:{signal}")
    } else {
        let code = t.get("exitCode").and_then(|v| v.as_i64()).unwrap_or(0);
        format!("{prefix}ExitCode:{code}")
    }
}

/// render.PodStatus.
pub(crate) fn pod_status(o: &DynamicObject) -> String {
    let st = o.data.get("status").cloned().unwrap_or(Value::Null);
    let mut reason = s(&st, "/phase").to_string();
    if !s(&st, "/reason").is_empty() {
        reason = s(&st, "/reason").to_string();
    }
    if let Some(cc) = st.get("conditions").and_then(|v| v.as_array()) {
        for c in cc {
            if s(c, "/type") == "PodScheduled" && s(c, "/reason") == "SchedulingGated" {
                reason = "SchedulingGated".into();
            }
        }
    }
    let init_total = o
        .data
        .pointer("/spec/initContainers")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let mut initializing = false;
    if let Some(ics) = st.get("initContainerStatuses").and_then(|v| v.as_array()) {
        for (i, c) in ics.iter().enumerate() {
            let term = c.pointer("/state/terminated");
            let wait = c.pointer("/state/waiting");
            if let Some(t) = term {
                if t.get("exitCode").and_then(|v| v.as_i64()).unwrap_or(0) == 0 {
                    continue;
                }
                reason = terminated_reason(t, "Init:");
                initializing = true;
            } else if let Some(w) = wait.filter(|w| {
                let r = s(w, "/reason");
                !r.is_empty() && r != "PodInitializing"
            }) {
                reason = format!("Init:{}", s(w, "/reason"));
                initializing = true;
            } else {
                reason = format!("Init:{i}/{init_total}");
                initializing = true;
            }
            break;
        }
    }
    if !initializing {
        let mut has_running = false;
        if let Some(cs) = st.get("containerStatuses").and_then(|v| v.as_array()) {
            for c in cs.iter().rev() {
                let wait_reason = c
                    .pointer("/state/waiting/reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if !wait_reason.is_empty() {
                    reason = wait_reason.to_string();
                } else if let Some(t) = c.pointer("/state/terminated") {
                    reason = terminated_reason(t, "");
                } else if c.get("ready").and_then(|v| v.as_bool()).unwrap_or(false)
                    && c.pointer("/state/running").is_some()
                {
                    has_running = true;
                }
            }
        }
        if reason == "Completed" && has_running {
            reason = if has_ready_condition(o) {
                "Running".into()
            } else {
                "NoReady".into()
            };
        }
    }
    if o.metadata.deletion_timestamp.is_some() {
        if s(&st, "/reason") == "NodeLost" {
            reason = "Unknown".into();
        } else {
            reason = "Terminating".into();
        }
    }
    reason
}

/// render.Pod.Phase == Running (podIsRunning).
pub(crate) fn pod_is_running(o: &DynamicObject) -> bool {
    if o.metadata.deletion_timestamp.is_some() {
        return false;
    }
    let st = o.data.get("status").cloned().unwrap_or(Value::Null);
    let mut status = s(&st, "/phase").to_string();
    if !s(&st, "/reason").is_empty() {
        status = s(&st, "/reason").to_string();
    }
    let mut running = false;
    if let Some(cs) = st.get("containerStatuses").and_then(|v| v.as_array()) {
        for c in cs.iter().rev() {
            let wait_reason = c
                .pointer("/state/waiting/reason")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if !wait_reason.is_empty() {
                status = wait_reason.to_string();
            } else if let Some(t) = c.pointer("/state/terminated") {
                status = terminated_reason(t, "");
            } else if c.get("ready").and_then(|v| v.as_bool()).unwrap_or(false)
                && c.pointer("/state/running").is_some()
            {
                running = true;
            }
        }
    }
    if status == "Completed" && running {
        status = "Running".into();
    }
    status == "Running"
}

/// Label selector (metav1.LabelSelectorAsSelector(...).String()).
pub(crate) fn label_selector_string(sel: &Value) -> Option<String> {
    let mut reqs: Vec<(String, String)> = Vec::new();
    if let Some(ml) = sel.get("matchLabels").and_then(|v| v.as_object()) {
        for (k, v) in ml {
            reqs.push((k.clone(), format!("{k}={}", v.as_str().unwrap_or(""))));
        }
    }
    if let Some(me) = sel.get("matchExpressions").and_then(|v| v.as_array()) {
        for e in me {
            let key = s(e, "/key").to_string();
            let mut vals: Vec<String> = e
                .get("values")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            vals.sort();
            let r = match s(e, "/operator") {
                "In" => format!("{key} in ({})", vals.join(",")),
                "NotIn" => format!("{key} notin ({})", vals.join(",")),
                "Exists" => key.clone(),
                "DoesNotExist" => format!("!{key}"),
                _ => continue,
            };
            reqs.push((key, r));
        }
    }
    if reqs.is_empty() {
        return None;
    }
    reqs.sort_by(|a, b| a.0.cmp(&b.0));
    Some(
        reqs.into_iter()
            .map(|(_, r)| r)
            .collect::<Vec<_>>()
            .join(","),
    )
}

/// labels.SelectorFromSet(map).String().
pub(crate) fn map_selector_string(m: &serde_json::Map<String, Value>) -> Option<String> {
    if m.is_empty() {
        return None;
    }
    let mut kv: Vec<String> = m
        .iter()
        .map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or("")))
        .collect();
    kv.sort();
    Some(kv.join(","))
}

/// Pod OS from a node selector / labels (osFromSelector).
pub(crate) fn os_from_labels(m: Option<&serde_json::Map<String, Value>>) -> Option<String> {
    let m = m?;
    m.get("beta.kubernetes.io/os")
        .or_else(|| m.get("kubernetes.io/os"))
        .and_then(|v| v.as_str())
        .map(String::from)
}

/// k9s view.Env with Substitute.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Env(pub BTreeMap<String, String>);

impl Env {
    pub(crate) fn set(&mut self, k: &str, v: impl Into<String>) {
        self.0.insert(k.to_string(), v.into());
    }

    /// Replace `$KEY`, `$!KEY`, `${KEY}` and `${!KEY}` occurrences.
    pub(crate) fn substitute(&self, arg: &str) -> String {
        let rx =
            regex::Regex::new(r"(\$(!?)([\w\-]+))|(\$\{(!?)([\w\-%/: ]+)\})").expect("env regex");
        let mut matches: Vec<(String, String, bool)> = rx
            .captures_iter(arg)
            .map(|c| {
                let whole = c.get(0).map(|m| m.as_str().to_string()).unwrap_or_default();
                if c.get(1).is_some() {
                    (whole, c[3].to_string(), &c[2] == "!")
                } else {
                    (whole, c[6].to_string(), &c[5] == "!")
                }
            })
            .collect();
        matches.sort_by_key(|m| std::cmp::Reverse(m.0.len()));
        let mut out = arg.to_string();
        for (whole, key, inverse) in matches {
            let Some(v) = self.0.get(&key.to_uppercase()) else {
                continue;
            };
            let v = match v.parse::<bool>() {
                Ok(b) => (if inverse { !b } else { b }).to_string(),
                Err(_) => v.clone(),
            };
            out = out.replace(&whole, &v);
        }
        out
    }
}

/// defaultEnv: kube identity + NAMESPACE/NAME + COL-<header> values.
pub(crate) fn default_env(
    info: &crate::k8s::ClusterInfo,
    kubeconfig: &str,
    path: &str,
    header: &Header,
    row: Option<&Row>,
) -> Env {
    let mut env = Env::default();
    let na = |s: &str| {
        if s.is_empty() {
            crate::model::NA_VALUE.to_string()
        } else {
            s.to_string()
        }
    };
    env.set("CONTEXT", na(&info.context));
    env.set("CLUSTER", na(&info.cluster));
    env.set("USER", na(&info.user));
    env.set("GROUPS", crate::model::NA_VALUE);
    env.set("KUBECONFIG", kubeconfig);
    let (ns, n) = crate::k8s::namespaced(path);
    env.set("NAMESPACE", ns);
    env.set("NAME", n);
    if let Some(row) = row {
        for (i, h) in header.iter().enumerate() {
            if let Some(f) = row.fields.get(i) {
                env.set(&format!("COL-{}", h.name), f.clone());
            }
        }
    }
    env
}

/// A k9s render.PolicyRes before conversion to a pseudo object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PolicyRes {
    pub namespace: String,
    pub binding: String,
    pub resource: String,
    pub group: String,
    pub resource_names: Vec<String>,
    pub non_resource_urls: Vec<String>,
    pub verbs: Vec<String>,
}

impl PolicyRes {
    fn key(&self) -> String {
        format!(
            "{}/{}/{}",
            self.group,
            self.resource,
            self.resource_names.join(",")
        )
    }
}

fn upsert(pp: &mut Vec<PolicyRes>, p: PolicyRes) {
    if let Some(existing) = pp.iter_mut().find(|x| x.key() == p.key()) {
        for v in p.verbs {
            if !existing.verbs.contains(&v) {
                existing.verbs.push(v);
            }
        }
        return;
    }
    pp.push(p);
}

fn strs(v: Option<&Value>) -> Vec<String> {
    v.and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// dao.parseRules.
pub(crate) fn parse_rules(ns: &str, binding: &str, rules: Option<&Value>) -> Vec<PolicyRes> {
    let mut pp = Vec::new();
    let Some(rules) = rules.and_then(|v| v.as_array()) else {
        return pp;
    };
    for rule in rules {
        let verbs = strs(rule.get("verbs"));
        let names = strs(rule.get("resourceNames"));
        for grp in strs(rule.get("apiGroups")) {
            let grp = if grp.is_empty() {
                "core".to_string()
            } else {
                grp
            };
            for res in strs(rule.get("resources")) {
                for na in &names {
                    upsert(
                        &mut pp,
                        PolicyRes {
                            namespace: ns.into(),
                            binding: binding.into(),
                            resource: res.clone(),
                            group: grp.clone(),
                            resource_names: vec![na.clone()],
                            non_resource_urls: vec![],
                            verbs: verbs.clone(),
                        },
                    );
                }
                upsert(
                    &mut pp,
                    PolicyRes {
                        namespace: ns.into(),
                        binding: binding.into(),
                        resource: res,
                        group: grp.clone(),
                        resource_names: vec![],
                        non_resource_urls: vec![],
                        verbs: verbs.clone(),
                    },
                );
            }
        }
        for nres in strs(rule.get("nonResourceURLs")) {
            let url = if nres.starts_with('/') {
                nres
            } else {
                format!("/{nres}")
            };
            upsert(
                &mut pp,
                PolicyRes {
                    namespace: ns.into(),
                    binding: binding.into(),
                    resource: url.clone(),
                    group: crate::model::NA_VALUE.into(),
                    resource_names: vec![],
                    non_resource_urls: vec![url],
                    verbs: verbs.clone(),
                },
            );
        }
    }
    pp
}

/// dao.isSameSubject.
pub(crate) fn is_same_subject(
    kind: &str,
    ns: &str,
    binding_ns: &str,
    name: &str,
    subject: &Value,
) -> bool {
    if s(subject, "/kind") != kind || s(subject, "/name") != name {
        return false;
    }
    if kind == "ServiceAccount" {
        let mut cns = s(subject, "/namespace").to_string();
        if cns.is_empty() {
            cns = binding_ns.to_string();
        }
        return crate::k8s::is_all_namespaces(ns) || cns == ns;
    }
    true
}

fn env_refs(c: &Value, key_ref: &str, from_ref: &str, name: &str) -> bool {
    let env_from = c.get("envFrom").and_then(|v| v.as_array());
    if env_from
        .map(|ee| {
            ee.iter().any(|e| {
                e.pointer(&format!("/{from_ref}/name"))
                    .and_then(|v| v.as_str())
                    == Some(name)
            })
        })
        .unwrap_or(false)
    {
        return true;
    }
    c.get("env")
        .and_then(|v| v.as_array())
        .map(|ee| {
            ee.iter().any(|e| {
                e.pointer(&format!("/valueFrom/{key_ref}/name"))
                    .and_then(|v| v.as_str())
                    == Some(name)
            })
        })
        .unwrap_or(false)
}

fn all_containers(spec: &Value) -> Vec<&Value> {
    ["initContainers", "containers", "ephemeralContainers"]
        .iter()
        .filter_map(|k| spec.get(*k).and_then(|v| v.as_array()))
        .flatten()
        .collect()
}

/// dao.hasConfigMap.
pub(crate) fn has_config_map(spec: &Value, name: &str) -> bool {
    if all_containers(spec)
        .iter()
        .any(|c| env_refs(c, "configMapKeyRef", "configMapRef", name))
    {
        return true;
    }
    spec.get("volumes")
        .and_then(|v| v.as_array())
        .map(|vv| {
            vv.iter()
                .any(|v| v.pointer("/configMap/name").and_then(|x| x.as_str()) == Some(name))
        })
        .unwrap_or(false)
}

/// dao.hasSecret (without the service account token lookup, handled by the caller).
pub(crate) fn has_secret(spec: &Value, name: &str) -> bool {
    if all_containers(spec)
        .iter()
        .any(|c| env_refs(c, "secretKeyRef", "secretRef", name))
    {
        return true;
    }
    if spec
        .get("imagePullSecrets")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .any(|s| s.get("name").and_then(|v| v.as_str()) == Some(name))
        })
        .unwrap_or(false)
    {
        return true;
    }
    spec.get("volumes")
        .and_then(|v| v.as_array())
        .map(|vv| {
            vv.iter()
                .any(|v| v.pointer("/secret/secretName").and_then(|x| x.as_str()) == Some(name))
        })
        .unwrap_or(false)
}

/// dao.hasPVC.
pub(crate) fn has_pvc(spec: &Value, name: &str) -> bool {
    spec.get("volumes")
        .and_then(|v| v.as_array())
        .map(|vv| {
            vv.iter().any(|v| {
                v.pointer("/persistentVolumeClaim/claimName")
                    .and_then(|x| x.as_str())
                    == Some(name)
            })
        })
        .unwrap_or(false)
}

/// dao.hasPC.
pub(crate) fn has_pc(spec: &Value, name: &str) -> bool {
    s(spec, "/priorityClassName") == name
}

/// dao.serviceAccountMatches.
pub(crate) fn service_account_matches(spec: &Value, sa: &str) -> bool {
    let mut pod_sa = s(spec, "/serviceAccountName");
    if pod_sa.is_empty() {
        pod_sa = "default";
    }
    pod_sa == sa
}

/// One release from `helm list -o json`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct HelmRelease {
    pub name: String,
    pub namespace: String,
    pub revision: String,
    pub updated: String,
    pub status: String,
    pub chart: String,
    pub app_version: String,
    pub description: String,
}

fn jstr(v: &Value, k: &str) -> String {
    match v.get(k) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// Normalize helm's `updated` stamps to RFC3339 when possible.
pub(crate) fn helm_time(s: &str) -> String {
    if chrono::DateTime::parse_from_rfc3339(s).is_ok() {
        return s.to_string();
    }
    let trimmed = s.trim();
    // helm list: "2024-01-02 15:04:05.123456 -0700 MST"
    let without_zone = trimmed
        .rsplit_once(' ')
        .filter(|(_, z)| z.chars().all(|c| c.is_ascii_alphabetic()))
        .map(|(a, _)| a)
        .unwrap_or(trimmed);
    for fmt in ["%Y-%m-%d %H:%M:%S%.f %z", "%Y-%m-%d %H:%M:%S %z"] {
        if let Ok(t) = chrono::DateTime::parse_from_str(without_zone, fmt) {
            return t.with_timezone(&chrono::Utc).to_rfc3339();
        }
    }
    s.to_string()
}

/// Parse `helm list -o json` / `helm history -o json`.
pub(crate) fn parse_helm_json(raw: &str) -> anyhow::Result<Vec<HelmRelease>> {
    let v: Value = if raw.trim().is_empty() {
        Value::Array(vec![])
    } else {
        serde_json::from_str(raw)?
    };
    Ok(v.as_array()
        .map(|a| {
            a.iter()
                .map(|r| HelmRelease {
                    name: jstr(r, "name"),
                    namespace: jstr(r, "namespace"),
                    revision: jstr(r, "revision"),
                    updated: helm_time(&jstr(r, "updated")),
                    status: jstr(r, "status"),
                    chart: jstr(r, "chart"),
                    app_version: jstr(r, "app_version"),
                    description: jstr(r, "description"),
                })
                .collect()
        })
        .unwrap_or_default())
}

/// data.SanitizeFileName.
pub(crate) fn sanitize_file_name(name: &str) -> String {
    let rx = regex::Regex::new(r"[:/]+").expect("rx");
    rx.replace_all(name, "-").to_string()
}

fn csv_field(f: &str) -> String {
    let needs = !f.is_empty()
        && (f == r"\."
            || f.contains([',', '"', '\r', '\n'])
            || f.starts_with(|c: char| c.is_whitespace()));
    if needs {
        format!("\"{}\"", f.replace('"', "\"\""))
    } else {
        f.to_string()
    }
}

/// encoding/csv writer output.
pub(crate) fn to_csv(header: &[String], rows: &[Vec<String>]) -> String {
    let mut out = String::new();
    for r in std::iter::once(header.to_vec()).chain(rows.iter().cloned()) {
        out.push_str(&r.iter().map(|f| csv_field(f)).collect::<Vec<_>>().join(","));
        out.push('\n');
    }
    out
}

/// view.pfToHuman.
pub(crate) fn pf_to_human(s: &str) -> Option<String> {
    let rx = regex::Regex::new(r"\A([\w-]+)/([\w-]+)\|([\w-]+)?\|(\d+):(\d+)").expect("rx");
    let rx2 = regex::Regex::new(r"\A([\w-]+)/([\w-]+)\|([\w-]+)?:(\d+):(\d+)").expect("rx");
    let m = rx.captures(s).or_else(|| rx2.captures(s))?;
    Some(format!(
        "{}::{} {}->{}",
        &m[2],
        m.get(3).map(|x| x.as_str()).unwrap_or(""),
        &m[4],
        &m[5]
    ))
}

/// Container ports (TCP) of a pod as `container::port(name)` (port.ContainerPortSpec).
pub(crate) fn container_ports(spec: &Value, only: Option<&str>) -> Vec<(String, String, u16)> {
    let mut out = Vec::new();
    if let Some(cc) = spec.get("containers").and_then(|v| v.as_array()) {
        for c in cc {
            let co = s(c, "/name");
            if only.map(|o| o != co).unwrap_or(false) {
                continue;
            }
            if let Some(pp) = c.get("ports").and_then(|v| v.as_array()) {
                for p in pp {
                    let proto = s(p, "/protocol");
                    if !proto.is_empty() && proto != "TCP" {
                        continue;
                    }
                    let num = p.get("containerPort").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
                    out.push((co.to_string(), s(p, "/name").to_string(), num));
                }
            }
        }
    }
    out
}

/// ContainerPortSpec.String.
pub(crate) fn port_spec_string(co: &str, name: &str, num: u16) -> String {
    let mut s = format!("{co}::{num}");
    if !name.is_empty() {
        s.push_str(&format!("({name})"));
    }
    s
}

/// One port-forward annotation entry (`[container::]local:remote` or `[container::]port`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PfAnn {
    pub container: String,
    pub local: String,
    pub remote: String,
}

/// port.ParsePFs for `k9scli.io/(auto-)port-forwards` annotations.
pub(crate) fn parse_pf_annotation(spec: &str) -> Vec<PfAnn> {
    let full = regex::Regex::new(r"\A([\w-]+)::(\d*):?(\d*|[\w-]*)\z").expect("rx");
    let plain = regex::Regex::new(r"\A(\d*):?(\d*|[\w-]*)\z").expect("rx");
    spec.split(',')
        .filter_map(|e| {
            let e = e.trim();
            if e.is_empty() {
                return None;
            }
            if let Some(m) = full.captures(e) {
                let local = m[2].to_string();
                let remote = if m[3].is_empty() {
                    local.clone()
                } else {
                    m[3].to_string()
                };
                return Some(PfAnn {
                    container: m[1].to_string(),
                    local,
                    remote,
                });
            }
            let m = plain.captures(e)?;
            if m[2].is_empty() {
                return Some(PfAnn {
                    container: String::new(),
                    local: m[1].to_string(),
                    remote: m[1].to_string(),
                });
            }
            Some(PfAnn {
                container: String::new(),
                local: m[1].to_string(),
                remote: m[2].to_string(),
            })
        })
        .collect()
}

/// A benchmark configuration (config.BenchConfig).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BenchCfg {
    pub concurrency: usize,
    pub requests: usize,
    pub method: String,
    pub host: String,
    pub path: String,
    pub http2: bool,
    pub body: String,
    pub headers: Vec<(String, String)>,
    pub user: Option<(String, String)>,
}

impl Default for BenchCfg {
    /// config.DefaultBenchSpec.
    fn default() -> Self {
        Self {
            concurrency: 1,
            requests: 200,
            method: "GET".into(),
            host: String::new(),
            path: "/".into(),
            http2: false,
            body: String::new(),
            headers: Vec::new(),
            user: None,
        }
    }
}

fn bench_from(v: &serde_yaml::Value) -> BenchCfg {
    let mut b = BenchCfg::default();
    let get = |k: &str| v.get(k);
    if let Some(c) = get("concurrency").and_then(|x| x.as_u64()) {
        b.concurrency = c as usize;
    }
    if let Some(n) = get("requests").and_then(|x| x.as_u64()) {
        b.requests = n as usize;
    }
    if let Some(h) = get("http") {
        let hs = |k: &str| h.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        if !hs("method").is_empty() {
            b.method = hs("method");
        }
        b.host = hs("host");
        if !hs("path").is_empty() {
            b.path = hs("path");
        }
        b.body = hs("body");
        b.http2 = h.get("http2").and_then(|x| x.as_bool()).unwrap_or(false);
        if let Some(m) = h.get("headers").and_then(|x| x.as_mapping()) {
            for (k, vv) in m {
                let k = k.as_str().unwrap_or("").to_string();
                match vv {
                    serde_yaml::Value::Sequence(seq) => {
                        for x in seq {
                            b.headers
                                .push((k.clone(), x.as_str().unwrap_or("").to_string()));
                        }
                    }
                    other => b
                        .headers
                        .push((k, other.as_str().unwrap_or("").to_string())),
                }
            }
        }
    }
    if let Some(a) = get("auth") {
        let u = a.get("user").and_then(|x| x.as_str()).unwrap_or("");
        let p = a.get("password").and_then(|x| x.as_str()).unwrap_or("");
        if !u.is_empty() {
            b.user = Some((u.to_string(), p.to_string()));
        }
    }
    b
}

fn read_bench(file: &std::path::Path) -> Option<serde_yaml::Value> {
    let raw = std::fs::read_to_string(file).ok()?;
    let v: serde_yaml::Value = serde_yaml::from_str(&raw).ok()?;
    v.get("benchmarks").cloned()
}

/// dao.PodToKey: `ns/pod|co...` → `ns/<pod without hash>:co`.
pub(crate) fn pod_to_key(path: &str) -> String {
    let (pod, rest) = path.split_once('|').unwrap_or((path, ""));
    let co = rest.split([':', '|']).next().unwrap_or("");
    let (ns, po) = crate::k8s::namespaced(pod);
    let rx = regex::Regex::new(r"\A(.+?)-[a-z0-9]{8,10}-[a-z0-9]{5}\z").expect("rx");
    let rx2 = regex::Regex::new(r"\A(.+?)-[a-z0-9]{5}\z").expect("rx");
    let po = rx
        .captures(po)
        .or_else(|| rx2.captures(po))
        .map(|m| m[1].to_string())
        .unwrap_or_else(|| po.to_string());
    format!("{}:{co}", crate::k8s::fqn(ns, &po))
}

/// dao.BenchConfigFor (port forwards) or the service entry (`service`).
pub(crate) fn bench_config_for(file: &std::path::Path, path: &str, service: bool) -> BenchCfg {
    lookup_bench(file, path, service).unwrap_or_default()
}

/// Service/container bench entry if configured.
pub(crate) fn lookup_bench(file: &std::path::Path, path: &str, service: bool) -> Option<BenchCfg> {
    let root = read_bench(file)?;
    if service {
        return root
            .get("services")
            .and_then(|m| m.get(path))
            .map(bench_from);
    }
    if let Some(c) = root
        .get("containers")
        .and_then(|m| m.get(pod_to_key(path).as_str()))
    {
        return Some(bench_from(c));
    }
    let mut def = BenchCfg::default();
    if let Some(d) = root.get("defaults") {
        if let Some(c) = d.get("concurrency").and_then(|x| x.as_u64()) {
            def.concurrency = c as usize;
        }
        if let Some(n) = d.get("requests").and_then(|x| x.as_u64()) {
            def.requests = n as usize;
        }
    }
    Some(def)
}

/// Standard base64 decode (secret data).
pub(crate) fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            b'\n' | b'\r' | b' ' => continue,
            _ => return None,
        } as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// k9s singularize.
pub(crate) fn singularize(s: &str) -> String {
    s.strip_suffix('s').unwrap_or(s).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::k8s::fake::obj;
    use serde_json::json;

    #[test]
    fn pod_helpers() {
        let p = obj(json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": {"name": "p", "namespace": "ns", "annotations": {DEFAULT_CONTAINER_ANNOTATION: "b"}},
            "spec": {"containers": [{"name": "a"}, {"name": "b"}],
                     "initContainers": [{"name": "i"}, {"name": "side", "restartPolicy": "Always"}]},
            "status": {"phase": "Running", "containerStatuses": [
                {"name": "a", "ready": true, "state": {"running": {}}},
                {"name": "b", "ready": true, "state": {"running": {}}}]}
        }));
        let spec = pod_spec(&p).unwrap();
        assert_eq!(default_container(&p, spec).as_deref(), Some("b"));
        assert_eq!(fetch_containers(&p, spec, false), vec!["b", "a", "side"]);
        assert_eq!(
            fetch_containers(&p, spec, true),
            vec!["b", "a", "i", "side"]
        );
        assert_eq!(pod_status(&p), "Running");
        assert!(pod_is_running(&p));
        let crash = obj(
            json!({"apiVersion": "v1", "kind": "Pod", "metadata": {"name": "c"},
            "status": {"phase": "Running", "containerStatuses": [
                {"name": "a", "ready": false, "state": {"waiting": {"reason": "CrashLoopBackOff"}}}]}}),
        );
        assert_eq!(pod_status(&crash), "CrashLoopBackOff");
        assert!(!pod_is_running(&crash));
        let done = obj(
            json!({"apiVersion": "v1", "kind": "Pod", "metadata": {"name": "d"},
            "status": {"phase": "Succeeded", "containerStatuses": [
                {"name": "a", "state": {"terminated": {"reason": "Completed", "exitCode": 0}}}]}}),
        );
        assert_eq!(pod_status(&done), "Completed");
    }

    #[test]
    fn selectors() {
        let sel = json!({"matchLabels": {"b": "2", "a": "1"},
            "matchExpressions": [{"key": "c", "operator": "In", "values": ["y", "x"]}, {"key": "d", "operator": "DoesNotExist"}]});
        assert_eq!(
            label_selector_string(&sel).unwrap(),
            "a=1,b=2,c in (x,y),!d"
        );
        assert!(label_selector_string(&json!({})).is_none());
        let m = json!({"app": "x", "tier": "web"});
        assert_eq!(
            map_selector_string(m.as_object().unwrap()).unwrap(),
            "app=x,tier=web"
        );
    }

    #[test]
    fn env_substitution() {
        let mut e = Env::default();
        e.set("NAMESPACE", "default");
        e.set("NAME", "nginx");
        e.set("COL-READY", "true");
        assert_eq!(
            e.substitute("-n $NAMESPACE ${NAME} $!COL-READY $MISSING"),
            "-n default nginx false $MISSING"
        );
        assert_eq!(e.substitute("$NAMESPACE/$NAME"), "default/nginx");
    }

    #[test]
    fn rules_and_refs() {
        let rules = json!([
            {"apiGroups": [""], "resources": ["pods", "pods"], "verbs": ["get"]},
            {"apiGroups": [""], "resources": ["pods"], "verbs": ["list", "get"]},
            {"apiGroups": ["apps"], "resources": ["deployments"], "resourceNames": ["web"], "verbs": ["patch"]},
            {"nonResourceURLs": ["healthz"], "verbs": ["get"]}
        ]);
        let pp = parse_rules("-", "CR:x", Some(&rules));
        assert_eq!(pp.len(), 4);
        assert_eq!(pp[0].group, "core");
        assert_eq!(pp[0].verbs, vec!["get", "list"]);
        assert_eq!(pp[1].resource_names, vec!["web"]);
        assert_eq!(pp[3].resource, "/healthz");
        let spec = json!({"containers": [{"name": "a", "envFrom": [{"configMapRef": {"name": "cm1"}}],
            "env": [{"name": "X", "valueFrom": {"secretKeyRef": {"name": "s1", "key": "k"}}}]}],
            "volumes": [{"name": "v", "persistentVolumeClaim": {"claimName": "data"}}, {"name": "w", "configMap": {"name": "cm2"}}],
            "priorityClassName": "high"});
        assert!(
            has_config_map(&spec, "cm1")
                && has_config_map(&spec, "cm2")
                && !has_config_map(&spec, "nope")
        );
        assert!(has_secret(&spec, "s1") && !has_secret(&spec, "s2"));
        assert!(has_pvc(&spec, "data") && has_pc(&spec, "high"));
        assert!(service_account_matches(&spec, "default"));
        let sub = json!({"kind": "ServiceAccount", "name": "sa", "namespace": "a"});
        assert!(is_same_subject("ServiceAccount", "a", "b", "sa", &sub));
        assert!(!is_same_subject("ServiceAccount", "b", "b", "sa", &sub));
        assert!(is_same_subject("ServiceAccount", "", "b", "sa", &sub));
    }

    #[test]
    fn misc() {
        let raw = r#"[{"name":"web","namespace":"default","revision":"3","updated":"2024-01-02 15:04:05.123456 -0700 MST","status":"deployed","chart":"web-1.0.0","app_version":"1.0"},
                      {"revision":2,"updated":"2024-01-02T15:04:05Z","status":"superseded","chart":"web-0.9","app_version":"0.9","description":"Upgrade complete"}]"#;
        let rr = parse_helm_json(raw).unwrap();
        assert_eq!(rr[0].name, "web");
        assert_eq!(rr[0].updated, "2024-01-02T22:04:05.123456+00:00");
        assert_eq!(rr[1].revision, "2");
        assert_eq!(sanitize_file_name("arn:aws:eks/x"), "arn-aws-eks-x");
        assert_eq!(
            to_csv(
                &["A".into(), "B".into()],
                &[vec!["x,y".into(), " z".into()]]
            ),
            "A,B\n\"x,y\",\" z\"\n"
        );
        assert_eq!(
            pf_to_human("default/nginx|web|8080:80").unwrap(),
            "nginx::web 8080->80"
        );
        assert_eq!(
            pf_to_human("default/nginx|web:8080:80").unwrap(),
            "nginx::web 8080->80"
        );
        let pfs = parse_pf_annotation("web::8080:80, 9090");
        assert_eq!(
            pfs[0],
            PfAnn {
                container: "web".into(),
                local: "8080".into(),
                remote: "80".into()
            }
        );
        assert_eq!(
            pfs[1],
            PfAnn {
                container: String::new(),
                local: "9090".into(),
                remote: "9090".into()
            }
        );
        assert_eq!(port_spec_string("web", "http", 80), "web::80(http)");
        assert_eq!(singularize("deployments"), "deployment");
        assert_eq!(base64_decode("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64_decode("YQ").unwrap(), b"a");
        assert_eq!(
            pod_to_key("default/nginx-6799fc88d8-abcde|web:8080:80"),
            "default/nginx:web"
        );
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("b.yaml");
        std::fs::write(&f, "benchmarks:\n  defaults:\n    concurrency: 2\n    requests: 10\n  services:\n    default/svc:\n      concurrency: 3\n      http:\n        host: 1.2.3.4\n        path: /x\n        headers:\n          A: [b]\n      auth:\n        user: u\n        password: p\n").unwrap();
        let svc = lookup_bench(&f, "default/svc", true).unwrap();
        assert_eq!(
            (svc.concurrency, svc.host.as_str(), svc.path.as_str()),
            (3, "1.2.3.4", "/x")
        );
        assert_eq!(svc.headers, vec![("A".to_string(), "b".to_string())]);
        assert_eq!(svc.user, Some(("u".into(), "p".into())));
        assert!(lookup_bench(&f, "default/nope", true).is_none());
        let pf = bench_config_for(&f, "default/p|c:1:2", false);
        assert_eq!((pf.concurrency, pf.requests), (2, 10));
    }
}
