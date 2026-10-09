//! Resource browser (owned by the `ui-table` slice): the table view over any
//! GVR, including pseudo resources (contexts, aliases, portforwards,
//! screendumps, benchmarks, dir, helm, rbac/policy/users/groups, workloads,
//! containers, references) built via `render::pseudo`. Drill-down,
//! namespace switching, sorting, marks, and all per-resource actions.
//!
//! Mirrors k9s view/browser.go + view/table.go + ui/table.go and the
//! per-resource viewers registered in view/registrar.go.

mod actions;
mod helpers;
mod keys;
mod kind;
mod loader;
mod markup;
mod picker;
mod table;

#[cfg(test)]
mod tests;

use std::any::Any;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{anyhow, bail, Result};
use crossterm::event::{KeyEvent, KeyModifiers};
use futures::future::BoxFuture;
use ratatui::layout::Rect;
use ratatui::Frame;
use tokio::task::JoinHandle;

use crate::cmd::Command;
use crate::config::{ContextConfig, ViewSetting};
use crate::k8s::{
    is_all_namespaces, is_cluster_scoped, namespaced, Gvr, LogOptions, PortForwardHandle,
    ServerTable, CLUSTER_SCOPE, NAMESPACE_ALL,
};
use crate::model::{FilterSpec, Header, ResEvent, Row, RowEvent, TableData};
use crate::render::Renderer;
use crate::ui::views::BenchSpec;

use super::{AppEvent, Ctx, FlashLevel, Hint, View, ViewCmd, ViewId};

use keys::*;
use kind::{fallback_meta, kind_for, meta_for, Kind, Meta};
use table::{cleanse_namespace, print_namespace, SortColumn, TableState, TitleColors};

/// Runs an external command, returning stdout (helm, kubectl cp/apply).
pub(crate) type Runner = Arc<
    dyn Fn(Vec<String>, Vec<(String, String)>) -> BoxFuture<'static, Result<String>> + Send + Sync,
>;
/// Runs a benchmark, returning the report path.
pub(crate) type BenchFn =
    fn(BenchSpec, String, String, String) -> BoxFuture<'static, Result<PathBuf>>;

/// Collaborators of the browser. Production values call the real
/// renderers, filters, detail views and processes; tests inject fakes.
#[derive(Clone)]
pub(crate) struct Deps {
    pub make_renderer: fn(&Gvr) -> Box<dyn Renderer>,
    pub parse_filter: fn(&str) -> FilterSpec,
    pub apply_filter: fn(&TableData, &FilterSpec) -> TableData,
    pub sort_rows: fn(&mut [RowEvent], &Header, usize, bool),
    pub server_table: fn(&ServerTable, &str) -> (Header, Vec<Row>),
    pub context_config: fn(&Ctx) -> ContextConfig,
    pub dumps_dir: fn(&Ctx) -> PathBuf,
    pub benchmarks_dir: fn(&Ctx) -> PathBuf,
    pub bench_config: fn(&Ctx) -> PathBuf,
    pub clipboard: fn(&str) -> Result<()>,
    pub parse_key: fn(&str) -> Option<KeyEvent>,
    pub describe: fn(&mut Ctx, Gvr, String) -> Box<dyn View>,
    pub yaml: fn(&mut Ctx, Gvr, String) -> Box<dyn View>,
    pub logs: fn(&mut Ctx, LogOptions) -> Box<dyn View>,
    pub text: fn(&mut Ctx, String, String, String) -> Box<dyn View>,
    pub help: fn(&mut Ctx, Vec<Hint>) -> Box<dyn View>,
    pub bench: BenchFn,
    pub run: Runner,
}

fn context_subpath(ctx: &Ctx) -> PathBuf {
    PathBuf::from(helpers::sanitize_file_name(&ctx.cluster_info.cluster))
        .join(helpers::sanitize_file_name(&ctx.cluster_info.context))
}

fn default_runner() -> Runner {
    Arc::new(|argv: Vec<String>, env: Vec<(String, String)>| {
        Box::pin(async move {
            let bin = argv
                .first()
                .cloned()
                .ok_or_else(|| anyhow!("no command given"))?;
            let path =
                which::which(&bin).map_err(|_| anyhow!("`{bin}` command not found in $PATH"))?;
            let out = tokio::process::Command::new(path)
                .args(&argv[1..])
                .envs(env)
                .output()
                .await?;
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            if !out.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
                bail!(
                    "{}",
                    if stderr.is_empty() {
                        out.status.to_string()
                    } else {
                        stderr
                    }
                );
            }
            Ok(stdout)
        }) as BoxFuture<'static, Result<String>>
    })
}

impl Deps {
    /// Real collaborators.
    pub(crate) fn production() -> Deps {
        Deps {
            make_renderer: crate::render::renderer_for,
            parse_filter: FilterSpec::parse,
            apply_filter: crate::model::apply_filter,
            sort_rows: crate::model::sort_rows,
            server_table: crate::render::core::render_server_table,
            context_config: |ctx| {
                ContextConfig::load(&ctx.cluster_info.cluster, &ctx.cluster_info.context)
                    .unwrap_or_default()
            },
            dumps_dir: |ctx| {
                let dir = ctx
                    .config
                    .read()
                    .map(|c| c.k9s.screen_dump_dir.clone())
                    .unwrap_or_default();
                let base = if dir.is_empty() {
                    crate::config::paths::dumps_dir()
                } else {
                    PathBuf::from(dir)
                };
                base.join(context_subpath(ctx))
            },
            benchmarks_dir: |ctx| {
                let dumps = crate::config::paths::dumps_dir();
                let state = dumps.parent().map(PathBuf::from).unwrap_or(dumps);
                state.join("benchmarks").join(context_subpath(ctx))
            },
            bench_config: |ctx| {
                crate::config::paths::contexts_dir()
                    .join(context_subpath(ctx))
                    .join("benchmarks.yaml")
            },
            clipboard: |text| {
                if text.is_empty() {
                    return Ok(());
                }
                arboard::Clipboard::new()?.set_text(text.to_string())?;
                Ok(())
            },
            parse_key: crate::ui::keys::parse_key,
            describe: crate::ui::views::describe,
            yaml: crate::ui::views::yaml,
            logs: crate::ui::views::logs,
            text: crate::ui::views::text,
            help: crate::ui::shell::help,
            bench: |spec, cluster, context, name| {
                Box::pin(crate::ui::views::run_benchmark(
                    spec, cluster, context, name,
                ))
            },
            run: default_runner(),
        }
    }
}

/// Construction options (k9s context values: path, labels, fields...).
#[derive(Clone, Default)]
pub(crate) struct Opts {
    pub ns: Option<String>,
    pub path: String,
    pub instance: String,
    pub label: Option<String>,
    pub field: Option<String>,
    pub uid: Option<String>,
    pub subject: Option<(String, String)>,
    pub source: Option<(Gvr, String)>,
    pub dir: Option<PathBuf>,
    pub filter: Option<String>,
    pub alias: String,
}

