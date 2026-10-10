//! Key action handlers and async follow-ups (k9s view/browser.go,
//! view/table.go, the per-resource viewers and their extenders).

use std::future::Future;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};
use kube::api::DynamicObject;
use serde_json::{json, Value};

use crate::k8s::{fqn, namespaced, DeleteOptions, PortForwardSpec, SharedAccessor};

use super::helpers::*;
use super::picker::Picker;
use super::*;
use crate::ui::Dialog;

/// k9s exec.go shellCheck.
const SHELL_CHECK: &str = "command -v bash >/dev/null && exec bash || exec sh";
/// k9s pod.go magicPrompt.
const MAGIC_PROMPT: &str = "Yes Please!";
/// k9s event.go faultsFilter.
const FAULTS_FILTER: &str = "Warning|Error";
/// k9s node shell container.
const K9S_SHELL: &str = "k9rs-shell";
const TOAST_PHASES: &[&str] = &[
    "Completed",
    "Pending",
    "CrashLoopBackOff",
    "Error",
    "ImagePullBackOff",
    "ContainerStatusUnknown",
    "Evicted",
    "OOMKilled",
];

fn info(msg: impl Into<String>) -> ViewCmd {
    ViewCmd::Flash(FlashLevel::Info, msg.into())
}
fn warn(msg: impl Into<String>) -> ViewCmd {
    ViewCmd::Flash(FlashLevel::Warn, msg.into())
}
fn err(msg: impl Into<String>) -> ViewCmd {
    ViewCmd::Flash(FlashLevel::Err, msg.into())
}

fn pods_gvr() -> Gvr {
    Gvr::parse("v1/pods")
}

/// Spawn `fut` and post its message (or error flash) back to view `id`.
pub(crate) fn spawn_msg<F>(ctx: &Ctx, id: ViewId, fut: F)
where
    F: Future<Output = Result<Msg>> + Send + 'static,
{
    let tx = ctx.sender();
    ctx.spawn(async move {
        match fut.await {
            Ok(m) => post(&tx, id, m),
            Err(e) => post_err(&tx, id, format!("{e:#}")),
        }
    });
}

/// The CVE page for a scan row id (`image|name|ver|fix|type|vul|sev`).
pub(crate) fn cve_url(path: &str) -> Option<String> {
    let tt: Vec<&str> = path.split('|').collect();
    let cve = tt.get(crate::render::core::CVE_PARSE_IDX)?;
    let site = if cve.starts_with("GHSA") {
        "https://github.com/advisories/"
    } else {
        "https://nvd.nist.gov/vuln/detail/"
    };
    Some(format!("{site}{cve}"))
}

/// img_scan.go viewCVE: open the CVE page in the browser, in the background.
fn view_cve(path: &str) -> Vec<ViewCmd> {
    let mut out = Vec::new();
    if path.split('|').count() < 7 {
        out.push(err(format!("parse path failed: {path}")));
    }
    let Some(site) = cve_url(path) else {
        return out;
    };
    let bin = if cfg!(target_os = "macos") {
        "open"
    } else {
        "sensible-browser"
    };
    let spawned = std::process::Command::new(bin)
        .arg(&site)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    match spawned {
        Ok(mut c) => {
            std::thread::spawn(move || {
                let _ = c.wait();
            });
        }
        Err(_) => out.push(err("unable to run browser command".to_string())),
    }
    out
}

/// Selector of a workload object (spec.selector).
fn workload_selector(o: &DynamicObject) -> Result<String> {
    o.data
        .pointer("/spec/selector")
        .and_then(label_selector_string)
        .ok_or_else(|| anyhow!("no selector found on {}", obj_fqn(o)))
}

/// view/exec.go bannerFmt, colored like fatih/color BgGreen+FgBlack+Bold.
pub(crate) fn shell_banner(fqn: &str, co: &str) -> String {
    let text = format!("<<K9rs-Shell>> Pod: {fqn} | Container: {co} \n");
    // fatih/color.NoColor: NO_COLOR set or TERM=dumb.
    if std::env::var_os("NO_COLOR").is_some() || std::env::var("TERM").as_deref() == Ok("dumb") {
        return text;
    }
    format!("\x1b[42;30;1m{text}\x1b[0m")
}

/// data.WriteYAML of `helm get values -o json` output (nil maps print `{}`).
fn values_yaml(json: &str) -> Result<String> {
    let v: Value = serde_json::from_str(json.trim())?;
    Ok(crate::k8s::goyaml::to_yaml(&null_map(v)))
}

fn null_map(v: Value) -> Value {
    if v.is_null() {
        Value::Object(Default::default())
    } else {
        v
    }
}

/// Chart default values from a helm release secret
/// (base64 of the gzipped release JSON, like helm's storage driver).
fn helm_chart_values(sec: &DynamicObject) -> Result<Value> {
    use base64::Engine as _;
    use std::io::Read as _;
    let b64 = base64::engine::general_purpose::STANDARD;
    let raw = sec
        .data
        .pointer("/data/release")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("no release data in {}", obj_fqn(sec)))?;
    let bytes = b64.decode(b64.decode(raw)?)?;
    let mut json = Vec::new();
    if bytes.starts_with(&[0x1f, 0x8b, 0x08]) {
        flate2::read::GzDecoder::new(&bytes[..]).read_to_end(&mut json)?;
    } else {
        json = bytes;
    }
    let rel: Value = serde_json::from_slice(&json)?;
    Ok(rel.pointer("/chart/values").cloned().unwrap_or(Value::Null))
}

/// view/pod.go defaultTxRetries.
const DEFAULT_TX_RETRIES: i64 = 999;

/// kubectl exec/attach argv built from the accessor's exec command.
pub(crate) fn exec_cmd(ctx: &Ctx, path: &str, co: &str, attach: bool, os: &str) -> ViewCmd {
    let (ns, po) = namespaced(path);
    let mut argv = ctx.accessor.exec_argv(ns, po, co, "sh");
    if attach {
        if let Some(i) = argv.iter().position(|a| a == "exec") {
            argv[i] = "attach".into();
        }
        if let Some(i) = argv.iter().position(|a| a == "--") {
            argv.truncate(i);
        }
    } else if argv.len() >= 2 && argv[argv.len() - 1] == "sh" && argv[argv.len() - 2] == "--" {
        argv.pop();
        if os == "windows" {
            argv.push("powershell".into());
        } else {
            argv.extend(["sh".to_string(), "-c".into(), SHELL_CHECK.into()]);
        }
    }
    let what = if attach { "Attach" } else { "Shell" };
    ViewCmd::Suspend {
        argv,
        banner: shell_banner(path, co),
        env: ctx.accessor.shell_env(),
        on_done: Some(Box::new(move |_ctx, st| match exec_failure(&st) {
            Some(e) => vec![err(format!("{what} exec failed: {e}"))],
            None => Vec::new(),
        })),
    }
}

/// exec.go pipe error for a finished foreground command: a non-zero exit
/// is "command failed. Check k9s logs: exit status N"; a signal is ignored.
pub(crate) fn exec_failure(st: &std::process::ExitStatus) -> Option<String> {
    match st.code() {
        Some(0) | None => None,
        Some(c) => Some(format!("command failed. Check k9rs logs: exit status {c}")),
    }
}

/// Editor argv (K9S_EDITOR, KUBE_EDITOR, EDITOR).
fn editor_argv(file: &str) -> Option<Vec<String>> {
    for var in ["K9S_EDITOR", "KUBE_EDITOR", "EDITOR"] {
        let Ok(v) = std::env::var(var) else {
            continue;
        };
        let tokens: Vec<String> = v
            .split(' ')
            .filter(|t| !t.is_empty())
            .map(String::from)
            .collect();
        let Some(bin) = tokens.first() else {
            continue;
        };
        if which::which(bin).is_ok() {
            let mut argv = tokens;
            argv.push(file.to_string());
            return Some(argv);
        }
    }
    None
}

fn edit_file(file: &str) -> Vec<ViewCmd> {
    match editor_argv(file) {
        Some(argv) => vec![ViewCmd::Suspend {
            argv,
            banner: String::new(),
            env: Vec::new(),
            on_done: Some(Box::new(|_ctx, st| {
                if st.success() {
                    Vec::new()
                } else {
                    vec![err("Failed to launch editor")]
                }
            })),
        }],
        None => vec![err(
            "You must set at least one of those env vars: K9S_EDITOR|KUBE_EDITOR|EDITOR",
        )],
    }
}

fn is_manifest(s: &str) -> bool {
    matches!(
        Path::new(s).extension().and_then(|e| e.to_str()),
        Some("yml" | "yaml")
    )
}

fn contains_dir(sel: &str) -> bool {
    !is_manifest(sel)
        && std::fs::read_dir(sel)
            .map(|rd| rd.flatten().any(|e| e.path().is_dir()))
            .unwrap_or(false)
}

fn is_kustomized(sel: &str) -> bool {
    !is_manifest(sel)
        && std::fs::read_dir(sel)
            .map(|rd| {
                rd.flatten().any(|e| {
                    let n = e.file_name().to_string_lossy().to_string();
                    n == "Kustomization" || n == "kustomization.yaml" || n == "kustomization.yml"
                })
            })
            .unwrap_or(false)
}

