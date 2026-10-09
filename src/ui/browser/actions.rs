//! Key action handlers and async follow-ups (k9s view/browser.go,
//! view/table.go, the per-resource viewers and their extenders).

use std::future::Future;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};
use kube::api::DynamicObject;
use serde_json::{json, Value};

use crate::k8s::{fqn, namespaced, DeleteOptions, ImageSpec, PortForwardSpec, SharedAccessor};

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
const K9S_SHELL: &str = "k9s-shell";
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

/// Selector of a workload object (spec.selector).
fn workload_selector(o: &DynamicObject) -> Result<String> {
    o.data
        .pointer("/spec/selector")
        .and_then(label_selector_string)
        .ok_or_else(|| anyhow!("no selector found on {}", obj_fqn(o)))
}

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
        env: ctx.accessor.shell_env(),
        on_done: Some(Box::new(move |_ctx, st| {
            if st.success() {
                Vec::new()
            } else {
                vec![err(format!("{what} exec failed: {st}"))]
            }
        })),
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

/// Port-forward ports + default spec for a pod.
fn pf_choices(
    pod: &DynamicObject,
    only: Option<&str>,
    address: &str,
) -> (Vec<String>, PortForwardSpec) {
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
    if let Some((co, _, num)) = ports.first() {
        def.container = co.clone();
        def.container_port = *num;
        def.local_port = *num;
    } else if let Some(co) = only {
        def.container = co.to_string();
    }
    let ann = pod
        .metadata
        .annotations
        .as_ref()
        .and_then(|a| a.get("k9scli.io/port-forwards").cloned())
        .unwrap_or_default();
    if let Some(pref) = parse_pf_annotation(&ann).into_iter().next() {
        if let Some((co, _, num)) = ports.iter().find(|(co, name, num)| {
            (pref.container.is_empty() || *co == pref.container)
                && (*name == pref.remote || num.to_string() == pref.remote)
        }) {
            def.container = co.clone();
            def.container_port = *num;
            def.local_port = pref.local.parse().unwrap_or(*num);
        }
    }
    let strs = ports
        .iter()
        .map(|(co, n, num)| port_spec_string(co, n, *num))
        .collect();
    (strs, def)
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

fn pf_dialog(id: ViewId, path: String, spec: PortForwardSpec, ports: Vec<String>) -> ViewCmd {
    ViewCmd::Dialog(Dialog::PortForward {
        spec,
        ports,
        on_ok: Box::new(move |ctx, spec| {
            if spec.container_port == 0 || spec.local_port == 0 {
                return vec![err("container to local port mismatch")];
            }
            if ctx.port_forwards.contains_key(&spec.id()) {
                return vec![err(format!("port-forward is already active on pod {path}"))];
            }
            let acc = ctx.accessor.clone();
            spawn_msg(ctx, id, async move {
                Ok(Msg::PfStarted(vec![acc.port_forward(spec).await?]))
            });
            Vec::new()
        }),
    })
}

/// k9s shell pod manifest (exec.go k9sShellPod).
fn shell_pod_manifest(node: &str, name: &str, cfg: &crate::config::ShellPod) -> Value {
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
    json!({
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
    })
}

/// Rollback a replicaset's deployment to its pod template (kubectl rollout undo).
async fn rollback_rs(acc: SharedAccessor, path: String) -> Result<()> {
    let rs = acc.get(&Gvr::parse("apps/v1/replicasets"), &path).await?;
    let owner = rs
        .metadata
        .owner_references
        .as_ref()
        .and_then(|oo| {
            oo.iter()
                .find(|o| o.controller == Some(true))
                .or_else(|| oo.first())
        })
        .ok_or_else(|| anyhow!("no controller found for replicaset {path}"))?;
    if owner.kind != "Deployment" {
        bail!("unsupported controller kind {:?}", owner.kind);
    }
    let ns = rs.metadata.namespace.clone().unwrap_or_default();
    let dp_gvr = Gvr::parse("apps/v1/deployments");
    let dp_path = fqn(&ns, &owner.name);
    let mut dp = acc.get(&dp_gvr, &dp_path).await?;
    let mut tpl = rs
        .data
        .pointer("/spec/template")
        .cloned()
        .ok_or_else(|| anyhow!("replicaset {path} has no pod template"))?;
    if let Some(labels) = tpl
        .pointer_mut("/metadata/labels")
        .and_then(|v| v.as_object_mut())
    {
        labels.remove("pod-template-hash");
    }
    if let Some(spec) = dp.data.get_mut("spec").and_then(|v| v.as_object_mut()) {
        spec.insert("template".into(), tpl);
    }
    let yaml = serde_yaml::to_string(&dp)?;
    acc.apply_yaml(&dp_gvr, &dp_path, &yaml).await
}

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
                    tracing::warn!(error = %e, "clipboard write failed");
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
            Act::Transfer => self.transfer(sel),
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
            Act::UseNamespace => {
                let Some(path) = sel else {
                    return Vec::new();
                };
                let ns = namespaced(&path).1.to_string();
                if print_namespace(&cleanse_namespace(&ctx.namespace)) == ns {
                    return Vec::new();
                }
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
                vec![ViewCmd::Command(self.env(ctx).substitute(&hk.command))]
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
                let p = Picker::new(
                    ctx,
                    "Jump To",
                    items,
                    Box::new(move |ctx, idx, _| jump_owner(ctx, &deps, &ns, &owners[idx])),
                );
                vec![ViewCmd::Push(Box::new(p))]
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
                for h in handles {
                    ctx.port_forwards.insert(h.info.spec.id(), h);
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
            Msg::Refresh => {
                self.dirty = true;
                self.spawn_load(ctx, true);
                Vec::new()
            }
            Msg::BenchDone(res) => {
                self.bench = None;
                match res {
                    Ok(_) => vec![info("Benchmark Completed!")],
                    Err(e) => vec![err(format!("Benchmark failed {e}"))],
                }
            }
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
        let res = (|| -> Result<PathBuf> {
            std::fs::create_dir_all(&dir)?;
            let p = dir.join(&file);
            let header: Vec<String> = td.header.iter().map(|h| h.name.clone()).collect();
            let rows: Vec<Vec<String>> = td.rows.iter().map(|r| r.row.fields.clone()).collect();
            let mut opts = std::fs::OpenOptions::new();
            opts.create(true).write(true).truncate(true);
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
        self.set_namespace(ns);
        self.table.select_first();
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
            Kind::Namespace => {
                let ns = namespaced(&path).1.to_string();
                let mut out = Vec::new();
                if print_namespace(&cleanse_namespace(&ctx.namespace)) != ns {
                    out.push(ViewCmd::SetNamespace(ns.clone()));
                }
                out.extend(self.push_child(
                    ctx,
                    pods_gvr(),
                    Opts {
                        ns: Some(ns),
                        ..Default::default()
                    },
                ));
                out
            }
            // view/context.go useContext -> app.switchContext: the shell
            // switches, flashes and jumps to the context's last view.
            Kind::Context => vec![ViewCmd::Command(format!("ctx {path}"))],
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
            Kind::Crd => {
                let crd = namespaced(&path).1.to_string();
                let (plural, group) = crd.split_once('.').unwrap_or((crd.as_str(), ""));
                let found = ctx
                    .resources
                    .iter()
                    .find(|m| m.gvr.r() == plural && m.gvr.g() == group)
                    .map(|m| m.gvr.clone());
                match found {
                    Some(g) => self.push_child(ctx, g, Opts::default()),
                    None => vec![ViewCmd::Dialog(Dialog::Error {
                        msg: format!("`{crd}` command not found"),
                    })],
                }
            }
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
                    Gvr::parse("dir"),
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
        let (_, n) = namespaced(&path);
        if n.is_empty() {
            return vec![err(format!("missing resource name in path {path:?}"))];
        }
        vec![ViewCmd::Suspend {
            argv: ctx.accessor.edit_argv(&gvr, &path),
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
            self.spawn(ctx, async move {
                let body = run(argv, env).await?;
                Ok(Msg::Text {
                    name: title.into(),
                    title: rel,
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
    fn helm_target(&self, id: &str) -> (String, Option<String>) {
        if self.kind == Kind::Helm {
            return (id.to_string(), None);
        }
        let rev = id.rsplit_once(':').map(|(_, r)| r).unwrap_or(id);
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
        if self.kind == Kind::Container {
            let (ns, po) = namespaced(&self.path);
            base.namespace = ns.to_string();
            base.path = po.to_string();
            base.container = Some(path);
            return vec![ViewCmd::Push((self.deps.logs)(ctx, base))];
        }
        let (acc, gvr, kind) = (ctx.accessor.clone(), self.gvr.clone(), self.kind);
        self.spawn(ctx, async move {
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
                    Ok(Msg::Logs(base))
                }
                _ => {
                    base.path = workload_selector(&o)?;
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
            let value = current
                .iter()
                .map(|(n, i, _)| format!("{n}={i}"))
                .collect::<Vec<_>>()
                .join(",");
            Ok(Msg::Cmds(vec![ViewCmd::Dialog(Dialog::Prompt {
                title: "<Set image>".into(),
                label: format!("Set image {gvr} {path}"),
                value,
                on_ok: Box::new(move |ctx, text| {
                    let mut specs = Vec::new();
                    for pair in text.split([',', ' ']).filter(|p| !p.is_empty()) {
                        let Some((n, img)) = pair.split_once('=') else {
                            continue;
                        };
                        let img = img.trim();
                        if let Some((_, old, init)) =
                            current.iter().find(|(cn, _, _)| cn == n.trim())
                        {
                            if !img.is_empty() && img != old {
                                specs.push(ImageSpec {
                                    name: n.trim().to_string(),
                                    docker_image: img.to_string(),
                                    init: *init,
                                });
                            }
                        }
                    }
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
                let (ports, spec) = pf_choices(&pod, Some(&path), &address);
                Ok(Msg::Cmds(vec![pf_dialog(
                    id,
                    format!("{pod_path}|{path}"),
                    spec,
                    ports,
                )]))
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
                    hh.push(acc.port_forward(s).await?);
                }
                return Ok(Msg::PfStarted(hh));
            }
            let (ports, spec) = pf_choices(&pod, None, &address);
            Ok(Msg::Cmds(vec![pf_dialog(id, pod_path, spec, ports)]))
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
        vec![ViewCmd::Dialog(Dialog::Confirm {
            title: "Confirm Restart".into(),
            msg,
            on_ok: Box::new(move |ctx| {
                let acc = ctx.accessor.clone();
                let tx = ctx.sender();
                ctx.spawn(async move {
                    for p in paths {
                        match acc.restart(&gvr, &p).await {
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

    fn transfer(&mut self, sel: Option<String>) -> Vec<ViewCmd> {
        let Some(path) = sel else {
            return Vec::new();
        };
        let (ns, n) = namespaced(&path);
        let pod = format!("{ns}/{n}:");
        let (id, run) = (self.id, self.deps.run.clone());
        vec![ViewCmd::Dialog(Dialog::Prompt {
            title: "Transfer".into(),
            label: "Download Files (from to):".into(),
            value: format!("{pod} ."),
            on_ok: Box::new(move |ctx, text| {
                let tt: Vec<&str> = text.split_whitespace().collect();
                if tt.len() != 2 {
                    return vec![err("transfer expects a source and a destination")];
                }
                let (from, to) = (tt[0].to_string(), tt[1].to_string());
                let download = from.starts_with(&pod);
                if !download && !Path::new(&from).exists() {
                    return vec![err(format!("stat {from}: no such file or directory"))];
                }
                let argv = vec![
                    "kubectl".to_string(),
                    "cp".into(),
                    from,
                    to,
                    "--no-preserve=false".into(),
                    "--retries=999".into(),
                ];
                let env = ctx.accessor.shell_env();
                let op = if download { "Download" } else { "Upload" };
                let fq = format!("{path}:");
                spawn_msg(ctx, id, async move {
                    run(argv, env).await?;
                    Ok(Msg::Cmds(vec![info(format!("{op} successful on {fq}!"))]))
                });
                Vec::new()
            }),
        })]
    }

    fn sanitize(&mut self) -> Vec<ViewCmd> {
        let (gvr, id, ns) = (self.gvr.clone(), self.id, self.ns.clone());
        vec![ViewCmd::Dialog(Dialog::Prompt {
            title: "Sanitize".into(),
            label: format!(
                "Sanitize deletes all pods in completed/error state\nPlease enter [orange::b]{MAGIC_PROMPT}[-::-] to proceed."
            ),
            value: String::new(),
            on_ok: Box::new(move |ctx, text| {
                if text.trim() != MAGIC_PROMPT {
                    return Vec::new();
                }
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
                let acc = ctx.accessor.clone();
                spawn_msg(ctx, id, async move {
                    for s in &sels {
                        acc.toggle_cordon(s, on).await?;
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
        let id = self.id;
        vec![ViewCmd::Dialog(Dialog::Drain {
            node: sels.join(","),
            on_ok: Box::new(move |ctx, opts| {
                let acc = ctx.accessor.clone();
                let tx = ctx.sender();
                ctx.spawn(async move {
                    let mut out = String::new();
                    for s in &sels {
                        match acc.drain(s, opts.clone()).await {
                            Ok(text) => out.push_str(&text),
                            Err(e) => post_err(&tx, id, e),
                        }
                    }
                    post(
                        &tx,
                        id,
                        Msg::Text {
                            name: "Drain Progress".into(),
                            title: "nodes".into(),
                            body: out,
                        },
                    );
                    post(&tx, id, Msg::Refresh);
                });
                Vec::new()
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
        let acc = ctx.accessor.clone();
        let msg = format!("Launching node shell on {node}...");
        self.spawn(ctx, async move {
            let pods = pods_gvr();
            let _ = acc.delete(&pods, &pod_path, DeleteOptions::default()).await;
            acc.apply_yaml(&pods, &pod_path, &serde_yaml::to_string(&manifest)?)
                .await?;
            let mut running = false;
            for _ in 0..50 {
                if let Ok(p) = acc.get(&pods, &pod_path).await {
                    if p.data.pointer("/status/phase").and_then(|v| v.as_str()) == Some("Running") {
                        running = true;
                        break;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            if !running {
                bail!("Launching node shell failed: unable to launch shell pod on node {node}");
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
            Ok(Msg::Cmds(vec![ViewCmd::Suspend {
                argv,
                env,
                on_done: Some(Box::new(move |ctx, _st| {
                    let acc = ctx.accessor.clone();
                    ctx.spawn(async move {
                        let _ = acc
                            .delete(&Gvr::parse("v1/pods"), &pod_path, DeleteOptions::default())
                            .await;
                    });
                    Vec::new()
                })),
            }]))
        });
        vec![info(msg)]
    }

    fn bench(&mut self, sel: Option<String>, ctx: &Ctx) -> Vec<ViewCmd> {
        if let Some(h) = self.bench.take() {
            h.abort();
            return vec![err("Benchmark Canceled!")];
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
            return vec![warn("Benchmark in progress...")];
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
        vec![warn("Benchmark in progress...")]
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
                    Err(e) => format!("status:\n  {e}\nmessage:\n{}", fmt_results("")),
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

    fn helm_values(&mut self, sel: Option<String>, ctx: &Ctx) -> Vec<ViewCmd> {
        let Some(id) = sel else {
            return Vec::new();
        };
        let (rel, rev) = self.helm_target(&id);
        let (ns, name) = namespaced(&rel);
        let mut argv = vec![
            "helm".to_string(),
            "get".into(),
            "values".into(),
            name.to_string(),
        ];
        if !ns.is_empty() {
            argv.extend(["-n".into(), ns.to_string()]);
        }
        if let Some(r) = &rev {
            argv.extend(["--revision".into(), r.clone()]);
        }
        argv.extend(self.kube_ctx_args(ctx));
        let (run, env) = (self.deps.run.clone(), ctx.accessor.shell_env());
        self.spawn(ctx, async move {
            let body = run(argv, env)
                .await
                .map_err(|e| anyhow!("Initializing the values model failed: {e}"))?;
            Ok(Msg::Text {
                name: "Values".into(),
                title: rel,
                body,
            })
        });
        Vec::new()
    }

    fn helm_rollback(&mut self, sel: Option<String>, ctx: &Ctx) -> Vec<ViewCmd> {
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
        let mut argv = vec![p.command.clone()];
        argv.extend(args.iter().cloned());
        if !p.pipes.is_empty() {
            let quote = |v: &[String]| {
                v.iter()
                    .map(|a| {
                        shlex::try_quote(a)
                            .map(|c| c.to_string())
                            .unwrap_or_else(|_| a.clone())
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            let mut line = quote(&argv);
            for pipe in &p.pipes {
                line.push_str(" | ");
                line.push_str(pipe);
            }
            argv = vec!["sh".into(), "-c".into(), line];
        }
        let (run, id) = (self.deps.run.clone(), self.id);
        let shell_env = ctx.accessor.shell_env();
        let desc = p.description.clone();
        let exec = move |ctx: &mut Ctx| -> Vec<ViewCmd> {
            if p.background {
                spawn_msg(ctx, id, async move {
                    match run(argv, shell_env).await {
                        Ok(out) if p.overwrite_output => Ok(Msg::Cmds(vec![info(
                            out.trim().trim_start_matches("[output]").trim().to_string(),
                        )])),
                        Ok(_) => Ok(Msg::Cmds(vec![info(format!(
                            "Plugin command launched successfully: {desc:?}"
                        ))])),
                        Err(e) => Ok(Msg::Cmds(vec![err(format!("Plugin command failed: {e}"))])),
                    }
                });
                return Vec::new();
            }
            vec![ViewCmd::Suspend {
                argv,
                env: shell_env,
                on_done: Some(Box::new(move |_ctx, st| {
                    if st.success() {
                        Vec::new()
                    } else {
                        vec![info(format!("Plugin command failed: {desc:?}"))]
                    }
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