/// Async follow-ups posted back to the browser.
pub(crate) enum Msg {
    Cmds(Vec<ViewCmd>),
    /// The first watch was refused: flash once, stop loading.
    WatchFailed(String),
    Logs(LogOptions),
    ShowPods {
        path: String,
        label: Option<String>,
        field: Option<String>,
        ns: String,
    },
    ShowReplicaSets {
        path: String,
        label: String,
    },
    ShowJobs {
        path: String,
        uid: String,
    },
    ShowNode(String),
    ShowPortForwards(String),
    Owners {
        ns: String,
        owners: Vec<(String, String, String)>,
    },
    PickShell {
        path: String,
        containers: Vec<String>,
        attach: bool,
        os: String,
    },
    PfStarted(Vec<PortForwardHandle>),
    Text {
        name: String,
        title: String,
        body: String,
    },
    Refs {
        gvr: Gvr,
        path: String,
    },
    Deleted(Vec<String>),
    Dim(Vec<String>),
    Refresh,
    BenchDone(std::result::Result<PathBuf, String>),
}

pub(crate) fn post(tx: &tokio::sync::mpsc::UnboundedSender<AppEvent>, view: ViewId, msg: Msg) {
    let _ = tx.send(AppEvent::Custom {
        view,
        payload: payload(msg),
    });
}

pub(crate) fn post_err(
    tx: &tokio::sync::mpsc::UnboundedSender<AppEvent>,
    view: ViewId,
    msg: impl std::fmt::Display,
) {
    let _ = tx.send(AppEvent::Error {
        view,
        msg: msg.to_string(),
    });
}

/// The resource table view.
pub struct Browser {
    id: ViewId,
    gvr: Gvr,
    kind: Kind,
    meta: Meta,
    deps: Deps,
    renderer: Arc<dyn Renderer>,
    view_setting: Option<ViewSetting>,
    table: TableState,
    /// Model namespace: "" all, "-" cluster scoped, "*" not namespaced.
    ns: String,
    path: String,
    instance: String,
    filter: String,
    label: Option<String>,
    label_from_cmd: bool,
    field: Option<String>,
    uid: Option<String>,
    subject: Option<(String, String)>,
    source: Option<(Gvr, String)>,
    dir: PathBuf,
    actions: Actions,
    cc: ContextConfig,
    has_metrics: bool,
    loading: Option<JoinHandle<()>>,
    /// browser.go Start: the watch failed, so no updates ever come.
    watch_failed: bool,
    bench: Option<JoinHandle<()>>,
    active: bool,
    dirty: bool,
}

/// Open a browser for the resolved command (`cmd.gvr` is a GVR string or a
/// pseudo resource). Errors for unknown resources.
pub fn open(ctx: &mut Ctx, cmd: &Command) -> Result<Box<dyn View>> {
    open_with(ctx, cmd, Deps::production())
}

/// Container table for pod `path` (`ns/name`).
pub fn containers(ctx: &mut Ctx, path: String) -> Box<dyn View> {
    Box::new(child(
        ctx,
        Deps::production(),
        Gvr::parse("containers"),
        Opts {
            path,
            ..Default::default()
        },
    ))
}

fn expand_dir(arg: &str) -> Result<PathBuf> {
    let p = if arg == "~" || arg.starts_with("~/") {
        let home = dirs::home_dir().ok_or_else(|| anyhow!("unable to locate home dir"))?;
        home.join(arg.trim_start_matches('~').trim_start_matches('/'))
    } else {
        PathBuf::from(arg)
    };
    let p = std::fs::canonicalize(&p).map_err(|e| anyhow!("invalid dir {arg:?}: {e}"))?;
    if !p.is_dir() {
        bail!("{arg:?} is not a directory");
    }
    Ok(p)
}

/// k9s mapSubject.
fn map_subject(s: &str) -> String {
    match s {
        "g" => "Group".into(),
        "s" => "ServiceAccount".into(),
        "u" => "User".into(),
        other => other.into(),
    }
}

pub(crate) fn open_with(ctx: &mut Ctx, cmd: &Command, deps: Deps) -> Result<Box<dyn View>> {
    let gvr = Gvr::parse(&cmd.gvr);
    let kind = kind_for(&gvr);
    let meta = meta_for(kind, &gvr, &ctx.resources)
        .ok_or_else(|| anyhow!("no resource meta defined for {:?}", cmd.gvr))?;
    let mut opts = Opts {
        ns: cmd.namespace.clone(),
        alias: cmd.alias.clone(),
        ..Default::default()
    };
    match kind {
        Kind::Dir => {
            let arg = cmd.args.first().map(String::as_str).unwrap_or(".");
            let dir = expand_dir(arg)?;
            opts.path = dir.to_string_lossy().to_string();
            opts.dir = Some(dir);
        }
        Kind::Policy => {
            let arg = cmd
                .args
                .first()
                .ok_or_else(|| anyhow!("Invalid command. Use `can [u|g|s]:xxx`"))?;
            let (cat, sub) = arg
                .split_once(':')
                .ok_or_else(|| anyhow!("Invalid command. Use `can [u|g|s]:xxx`"))?;
            opts.subject = Some((map_subject(cat), sub.to_string()));
        }
        _ => {}
    }
    if let Some(f) = &cmd.filter {
        opts.filter = Some(f.clone());
    }
    if let Some(f) = &cmd.fuzzy {
        opts.filter = Some(format!("-f {f}"));
    }
    if let Some(l) = &cmd.label_selector {
        opts.label = Some(l.clone());
    }
    // browser.go Init: CanForResource(active ns, gvr, list) for k8s resources.
    if is_k8s_resource(ctx, &gvr) {
        let ns = crate::k8s::clean_namespace(&ctx.namespace).to_string();
        let acc = ctx.accessor.clone();
        let g = gvr.clone();
        let n = ns.clone();
        if !block_on(async move { acc.can(&g, &n, "list").await })? {
            bail!("{}", access_denied(&ns, &gvr));
        }
    }
    Ok(Box::new(Browser::new(ctx, gvr, kind, meta, deps, opts)))
}

/// A resource the API server serves (not a k9s pseudo resource).
fn is_k8s_resource(ctx: &Ctx, gvr: &Gvr) -> bool {
    ctx.resources.iter().any(|m| &m.gvr == gvr)
}

/// watch.Factory.CanForResource denial message.
fn access_denied(ns: &str, gvr: &Gvr) -> String {
    format!(
        "[list] access denied on resource {}:{}",
        crate::ui::shell::text::go_quote(ns),
        crate::ui::shell::text::go_quote(&gvr.to_string())
    )
}

/// Run a short async check from sync view setup, as k9s' Init blocks on
/// its access review (cached by the accessor).
fn block_on<T>(f: impl std::future::Future<Output = T> + Send) -> T
where
    T: Send,
{
    match tokio::runtime::Handle::try_current() {
        Ok(h) if h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(|| h.block_on(f))
        }
        _ => futures::executor::block_on(f),
    }
}