fn fmt_results(res: &str) -> String {
    res.trim()
        .lines()
        .map(|l| format!("  {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Resolve the first pod behind a selection (dao.Controller.Pod).
async fn resolve_pod(
    acc: SharedAccessor,
    kind: Kind,
    gvr: Gvr,
    path: String,
    parent: String,
) -> Result<String> {
    match kind {
        Kind::Pod => Ok(path),
        Kind::Container => Ok(parent),
        _ => {
            let o = acc.get(&gvr, &path).await?;
            let sel = if kind == Kind::Service {
                o.data
                    .pointer("/spec/selector")
                    .and_then(|v| v.as_object())
                    .and_then(map_selector_string)
                    .ok_or_else(|| anyhow!("no selector found on service {path}"))?
            } else {
                workload_selector(&o)?
            };
            let (ns, _) = namespaced(&path);
            let pods = acc.list(&pods_gvr(), ns, Some(&sel)).await?;
            pods.first()
                .map(obj_fqn)
                .ok_or_else(|| anyhow!("no pods found for {path}"))
        }
    }
}

/// Pod OS (getPodOS): pod node selector, else the node labels.
async fn pod_os(acc: &SharedAccessor, pod: &DynamicObject) -> String {
    if let Some(os) = os_from_labels(
        pod.data
            .pointer("/spec/nodeSelector")
            .and_then(|v| v.as_object()),
    ) {
        return os;
    }
    let node = pod
        .data
        .pointer("/spec/nodeName")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !node.is_empty() {
        if let Ok(n) = acc.get(&Gvr::parse("v1/nodes"), node).await {
            let labels: serde_json::Map<String, Value> = n
                .metadata
                .labels
                .unwrap_or_default()
                .into_iter()
                .map(|(k, v)| (k, Value::String(v)))
                .collect();
            if let Some(os) = os_from_labels(Some(&labels)) {
                return os;
            }
        }
    }
    "linux".into()
}

/// view.podLogOptions.
fn pod_log_options(o: &DynamicObject, spec: &Value, mut base: LogOptions) -> LogOptions {
    let cc = fetch_containers(o, spec, true);
    if let Some(def) = default_container(o, spec) {
        base.container = Some(def.clone());
        base.default_container = Some(def);
    } else if cc.len() == 1 {
        base.container = Some(cc[0].clone());
    } else {
        base.all_containers = true;
    }
    base
}

/// Jump to an owner reference (OwnerExtender.jumpOwner).
fn jump_owner(
    ctx: &mut Ctx,
    deps: &Deps,
    ns: &str,
    owner: &(String, String, String),
) -> Vec<ViewCmd> {
    let (api, kind, name) = owner;
    let (group, version) = match api.split_once('/') {
        Some((g, v)) => (g.to_string(), v.to_string()),
        None => (String::new(), api.clone()),
    };
    let Some(meta) = ctx
        .resources
        .iter()
        .find(|m| m.kind == *kind && m.gvr.g() == group && m.gvr.v() == version)
        .cloned()
    else {
        return vec![warn(format!(
            "Unable to jump owner: unsupported GVK: {api}/{kind}"
        ))];
    };
    let owner_fqn = if meta.namespaced {
        fqn(ns, name)
    } else {
        name.clone()
    };
    let b = super::child(
        ctx,
        deps.clone(),
        meta.gvr.clone(),
        Opts {
            instance: owner_fqn,
            ns: meta.namespaced.then(|| ns.to_string()),
            ..Default::default()
        },
    );
    vec![ViewCmd::Push(Box::new(b))]
}

/// Port-forward ports, the default spec and the dialog prefill for a pod
/// (Annotations.PreferredPorts + PFAnns.ToPortSpec).
fn pf_choices(
    pod: &DynamicObject,
    only: Option<&str>,
    address: &str,
) -> (Vec<String>, PortForwardSpec, (String, String)) {
    let spec = pod.data.get("spec").cloned().unwrap_or(Value::Null);
    let ports = container_ports(&spec, only);
    let ns = pod.metadata.namespace.clone().unwrap_or_default();
    let name = pod.metadata.name.clone().unwrap_or_default();
    let mut def = PortForwardSpec {
        namespace: ns,
        pod: name,
        address: address.to_string(),
        ..Default::default()
    };
    if let Some((co, _, _)) = ports.first() {
        def.container = co.clone();
    } else if let Some(co) = only {
        def.container = co.to_string();
    }
    let ann = pod
        .metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get("k9scli.io/port-forwards").cloned());
    // (container::port, local port) pairs.
    let prefs: Vec<(String, String)> = match (&ann, ports.first()) {
        (_, None) => Vec::new(),
        (None, Some((co, _, num))) => vec![(format!("{co}::{num}"), num.to_string())],
        // MatchAnnotations: the container must match and the port by name or number.
        (Some(ann), Some(_)) => parse_pf_annotation(ann)
            .into_iter()
            .filter_map(|a| {
                let (co, _, num) = ports.iter().find(|(co, name, num)| {
                    *co == a.container && (*name == a.remote || num.to_string() == a.remote)
                })?;
                let local = if a.local.is_empty() {
                    num.to_string()
                } else {
                    a.local
                };
                Some((format!("{co}::{num}"), local))
            })
            .collect(),
    };
    let join = |f: fn(&(String, String)) -> &String| {
        prefs.iter().map(f).cloned().collect::<Vec<_>>().join(",")
    };
    let prefill = (join(|p| &p.0), join(|p| &p.1));
    let strs = ports
        .iter()
        .map(|(co, n, num)| port_spec_string(co, n, *num))
        .collect();
    (strs, def, prefill)
}

/// Auto port-forwards from `k9scli.io/auto-port-forwards`.
fn auto_forwards(pod: &DynamicObject, address: &str) -> Option<Vec<PortForwardSpec>> {
    let ann = pod
        .metadata
        .annotations
        .as_ref()?
        .get("k9scli.io/auto-port-forwards")?
        .clone();
    let spec = pod.data.get("spec").cloned().unwrap_or(Value::Null);
    let ports = container_ports(&spec, None);
    let first_co = ports.first().map(|p| p.0.clone()).unwrap_or_default();
    let out: Vec<PortForwardSpec> = parse_pf_annotation(&ann)
        .into_iter()
        .filter_map(|a| {
            let co = if a.container.is_empty() {
                first_co.clone()
            } else {
                a.container.clone()
            };
            let remote = a.remote.parse::<u16>().ok().or_else(|| {
                ports
                    .iter()
                    .find(|(c, n, _)| *c == co && *n == a.remote)
                    .map(|p| p.2)
            })?;
            Some(PortForwardSpec {
                namespace: pod.metadata.namespace.clone().unwrap_or_default(),
                pod: pod.metadata.name.clone().unwrap_or_default(),
                container: co,
                address: address.to_string(),
                local_port: a.local.parse().unwrap_or(remote),
                container_port: remote,
            })
        })
        .collect();
    Some(out)
}

fn pf_dialog(
    id: ViewId,
    spec: PortForwardSpec,
    ports: Vec<String>,
    prefill: (String, String),
) -> ViewCmd {
    ViewCmd::Dialog(Dialog::PortForward {
        spec,
        ports,
        prefill,
        // The dialog already checked the ports (startFwdCB).
        on_ok: Box::new(move |ctx, tunnels| {
            let acc = ctx.accessor.clone();
            spawn_msg(ctx, id, async move {
                let mut hh = Vec::new();
                for t in tunnels {
                    hh.push(acc.port_forward(t).await?);
                }
                Ok(Msg::PfStarted(hh))
            });
            Vec::new()
        }),
    })
}

/// exec.go nukeK9sShell: delete the shell pod; a missing pod is fine.
async fn nuke_shell(acc: &SharedAccessor, pod_path: &str) -> Result<()> {
    let pods = pods_gvr();
    let del = acc.delete(&pods, pod_path, DeleteOptions::default());
    match tokio::time::timeout(std::time::Duration::from_millis(500), del).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) if format!("{e:#}").contains("not found") => Ok(()),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(anyhow!("context deadline exceeded")),
    }
}