/// Build a browser for drill-downs (falls back to a guessed meta).
pub(crate) fn child(ctx: &mut Ctx, deps: Deps, gvr: Gvr, opts: Opts) -> Browser {
    let kind = kind_for(&gvr);
    let meta = meta_for(kind, &gvr, &ctx.resources).unwrap_or_else(|| fallback_meta(&gvr));
    Browser::new(ctx, gvr, kind, meta, deps, opts)
}

impl Browser {
    pub(crate) fn new(
        ctx: &mut Ctx,
        gvr: Gvr,
        kind: Kind,
        meta: Meta,
        deps: Deps,
        opts: Opts,
    ) -> Browser {
        let id = ctx.new_view_id();
        let view_setting = Self::lookup_view_setting(ctx, &gvr, &opts.alias);
        let renderer: Arc<dyn Renderer> = {
            let mut r = (deps.make_renderer)(&gvr);
            r.set_view_setting(view_setting.clone());
            Arc::from(r)
        };
        let mut table = TableState::new();
        let initial_sort = match kind {
            Kind::Container => Some(("IDX", true)),
            Kind::Job | Kind::PortForward | Kind::ScreenDump | Kind::Benchmark => {
                Some(("AGE", true))
            }
            Kind::Event => Some(("LAST SEEN", false)),
            Kind::Rules => Some(("API-GROUP", true)),
            Kind::Policy => Some(("API-GROUP", false)),
            Kind::Workload => Some(("KIND", true)),
            Kind::HelmHistory => Some(("REVISION", false)),
            _ => None,
        };
        if let Some((n, asc)) = initial_sort {
            table.sort = SortColumn {
                name: n.into(),
                asc,
            };
        }
        let dir = match kind {
            Kind::ScreenDump => (deps.dumps_dir)(ctx),
            Kind::Benchmark => (deps.benchmarks_dir)(ctx),
            _ => opts.dir.clone().unwrap_or_default(),
        };
        let mut path = opts.path.clone();
        if let Some((k, n)) = &opts.subject {
            if kind == Kind::Policy {
                path = format!("{k}:{n}");
            }
        }
        if let (Kind::Rules | Kind::Reference, Some((_, p))) = (kind, &opts.source) {
            path = p.clone();
        }
        let cc = (deps.context_config)(ctx);
        let mut b = Browser {
            id,
            gvr,
            kind,
            meta,
            deps,
            renderer,
            view_setting,
            table,
            ns: String::new(),
            path,
            instance: opts.instance.clone(),
            filter: String::new(),
            label: None,
            label_from_cmd: false,
            field: opts.field.clone(),
            uid: opts.uid.clone(),
            subject: opts.subject.clone(),
            source: opts.source.clone(),
            dir,
            actions: Actions::new(),
            cc,
            has_metrics: ctx.has_metrics,
            loading: None,
            watch_failed: false,
            bench: None,
            active: false,
            dirty: true,
        };
        let ns = opts.ns.clone().unwrap_or_else(|| ctx.namespace.clone());
        b.set_namespace(&ns);
        if let Some(f) = opts.filter {
            b.set_filter_text(&f);
        }
        if let Some(l) = opts.label {
            b.label = Some(l.clone());
            b.label_from_cmd = true;
            b.filter = l;
        }
        b.refresh_actions(ctx);
        b
    }

    fn lookup_view_setting(ctx: &Ctx, gvr: &Gvr, alias: &str) -> Option<ViewSetting> {
        let views = &ctx.views.views;
        let gs = gvr.to_string();
        if let Some(v) = views.get(&gs) {
            return Some(v.clone());
        }
        if !alias.is_empty() {
            if let Some(v) = views.get(alias) {
                return Some(v.clone());
            }
        }
        if let Some(v) = views.get(gvr.r()) {
            return Some(v.clone());
        }
        ctx.aliases
            .aliases
            .iter()
            .filter(|(_, g)| **g == gs)
            .find_map(|(a, _)| views.get(a).cloned())
    }

    /// Browser.setNamespace.
    fn set_namespace(&mut self, ns: &str) {
        let mut ns = cleanse_namespace(ns);
        if !self.meta.namespaced {
            ns = CLUSTER_SCOPE.to_string();
        }
        match self.kind {
            Kind::Alias => ns = table::NOT_NAMESPACED.to_string(),
            Kind::Reference => ns = String::new(),
            _ => {}
        }
        if ns != self.ns {
            self.ns = ns.clone();
            self.table.data = TableData::new(self.gvr.clone(), Header::default(), ns);
            self.table.rows.clear();
            self.table.loaded = false;
            self.dirty = true;
        }
    }

    /// The `/` buffer changed: recompute the server-side label selector.
    fn set_filter_text(&mut self, text: &str) -> bool {
        self.filter = text.to_string();
        let old = self.label.clone();
        if self.label_from_cmd && self.label.as_deref() == Some(text) {
            return false;
        }
        self.label_from_cmd = false;
        self.label = if text.is_empty() {
            None
        } else {
            match (self.deps.parse_filter)(text) {
                FilterSpec::Label(l) => Some(l),
                _ => None,
            }
        };
        self.dirty = true;
        old != self.label
    }

    /// Client side filter (label selectors are applied server side).
    fn client_filter(&self) -> FilterSpec {
        if self.filter.is_empty()
            || (self.label_from_cmd && self.label.as_deref() == Some(self.filter.as_str()))
        {
            return FilterSpec::None;
        }
        match (self.deps.parse_filter)(&self.filter) {
            FilterSpec::Label(_) => FilterSpec::None,
            other => other,
        }
    }

    fn resource_name(&self) -> String {
        match self.kind {
            Kind::Dir => "dirs".into(),
            _ => self.gvr.r().to_string(),
        }
    }

    fn title_parts(&self) -> (String, String, usize) {
        let ns = table::title_ns(&self.ns, &self.path, "");
        (self.resource_name(), ns, self.table.rows.len())
    }

    fn load_spec(&self, ctx: &Ctx) -> loader::LoadSpec {
        let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
        if self.kind == Kind::Alias {
            for (a, g) in &ctx.aliases.aliases {
                groups.entry(g.clone()).or_default().push(a.clone());
            }
        }
        loader::LoadSpec {
            kind: self.kind,
            gvr: self.gvr.clone(),
            ns: self.ns.clone(),
            path: self.path.clone(),
            instance: self.instance.clone(),
            label: self.label.clone(),
            field: self.field.clone(),
            uid: self.uid.clone(),
            has_metrics: ctx.has_metrics,
            accessor: ctx.accessor.clone(),
            renderer: self.renderer.clone(),
            server_table: self.deps.server_table,
            run: self.deps.run.clone(),
            aliases: groups
                .into_iter()
                .map(|(g, a)| (Gvr::parse(&g), a))
                .collect(),
            port_forwards: ctx.port_forwards.values().map(|h| h.info.clone()).collect(),
            dir: self.dir.clone(),
            subject: self.subject.clone(),
            source: self.source.clone(),
            kube_context: ctx.accessor.active_context(),
            env: ctx.accessor.shell_env(),
            pod_counting: !ctx
                .config
                .read()
                .map(|c| c.k9s.disable_pod_counting)
                .unwrap_or(false),
            bench: if self.kind == Kind::PortForward {
                let file = (self.deps.bench_config)(ctx);
                ctx.port_forwards
                    .values()
                    .map(|h| {
                        let b = helpers::bench_config_for(&file, &h.info.spec.id(), false);
                        (
                            h.info.spec.id(),
                            (b.concurrency as i64, b.requests as i64, b.host, b.path),
                        )
                    })
                    .collect()
            } else {
                Default::default()
            },
        }
    }

    /// Kick off a refresh unless one is in flight (`force` restarts it).
    fn spawn_load(&mut self, ctx: &Ctx, force: bool) {
        if self.watch_failed {
            return;
        }
        if let Some(h) = &self.loading {
            if !h.is_finished() {
                if !force {
                    return;
                }
                h.abort();
            }
        }
        let spec = self.load_spec(ctx);
        let tx = ctx.sender();
        let id = self.id;
        // browser.go Start: a watch the user may not list fails once with
        // "Watcher failed"; no updates follow.
        let watch_check = (!self.table.loaded && is_k8s_resource(ctx, &self.gvr)).then(|| {
            (
                self.gvr.clone(),
                crate::k8s::clean_namespace(&spec.ns).to_string(),
            )
        });
        self.loading = Some(ctx.spawn(async move {
            if let Some((gvr, ns)) = watch_check {
                let denied = match spec.accessor.can(&gvr, &ns, "list").await {
                    Ok(true) => None,
                    Ok(false) => Some(access_denied(&ns, &gvr)),
                    Err(e) => Some(e.to_string()),
                };
                if let Some(e) = denied {
                    post(
                        &tx,
                        id,
                        Msg::WatchFailed(format!("Watcher failed for {gvr} -- {e}")),
                    );
                    return;
                }
            }
            match loader::load(spec).await {
                Ok(data) => {
                    let _ = tx.send(AppEvent::Data { view: id, data });
                }
                Err(e) => post_err(&tx, id, format!("{e:#}")),
            }
        }));
    }

    /// New model data landed (TableData.Update + TableDataChanged).
    fn apply(&mut self, td: TableData, ctx: &Ctx) -> Vec<ViewCmd> {
        if td.namespace != self.ns || td.gvr != self.gvr {
            return Vec::new();
        }
        let first = !self.table.loaded;
        if self.table.data.header != td.header {
            self.table.data.rows.clear();
        }
        self.table.data.header = td.header;
        self.table.data.namespace = td.namespace;
        self.table.data.gvr = td.gvr;
        let empty = td.rows.is_empty();
        self.table
            .data
            .update(td.rows.into_iter().map(|r| r.row).collect());
        for re in self.table.data.rows.iter_mut() {
            if re.kind == ResEvent::Update && re.deltas.iter().all(|d| d.is_empty()) {
                re.kind = ResEvent::Unchanged;
                re.deltas.clear();
            }
        }
        self.table.deleted.clear();
        self.table.loaded = true;
        self.has_metrics = ctx.has_metrics;
        self.dirty = true;
        self.rebuild(ctx);
        self.refresh_actions(ctx);
        if !first {
            return Vec::new();
        }
        let msg = if empty {
            (
                FlashLevel::Warn,
                format!(
                    "No resources found for {} in {:?} namespace",
                    self.gvr,
                    print_namespace(&self.ns)
                ),
            )
        } else if is_cluster_scoped(&self.ns) {
            (FlashLevel::Info, format!("Viewing {}...", self.gvr))
        } else {
            (
                FlashLevel::Info,
                format!(
                    "Viewing {} in namespace {}",
                    self.gvr,
                    print_namespace(&self.ns)
                ),
            )
        };
        vec![ViewCmd::Flash(msg.0, msg.1)]
    }

    /// Decorations k9s applies before display (pod PF indicator,
    /// cpu/mem thresholds, namespace favorites).
    fn decorate(&self, td: &mut TableData, ctx: &Ctx) {
        match self.kind {
            Kind::Pod | Kind::Container => {
                if let Some(idx) = td.header.index_of("PF") {
                    for re in td.rows.iter_mut() {
                        let forwarded = ctx.port_forwards.values().any(|h| {
                            let s = &h.info.spec;
                            let pod = crate::k8s::fqn(&s.namespace, &s.pod);
                            if self.kind == Kind::Pod {
                                pod == re.row.id
                            } else {
                                pod == self.path && s.container == re.row.id
                            }
                        });
                        if forwarded && idx < re.row.fields.len() {
                            re.row.fields[idx] = "[orange::b]Ⓕ".into();
                        }
                    }
                }
                self.decorate_thresholds(td, ctx);
            }
            Kind::Namespace => {
                if td.rows.is_empty() {
                    return;
                }
                if !td.rows.iter().any(|r| r.row.id == NAMESPACE_ALL) {
                    let mut fields = vec![NAMESPACE_ALL.to_string(), "Active".into()];
                    fields.resize(td.header.len().max(2), String::new());
                    td.rows.push(RowEvent::new(
                        ResEvent::Unchanged,
                        Row::with(NAMESPACE_ALL, fields),
                    ));
                }
                let active = ctx.namespace.clone();
                for re in td.rows.iter_mut() {
                    let (_, n) = namespaced(&re.row.id);
                    let n = n.to_string();
                    if let Some(f0) = re.row.fields.get_mut(0) {
                        if self.cc.namespace.favorites.contains(&n) {
                            f0.push('+');
                        }
                        if n == active {
                            f0.push_str("(*)");
                        }
                    }
                    re.kind = ResEvent::Unchanged;
                }
            }
            _ => {}
        }
    }

    fn decorate_thresholds(&self, td: &mut TableData, ctx: &Ctx) {
        let th = ctx
            .config
            .read()
            .map(|c| c.k9s.thresholds.clone())
            .unwrap_or_default();
        for (ci, h) in td.header.0.iter().enumerate() {
            let sev = match h.name.as_str() {
                "%CPU/L" => &th.cpu,
                "%MEM/L" => &th.memory,
                _ => continue,
            };
            for re in td.rows.iter_mut() {
                let Some(f) = re.row.fields.get_mut(ci) else {
                    continue;
                };
                let Ok(mut n) = f.parse::<i32>() else {
                    continue;
                };
                n = n.min(100);
                if n < 0 {
                    continue;
                }
                let color = if n >= sev.critical {
                    "red"
                } else if n >= sev.warn {
                    "orangered"
                } else {
                    continue;
                };
                *f = format!("[{color}::b]{f}");
            }
        }
    }