/// exec.go launchShellPod: create the pod, then poll (50 x 2s) for Running.
async fn launch_shell_pod(
    acc: &SharedAccessor,
    pod_path: &str,
    manifest: &Value,
    node: &str,
) -> Result<()> {
    let pods = pods_gvr();
    acc.create(&pods, pod_path, manifest).await?;
    for _ in 0..50 {
        if let Ok(p) = acc.get(&pods, pod_path).await {
            if p.data.pointer("/status/phase").and_then(|v| v.as_str()) == Some("Running") {
                return Ok(());
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    bail!("unable to launch shell pod on node {node}")
}

/// k9s shell pod manifest (exec.go k9sShellPod).
pub(super) fn shell_pod_manifest(node: &str, name: &str, cfg: &crate::config::ShellPod) -> Value {
    let mut container = json!({
        "name": K9S_SHELL,
        "image": cfg.image,
        "volumeMounts": [{"name": "root-vol", "mountPath": "/host", "readOnly": true}],
        "resources": {"limits": {
            "cpu": cfg.limits.get("cpu").cloned().unwrap_or_else(|| "100m".into()),
            "memory": cfg.limits.get("memory").cloned().unwrap_or_else(|| "100Mi".into())}},
        "stdin": true,
        "tty": cfg.tty,
        "securityContext": {"privileged": true},
    });
    let mut volumes = vec![json!({"name": "root-vol", "hostPath": {"path": "/"}})];
    if !cfg.image_pull_policy.is_empty() {
        container["imagePullPolicy"] = json!(cfg.image_pull_policy);
    }
    if !cfg.command.is_empty() {
        container["command"] = json!(cfg.command);
    }
    if !cfg.args.is_empty() {
        container["args"] = json!(cfg.args);
    }
    for h in &cfg.host_path_volume {
        if let Some(m) = container["volumeMounts"].as_array_mut() {
            m.push(json!({"name": h.name, "mountPath": h.mount_path, "readOnly": h.read_only}));
        }
        volumes.push(json!({"name": h.name, "hostPath": {"path": h.host_path}}));
    }
    let mut pod = json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {"name": name, "namespace": cfg.namespace, "labels": cfg.labels},
        "spec": {
            "nodeName": node,
            "restartPolicy": "Never",
            "hostPID": true,
            "hostNetwork": true,
            "terminationGracePeriodSeconds": 0,
            "volumes": volumes,
            "containers": [container],
            "tolerations": [{"operator": "Exists"}],
        }
    });
    if !cfg.image_pull_secrets.is_empty() {
        pod["spec"]["imagePullSecrets"] = json!(cfg.image_pull_secrets);
    }
    pod
}

/// Rollback a replicaset's deployment to its pod template (kubectl rollout undo).
/// dao.ReplicaSet.Rollback -> kubectl DeploymentRollbacker.Rollback.
pub(super) async fn rollback_rs(acc: SharedAccessor, path: String) -> Result<()> {
    let rs_gvr = Gvr::parse("apps/v1/replicasets");
    let rs = acc.get(&rs_gvr, &path).await?;
    // getRSRevision
    if rs
        .data
        .pointer("/status/replicas")
        .and_then(Value::as_i64)
        .unwrap_or(0)
        != 0
    {
        bail!("can not rollback current replica");
    }
    let revision = |o: &DynamicObject| {
        o.metadata
            .annotations
            .as_ref()
            .and_then(|a| a.get(RS_REVISION))
            .and_then(|r| r.parse::<i64>().ok())
    };
    let version = revision(&rs).ok_or_else(|| anyhow!("revision conversion failed"))?;
    // controllerInfo
    let owner = rs
        .metadata
        .owner_references
        .as_ref()
        .and_then(|oo| oo.iter().find(|o| o.controller.is_some()))
        .ok_or_else(|| {
            anyhow!(
                "unable to find controller for replicaset: {}",
                rs.metadata.name.clone().unwrap_or_default()
            )
        })?;
    if owner.kind != "Deployment" {
        bail!("no rollbacker has been implemented for {:?}", owner.kind);
    }
    let ns = rs.metadata.namespace.clone().unwrap_or_default();
    let dp_gvr = Gvr::parse("apps/v1/deployments");
    let dp_path = fqn(&ns, &owner.name);
    let dp = acc
        .get(&dp_gvr, &dp_path)
        .await
        .map_err(|e| anyhow!("failed to retrieve Deployment {}: {e}", owner.name))?;
    // deploymentRevision over the deployment's replica sets.
    let uid = dp.metadata.uid.clone().unwrap_or_default();
    let sel = workload_selector(&dp)?;
    let target = acc
        .list(&rs_gvr, &ns, Some(&sel))
        .await?
        .into_iter()
        .filter(|r| {
            r.metadata
                .owner_references
                .iter()
                .flatten()
                .any(|o| o.controller == Some(true) && o.uid == uid)
        })
        .find(|r| revision(r) == Some(version))
        .ok_or_else(|| anyhow!("unable to find specified revision {version} in history"))?;
    if dp.data.pointer("/spec/paused").and_then(Value::as_bool) == Some(true) {
        bail!("you cannot rollback a paused deployment; resume it first with 'kubectl rollout resume' and try again");
    }
    let hashless = |t: Option<&Value>| {
        let mut t = t.cloned().unwrap_or(Value::Null);
        if let Some(l) = t
            .pointer_mut("/metadata/labels")
            .and_then(Value::as_object_mut)
        {
            l.remove(POD_TEMPLATE_HASH);
        }
        t
    };
    let tpl = hashless(target.data.pointer("/spec/template"));
    // "skipped rollback": the template already matches (k9s still reports success).
    if tpl == hashless(dp.data.pointer("/spec/template")) {
        return Ok(());
    }
    // Deployment keeps the skipped annotations; the rest come from the revision.
    let mut annotations = serde_json::Map::new();
    for k in ROLLBACK_SKIP_ANNOTATIONS {
        if let Some(v) = dp.metadata.annotations.as_ref().and_then(|a| a.get(k)) {
            annotations.insert(k.to_string(), json!(v));
        }
    }
    for (k, v) in target.metadata.annotations.iter().flatten() {
        if !ROLLBACK_SKIP_ANNOTATIONS.contains(&k.as_str()) {
            annotations.insert(k.clone(), json!(v));
        }
    }
    let patch = json!([
        {"op": "replace", "path": "/spec/template", "value": tpl},
        {"op": "replace", "path": "/metadata/annotations", "value": annotations},
    ]);
    acc.patch_json(&dp_gvr, &dp_path, &patch)
        .await
        .map_err(|e| anyhow!("failed restoring revision {version}: {e}"))
}

const RS_REVISION: &str = "deployment.kubernetes.io/revision";
const POD_TEMPLATE_HASH: &str = "pod-template-hash";
/// kubectl polymorphichelpers annotationsToSkip.
const ROLLBACK_SKIP_ANNOTATIONS: [&str; 6] = [
    "kubectl.kubernetes.io/last-applied-configuration",
    "deployment.kubernetes.io/revision",
    "deployment.kubernetes.io/revision-history",
    "deployment.kubernetes.io/desired-replicas",
    "deployment.kubernetes.io/max-replicas",
    "deprecated.deployment.rollback.to",
];

impl Browser {
    fn spawn(&self, ctx: &Ctx, fut: impl Future<Output = Result<Msg>> + Send + 'static) {
        spawn_msg(ctx, self.id, fut);
    }

    fn push_child(&self, ctx: &mut Ctx, gvr: Gvr, opts: Opts) -> Vec<ViewCmd> {
        vec![ViewCmd::Push(Box::new(super::child(
            ctx,
            self.deps.clone(),
            gvr,
            opts,
        )))]
    }

    fn logger(&self, ctx: &Ctx, prev: bool) -> LogOptions {
        let cfg = ctx
            .config
            .read()
            .map(|c| c.k9s.logger.clone())
            .unwrap_or_default();
        LogOptions {
            lines: cfg.tail,
            since_seconds: (cfg.since_seconds > 0).then_some(cfg.since_seconds),
            show_timestamp: cfg.show_time,
            previous: prev,
            follow: true,
            ..Default::default()
        }
    }

    /// Plugin/hotkey environment (Table.defaultEnv / Container.k9sEnv).
    fn env(&self, ctx: &Ctx) -> Env {
        let path = self.selected().unwrap_or_default();
        let row = self
            .table
            .data
            .rows
            .iter()
            .find(|r| r.row.id == path)
            .map(|r| &r.row);
        let kubeconfig = ctx
            .accessor
            .shell_env()
            .into_iter()
            .find(|(k, _)| k == "KUBECONFIG")
            .map(|(_, v)| v)
            .or_else(|| std::env::var("KUBECONFIG").ok())
            .unwrap_or_default();
        let mut env = default_env(
            &ctx.cluster_info,
            &kubeconfig,
            &path,
            &self.table.data.header,
            row,
        );
        env.set(
            "GROUPS",
            crate::ui::shell::plugins::groups_env(&ctx.accessor.impersonate_groups()),
        );
        env.set("FILTER", self.filter.clone());
        if self.filter.is_empty() {
            let (ns, n) = namespaced(&path);
            env.set("NAMESPACE", ns);
            env.set("FILTER", n);
        }
        env.set("RESOURCE_GROUP", self.gvr.g());
        env.set("RESOURCE_VERSION", self.gvr.v());
        env.set("RESOURCE_NAME", self.gvr.r());
        if self.kind == Kind::Container {
            let (ns, po) = namespaced(&self.path);
            env.set("NAMESPACE", ns);
            env.set("POD", po);
        }
        env
    }

    /// Dispatch an action.
    pub(super) fn run(&mut self, act: Act, ctx: &mut Ctx) -> Vec<ViewCmd> {
        let sel = self.selected();
        match act {
            Act::Help => {
                let hints = self.hint_list();
                vec![ViewCmd::Push((self.deps.help)(ctx, hints))]
            }
            Act::Mark => {
                self.table.toggle_mark();
                Vec::new()
            }
            Act::MarkRange => {
                self.table.span_mark();
                Vec::new()
            }
            Act::ClearMarks => {
                self.table.marks.clear();
                Vec::new()
            }
            Act::Save => self.save(ctx),
            Act::FilterMode => vec![ViewCmd::OpenFilterPrompt],
            Act::ToggleFaults => {
                self.table.toast = !self.table.toast;
                self.dirty = true;
                Vec::new()
            }
            Act::ToggleWide => {
                self.table.wide = !self.table.wide;
                Vec::new()
            }
            Act::SortCol(name, asc) => {
                self.table.sort_col_cmd(&name, asc);
                self.table.init_selected_column(&self.ns, ctx.has_metrics);
                self.dirty = true;
                Vec::new()
            }
            Act::SortSelected => {
                self.table.sort_selected_column(&self.ns, ctx.has_metrics);
                self.dirty = true;
                Vec::new()
            }
            Act::FilterReset => {
                let had = !self.filter.is_empty();
                self.filter.clear();
                if had {
                    self.label = None;
                    self.label_from_cmd = false;
                    self.dirty = true;
                    self.spawn_load(ctx, true);
                }
                vec![ViewCmd::Pop]
            }
            Act::Copy | Act::CopyNs => {
                let paths = self.table.selected_items();
                if paths.is_empty() {
                    return Vec::new();
                }
                let ns = act == Act::CopyNs;
                let parts: Vec<String> = paths
                    .iter()
                    .map(|p| {
                        let (a, b) = namespaced(p);
                        if ns { a } else { b }.to_string()
                    })
                    .collect();
                if let Err(e) = (self.deps.clipboard)(&parts.join("\n")) {
                    return vec![err(e.to_string())];
                }
                let what = if ns { "namespace" } else { "name" };
                if parts.len() > 1 {
                    vec![info(format!(
                        "{} resource {what}s copied to clipboard...",
                        parts.len()
                    ))]
                } else {
                    vec![info(format!("Resource {what} copied to clipboard..."))]
                }
            }
            Act::Enter => self.enter(ctx),
            Act::Refresh => {
                self.spawn_load(ctx, true);
                vec![info("Refreshing...")]
            }
            Act::Edit => self.edit(sel, ctx),
            Act::Delete => self.delete(ctx),
            Act::Yaml | Act::Describe => self.details(act == Act::Yaml, sel, ctx),
            Act::Warp => {
                let Some(p) = sel else {
                    return Vec::new();
                };
                let (ns, _) = namespaced(&p);
                vec![ViewCmd::Command(format!("{} {}", self.gvr, ns))]
            }
            Act::SwitchNs(ns) => self.switch_ns(&ns, ctx),
            Act::Logs(prev) => self.logs(prev, ctx),
            Act::SetImage => self.set_image(sel, ctx),
            Act::JumpOwner => {
                let Some(path) = sel else {
                    return Vec::new();
                };
                let (acc, gvr) = (ctx.accessor.clone(), self.gvr.clone());
                self.spawn(ctx, async move {
                    let o = match acc.get(&gvr, &path).await {
                        Ok(o) => o,
                        Err(e) => {
                            return Ok(Msg::Cmds(vec![warn(format!("Unable to jump owner: {e}"))]))
                        }
                    };
                    let owners: Vec<(String, String, String)> = o
                        .metadata
                        .owner_references
                        .unwrap_or_default()
                        .into_iter()
                        .map(|r| (r.api_version, r.kind, r.name))
                        .collect();
                    if owners.is_empty() {
                        return Ok(Msg::Cmds(vec![warn(
                            "Unable to jump owner: no owner found",
                        )]));
                    }
                    Ok(Msg::Owners {
                        ns: namespaced(&path).0.to_string(),
                        owners,
                    })
                });
                Vec::new()
            }
            Act::ShowPortForward => self.show_pf(sel, ctx),
            Act::PortForward => self.port_forward(sel, ctx),
            Act::Scale => self.scale(ctx),
            Act::Restart => self.restart(),
            Act::Kill => self.kill(ctx),
            Act::Shell | Act::Attach => self.shell(sel, act == Act::Attach, ctx),
            Act::Transfer => self.transfer(sel, ctx),
            Act::Sanitize => self.sanitize(),
            Act::ShowNode => {
                let Some(path) = sel else {
                    return Vec::new();
                };
                let acc = ctx.accessor.clone();
                self.spawn(ctx, async move {
                    let po = acc.get(&pods_gvr(), &path).await?;
                    let node = po
                        .data
                        .pointer("/spec/nodeName")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if node.is_empty() {
                        return Ok(Msg::Cmds(vec![err("no node assigned")]));
                    }
                    Ok(Msg::ShowNode(node.to_string()))
                });
                Vec::new()
            }
            Act::ReplicaSets => {
                let Some(path) = sel else {
                    return Vec::new();
                };
                let (acc, gvr) = (ctx.accessor.clone(), self.gvr.clone());
                self.spawn(ctx, async move {
                    let dp = acc.get(&gvr, &path).await?;
                    Ok(Msg::ShowReplicaSets {
                        label: workload_selector(&dp)?,
                        path,
                    })
                });
                Vec::new()
            }
            Act::Rollback => {
                let Some(path) = sel else {
                    return Vec::new();
                };
                let (gvr, id) = (self.gvr.clone(), self.id);
                vec![ViewCmd::Dialog(Dialog::Confirm {
                    title: "Rollback".into(),
                    msg: format!("Rollback {gvr} {path}?"),
                    on_ok: Box::new(move |ctx| {
                        let acc = ctx.accessor.clone();
                        let p = path.clone();
                        spawn_msg(ctx, id, async move {
                            rollback_rs(acc, p.clone()).await?;
                            Ok(Msg::Cmds(vec![info(format!(
                                "{p} successfully rolled back"
                            ))]))
                        });
                        let tx = ctx.sender();
                        post(&tx, id, Msg::Refresh);
                        vec![info(format!("Rolling back {gvr} {path}"))]
                    }),
                })]
            }
            Act::Trigger => self.trigger(),
            Act::SuspendResume => self.suspend_resume(ctx),
            Act::Cordon(on) => self.cordon(on),
            Act::Drain => self.drain(),
            Act::NodeShell => self.node_shell(sel, ctx),
            Act::RenameContext => {
                let Some(name) = sel else {
                    return Vec::new();
                };
                let id = self.id;
                vec![ViewCmd::Dialog(Dialog::Rename {
                    name: name.clone(),
                    on_ok: Box::new(move |ctx, new| {
                        // renameDialogCallback: flash the error, refresh on success.
                        let acc = ctx.accessor.clone();
                        spawn_msg(ctx, id, async move {
                            acc.rename_context(&name, &new).await?;
                            // c.Refresh repaints the model's last data; the next
                            // refresh tick shows the change.
                            Ok(Msg::Cmds(Vec::new()))
                        });
                        Vec::new()
                    }),
                })]
            }
            Act::DeleteContext => {
                let Some(name) = sel else {
                    return Vec::new();
                };
                let id = self.id;
                vec![ViewCmd::Dialog(Dialog::Confirm {
                    title: "Delete".into(),
                    msg: format!(
                        "Delete context {}?",
                        crate::ui::shell::text::go_quote(&name)
                    ),
                    on_ok: Box::new(move |ctx| {
                        let acc = ctx.accessor.clone();
                        spawn_msg(ctx, id, async move {
                            acc.delete_context(&name).await?;
                            // c.Refresh repaints the model's last data; the next
                            // refresh tick shows the change.
                            Ok(Msg::Cmds(Vec::new()))
                        });
                        Vec::new()
                    }),
                })]
            }
            Act::UseNamespace => {
                let Some(path) = sel else {
                    return Vec::new();
                };
                let ns = namespaced(&path).1.to_string();
                if print_namespace(&cleanse_namespace(&ctx.namespace)) == ns {
                    return Vec::new();
                }
                // Config.SetActiveNamespace adds the favorite the ns view decorates.
                self.cc.set_active_namespace(&ns);
                self.dirty = true;
                vec![ViewCmd::SetNamespace(ns)]
            }
            Act::Bench => self.bench(sel, ctx),
            Act::Decode => {
                let Some(path) = sel else {
                    return Vec::new();
                };
                let (acc, gvr) = (ctx.accessor.clone(), self.gvr.clone());
                self.spawn(ctx, async move {
                    let o = acc.get(&gvr, &path).await?;
                    let mut m = serde_yaml::Mapping::new();
                    if let Some(data) = o.data.get("data").and_then(|v| v.as_object()) {
                        for (k, v) in data {
                            let raw = v.as_str().unwrap_or("");
                            let dec = base64_decode(raw)
                                .ok_or_else(|| anyhow!("Error decoding secret {k}"))?;
                            m.insert(
                                k.clone().into(),
                                String::from_utf8_lossy(&dec).to_string().into(),
                            );
                        }
                    }
                    Ok(Msg::Text {
                        name: "Secret Decoder".into(),
                        title: path,
                        body: serde_yaml::to_string(&m)?,
                    })
                });
                Vec::new()
            }
            Act::ShowVulns => self.push_child(
                ctx,
                Gvr::parse("scans"),
                Opts {
                    source: Some((self.gvr.clone(), sel.unwrap_or_default())),
                    ..Default::default()
                },
            ),
            Act::UsedBy => {
                let Some(path) = sel else {
                    return Vec::new();
                };
                let (acc, gvr) = (ctx.accessor.clone(), self.gvr.clone());
                self.spawn(ctx, async move {
                    let refs = loader::scan_refs(&acc, &gvr, &path).await?;
                    if refs.is_empty() {
                        return Ok(Msg::Cmds(vec![warn(format!(
                            "No references found at this time for {gvr}::{path}. Check again later!"
                        ))]));
                    }
                    Ok(Msg::Refs { gvr, path })
                });
                Vec::new()
            }
            Act::Rules => {
                let Some(path) = sel else {
                    return Vec::new();
                };
                let kind = match self.kind {
                    Kind::ServiceAccount => "ServiceAccount",
                    Kind::Group => "Group",
                    _ => "User",
                };
                self.push_child(
                    ctx,
                    Gvr::parse("policy"),
                    Opts {
                        subject: Some((kind.into(), path)),
                        ..Default::default()
                    },
                )
            }
            Act::EventFaults => {
                let msg = if self.filter == FAULTS_FILTER {
                    self.set_filter_text("");
                    "Showing all events"
                } else {
                    self.set_filter_text(FAULTS_FILTER);
                    "Showing Warning and Error events only"
                };
                self.table.select_first();
                vec![info(msg)]
            }
            Act::PfDelete => self.pf_delete(),
            Act::DirApply | Act::DirDelete => self.dir_kubectl(sel, act == Act::DirApply, ctx),
            Act::DirEdit => {
                let Some(path) = sel else {
                    return Vec::new();
                };
                if !is_manifest(&path) {
                    return vec![err("you must select a manifest")];
                }
                edit_file(&path)
            }
            Act::HelmValues => self.helm_values(sel, ctx),
            Act::HelmReleases => {
                let Some(path) = sel else {
                    return Vec::new();
                };
                self.push_child(
                    ctx,
                    Gvr::parse("helm-history"),
                    Opts {
                        ns: Some(namespaced(&path).0.to_string()),
                        path,
                        ..Default::default()
                    },
                )
            }
            Act::HelmRollback => self.helm_rollback(sel, ctx),
            Act::Plugin(name) => self.plugin(&name, sel, ctx),
            Act::HotKey(name) => {
                let Some(hk) = ctx.hotkeys.hot_keys.get(&name) else {
                    return Vec::new();
                };
                let cmd = self.env(ctx).substitute(&hk.command);
                vec![crate::ui::shell::plugins::hotkey_goto(cmd, hk.keep_history)]
            }
        }
    }

    /// Async follow-ups.
    pub(super) fn on_msg(&mut self, msg: Msg, ctx: &mut Ctx) -> Vec<ViewCmd> {
        match msg {
            Msg::Cmds(c) => c,
            Msg::Logs(opts) => vec![ViewCmd::Push((self.deps.logs)(ctx, opts))],
            Msg::ShowPods {
                path,
                label,
                field,
                ns,
            } => self.push_child(
                ctx,
                pods_gvr(),
                Opts {
                    ns: Some(if ns.is_empty() {
                        NAMESPACE_ALL.into()
                    } else {
                        ns
                    }),
                    path,
                    label,
                    field,
                    ..Default::default()
                },
            ),
            Msg::ShowReplicaSets { path, label } => self.push_child(
                ctx,
                Gvr::parse("apps/v1/replicasets"),
                Opts {
                    ns: Some(namespaced(&path).0.to_string()),
                    path,
                    label: Some(label),
                    ..Default::default()
                },
            ),
            Msg::ShowJobs { path, uid } => self.push_child(
                ctx,
                Gvr::parse("batch/v1/jobs"),
                Opts {
                    ns: Some(namespaced(&path).0.to_string()),
                    path,
                    uid: Some(uid),
                    ..Default::default()
                },
            ),
            Msg::ShowNode(node) => self.push_child(
                ctx,
                Gvr::parse("v1/nodes"),
                Opts {
                    instance: node,
                    ..Default::default()
                },
            ),
            Msg::ShowPortForwards(path) => self.push_child(
                ctx,
                Gvr::parse("portforwards"),
                Opts {
                    path,
                    ..Default::default()
                },
            ),
            Msg::Owners { ns, owners } => {
                if owners.len() == 1 {
                    return jump_owner(ctx, &self.deps, &ns, &owners[0]);
                }
                let items = owners
                    .iter()
                    .enumerate()
                    .map(|(i, o)| format!("{i}: {}", o.1))
                    .collect();
                let deps = self.deps.clone();
                // owner_extender.go: dialog.ShowSelection over the table; the
                // picked list index addresses the owner list directly.
                vec![ViewCmd::Dialog(Dialog::Selection {
                    title: "Jump To".into(),
                    options: items,
                    on_select: Box::new(move |ctx, idx| match owners.get(idx) {
                        Some(o) => jump_owner(ctx, &deps, &ns, o),
                        None => Vec::new(),
                    }),
                })]
            }
            Msg::PickShell {
                path,
                containers,
                attach,
                os,
            } => {
                if containers.len() == 1 {
                    return vec![exec_cmd(ctx, &path, &containers[0], attach, &os)];
                }
                let p = Picker::new(
                    ctx,
                    "Containers Picker",
                    containers,
                    Box::new(move |ctx, _, co| vec![exec_cmd(ctx, &path, &co, attach, &os)]),
                );
                vec![ViewCmd::Push(Box::new(p))]
            }
            Msg::PfStarted(handles) => {
                let ports: Vec<String> = handles
                    .iter()
                    .map(|h| h.info.spec.local_port.to_string())
                    .collect();
                for mut h in handles {
                    let id = h.info.spec.id();
                    if let Some(died) = h.died.take() {
                        let tx = ctx.sender();
                        let pid = id.clone();
                        ctx.spawn(async move {
                            if let Ok(err) = died.await {
                                let _ = tx.send(AppEvent::Custom {
                                    view: crate::ui::shell::SHELL_VIEW_ID,
                                    payload: Box::new(crate::ui::app::PortForwardDied {
                                        id: pid,
                                        err,
                                    }),
                                });
                            }
                        });
                    }
                    ctx.port_forwards.insert(id, h);
                }
                self.dirty = true;
                if ports.len() == 1 {
                    vec![info(format!("PortForward activated {}", ports[0]))]
                } else {
                    vec![info(format!("PortForwards activated {}", ports.join(",")))]
                }
            }
            Msg::Text { name, title, body } => {
                vec![ViewCmd::Push((self.deps.text)(ctx, name, title, body))]
            }
            Msg::Release {
                title,
                subject,
                body,
            } => vec![ViewCmd::Push((self.deps.release)(
                ctx, title, subject, body,
            ))],
            Msg::Values { subject, user, all } => {
                vec![ViewCmd::Push((self.deps.values)(ctx, subject, user, all))]
            }
            Msg::Refs { gvr, path } => {
                let mut out = vec![info(format!("Viewing references for {gvr}::{path}"))];
                out.extend(self.push_child(
                    ctx,
                    Gvr::parse("references"),
                    Opts {
                        source: Some((gvr, path)),
                        ..Default::default()
                    },
                ));
                out
            }
            Msg::Deleted(paths) => {
                ctx.port_forwards.retain(|k, _| {
                    !paths
                        .iter()
                        .any(|p| k == p || k.starts_with(&format!("{p}|")))
                });
                for p in &paths {
                    self.table.marks.remove(p);
                }
                self.spawn_load(ctx, true);
                Vec::new()
            }
            Msg::Dim(paths) => {
                self.table.deleted.extend(paths);
                Vec::new()
            }
            Msg::WatchFailed(e) => {
                self.watch_failed = true;
                vec![ViewCmd::Flash(FlashLevel::Err, e)]
            }
            Msg::FilterCompleted => {
                self.completion_pending = false;
                self.filter_completed();
                Vec::new()
            }
            Msg::ListAllNs(ok) => {
                if self.list_all_ns != ok {
                    self.list_all_ns = ok;
                    self.refresh_actions(ctx);
                }
                Vec::new()
            }
            Msg::Refresh => {
                self.dirty = true;
                self.spawn_load(ctx, true);
                Vec::new()
            }
            Msg::InitRefresh => {
                if self.active {
                    self.spawn_load(ctx, false);
                }
                Vec::new()
            }
            Msg::BenchDone(res) => {
                self.bench = None;
                match res {
                    Ok(_) => {
                        // runBenchmark: completed status, cleared 2s later.
                        let tx = ctx.sender();
                        let id = self.id;
                        ctx.spawn(async move {
                            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                            post(&tx, id, Msg::ClearStatus);
                        });
                        vec![ViewCmd::Status(
                            FlashLevel::Info,
                            "Benchmark Completed!".into(),
                        )]
                    }
                    Err(e) => vec![
                        err(format!("Benchmark failed {e}")),
                        ViewCmd::ClearStatus(false),
                    ],
                }
            }
            Msg::ClearStatus => vec![ViewCmd::ClearStatus(true)],
        }
    }

    fn save(&mut self, ctx: &Ctx) -> Vec<ViewCmd> {
        let dir = (self.deps.dumps_dir)(ctx);
        let mut td = self.table.data.clone();
        if self.table.toast {
            td.rows = match td.header.index_of("VALID") {
                Some(i) => td
                    .rows
                    .into_iter()
                    .filter(|r| r.row.fields.get(i).map(|f| !f.is_empty()).unwrap_or(false))
                    .collect(),
                None => Vec::new(),
            };
        }
        let spec = self.client_filter();
        if !spec.is_none() {
            td = (self.deps.apply_filter)(&td, &spec);
        }
        let ns = if is_cluster_wide_ns(&td.namespace) {
            NAMESPACE_ALL.to_string()
        } else {
            td.namespace.clone()
        };
        let title = self.gvr.r().to_string();
        let name = if self.path.is_empty() {
            title
        } else {
            format!("{title}-{}", sanitize_file_name(&self.path))
        };
        let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0);
        let file = format!("{name}-{ns}-{nanos}.csv").to_lowercase();
        let with_ns = !self.renderer.is_generic()
            && td.header.index_of("NAMESPACE").is_none()
            && self.renderer.header(NAMESPACE_ALL).index_of("NAMESPACE") == Some(0);
        let res = (|| -> Result<PathBuf> {
            crate::config::paths::ensure_full_path(&dir)?;
            let p = dir.join(&file);
            let mut header: Vec<String> = td.header.iter().map(|h| h.name.clone()).collect();
            let mut rows: Vec<Vec<String>> = td.rows.iter().map(|r| r.row.fields.clone()).collect();
            // k9s typed renderers always carry NAMESPACE (only the view hides
            // it); ours drop it outside all-namespaces, so put it back.
            if with_ns {
                header.insert(0, "NAMESPACE".into());
                for (r, re) in rows.iter_mut().zip(td.rows.iter()) {
                    r.insert(0, namespaced(&re.row.id).0.to_string());
                }
            }
            // k9s OpenFile(O_CREATE|O_WRONLY, 0600): no truncation.
            let mut opts = std::fs::OpenOptions::new();
            #[allow(clippy::suspicious_open_options)]
            opts.create(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            use std::io::Write;
            opts.open(&p)?
                .write_all(to_csv(&header, &rows).as_bytes())?;
            Ok(p)
        })();
        match res {
            Ok(p) => {
                let base = p
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_default();
                vec![info(format!(
                    "File saved successfully: {:?}",
                    markup::truncate(&base, 50)
                ))]
            }
            Err(e) => vec![err(e.to_string())],
        }
    }

    fn switch_ns(&mut self, ns: &str, ctx: &Ctx) -> Vec<ViewCmd> {
        // browser.go switchNamespaceCmd: the user must list the resource there.
        let (acc, g, n) = (ctx.accessor.clone(), self.gvr.clone(), ns.to_string());
        match block_on(async move { acc.can(&g, &n, "list").await }) {
            Ok(true) => {}
            Ok(false) => return vec![err(format!("access denied for user on: {ns}/{}", self.gvr))],
            Err(e) => return vec![err(e.to_string())],
        }
        self.set_namespace(ns);
        self.cc.set_active_namespace(ns);
        self.table.select_first();
        // b.refresh() is Start: reset the first view counter and watch again.
        self.first_view = false;
        self.watch_checked = false;
        self.spawn_load(ctx, true);
        let msg = if is_cluster_scoped(ns) {
            format!("Viewing {}...", self.gvr)
        } else {
            format!(
                "Viewing {} in namespace `{}`...",
                self.gvr,
                print_namespace(&cleanse_namespace(ns))
            )
        };
        vec![ViewCmd::SetNamespace(ns.to_string()), info(msg)]
    }

    fn enter(&mut self, ctx: &mut Ctx) -> Vec<ViewCmd> {
        let Some(path) = self.selected() else {
            return Vec::new();
        };
        let acc = ctx.accessor.clone();
        let gvr = self.gvr.clone();
        match self.kind {
            Kind::Generic
            | Kind::Secret
            | Kind::ConfigMap
            | Kind::Pvc
            | Kind::PriorityClass
            | Kind::Event => {
                vec![ViewCmd::Push((self.deps.describe)(ctx, gvr, path))]
            }
            Kind::RbacSource => self.push_child(
                ctx,
                Gvr::parse("rbac"),
                Opts {
                    source: Some((gvr, path)),
                    ..Default::default()
                },
            ),
            Kind::Pod => self.push_child(
                ctx,
                Gvr::parse("containers"),
                Opts {
                    path,
                    ..Default::default()
                },
            ),
            Kind::Container => self.logs(false, ctx),
            Kind::Deploy | Kind::StatefulSet | Kind::DaemonSet | Kind::ReplicaSet | Kind::Job => {
                self.spawn(ctx, async move {
                    let o = acc.get(&gvr, &path).await?;
                    Ok(Msg::ShowPods {
                        ns: namespaced(&path).0.to_string(),
                        label: Some(workload_selector(&o)?),
                        field: None,
                        path,
                    })
                });
                Vec::new()
            }
            Kind::CronJob => {
                self.spawn(ctx, async move {
                    let o = acc.get(&gvr, &path).await?;
                    Ok(Msg::ShowJobs {
                        uid: o.metadata.uid.clone().unwrap_or_default(),
                        path,
                    })
                });
                Vec::new()
            }
            Kind::Node => self.on_msg(
                Msg::ShowPods {
                    field: Some(format!("spec.nodeName={path}")),
                    label: None,
                    ns: String::new(),
                    path,
                },
                ctx,
            ),
            // ns.go switchNs: useNamespace, then gotoResource("v1/pods <ns>",
            // clearStack=false, pushCmd=true): the pods view lands in history.
            Kind::Namespace => {
                let ns = namespaced(&path).1.to_string();
                let mut out = Vec::new();
                if print_namespace(&cleanse_namespace(&ctx.namespace)) != ns {
                    out.push(ViewCmd::SetNamespace(ns.clone()));
                }
                out.push(ViewCmd::Goto {
                    cmd: format!("{} {ns}", pods_gvr()),
                    path: String::new(),
                });
                out
            }
            // view/context.go useContext -> app.switchContext: the shell
            // switches, flashes and jumps to the context's last view.
            Kind::Context => vec![ViewCmd::UseContext(path)],
            Kind::Service => {
                self.spawn(ctx, async move {
                    let o = acc.get(&gvr, &path).await?;
                    if o.data.pointer("/spec/type").and_then(|v| v.as_str()) == Some("ExternalName")
                    {
                        return Ok(Msg::Cmds(vec![warn(format!(
                            "No matching pods. Service {path} is an external service."
                        ))]));
                    }
                    let Some(sel) = o
                        .data
                        .pointer("/spec/selector")
                        .and_then(|v| v.as_object())
                        .and_then(map_selector_string)
                    else {
                        return Ok(Msg::Cmds(vec![warn(format!(
                            "No matching pods. Service {path} does not provide any selectors"
                        ))]));
                    };
                    Ok(Msg::ShowPods {
                        ns: namespaced(&path).0.to_string(),
                        label: Some(sel),
                        field: None,
                        path,
                    })
                });
                Vec::new()
            }
            // crd.go showCRD: gotoResource(crd name, "", false, true), i.e.
            // the name runs as a command (aliases, history).
            Kind::Crd => vec![ViewCmd::Goto {
                cmd: namespaced(&path).1.to_string(),
                path: String::new(),
            }],
            Kind::ServiceAccount | Kind::User | Kind::Group => self.run(Act::Rules, ctx),
            Kind::Rules | Kind::Policy => Vec::new(),
            Kind::Alias => vec![ViewCmd::Command(Gvr::parse(&path).to_string())],
            Kind::PortForward => self.push_child(
                ctx,
                Gvr::parse("benchmarks"),
                Opts {
                    path,
                    ..Default::default()
                },
            ),
            Kind::ScreenDump => edit_file(&path),
            Kind::Benchmark => {
                let file = if Path::new(&path).is_absolute() {
                    PathBuf::from(&path)
                } else {
                    self.dir.join(&path)
                };
                match std::fs::read_to_string(&file) {
                    Ok(body) => {
                        let base = file
                            .file_name()
                            .map(|f| f.to_string_lossy().to_string())
                            .unwrap_or_default();
                        let ee: Vec<&str> = base.split('_').collect();
                        let subject =
                            format!("{}/{}", ee.first().unwrap_or(&""), ee.get(1).unwrap_or(&""));
                        vec![ViewCmd::Push((self.deps.text)(
                            ctx,
                            "Results".into(),
                            subject,
                            body,
                        ))]
                    }
                    Err(e) => vec![err(format!("Unable to load bench file {e}"))],
                }
            }
            Kind::Dir => {
                if is_manifest(&path) {
                    return vec![err("you must select a directory")];
                }
                self.push_child(
                    ctx,
                    Gvr::parse("dirs"),
                    Opts {
                        dir: Some(PathBuf::from(&path)),
                        path,
                        ..Default::default()
                    },
                )
            }
            Kind::Helm => self.run(Act::HelmReleases, ctx),
            Kind::HelmHistory => self.helm_values(Some(path), ctx),
            Kind::Workload => {
                let tt: Vec<&str> = path.split('|').collect();
                if tt.len() != 3 {
                    return vec![err(format!("unable to parse path: {path:?}"))];
                }
                self.push_child(
                    ctx,
                    Gvr::parse(tt[0]),
                    Opts {
                        instance: fqn(tt[1], tt[2]),
                        ns: Some(tt[1].to_string()),
                        ..Default::default()
                    },
                )
            }
            Kind::ImageScan => view_cve(&path),
            Kind::Reference => {
                let g = self.table.selected_cell("GVR").unwrap_or_default();
                let (ns, _) = namespaced(&path);
                self.push_child(
                    ctx,
                    Gvr::parse(&g),
                    Opts {
                        instance: path.clone(),
                        ns: Some(ns.to_string()),
                        ..Default::default()
                    },
                )
            }
        }
    }

    /// Workload rows carry `gvr|ns|name`.
    fn target(&self, path: &str) -> Option<(Gvr, String)> {
        if self.kind == Kind::Workload {
            let tt: Vec<&str> = path.split('|').collect();
            if tt.len() != 3 {
                return None;
            }
            return Some((Gvr::parse(tt[0]), fqn(tt[1], tt[2])));
        }
        Some((self.gvr.clone(), path.to_string()))
    }

    fn edit(&mut self, sel: Option<String>, ctx: &Ctx) -> Vec<ViewCmd> {
        let Some(path) = sel else {
            return Vec::new();
        };
        let Some((gvr, path)) = self.target(&path) else {
            return vec![err(format!("unable to parse path: {path:?}"))];
        };
        let (ns, n) = namespaced(&path);
        if n.is_empty() {
            return vec![err(format!("missing resource name in path {path:?}"))];
        }
        // browser.go editRes: CanI patch first.
        let ns = if is_cluster_scoped(ns) { "" } else { ns };
        let (acc, g, ns2, n2) = (
            ctx.accessor.clone(),
            gvr.clone(),
            ns.to_string(),
            n.to_string(),
        );
        if !matches!(
            block_on(async move { acc.can_named(&g, &ns2, &n2, "patch").await }),
            Ok(true)
        ) {
            return vec![err(format!("current user can't edit resource {gvr}"))];
        }
        vec![ViewCmd::Suspend {
            argv: ctx.accessor.edit_argv(&gvr, &path),
            banner: String::new(),
            env: ctx.accessor.shell_env(),
            on_done: Some(Box::new(|_ctx, st| {
                if st.success() {
                    Vec::new()
                } else {
                    vec![err(format!("Edit command failed: {st}"))]
                }
            })),
        }]
    }

    fn details(&mut self, yaml: bool, sel: Option<String>, ctx: &mut Ctx) -> Vec<ViewCmd> {
        let Some(path) = sel else {
            return Vec::new();
        };
        if matches!(self.kind, Kind::Helm | Kind::HelmHistory) {
            let (rel, rev) = self.helm_target(&path);
            let (ns, name) = namespaced(&rel);
            let mut argv = vec![
                "helm".to_string(),
                "get".into(),
                if yaml { "manifest" } else { "notes" }.into(),
                name.to_string(),
            ];
            if !ns.is_empty() {
                argv.extend(["-n".into(), ns.to_string()]);
            }
            if let Some(r) = rev {
                argv.extend(["--revision".into(), r]);
            }
            argv.extend(self.kube_ctx_args(ctx));
            let (run, env) = (self.deps.run.clone(), ctx.accessor.shell_env());
            let title = if yaml { "YAML" } else { "Describe" };
            // The selected path (release, or release:revision) is the subject.
            let subject = if self.kind == Kind::Helm { rel } else { path };
            self.spawn(ctx, async move {
                let out = run(argv, env).await?;
                // Release.Info.Notes, without the CLI's "NOTES:" header.
                let body = if yaml {
                    out
                } else {
                    out.strip_prefix("NOTES:\n").unwrap_or(&out).to_string()
                };
                Ok(Msg::Release {
                    title: title.into(),
                    subject,
                    body,
                })
            });
            return Vec::new();
        }
        if self.kind == Kind::Dir {
            if Path::new(&path).extension().is_none() {
                return Vec::new();
            }
            return match std::fs::read_to_string(&path) {
                Ok(body) => vec![ViewCmd::Push((self.deps.text)(
                    ctx,
                    "YAML".into(),
                    path,
                    body,
                ))],
                Err(e) => vec![err(e.to_string())],
            };
        }
        let Some((gvr, path)) = self.target(&path) else {
            return vec![err(format!("unable to parse path: {path:?}"))];
        };
        // node.go yamlCmd: a one-shot YAML Details view, not a live view.
        if yaml && self.kind == Kind::Node {
            let acc = ctx.accessor.clone();
            self.spawn(ctx, async move {
                let body = acc.to_yaml(&gvr, &path, false).await.map_err(|e| {
                    anyhow!(
                        "Unable to get resource {} -- {e}",
                        crate::ui::shell::text::go_quote(&gvr.to_string())
                    )
                })?;
                Ok(Msg::Text {
                    name: "YAML".into(),
                    title: path,
                    body,
                })
            });
            return Vec::new();
        }
        let v = if yaml {
            (self.deps.yaml)(ctx, gvr, path)
        } else {
            (self.deps.describe)(ctx, gvr, path)
        };
        vec![ViewCmd::Push(v)]
    }

    fn kube_ctx_args(&self, ctx: &Ctx) -> Vec<String> {
        let c = ctx.accessor.active_context();
        if c.is_empty() {
            Vec::new()
        } else {
            vec!["--kube-context".into(), c]
        }
    }

    /// (release fqn, revision) of a helm row.
    pub(super) fn helm_target(&self, id: &str) -> (String, Option<String>) {
        if self.kind == Kind::Helm {
            return (id.to_string(), None);
        }
        // `name:revision`; anything else has no revision (helm_history.go).
        let tt: Vec<&str> = namespaced(id).1.split(':').collect();
        let rev = if tt.len() == 2 { tt[1] } else { "" };
        (self.path.clone(), Some(rev.to_string()))
    }

    fn delete(&mut self, ctx: &Ctx) -> Vec<ViewCmd> {
        let sels = self.table.selected_items();
        if sels.is_empty() {
            return Vec::new();
        }
        let gvr = self.gvr.clone();
        let msg = if sels.len() > 1 {
            format!("Delete {} marked {}?", sels.len(), gvr)
        } else {
            format!("Delete {} {}?", gvr.r(), sels[0])
        };
        let id = self.id;
        let kind = self.kind;
        let run = self.deps.run.clone();
        let kctx = self.kube_ctx_args(ctx);
        let flash = move |n: usize, first: &str| {
            if n > 1 {
                info(format!("Delete {n} marked {gvr}"))
            } else {
                info(format!("Delete resource {gvr} {first}"))
            }
        };
        let gvr = self.gvr.clone();
        let exec = move |ctx: &mut Ctx, sels: Vec<String>, opts: DeleteOptions| -> Vec<ViewCmd> {
            let out = vec![flash(sels.len(), &sels[0])];
            let acc = ctx.accessor.clone();
            let env = acc.shell_env();
            let tx = ctx.sender();
            post(&tx, id, Msg::Dim(sels.clone()));
            ctx.spawn(async move {
                let mut done = Vec::new();
                for sel in &sels {
                    let res: Result<()> = match kind {
                        Kind::ScreenDump | Kind::Benchmark => {
                            std::fs::remove_file(sel).map_err(Into::into)
                        }
                        Kind::Helm => {
                            let (ns, n) = namespaced(sel);
                            let mut argv =
                                vec!["helm".to_string(), "uninstall".into(), n.to_string()];
                            if !ns.is_empty() {
                                argv.extend(["-n".into(), ns.to_string()]);
                            }
                            argv.extend(kctx.clone());
                            run(argv, env.clone()).await.map(|_| ())
                        }
                        Kind::Workload => {
                            let tt: Vec<&str> = sel.split('|').collect();
                            if tt.len() != 3 {
                                Err(anyhow!("unable to parse path: {sel:?}"))
                            } else {
                                acc.delete(&Gvr::parse(tt[0]), &fqn(tt[1], tt[2]), opts)
                                    .await
                            }
                        }
                        _ => acc.delete(&gvr, sel, opts).await,
                    };
                    match res {
                        Ok(()) => done.push(sel.clone()),
                        Err(e) => post_err(&tx, id, format!("Delete failed with `{e}")),
                    }
                }
                post(&tx, id, Msg::Deleted(done));
            });
            out
        };
        if !self.meta.is_k8s() {
            return vec![ViewCmd::Dialog(Dialog::Confirm {
                title: "Confirm Delete".into(),
                msg,
                on_ok: Box::new(move |ctx| exec(ctx, sels, DeleteOptions::default())),
            })];
        }
        vec![ViewCmd::Dialog(Dialog::Delete {
            msg,
            on_ok: Box::new(move |ctx, mut opts| {
                if opts.force && opts.grace.is_none() {
                    opts.grace = Some(0);
                }
                exec(ctx, sels, opts)
            }),
        })]
    }

    fn logs(&mut self, prev: bool, ctx: &mut Ctx) -> Vec<ViewCmd> {
        let Some(path) = self.selected() else {
            return Vec::new();
        };
        let mut base = self.logger(ctx, prev);
        // showLogs: CanForResource(ns, pods, list) first.
        let pods_ns = if self.kind == Kind::Container {
            namespaced(&self.path).0.to_string()
        } else {
            namespaced(&path).0.to_string()
        };
        let can_list = |acc: SharedAccessor, ns: String| async move {
            if !acc.can(&crate::k8s::known::pods(), &ns, "list").await? {
                bail!("[list] access denied on resource {ns:?}:\"v1/pods\"");
            }
            Ok::<(), anyhow::Error>(())
        };
        if self.kind == Kind::Container {
            let (ns, po) = namespaced(&self.path);
            base.namespace = ns.to_string();
            base.path = po.to_string();
            base.container = Some(path);
            let acc = ctx.accessor.clone();
            self.spawn(ctx, async move {
                can_list(acc, pods_ns).await?;
                Ok(Msg::Logs(base))
            });
            return Vec::new();
        }
        let (acc, gvr, kind) = (ctx.accessor.clone(), self.gvr.clone(), self.kind);
        self.spawn(ctx, async move {
            can_list(acc.clone(), pods_ns).await?;
            let o = acc.get(&gvr, &path).await?;
            let (ns, name) = namespaced(&path);
            base.namespace = ns.to_string();
            match kind {
                Kind::Pod => {
                    base.path = name.to_string();
                    let spec = pod_spec(&o).cloned().unwrap_or(Value::Null);
                    Ok(Msg::Logs(pod_log_options(&o, &spec, base)))
                }
                Kind::Service => {
                    base.path = o
                        .data
                        .pointer("/spec/selector")
                        .and_then(|v| v.as_object())
                        .and_then(map_selector_string)
                        .ok_or_else(|| anyhow!("no selector found on service {path}"))?;
                    base.all_containers = true;
                    base.view_path = Some(path.clone());
                    Ok(Msg::Logs(base))
                }
                _ => {
                    base.path = workload_selector(&o)?;
                    base.view_path = Some(path.clone());
                    let spec = pod_spec(&o).cloned().unwrap_or(Value::Null);
                    Ok(Msg::Logs(pod_log_options(&o, &spec, base)))
                }
            }
        });
        Vec::new()
    }

    fn set_image(&mut self, sel: Option<String>, ctx: &Ctx) -> Vec<ViewCmd> {
        let Some(path) = sel else {
            return Vec::new();
        };
        let (acc, gvr, id) = (ctx.accessor.clone(), self.gvr.clone(), self.id);
        self.spawn(ctx, async move {
            let o = acc.get(&gvr, &path).await?;
            let spec = pod_spec(&o)
                .cloned()
                .ok_or_else(|| anyhow!("no pod spec found for {gvr} {path}"))?;
            let mut current: Vec<(String, String, bool)> = Vec::new();
            for (key, init) in [("initContainers", true), ("containers", false)] {
                for c in spec
                    .get(key)
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default()
                {
                    let name = c
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let image = c
                        .get("image")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    current.push((name, image, init));
                }
            }
            Ok(Msg::Cmds(vec![ViewCmd::Dialog(Dialog::SetImage {
                msg: format!("Set image {gvr} {path}"),
                containers: current,
                on_ok: Box::new(move |ctx, specs| {
                    let acc = ctx.accessor.clone();
                    spawn_msg(ctx, id, async move {
                        acc.set_images(&gvr, &path, specs).await?;
                        Ok(Msg::Cmds(vec![info(format!(
                            "Resource {gvr}:{path} image updated successfully"
                        ))]))
                    });
                    Vec::new()
                }),
            })]))
        });
        Vec::new()
    }

    fn show_pf(&mut self, sel: Option<String>, ctx: &mut Ctx) -> Vec<ViewCmd> {
        let Some(path) = sel else {
            return Vec::new();
        };
        let forwards: Vec<(String, String)> = ctx
            .port_forwards
            .values()
            .map(|h| {
                (
                    fqn(&h.info.spec.namespace, &h.info.spec.pod),
                    h.info.spec.container.clone(),
                )
            })
            .collect();
        if self.kind == Kind::Container {
            if !forwards.iter().any(|(p, c)| *p == self.path && *c == path) {
                return vec![err("no port-forward defined")];
            }
            let parent = self.path.clone();
            return self.push_child(
                ctx,
                Gvr::parse("portforwards"),
                Opts {
                    path: format!("{parent}|{path}"),
                    ..Default::default()
                },
            );
        }
        let (acc, kind, gvr, parent) = (
            ctx.accessor.clone(),
            self.kind,
            self.gvr.clone(),
            self.path.clone(),
        );
        self.spawn(ctx, async move {
            let pod = resolve_pod(acc, kind, gvr, path, parent).await?;
            if !forwards.iter().any(|(p, _)| *p == pod) {
                return Ok(Msg::Cmds(vec![err("no port-forward defined")]));
            }
            Ok(Msg::ShowPortForwards(pod))
        });
        Vec::new()
    }

    fn port_forward(&mut self, sel: Option<String>, ctx: &Ctx) -> Vec<ViewCmd> {
        let Some(path) = sel else {
            return Vec::new();
        };
        let address = ctx
            .config
            .read()
            .map(|c| c.k9s.port_forward_address.clone())
            .unwrap_or_default();
        let id = self.id;
        if self.kind == Kind::Container {
            let exists = ctx.port_forwards.values().any(|h| {
                fqn(&h.info.spec.namespace, &h.info.spec.pod) == self.path
                    && h.info.spec.container == path
            });
            if exists {
                return vec![err(format!(
                    "A port-forward already exists on container {}",
                    self.path
                ))];
            }
            let (acc, pod_path) = (ctx.accessor.clone(), self.path.clone());
            self.spawn(ctx, async move {
                let pod = acc.get(&pods_gvr(), &pod_path).await?;
                let has = pod
                    .data
                    .pointer("/spec/containers")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .any(|c| c.get("name").and_then(|n| n.as_str()) == Some(path.as_str()))
                    })
                    .unwrap_or(false);
                if !has {
                    bail!("unable to locate container named {path:?}");
                }
                let st = pod
                    .data
                    .pointer("/status/containerStatuses")
                    .and_then(|v| v.as_array())
                    .and_then(|a| {
                        a.iter()
                            .find(|c| c.get("name").and_then(|n| n.as_str()) == Some(path.as_str()))
                            .cloned()
                    })
                    .ok_or_else(|| anyhow!("unable to locate container status for {path:?}"))?;
                if st.pointer("/state/running").is_none() {
                    bail!("Container {path} is not running?");
                }
                let (ports, spec, prefill) = pf_choices(&pod, Some(&path), &address);
                Ok(Msg::Cmds(vec![pf_dialog(id, spec, ports, prefill)]))
            });
            return Vec::new();
        }
        let (acc, kind, gvr, parent) = (
            ctx.accessor.clone(),
            self.kind,
            self.gvr.clone(),
            self.path.clone(),
        );
        self.spawn(ctx, async move {
            let pod_path = resolve_pod(acc.clone(), kind, gvr, path, parent).await?;
            let pod = acc.get(&pods_gvr(), &pod_path).await?;
            let phase = pod
                .data
                .pointer("/status/phase")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if phase != "Running" {
                bail!("pod must be running. Current status={phase}");
            }
            if let Some(specs) = auto_forwards(&pod, &address) {
                let mut hh = Vec::new();
                for s in specs {
                    // PFAnns.ToTunnels with IsPortFree.
                    if !crate::ui::shell::dialog::port_free(&s.address, s.local_port) {
                        bail!("port {} is not available on host", s.local_port);
                    }
                    hh.push(acc.port_forward(s).await?);
                }
                return Ok(Msg::PfStarted(hh));
            }
            let (ports, spec, prefill) = pf_choices(&pod, None, &address);
            Ok(Msg::Cmds(vec![pf_dialog(id, spec, ports, prefill)]))
        });
        Vec::new()
    }

    fn scale(&mut self, ctx: &Ctx) -> Vec<ViewCmd> {
        let paths = self.table.selected_items();
        if paths.is_empty() {
            return Vec::new();
        }
        let (gvr, id) = (self.gvr.clone(), self.id);
        let msg = if paths.len() > 1 {
            format!("Scale [{}] {}?", paths.len(), gvr.r())
        } else {
            format!("Scale {} {}?", singularize(gvr.r()), paths[0])
        };
        let dialog = move |current: i32, paths: Vec<String>, gvr: Gvr| -> ViewCmd {
            ViewCmd::Dialog(Dialog::Scale {
                msg,
                current,
                on_ok: Box::new(move |ctx, n| {
                    let acc = ctx.accessor.clone();
                    spawn_msg(ctx, id, async move {
                        for p in &paths {
                            acc.scale(&gvr, p, n).await?;
                        }
                        let m = if paths.len() != 1 {
                            format!(
                                "[{}] {} scaled successfully",
                                paths.len(),
                                singularize(gvr.r())
                            )
                        } else {
                            format!("{} {} scaled successfully", gvr.r(), paths[0])
                        };
                        Ok(Msg::Cmds(vec![info(m)]))
                    });
                    Vec::new()
                }),
            })
        };
        if paths.len() > 1 {
            return vec![dialog(0, paths, gvr)];
        }
        let ready = self.table.selected_cell("READY");
        let acc = ctx.accessor.clone();
        self.spawn(ctx, async move {
            let mut current = acc.replicas(&gvr, &paths[0]).await.unwrap_or(0);
            if current == 0 {
                let r = ready.ok_or_else(|| anyhow!("no column index for READY"))?;
                let tt: Vec<&str> = r.split('/').collect();
                if tt.len() < 2 {
                    bail!("unable to locate replicas from {r}");
                }
                current = markup::strip_tags(tt[1])
                    .trim_end_matches(markup::DELTA_SIGN)
                    .trim_end_matches(['↑', '↓'])
                    .trim()
                    .parse()
                    .unwrap_or(0);
            }
            Ok(Msg::Cmds(vec![dialog(current, paths, gvr)]))
        });
        Vec::new()
    }

    fn restart(&mut self) -> Vec<ViewCmd> {
        let paths = self.table.selected_items();
        if paths.is_empty() || paths[0].is_empty() {
            return Vec::new();
        }
        let (gvr, id) = (self.gvr.clone(), self.id);
        let msg = if paths.len() > 1 {
            format!("Restart {} {}?", paths.len(), gvr.r())
        } else {
            format!("Restart {} {}?", singularize(gvr.r()), paths[0])
        };
        vec![ViewCmd::Dialog(Dialog::Restart {
            title: "Confirm Restart".into(),
            msg,
            field_manager: "kubectl-rollout".into(),
            on_ok: Box::new(move |ctx, fm| {
                let acc = ctx.accessor.clone();
                let tx = ctx.sender();
                ctx.spawn(async move {
                    for p in paths {
                        match acc.restart(&gvr, &p, &fm).await {
                            Ok(()) => post(
                                &tx,
                                id,
                                Msg::Cmds(vec![info(format!("Restart in progress for `{p}..."))]),
                            ),
                            Err(e) => post_err(&tx, id, e),
                        }
                    }
                });
                Vec::new()
            }),
        })]
    }

    fn kill(&mut self, ctx: &Ctx) -> Vec<ViewCmd> {
        let sels = self.table.selected_items();
        if sels.is_empty() {
            return Vec::new();
        }
        let msg = if sels.len() > 1 {
            format!("Delete {} marked {}", sels.len(), self.gvr)
        } else {
            format!("Delete resource {} {}", self.gvr, sels[0])
        };
        self.table.deleted.extend(sels.iter().cloned());
        let (acc, gvr, id, tx) = (
            ctx.accessor.clone(),
            self.gvr.clone(),
            self.id,
            ctx.sender(),
        );
        ctx.spawn(async move {
            let mut done = Vec::new();
            for p in sels {
                let opts = DeleteOptions {
                    grace: Some(1),
                    ..Default::default()
                };
                match acc.delete(&gvr, &p, opts).await {
                    Ok(()) => done.push(p),
                    Err(e) => post_err(&tx, id, format!("Delete failed with {e}")),
                }
            }
            post(&tx, id, Msg::Deleted(done));
        });
        vec![info(msg)]
    }

    fn shell(&mut self, sel: Option<String>, attach: bool, ctx: &Ctx) -> Vec<ViewCmd> {
        let Some(sel) = sel else {
            return Vec::new();
        };
        if self.kind == Kind::Container {
            if attach {
                return vec![exec_cmd(ctx, &self.path, &sel, true, "linux")];
            }
            let (acc, path) = (ctx.accessor.clone(), self.path.clone());
            self.spawn(ctx, async move {
                let pod = acc.get(&pods_gvr(), &path).await?;
                let os = pod_os(&acc, &pod).await;
                Ok(Msg::PickShell {
                    path,
                    containers: vec![sel],
                    attach: false,
                    os,
                })
            });
            return Vec::new();
        }
        let acc = ctx.accessor.clone();
        self.spawn(ctx, async move {
            let pod = acc.get(&pods_gvr(), &sel).await?;
            if !pod_is_running(&pod) {
                let m = if attach {
                    "is not in a happy state"
                } else {
                    "is not in a running state"
                };
                return Ok(Msg::Cmds(vec![err(format!("{sel} {m}"))]));
            }
            let spec = pod_spec(&pod).cloned().unwrap_or(Value::Null);
            let containers = match (attach, default_container(&pod, &spec)) {
                (false, Some(d)) => vec![d],
                _ => fetch_containers(&pod, &spec, false),
            };
            let os = if attach {
                "linux".to_string()
            } else {
                pod_os(&acc, &pod).await
            };
            Ok(Msg::PickShell {
                path: sel,
                containers,
                attach,
                os,
            })
        });
        Vec::new()
    }

    fn transfer(&mut self, sel: Option<String>, ctx: &Ctx) -> Vec<ViewCmd> {
        let Some(path) = sel else {
            return Vec::new();
        };
        let (ns, n) = namespaced(&path);
        let pod = format!("{ns}/{n}:");
        let (id, run) = (self.id, self.deps.run.clone());
        let acc = ctx.accessor.clone();
        self.spawn(ctx, async move {
            let po = acc.get(&pods_gvr(), &path).await?;
            let spec = po.data.get("spec").cloned().unwrap_or(Value::Null);
            let containers = fetch_containers(&po, &spec, false);
            let on_ok = move |ctx: &mut Ctx, args: crate::ui::TransferArgs| {
                let local = if args.download { &args.to } else { &args.from };
                if !args.download && !Path::new(local).exists() {
                    return Err(format!("stat {local}: no such file or directory"));
                }
                // pod.go transferCmd (including its repeated --retries).
                let mut argv = vec![
                    "kubectl".to_string(),
                    "cp".into(),
                    args.from.trim().to_string(),
                    args.to.trim().to_string(),
                    format!("--no-preserve={}", args.no_preserve),
                    format!("--retries={}", args.retries),
                ];
                if !args.container.is_empty() {
                    argv.push(format!("-c={}", args.container));
                }
                argv.push(format!("--retries={}", args.retries));
                let env = ctx.accessor.shell_env();
                let op = if args.download { "Download" } else { "Upload" };
                let fq = format!("{path}:{}", args.container);
                let run = run.clone();
                spawn_msg(ctx, id, async move {
                    // runK failures open the cow (App.cowCmd).
                    Ok(match run(argv, env).await {
                        Ok(_) => Msg::Cmds(vec![info(format!("{op} successful on {fq}!"))]),
                        Err(e) => Msg::Text {
                            name: "cow".into(),
                            title: String::new(),
                            body: format!("{e:#}"),
                        },
                    })
                });
                Ok(Vec::new())
            };
            Ok(Msg::Cmds(vec![ViewCmd::Dialog(Dialog::Transfer {
                pod,
                containers,
                retries: DEFAULT_TX_RETRIES,
                on_ok: Box::new(on_ok),
            })]))
        });
        Vec::new()
    }

    fn sanitize(&mut self) -> Vec<ViewCmd> {
        let (gvr, id, ns) = (self.gvr.clone(), self.id, self.ns.clone());
        vec![ViewCmd::Dialog(Dialog::ConfirmAck {
            title: "Sanitize".into(),
            msg: format!(
                "Sanitize deletes all pods in completed/error state\nPlease enter [orange::b]{MAGIC_PROMPT}[-::-] to proceed."
            ),
            accept: MAGIC_PROMPT.into(),
            on_ok: Box::new(move |ctx| {
                let acc = ctx.accessor.clone();
                spawn_msg(ctx, id, async move {
                    let mut ns = cleanse_namespace(&ns);
                    if is_cluster_scoped(&ns) {
                        ns.clear();
                    }
                    let mut count = 0;
                    for p in acc.list(&gvr, &ns, None).await? {
                        if TOAST_PHASES.contains(&pod_status(&p).as_str()) {
                            let opts = DeleteOptions {
                                grace: Some(0),
                                ..Default::default()
                            };
                            acc.delete(&gvr, &obj_fqn(&p), opts).await?;
                            count += 1;
                        }
                    }
                    Ok(Msg::Cmds(vec![info(format!("Sanitized {count} {gvr}"))]))
                });
                let tx = ctx.sender();
                post(&tx, id, Msg::Refresh);
                Vec::new()
            }),
        })]
    }

    fn trigger(&mut self) -> Vec<ViewCmd> {
        let fqns = self.table.selected_items();
        if fqns.is_empty() {
            return Vec::new();
        }
        let msg = if fqns.len() > 1 {
            format!("Trigger {} CronJobs?", fqns.len())
        } else {
            format!("Trigger CronJob: {}?", fqns[0])
        };
        let (gvr, id) = (self.gvr.clone(), self.id);
        vec![ViewCmd::Dialog(Dialog::Confirm {
            title: "Confirm Job Trigger".into(),
            msg,
            on_ok: Box::new(move |ctx| {
                let (acc, tx) = (ctx.accessor.clone(), ctx.sender());
                ctx.spawn(async move {
                    for f in fqns {
                        let m = match acc.trigger_cronjob(&f).await {
                            Ok(()) => info(format!("Triggered Job {gvr} {f}")),
                            Err(e) => err(format!("CronJob trigger failed for {f}: {e}")),
                        };
                        post(&tx, id, Msg::Cmds(vec![m]));
                    }
                });
                Vec::new()
            }),
        })]
    }

    fn suspend_resume(&mut self, ctx: &Ctx) -> Vec<ViewCmd> {
        let Some(sel) = self.selected() else {
            return Vec::new();
        };
        let name_col = if is_cluster_scoped(&self.ns) || !is_cluster_wide_ns(&self.ns) {
            0
        } else {
            1
        };
        let cols = self
            .table
            .visible_cols(&self.table.data.header, &self.ns, ctx.has_metrics);
        let cell = cols
            .get(name_col + 2)
            .and_then(|&c| self.table.selected().and_then(|r| r.row.fields.get(c)))
            .map(|s| markup::strip_tags(s).trim().to_string());
        let Some(cell) = cell else {
            return vec![err("Unable to assert current status")];
        };
        let title = if cell == "true" { "Resume" } else { "Suspend" };
        let id = self.id;
        vec![ViewCmd::Dialog(Dialog::Confirm {
            title: title.into(),
            msg: sel.clone(),
            on_ok: Box::new(move |ctx| {
                let acc = ctx.accessor.clone();
                spawn_msg(ctx, id, async move {
                    match acc.toggle_suspend_cronjob(&sel).await {
                        Ok(()) => Ok(Msg::Refresh),
                        Err(e) => Ok(Msg::Cmds(vec![err(format!(
                            "Cronjob {} failed for {e}",
                            title.to_lowercase()
                        ))])),
                    }
                });
                Vec::new()
            }),
        })]
    }

    fn cordon(&mut self, on: bool) -> Vec<ViewCmd> {
        let sels = self.table.selected_items();
        if sels.is_empty() {
            return Vec::new();
        }
        let (mut title, mut msg) = ("Confirm ".to_string(), String::new());
        if on {
            title.push_str("Cordon");
            msg.push_str("Cordon ");
        } else {
            title.push_str("Uncordon");
            msg.push_str("Uncordon ");
        }
        if sels.len() == 1 {
            msg.push_str(&format!("{}?", sels[0]));
        } else {
            msg.push_str(&format!("({}) marked {}?", sels.len(), self.gvr.r()));
        }
        let id = self.id;
        vec![ViewCmd::Dialog(Dialog::Confirm {
            title,
            msg,
            on_ok: Box::new(move |ctx| {
                let (acc, tx) = (ctx.accessor.clone(), ctx.sender());
                spawn_msg(ctx, id, async move {
                    // node.go toggleCordonCmd: flash each failure, keep going.
                    for s in &sels {
                        if let Err(e) = acc.toggle_cordon(s, on).await {
                            post_err(&tx, id, e);
                        }
                    }
                    Ok(Msg::Refresh)
                });
                Vec::new()
            }),
        })]
    }

    fn drain(&mut self) -> Vec<ViewCmd> {
        let sels = self.table.selected_items();
        if sels.is_empty() {
            return Vec::new();
        }
        let (id, text_view) = (self.id, self.deps.text);
        // drain_dialog.go: "Drain <node>?" or "Drain (N) nodes?".
        let node = if sels.len() == 1 {
            sels[0].clone()
        } else {
            format!("({}) nodes", sels.len())
        };
        vec![ViewCmd::Dialog(Dialog::Drain {
            node,
            on_ok: Box::new(move |ctx, opts| {
                // node.go drainNode: open "Drain Progress" first and stream into it.
                let v = text_view(ctx, "Drain Progress".into(), "nodes".into(), String::new());
                let (vid, tx, acc) = (v.id(), ctx.sender(), ctx.accessor.clone());
                let log = Arc::new(std::sync::Mutex::new(String::new()));
                let sink_tx = tx.clone();
                let progress: crate::k8s::DrainProgress = Arc::new(move |s: &str| {
                    let mut l = log.lock().unwrap();
                    l.push_str(s);
                    let _ = sink_tx.send(AppEvent::Text {
                        view: vid,
                        text: l.clone(),
                    });
                });
                ctx.spawn(async move {
                    for s in &sels {
                        if let Err(e) = acc.drain(s, opts.clone(), progress.clone()).await {
                            post_err(&tx, id, e);
                        }
                    }
                    post(&tx, id, Msg::Refresh);
                });
                vec![ViewCmd::Push(v)]
            }),
        })]
    }

    fn node_shell(&mut self, sel: Option<String>, ctx: &Ctx) -> Vec<ViewCmd> {
        let Some(path) = sel else {
            return Vec::new();
        };
        let node = namespaced(&path).1.to_string();
        let cfg = ctx
            .config
            .read()
            .map(|c| c.k9s.shell_pod.clone())
            .unwrap_or_default();
        let name = format!("{K9S_SHELL}-{}", std::process::id());
        let pod_path = fqn(&cfg.namespace, &name);
        let manifest = shell_pod_manifest(&node, &name, &cfg);
        let (acc, id, tx) = (ctx.accessor.clone(), self.id, ctx.sender());
        let cancel = tokio_util::sync::CancellationToken::new();
        self.spawn(ctx, async move {
            // exec.go launchNodeShell: nuke a leftover shell pod first.
            if let Err(e) = nuke_shell(&acc, &pod_path).await {
                bail!("Cleaning node shell failed: {e}");
            }
            let nuke_path = pod_path.clone();
            let launch = Dialog::Launching {
                title: "Launching".into(),
                msg: format!("Launching node shell on {node}..."),
                cancel: cancel.clone(),
                on_cancel: Box::new(move |ctx| {
                    let acc = ctx.accessor.clone();
                    spawn_msg(ctx, id, async move {
                        nuke_shell(&acc, &nuke_path)
                            .await
                            .map_err(|e| anyhow!("Cleaning node shell failed: {e}"))?;
                        Ok(Msg::Cmds(Vec::new()))
                    });
                    Vec::new()
                }),
            };
            post(&tx, id, Msg::Cmds(vec![ViewCmd::Dialog(launch)]));
            let launched = tokio::select! {
                // context.Canceled is not reported.
                _ = cancel.cancelled() => return Ok(Msg::Cmds(Vec::new())),
                r = launch_shell_pod(&acc, &pod_path, &manifest, &node) => r,
            };
            if let Err(e) = launched {
                return Ok(Msg::Cmds(vec![
                    ViewCmd::CloseDialog,
                    err(format!("Launching node shell failed: {e}")),
                ]));
            }
            let (ns, po) = namespaced(&pod_path);
            let mut argv = acc.exec_argv(ns, po, K9S_SHELL, "sh");
            if argv.len() >= 2 && argv[argv.len() - 1] == "sh" && argv[argv.len() - 2] == "--" {
                argv.pop();
                if cfg.command.is_empty() {
                    argv.extend(["sh".to_string(), "-c".into(), SHELL_CHECK.into()]);
                } else {
                    argv.extend(cfg.command.iter().cloned());
                    argv.extend(cfg.args.iter().cloned());
                }
            }
            let env = acc.shell_env();
            Ok(Msg::Cmds(vec![
                ViewCmd::CloseDialog,
                ViewCmd::Suspend {
                    argv,
                    banner: shell_banner(&pod_path, K9S_SHELL),
                    env,
                    on_done: Some(Box::new(move |ctx, _st| {
                        let acc = ctx.accessor.clone();
                        ctx.spawn(async move {
                            let _ = nuke_shell(&acc, &pod_path).await;
                        });
                        Vec::new()
                    })),
                },
            ]))
        });
        Vec::new()
    }

    fn bench(&mut self, sel: Option<String>, ctx: &Ctx) -> Vec<ViewCmd> {
        if let Some(h) = self.bench.take() {
            h.abort();
            // toggleBenchCmd: status then ClearStatus(true) right away.
            return vec![
                ViewCmd::Status(FlashLevel::Err, "Benchmark Canceled!".into()),
                ViewCmd::ClearStatus(true),
            ];
        }
        let Some(path) = sel else {
            return Vec::new();
        };
        let file = (self.deps.bench_config)(ctx);
        let (cluster, context) = (
            ctx.cluster_info.cluster.clone(),
            ctx.cluster_info.context.clone(),
        );
        let (bench, id, tx) = (self.deps.bench, self.id, ctx.sender());
        let to_spec = |cfg: &BenchCfg, url: String| BenchSpec {
            url,
            method: cfg.method.clone(),
            concurrency: cfg.concurrency,
            requests: cfg.requests,
            headers: cfg.headers.clone(),
            body: cfg.body.clone(),
            user: cfg.user.clone(),
            http2: cfg.http2,
        };
        if self.kind == Kind::PortForward {
            let cfg = bench_config_for(&file, &path, false);
            let url = self
                .table
                .selected_cell("URL")
                .map(|u| markup::strip_tags(&u))
                .unwrap_or_default();
            let url = if url.is_empty() {
                let local = ctx
                    .port_forwards
                    .get(&path)
                    .map(|h| h.info.spec.local_port)
                    .unwrap_or(0);
                let host = if cfg.host.is_empty() {
                    "localhost".to_string()
                } else {
                    cfg.host.clone()
                };
                format!("http://{host}:{local}{}", cfg.path)
            } else {
                url
            };
            let spec = to_spec(&cfg, url);
            self.bench = Some(ctx.spawn(async move {
                let r = bench(spec, cluster, context, path)
                    .await
                    .map_err(|e| e.to_string());
                post(&tx, id, Msg::BenchDone(r));
            }));
            return vec![ViewCmd::Status(
                FlashLevel::Warn,
                "Benchmark in progress...".into(),
            )];
        }
        let Some(cfg) = lookup_bench(&file, &path, true) else {
            return vec![err(format!(
                "No bench config found for service {path} in {}",
                file.display()
            ))];
        };
        let acc = ctx.accessor.clone();
        let gvr = self.gvr.clone();
        self.bench = Some(ctx.spawn(async move {
            let run = async {
                let svc = acc.get(&gvr, &path).await?;
                let typ = svc
                    .data
                    .pointer("/spec/type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if typ != "NodePort" && typ != "LoadBalancer" {
                    bail!("you must select a reachable service");
                }
                let port = if typ == "LoadBalancer" {
                    String::new()
                } else {
                    svc.data
                        .pointer("/spec/ports/0/nodePort")
                        .and_then(|v| v.as_i64())
                        .map(|p| p.to_string())
                        .ok_or_else(|| anyhow!("no ports pair found"))?
                };
                if cfg.host.is_empty() {
                    bail!("invalid benchmark host {:?}", cfg.host);
                }
                let mut base = cfg.host.clone();
                if base.contains(':') {
                    base.push_str(&cfg.path);
                } else {
                    base = format!("{base}:{port}{}", cfg.path);
                }
                if !base.starts_with("http") {
                    base = format!("http://{base}");
                }
                bench(to_spec(&cfg, base), cluster, context, path).await
            };
            let r = run.await.map_err(|e| e.to_string());
            post(&tx, id, Msg::BenchDone(r));
        }));
        vec![ViewCmd::Status(
            FlashLevel::Warn,
            "Benchmark in progress...".into(),
        )]
    }

    fn pf_delete(&mut self) -> Vec<ViewCmd> {
        if !self.filter.is_empty() {
            self.set_filter_text("");
            return Vec::new();
        }
        let sels = self.table.selected_items();
        if sels.is_empty() {
            return Vec::new();
        }
        let msg = if sels.len() > 1 {
            format!("Delete {} marked {}?", sels.len(), self.gvr)
        } else {
            match pf_to_human(&sels[0]) {
                Some(h) => format!("Delete {} {h}?", self.gvr.r()),
                None => return vec![err(format!("unable to parse selection {}", sels[0]))],
            }
        };
        let id = self.id;
        vec![ViewCmd::Dialog(Dialog::Confirm {
            title: "Delete".into(),
            msg,
            on_ok: Box::new(move |ctx| {
                for s in &sels {
                    ctx.port_forwards.remove(s);
                }
                let tx = ctx.sender();
                post(&tx, id, Msg::Refresh);
                vec![info(format!(
                    "Successfully deleted {} PortForward!",
                    sels.len()
                ))]
            }),
        })]
    }

    fn dir_kubectl(&mut self, sel: Option<String>, apply: bool, ctx: &mut Ctx) -> Vec<ViewCmd> {
        let Some(sel) = sel else {
            return Vec::new();
        };
        let mut opts = vec!["-f".to_string()];
        let mut what = "manifest";
        if contains_dir(&sel) {
            opts.push("-R".into());
        }
        if is_kustomized(&sel) {
            opts = vec!["-k".into()];
            what = "kustomization";
        }
        let mut argv = vec![
            "kubectl".to_string(),
            if apply { "apply" } else { "delete" }.into(),
        ];
        argv.extend(opts);
        argv.push(sel.clone());
        let (run, env, id) = (self.deps.run.clone(), ctx.accessor.shell_env(), self.id);
        let confirm = format!("Delete resource(s) in {what} {sel}");
        let title = if apply {
            "Applied Manifest"
        } else {
            "Deleted Manifest"
        };
        let go = move |ctx: &mut Ctx| {
            spawn_msg(ctx, id, async move {
                let body = match run(argv, env).await {
                    Ok(out) => format!("message:\n{}", fmt_results(&out)),
                    // dir.go: status is the exit error, message keeps the output.
                    Err(e) => match e.downcast_ref::<RunFailed>() {
                        Some(f) => format!(
                            "status:\n  {}\nmessage:\n{}",
                            f.status,
                            fmt_results(&f.output)
                        ),
                        None => format!("status:\n  {e}\nmessage:\n{}", fmt_results("")),
                    },
                };
                Ok(Msg::Text {
                    name: title.into(),
                    title: sel,
                    body,
                })
            });
            Vec::new()
        };
        if apply {
            return go(ctx);
        }
        vec![ViewCmd::Dialog(Dialog::Confirm {
            title: "Confirm Delete".into(),
            msg: confirm,
            on_ok: Box::new(go),
        })]
    }

    pub(super) fn helm_values(&mut self, sel: Option<String>, ctx: &Ctx) -> Vec<ViewCmd> {
        let Some(id) = sel else {
            return Vec::new();
        };
        // helm_history.go getValsCmd: the id must be `name:revision`.
        if self.kind == Kind::HelmHistory && !namespaced(&id).1.contains(':') {
            return vec![err(format!("unable to parse version in {id:?}"))];
        }
        let (rel, rev) = self.helm_target(&id);
        let (ns, name) = namespaced(&rel);
        let (ns, name) = (ns.to_string(), name.to_string());
        // The release (list) or release:revision (history) is the subject.
        let subject = if self.kind == Kind::Helm {
            rel.clone()
        } else {
            id.clone()
        };
        let get = |all: bool| {
            let mut argv = vec![
                "helm".to_string(),
                "get".into(),
                "values".into(),
                name.clone(),
                "-o".into(),
                "json".into(),
            ];
            if !ns.is_empty() {
                argv.extend(["-n".into(), ns.clone()]);
            }
            if let Some(r) = &rev {
                argv.extend(["--revision".into(), r.clone()]);
            }
            if all {
                argv.push("--all".into());
            }
            argv.extend(self.kube_ctx_args(ctx));
            argv
        };
        let (user_argv, all_argv) = (get(false), get(true));
        let (run, env) = (self.deps.run.clone(), ctx.accessor.shell_env());
        let acc = ctx.accessor.clone();
        self.spawn(ctx, async move {
            let fail = |e: anyhow::Error| anyhow!("Initializing the values model failed: {e}");
            let user = values_yaml(&run(user_argv, env.clone()).await.map_err(fail)?)?;
            let all = match rev {
                // RevValues toggles to the chart's own values for that revision.
                Some(r) => {
                    let path = fqn(&ns, &format!("sh.helm.release.v1.{name}.v{r}"));
                    let sec = acc.get(&Gvr::parse("v1/secrets"), &path).await?;
                    crate::k8s::goyaml::to_yaml(&null_map(helm_chart_values(&sec)?))
                }
                None => values_yaml(&run(all_argv, env).await.map_err(fail)?)?,
            };
            Ok(Msg::Values { subject, user, all })
        });
        Vec::new()
    }

    pub(super) fn helm_rollback(&mut self, sel: Option<String>, ctx: &Ctx) -> Vec<ViewCmd> {
        let Some(id) = sel else {
            return Vec::new();
        };
        let (rel, rev) = self.helm_target(&id);
        let rev = rev.unwrap_or_default();
        let (ns, n) = namespaced(&rel);
        let (ns, n) = (ns.to_string(), n.to_string());
        let mut argv = vec![
            "helm".to_string(),
            "rollback".into(),
            n.clone(),
            rev.clone(),
        ];
        if !ns.is_empty() {
            argv.extend(["-n".into(), ns]);
        }
        argv.extend(self.kube_ctx_args(ctx));
        let (run, env, vid) = (self.deps.run.clone(), ctx.accessor.shell_env(), self.id);
        vec![ViewCmd::Dialog(Dialog::Confirm {
            title: "Confirm Rollback".into(),
            msg: format!(
                "RollingBack chart [yellow::b]{n}[-::-] to release <[orangered::b]{rev}[-::-]>?"
            ),
            on_ok: Box::new(move |ctx| {
                spawn_msg(ctx, vid, async move {
                    // HelmHistory.Rollback: strconv.Atoi(rev).
                    if rev.parse::<i64>().is_err() {
                        bail!(
                            "could not convert revision to a number: strconv.Atoi: parsing {rev:?}: invalid syntax"
                        );
                    }
                    run(argv, env).await?;
                    Ok(Msg::Cmds(vec![info(format!(
                        "Rollout restart in progress for char `{n}..."
                    ))]))
                });
                let tx = ctx.sender();
                post(&tx, vid, Msg::Refresh);
                Vec::new()
            }),
        })]
    }

    fn plugin(&mut self, name: &str, sel: Option<String>, ctx: &mut Ctx) -> Vec<ViewCmd> {
        let Some(p) = ctx.plugins.plugins.get(name).cloned() else {
            return Vec::new();
        };
        if sel.is_none() {
            return Vec::new();
        }
        let env = self.env(ctx);
        let args: Vec<String> = p.args.iter().map(|a| env.substitute(a)).collect();
        // exec.go execute: pipes split on spaces, short ones dropped.
        let argv = crate::ui::shell::plugins::plugin_argv(&p, &args);
        let (run, id) = (self.deps.run.clone(), self.id);
        let shell_env = ctx.accessor.shell_env();
        let cmd_str = crate::ui::shell::plugins::cmd_string(&p.command, &args);
        let exec = move |ctx: &mut Ctx| -> Vec<ViewCmd> {
            if p.background {
                let overwrite = p.overwrite_output;
                spawn_msg(ctx, id, async move {
                    // exec.go pipe: a failed background run is only logged.
                    let out = match run(argv, shell_env).await {
                        Ok(out) => out,
                        Err(e) => {
                            tracing::error!(error = %e, "Command exec failed");
                            return Ok(Msg::Cmds(Vec::new()));
                        }
                    };
                    let msg =
                        crate::ui::shell::plugins::background_flash(&out, &cmd_str, overwrite);
                    Ok(Msg::Cmds(msg.into_iter().map(info).collect()))
                });
                return Vec::new();
            }
            vec![ViewCmd::Suspend {
                argv,
                banner: String::new(),
                env: shell_env,
                on_done: Some(Box::new(move |_ctx, st| {
                    crate::ui::shell::plugins::foreground_done(&cmd_str, st)
                })),
            }]
        };
        if p.confirm {
            return vec![ViewCmd::Dialog(Dialog::Confirm {
                title: format!("Confirm {}", p.description),
                msg: format!("Run?\n{} {}", p.command, args.join(" ")),
                on_ok: Box::new(exec),
            })];
        }
        exec(ctx)
    }
}

fn is_cluster_wide_ns(ns: &str) -> bool {
    table::is_cluster_wide(ns)
}