    /// TableData.ComputeSortCol.
    fn compute_sort_col(&self, td: &TableData) -> SortColumn {
        let sc = self.table.sort.clone();
        let vs_sort = self.view_setting.as_ref().and_then(|vs| {
            let (n, o) = vs.sort_column.split_once(':')?;
            Some(SortColumn {
                name: n.to_string(),
                asc: o == "asc",
            })
        });
        let blank = self
            .view_setting
            .as_ref()
            .map(|v| v.columns.is_empty() && v.sort_column.is_empty())
            .unwrap_or(true);
        if blank {
            if !sc.name.is_empty() || td.header.is_empty() {
                return sc;
            }
            let has = |n: &str| td.header.iter().any(|h| h.name == n);
            let name = if is_all_namespaces(&td.namespace) {
                if has("NAMESPACE") {
                    "NAMESPACE".to_string()
                } else if has("NAME") {
                    "NAME".to_string()
                } else {
                    String::new()
                }
            } else if has("NAME") {
                "NAME".to_string()
            } else {
                td.header.0[0].name.clone()
            };
            return SortColumn { name, asc: true };
        }
        if self.table.manual_sort && !sc.name.is_empty() {
            return sc;
        }
        vs_sort.unwrap_or(sc)
    }

    /// Update + UpdateUI: decorate, filter, sort into `table.rows`.
    fn rebuild(&mut self, ctx: &Ctx) {
        let mut cdata = self.table.data.clone();
        self.decorate(&mut cdata, ctx);
        if self.table.toast {
            cdata.rows = match cdata.header.index_of("VALID") {
                Some(idx) => cdata
                    .rows
                    .into_iter()
                    .filter(|r| {
                        r.row
                            .fields
                            .get(idx)
                            .map(|f| !f.is_empty())
                            .unwrap_or(false)
                    })
                    .collect(),
                None => Vec::new(),
            };
        }
        let spec = self.client_filter();
        if !spec.is_none() {
            cdata = (self.deps.apply_filter)(&cdata, &spec);
        }
        let old = self.table.sort.name.clone();
        self.table.sort = self.compute_sort_col(&cdata);
        if old != self.table.sort.name {
            self.table.init_selected_column(&self.ns, ctx.has_metrics);
        }
        if let Some(idx) = cdata
            .header
            .iter()
            .position(|h| h.name == self.table.sort.name)
        {
            (self.deps.sort_rows)(&mut cdata.rows, &cdata.header, idx, self.table.sort.asc);
        }
        self.table.rows = cdata.rows;
        self.table.clamp_cursor();
        self.dirty = false;
    }

    fn sync(&mut self, ctx: &Ctx) {
        if self.dirty {
            self.rebuild(ctx);
        }
        self.refresh_actions(ctx);
    }

    fn selected(&self) -> Option<String> {
        self.table.selected_id()
    }

    /// Visible hints (KeyActions.Hints: non shared, key order).
    fn hint_list(&self) -> Vec<Hint> {
        self.actions
            .iter()
            .filter(|(_, a)| !a.shared)
            .map(|(k, a)| {
                let h = Hint::new(key_name(*k), a.desc.clone());
                if a.visible {
                    h
                } else {
                    h.hidden()
                }
            })
            .collect()
    }

    /// Aliases of this resource (plugin scopes).
    fn scope_aliases(&self, ctx: &Ctx) -> Vec<String> {
        let gs = self.gvr.to_string();
        let mut out: Vec<String> = ctx
            .aliases
            .aliases
            .iter()
            .filter(|(_, g)| **g == gs)
            .map(|(a, _)| a.clone())
            .collect();
        out.push(self.gvr.r().to_string());
        if let Some(m) = ctx.resources.iter().find(|m| m.gvr == self.gvr) {
            out.extend(m.short_names.iter().cloned());
            if !m.singular_name.is_empty() {
                out.push(m.singular_name.clone());
            }
        }
        out
    }

    /// Table.bindKeys + Browser.bindKeys + refreshActions + extenders,
    /// plugins and hotkeys.
    fn refresh_actions(&mut self, ctx: &Ctx) {
        let ro = ctx.readonly;
        let mut aa = Actions::new();
        aa.insert(rune('?'), Action::new("Help", Act::Help, true));
        aa.insert(KEY_SPACE, Action::shared("Mark", Act::Mark, false));
        aa.insert(
            KEY_CTRL_SPACE,
            Action::shared("Mark Range", Act::MarkRange, false),
        );
        aa.insert(
            KEY_CTRL_BACKSLASH,
            Action::shared("Marks Clear", Act::ClearMarks, false),
        );
        aa.insert(ctrl('s'), Action::shared("Save", Act::Save, false));
        aa.insert(
            rune('/'),
            Action::shared("Filter Mode", Act::FilterMode, false),
        );
        aa.insert(
            ctrl('z'),
            Action::new("Toggle Faults", Act::ToggleFaults, false),
        );
        aa.insert(
            ctrl('w'),
            Action::new("Toggle Wide", Act::ToggleWide, false),
        );
        aa.insert(
            rune('N'),
            Action::new("Sort Name", Act::SortCol("NAME".into(), true), false),
        );
        aa.insert(
            rune('A'),
            Action::new("Sort Age", Act::SortCol("AGE".into(), true), false),
        );
        aa.insert(
            rune('S'),
            Action::new("Sort Status", Act::SortCol("STATUS".into(), true), false),
        );
        aa.insert(
            rune('O'),
            Action::new("Sort Selected Column", Act::SortSelected, false),
        );
        aa.insert(
            KEY_ESC,
            Action::shared("Filter Reset", Act::FilterReset, false),
        );
        aa.insert(
            rune('q'),
            Action::shared("Filter Reset", Act::FilterReset, false),
        );
        aa.insert(KEY_ENTER, Action::shared("Filter", Act::Enter, false));
        self.kind_keys(&mut aa, ctx);
        // doUpdate: namespace sort only for all-namespaces data.
        if is_all_namespaces(&self.table.data.namespace) && self.table.loaded {
            aa.insert(
                rune('P'),
                Action::new(
                    "Sort Namespace",
                    Act::SortCol("NAMESPACE".into(), true),
                    false,
                ),
            );
        } else {
            aa.remove(&rune('P'));
        }

        let mut a2 = Actions::new();
        a2.insert(rune('c'), Action::new("Copy", Act::Copy, false));
        a2.insert(KEY_ENTER, Action::new("View", Act::Enter, false));
        a2.insert(ctrl('r'), Action::new("Refresh", Act::Refresh, false));
        if self.meta.namespaced && self.path.is_empty() {
            a2.insert(rune('n'), Action::new("Copy Namespace", Act::CopyNs, false));
            a2.insert(rune('w'), Action::new("Warp To Namespace", Act::Warp, true));
            a2.insert(
                rune('0'),
                Action::new(NAMESPACE_ALL, Act::SwitchNs(NAMESPACE_ALL.into()), true),
            );
            let mut index = 1u32;
            for ns in &self.cc.namespace.favorites {
                if ns == NAMESPACE_ALL {
                    continue;
                }
                if index > 9 {
                    tracing::warn!(namespace = %ns, "no number key available for favorite namespace");
                    break;
                }
                a2.insert(
                    rune('0') + index,
                    Action::new(ns, Act::SwitchNs(ns.clone()), true),
                );
                index += 1;
            }
        }
        if !ro {
            if self.meta.can("edit") {
                a2.insert(rune('e'), Action::danger("Edit", Act::Edit));
            }
            if self.meta.can("delete") {
                a2.insert(ctrl('d'), Action::danger("Delete", Act::Delete));
            }
        }
        if !self.meta.k9s {
            a2.insert(rune('y'), Action::new("YAML", Act::Yaml, true));
            a2.insert(rune('d'), Action::new("Describe", Act::Describe, true));
        }
        self.kind_keys(&mut a2, ctx);
        aa.extend(a2);
        self.plugin_keys(&mut aa, ctx);
        self.hotkey_keys(&mut aa, ctx);
        if ro {
            aa.retain(|_, a| !a.dangerous);
        }
        self.actions = aa;
    }

    fn plugin_keys(&self, aa: &mut Actions, ctx: &Ctx) {
        if ctx.plugins.plugins.is_empty() {
            return;
        }
        aa.retain(|_, a| !matches!(a.act, Act::Plugin(_)));
        let aliases = self.scope_aliases(ctx);
        for (name, p) in &ctx.plugins.plugins {
            let in_scope = p.scopes.iter().any(|s| s == "all" || aliases.contains(s));
            if !in_scope || (ctx.readonly && p.dangerous) {
                continue;
            }
            let Some(key) = (self.deps.parse_key)(&p.short_cut).and_then(|k| key_of(&k)) else {
                tracing::warn!(plugin = %name, shortcut = %p.short_cut, "invalid plugin shortcut");
                continue;
            };
            if aa.contains_key(&key) && !p.override_ {
                tracing::warn!(plugin = %name, shortcut = %p.short_cut, "duplicate plugin key");
                continue;
            }
            aa.insert(
                key,
                Action {
                    desc: p.description.clone(),
                    act: Act::Plugin(name.clone()),
                    visible: true,
                    shared: false,
                    dangerous: p.dangerous,
                },
            );
        }
    }

    fn hotkey_keys(&self, aa: &mut Actions, ctx: &Ctx) {
        if ctx.hotkeys.hot_keys.is_empty() {
            return;
        }
        aa.retain(|_, a| !matches!(a.act, Act::HotKey(_)));
        for (name, hk) in &ctx.hotkeys.hot_keys {
            let Some(key) = (self.deps.parse_key)(&hk.short_cut).and_then(|k| key_of(&k)) else {
                tracing::warn!(hotkey = %name, shortcut = %hk.short_cut, "invalid hotkey shortcut");
                continue;
            };
            if aa.contains_key(&key) && !hk.override_ {
                tracing::warn!(hotkey = %name, shortcut = %hk.short_cut, "duplicate hotkey");
                continue;
            }
            aa.insert(
                key,
                Action::shared(&hk.description, Act::HotKey(name.clone()), false),
            );
        }
    }

    /// Extender and per-viewer bindings (bindKeysFn chain).
    fn kind_keys(&self, aa: &mut Actions, ctx: &Ctx) {
        let ro = ctx.readonly;
        let logs = |aa: &mut Actions| {
            aa.insert(rune('l'), Action::new("Logs", Act::Logs(false), true));
            aa.insert(
                rune('p'),
                Action::new("Logs Previous", Act::Logs(true), true),
            );
        };
        let image = |aa: &mut Actions| {
            if !ro {
                aa.insert(rune('i'), Action::new("Set Image", Act::SetImage, false));
            }
        };
        let owner = |aa: &mut Actions| {
            aa.insert(rune('J'), Action::new("Jump Owner", Act::JumpOwner, true));
        };
        let pf = |aa: &mut Actions| {
            aa.insert(
                rune('f'),
                Action::new("Show PortForward", Act::ShowPortForward, true),
            );
            aa.insert(
                rune('F'),
                Action::new("Port-Forward", Act::PortForward, true),
            );
        };
        let scale = |aa: &mut Actions| {
            if !ro && self.meta.scalable {
                aa.insert(rune('s'), Action::danger("Scale", Act::Scale));
            }
        };
        let restart = |aa: &mut Actions| {
            if !ro {
                aa.insert(rune('r'), Action::danger("Restart", Act::Restart));
            }
        };
        let del = |aa: &mut Actions, keys: &[Key]| {
            for k in keys {
                aa.remove(k);
            }
        };
        let used_by = |aa: &mut Actions| {
            aa.insert(rune('u'), Action::new("UsedBy", Act::UsedBy, true));
        };
        match self.kind {
            Kind::Generic => {
                owner(aa);
                scale(aa);
            }
            Kind::Pod => {
                logs(aa);
                image(aa);
                owner(aa);
                pf(aa);
                if !ro {
                    aa.insert(ctrl('k'), Action::danger("Kill", Act::Kill));
                    aa.insert(rune('s'), Action::danger("Shell", Act::Shell));
                    aa.insert(rune('a'), Action::danger("Attach", Act::Attach));
                    aa.insert(rune('t'), Action::danger("Transfer", Act::Transfer));
                    aa.insert(rune('z'), Action::danger("Sanitize", Act::Sanitize));
                }
                aa.insert(rune('o'), Action::new("Show Node", Act::ShowNode, true));
            }
            Kind::Container => {
                logs(aa);
                del(aa, &[KEY_CTRL_SPACE, KEY_SPACE]);
                if !ro {
                    aa.insert(rune('s'), Action::danger("Shell", Act::Shell));
                    aa.insert(rune('a'), Action::danger("Attach", Act::Attach));
                }
                aa.insert(
                    rune('f'),
                    Action::new("Show PortForward", Act::ShowPortForward, true),
                );
                aa.insert(
                    rune('F'),
                    Action::new("PortForward", Act::PortForward, true),
                );
            }
            Kind::Deploy | Kind::StatefulSet => {
                logs(aa);
                owner(aa);
                image(aa);
                scale(aa);
                restart(aa);
                pf(aa);
                if self.kind == Kind::Deploy {
                    aa.insert(
                        rune('z'),
                        Action::new("ReplicaSets", Act::ReplicaSets, true),
                    );
                }
            }
            Kind::DaemonSet => {
                logs(aa);
                owner(aa);
                image(aa);
                restart(aa);
                pf(aa);
            }
            Kind::ReplicaSet => {
                owner(aa);
                aa.insert(ctrl('l'), Action::new("Rollback", Act::Rollback, true));
            }
            Kind::Job => {
                logs(aa);
                owner(aa);
            }
            Kind::CronJob => {
                owner(aa);
                aa.insert(rune('t'), Action::new("Trigger", Act::Trigger, true));
                aa.insert(
                    rune('s'),
                    Action::new("Suspend/Resume", Act::SuspendResume, true),
                );
            }
            Kind::Node => {
                if !ro {
                    aa.insert(rune('c'), Action::danger("Cordon", Act::Cordon(true)));
                    aa.insert(rune('u'), Action::danger("Uncordon", Act::Cordon(false)));
                    aa.insert(rune('r'), Action::danger("Drain", Act::Drain));
                    if self.cc.feature_gates.node_shell {
                        aa.insert(rune('s'), Action::new("Shell", Act::NodeShell, true));
                    }
                }
                aa.insert(rune('y'), Action::new("YAML", Act::Yaml, true));
            }
            Kind::Namespace => {
                aa.insert(rune('u'), Action::new("Use", Act::UseNamespace, true));
            }
            Kind::Context => {
                del(aa, &[rune('A'), KEY_CTRL_SPACE, KEY_SPACE]);
            }
            Kind::Service => {
                logs(aa);
                owner(aa);
                pf(aa);
                aa.insert(rune('b'), Action::new("Bench Run/Stop", Act::Bench, true));
            }
            Kind::Secret => {
                owner(aa);
                aa.insert(rune('x'), Action::new("Decode", Act::Decode, true));
                used_by(aa);
            }
            Kind::ConfigMap | Kind::Pvc => {
                owner(aa);
                used_by(aa);
            }
            Kind::Crd => owner(aa),
            Kind::ServiceAccount => {
                owner(aa);
                used_by(aa);
                aa.insert(KEY_ENTER, Action::new("Rules", Act::Rules, true));
            }
            Kind::PriorityClass => used_by(aa),
            Kind::Event => {
                del(aa, &[ctrl('d'), rune('e'), rune('a')]);
                aa.insert(
                    ctrl('z'),
                    Action::new("Toggle Faults", Act::EventFaults, false),
                );
            }
            Kind::RbacSource | Kind::ScreenDump | Kind::Benchmark => {}
            Kind::Rules | Kind::Policy => del(aa, &[rune('A'), KEY_CTRL_SPACE, KEY_SPACE]),
            Kind::User => {
                del(
                    aa,
                    &[
                        rune('A'),
                        rune('P'),
                        KEY_CTRL_SPACE,
                        KEY_SPACE,
                        ctrl('d'),
                        rune('e'),
                    ],
                );
                aa.insert(KEY_ENTER, Action::new("Rules", Act::Rules, true));
            }
            Kind::Group => {
                del(aa, &[rune('A'), rune('P'), KEY_CTRL_SPACE, KEY_SPACE]);
                aa.insert(KEY_ENTER, Action::new("Rules", Act::Rules, true));
            }
            Kind::Alias => {
                del(
                    aa,
                    &[
                        rune('A'),
                        rune('N'),
                        rune('S'),
                        ctrl('s'),
                        KEY_CTRL_SPACE,
                        KEY_SPACE,
                    ],
                );
                del(aa, &[ctrl('w'), ctrl('l')]);
                aa.insert(KEY_ENTER, Action::new("Goto", Act::Enter, true));
                aa.insert(
                    rune('R'),
                    Action::new(
                        "Sort Resource",
                        Act::SortCol("RESOURCE".into(), true),
                        false,
                    ),
                );
                aa.insert(
                    rune('C'),
                    Action::new("Sort Command", Act::SortCol("COMMAND".into(), true), false),
                );
                aa.insert(
                    rune('A'),
                    Action::new(
                        "Sort ApiGroup",
                        Act::SortCol("API-GROUP".into(), true),
                        false,
                    ),
                );
            }
            Kind::PortForward => {
                del(aa, &[rune('S')]);
                aa.insert(KEY_ENTER, Action::new("View Benchmarks", Act::Enter, true));
                aa.insert(
                    rune('b'),
                    Action::new("Benchmark Run/Stop", Act::Bench, true),
                );
                aa.insert(ctrl('d'), Action::new("Delete", Act::PfDelete, true));
                aa.insert(
                    rune('P'),
                    Action::new("Sort Ports", Act::SortCol("PORTS".into(), true), false),
                );
                aa.insert(
                    rune('U'),
                    Action::new("Sort URL", Act::SortCol("URL".into(), true), false),
                );
            }
            Kind::Dir => {
                del(aa, &[rune('A'), ctrl('s'), KEY_CTRL_SPACE, KEY_SPACE]);
                del(aa, &[ctrl('w'), ctrl('l'), ctrl('d'), ctrl('z')]);
                if !ro {
                    aa.insert(rune('a'), Action::danger("Apply", Act::DirApply));
                    aa.insert(rune('d'), Action::danger("Delete", Act::DirDelete));
                    aa.insert(rune('e'), Action::danger("Edit", Act::DirEdit));
                }
                aa.insert(rune('y'), Action::new("YAML", Act::Yaml, true));
                aa.insert(KEY_ENTER, Action::new("Goto", Act::Enter, true));
            }
            Kind::Helm => {
                aa.insert(rune('v'), Action::new("Values", Act::HelmValues, true));
                del(aa, &[ctrl('s')]);
                aa.insert(rune('r'), Action::new("Releases", Act::HelmReleases, true));
            }
            Kind::HelmHistory => {
                aa.insert(rune('v'), Action::new("Values", Act::HelmValues, true));
                if !ro {
                    aa.insert(
                        rune('r'),
                        Action::danger("RollBackTo...", Act::HelmRollback),
                    );
                }
                del(
                    aa,
                    &[
                        rune('A'),
                        rune('N'),
                        ctrl('s'),
                        KEY_CTRL_SPACE,
                        KEY_SPACE,
                        ctrl('d'),
                    ],
                );
                aa.insert(
                    rune('N'),
                    Action::new(
                        "Sort Revision",
                        Act::SortCol("REVISION".into(), true),
                        false,
                    ),
                );
                aa.insert(
                    rune('A'),
                    Action::new("Sort Age", Act::SortCol("AGE".into(), true), false),
                );
            }
            Kind::Workload => {
                if !ro {
                    aa.insert(rune('e'), Action::danger("Edit", Act::Edit));
                    aa.insert(ctrl('d'), Action::danger("Delete", Act::Delete));
                }
                aa.insert(
                    rune('K'),
                    Action::new("Sort Kind", Act::SortCol("KIND".into(), true), false),
                );
                aa.insert(
                    rune('R'),
                    Action::new("Sort Ready", Act::SortCol("READY".into(), true), false),
                );
                aa.insert(
                    rune('A'),
                    Action::new("Sort Age", Act::SortCol("AGE".into(), true), false),
                );
                aa.insert(rune('y'), Action::new("YAML", Act::Yaml, true));
                aa.insert(rune('d'), Action::new("Describe", Act::Describe, true));
            }
            Kind::Reference => {
                del(aa, &[rune('A'), ctrl('s'), KEY_CTRL_SPACE, KEY_SPACE]);
                del(aa, &[ctrl('w'), ctrl('l'), ctrl('z')]);
                aa.insert(KEY_ENTER, Action::new("Goto", Act::Enter, true));
                aa.insert(
                    rune('V'),
                    Action::new("Sort GVR", Act::SortCol("GVR".into(), true), false),
                );
            }
        }
    }

    /// tview Table navigation for keys without an action.
    fn navigate(&mut self, k: Key) {
        let page = self.table.page.max(1) as isize;
        match k {
            KEY_UP => self.table.move_cursor(-1),
            KEY_DOWN => self.table.move_cursor(1),
            KEY_HOME => self.table.select_first(),
            KEY_END => self.table.select_last(),
            KEY_PGDN => self.table.move_cursor(page),
            KEY_PGUP => self.table.move_cursor(-page),
            KEY_LEFT => self.table.col_offset = self.table.col_offset.saturating_sub(1),
            KEY_RIGHT => self.table.col_offset += 1,
            _ if k == rune('j') => self.table.move_cursor(1),
            _ if k == rune('k') => self.table.move_cursor(-1),
            _ if k == rune('g') => self.table.select_first(),
            _ if k == rune('G') => self.table.select_last(),
            _ if k == rune('h') => self.table.col_offset = self.table.col_offset.saturating_sub(1),
            _ if k == rune('l') => self.table.col_offset += 1,
            _ if k == ctrl('f') => self.table.move_cursor(page),
            _ if k == ctrl('b') => self.table.move_cursor(-page),
            _ => {}
        }
    }

    #[cfg(test)]
    pub(crate) fn table(&self) -> &TableState {
        &self.table
    }
    #[cfg(test)]
    pub(crate) fn namespace(&self) -> &str {
        &self.ns
    }
    #[cfg(test)]
    pub(crate) fn actions(&self) -> &Actions {
        &self.actions
    }
    #[cfg(test)]
    pub(crate) fn path(&self) -> &str {
        &self.path
    }
    #[cfg(test)]
    pub(crate) fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }
    #[cfg(test)]
    pub(crate) fn gvr(&self) -> &Gvr {
        &self.gvr
    }
    #[cfg(test)]
    pub(crate) fn instance(&self) -> &str {
        &self.instance
    }
    #[cfg(test)]
    pub(crate) fn field(&self) -> Option<&str> {
        self.field.as_deref()
    }
}

impl View for Browser {
    fn owns_frame(&self) -> bool {
        true
    }
    fn id(&self) -> ViewId {
        self.id
    }

    fn name(&self) -> String {
        self.meta.kind.clone()
    }

    fn title(&self) -> String {
        // k9s styles the title on the first data update; until then (or
        // when the watch failed) the frame has none.
        if !self.table.loaded {
            return String::new();
        }
        let (r, ns, n) = self.title_parts();
        let buff = table::title_filter(&self.filter, self.label.as_deref());
        table::plain_title(&r, &ns, n, &buff)
    }

    fn hints(&self) -> Vec<Hint> {
        self.hint_list()
    }

    fn start(&mut self, ctx: &mut Ctx) -> Vec<ViewCmd> {
        self.active = true;
        self.cc = (self.deps.context_config)(ctx);
        let mut out = Vec::new();
        if self.meta.namespaced && !is_cluster_scoped(&self.ns) && self.ns != table::NOT_NAMESPACED
        {
            let want = print_namespace(&self.ns);
            if print_namespace(&ctx.namespace) != want {
                out.push(ViewCmd::SetNamespace(want));
            }
        }
        self.refresh_actions(ctx);
        self.spawn_load(ctx, true);
        out
    }

    fn stop(&mut self, _ctx: &mut Ctx) {
        self.active = false;
        if let Some(h) = self.loading.take() {
            h.abort();
        }
    }

    fn handle_key(&mut self, key: KeyEvent, ctx: &mut Ctx) -> Vec<ViewCmd> {
        self.sync(ctx);
        if key.modifiers.contains(KeyModifiers::SHIFT) {
            match key.code {
                crossterm::event::KeyCode::Left => {
                    self.table
                        .move_selected_column(-1, &self.ns, ctx.has_metrics);
                    return Vec::new();
                }
                crossterm::event::KeyCode::Right => {
                    self.table
                        .move_selected_column(1, &self.ns, ctx.has_metrics);
                    return Vec::new();
                }
                _ => {}
            }
        }
        let Some(k) = key_of(&key) else {
            return Vec::new();
        };
        if k == KEY_UP || k == KEY_DOWN {
            self.navigate(k);
            return Vec::new();
        }
        if let Some(a) = self.actions.get(&k).cloned() {
            let out = self.run(a.act, ctx);
            if self.dirty {
                self.rebuild(ctx);
            }
            self.refresh_actions(ctx);
            return out;
        }
        self.navigate(k);
        Vec::new()
    }

    fn on_event(&mut self, ev: &AppEvent, ctx: &mut Ctx) -> Vec<ViewCmd> {
        match ev {
            AppEvent::Tick => {
                if self.active {
                    self.spawn_load(ctx, false);
                }
                Vec::new()
            }
            AppEvent::Data { view, data } if *view == self.id => self.apply(data.clone(), ctx),
            AppEvent::Error { view, msg } if *view == self.id => {
                vec![ViewCmd::Flash(FlashLevel::Err, msg.clone())]
            }
            AppEvent::Info { view, msg } if *view == self.id => {
                vec![ViewCmd::Flash(FlashLevel::Info, msg.clone())]
            }
            AppEvent::Custom { view, payload } if *view == self.id => {
                // Payloads are single-use; the shell hands us a shared ref,
                // so we rebuild owned messages via downcast_ref + take.
                match payload.downcast_ref::<std::sync::Mutex<Option<Msg>>>() {
                    Some(m) => match m.lock().ok().and_then(|mut g| g.take()) {
                        Some(msg) => self.on_msg(msg, ctx),
                        None => Vec::new(),
                    },
                    None => Vec::new(),
                }
            }
            _ => Vec::new(),
        }
    }

    fn on_filter(&mut self, text: &str, ctx: &mut Ctx) -> Vec<ViewCmd> {
        let changed = self.set_filter_text(text);
        self.table.select_first();
        self.rebuild(ctx);
        if changed {
            self.spawn_load(ctx, true);
        }
        Vec::new()
    }

    fn draw(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &Ctx) {
        if self.dirty {
            self.rebuild(ctx);
        }
        let (r, ns, n) = self.title_parts();
        let title = if self.table.loaded {
            let buff = table::title_filter(&self.filter, self.label.as_deref());
            table::styled_title(&r, &ns, n, &buff, &TitleColors::from_styles(&ctx.styles))
        } else {
            String::new()
        };
        let header = self.table.data.header.clone();
        let palette = ctx.styles.palette;
        let renderer = self.renderer.clone();
        let model_ns = self.ns.clone();
        let colorer = move |re: &RowEvent| renderer.color_for(&model_ns, &header, re, &palette);
        let input = table::DrawInput {
            styles: &ctx.styles,
            ns: &self.ns,
            has_metrics: ctx.has_metrics,
            title,
            colorer: &colorer,
            now: chrono::Utc::now(),
        };
        table::draw_table(frame, area, &mut self.table, &input);
    }

    fn filter_text(&self) -> String {
        self.filter.clone()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// Wrap a message so `on_event` (which only sees `&AppEvent`) can take it.
pub(crate) fn payload(msg: Msg) -> Box<dyn Any + Send> {
    Box::new(std::sync::Mutex::new(Some(msg)))
}
