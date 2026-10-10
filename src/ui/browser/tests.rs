//! Browser tests: FakeAccessor data, a test renderer for pods/configmaps,
//! the real renderers for everything else, stub detail views.

use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use pretty_assertions::assert_eq;
use ratatui::backend::TestBackend;
use ratatui::style::{Color, Modifier};
use ratatui::Terminal;
use serde_json::json;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};

use super::*;
use crate::config::{Config, Styles};
use crate::k8s::fake::{obj, FakeAccessor};
use crate::k8s::{DeleteOptions, PortForwardInfo, PortForwardSpec};
use crate::model::{Align, HeaderColumn};
use crate::render::RenderInput;
use crate::ui::Dialog;
use kube::api::DynamicObject;

// ---------------------------------------------------------------- fixtures

/// NAME, STATUS, IP (wide), AGE (+ NAMESPACE for all-namespaces).
struct TestRenderer;

impl Renderer for TestRenderer {
    fn header(&self, ns: &str) -> Header {
        let mut cols = Vec::new();
        if is_all_namespaces(ns) {
            cols.push(HeaderColumn::new("NAMESPACE"));
        }
        cols.extend([
            HeaderColumn::new("NAME"),
            HeaderColumn::new("READY"),
            HeaderColumn::new("STATUS"),
            HeaderColumn::new("RESTARTS").align(Align::Right),
            HeaderColumn::new("IP").wide(),
            HeaderColumn::new("VALID").wide(),
            HeaderColumn::new("AGE").time(),
        ]);
        Header::new(cols)
    }
    fn render(&self, input: &RenderInput<'_>, ns: &str, row: &mut Row) -> anyhow::Result<()> {
        let o = input.obj;
        let name = o.metadata.name.clone().unwrap_or_default();
        let ons = o.metadata.namespace.clone().unwrap_or_default();
        row.id = crate::k8s::fqn(&ons, &name);
        let status = o
            .data
            .pointer("/status/phase")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let valid = if status == "Failed" {
            "false".to_string()
        } else {
            String::new()
        };
        let mut f = Vec::new();
        if is_all_namespaces(ns) {
            f.push(ons);
        }
        f.extend([
            name,
            "1/1".into(),
            status,
            "0".into(),
            "10.0.0.1".into(),
            valid,
            "5m".into(),
        ]);
        row.fields = f;
        Ok(())
    }
}

fn make_renderer(gvr: &Gvr) -> Box<dyn Renderer> {
    match gvr.to_string().as_str() {
        "v1/pods" | "v1/configmaps" => Box::new(TestRenderer),
        _ => crate::render::renderer_for(gvr),
    }
}

struct Stub {
    id: ViewId,
    name: String,
}

impl View for Stub {
    fn id(&self) -> ViewId {
        self.id
    }
    fn name(&self) -> String {
        self.name.clone()
    }
    fn hints(&self) -> Vec<Hint> {
        Vec::new()
    }
    fn handle_key(&mut self, _key: KeyEvent, _ctx: &mut Ctx) -> Vec<ViewCmd> {
        Vec::new()
    }
    fn draw(&mut self, _f: &mut Frame<'_>, _a: Rect, _c: &Ctx) {}
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

fn stub(ctx: &mut Ctx, name: String) -> Box<dyn View> {
    Box::new(Stub {
        id: ctx.new_view_id(),
        name,
    })
}

fn test_cc(_: &Ctx) -> ContextConfig {
    let mut cc = ContextConfig::default();
    cc.namespace.favorites = vec!["default".into(), "all".into(), "kube-system".into()];
    cc.feature_gates.node_shell = true;
    cc
}

fn test_dumps(_: &Ctx) -> PathBuf {
    std::env::temp_dir().join(format!("k9rs-browser-dumps-{}", std::process::id()))
}

fn test_parse_key(s: &str) -> Option<KeyEvent> {
    if let Some(c) = s.strip_prefix("Shift-") {
        return Some(KeyEvent::new(
            KeyCode::Char(c.chars().next()?.to_ascii_uppercase()),
            KeyModifiers::SHIFT,
        ));
    }
    if let Some(c) = s.strip_prefix("Ctrl-") {
        return Some(KeyEvent::new(
            KeyCode::Char(c.chars().next()?.to_ascii_lowercase()),
            KeyModifiers::CONTROL,
        ));
    }
    let c = s.chars().next()?;
    Some(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
}

static RUNS: Mutex<Vec<Vec<String>>> = Mutex::new(Vec::new());

fn test_runner() -> Runner {
    Arc::new(|argv: Vec<String>, _env| {
        RUNS.lock().unwrap().push(argv.clone());
        Box::pin(async move {
            let line = argv.join(" ");
            if line.starts_with("helm list") {
                return Ok(r#"[{"name":"web","namespace":"default","revision":"2","updated":"2024-01-02T15:04:05Z","status":"deployed","chart":"web-1.0","app_version":"1.0"}]"#.to_string());
            }
            if line.starts_with("helm get values") {
                return Ok(if line.contains("--all") {
                    r#"{"greeting":"hola","replicas":1}"#
                } else {
                    r#"{"greeting":"hola"}"#
                }
                .to_string());
            }
            if line.starts_with("helm history") {
                return Ok(r#"[{"revision":1,"updated":"2024-01-01T00:00:00Z","status":"superseded","chart":"web-0.9","app_version":"0.9","description":"Install complete"},{"revision":2,"updated":"2024-01-02T00:00:00Z","status":"deployed","chart":"web-1.0","app_version":"1.0","description":"Upgrade complete"}]"#.to_string());
            }
            Ok(format!("ran {line}"))
        }) as BoxFuture<'static, anyhow::Result<String>>
    })
}

fn runs_matching(p: &str) -> Vec<String> {
    RUNS.lock()
        .unwrap()
        .iter()
        .map(|a| a.join(" "))
        .filter(|l| l.contains(p))
        .collect()
}

fn deps() -> Deps {
    Deps {
        make_renderer,
        parse_filter: FilterSpec::parse,
        apply_filter: crate::model::apply_filter,
        sort_rows: crate::model::sort_rows,
        server_table: crate::render::core::render_server_table_with,
        context_config: test_cc,
        dumps_dir: test_dumps,
        benchmarks_dir: |_| std::env::temp_dir().join("k9rs-browser-bench-none"),
        bench_config: |_| std::env::temp_dir().join("k9rs-browser-bench-none.yaml"),
        clipboard: |_| Ok(()),
        parse_key: test_parse_key,
        describe: |ctx, g, p| stub(ctx, format!("describe {g} {p}")),
        yaml: |ctx, g, p| stub(ctx, format!("yaml {g} {p}")),
        logs: |ctx, o| {
            stub(
                ctx,
                format!(
                    "logs {}|{}|{:?}|{}|{}",
                    o.namespace, o.path, o.container, o.all_containers, o.previous
                ),
            )
        },
        text: |ctx, n, t, b| stub(ctx, format!("text {n}|{t}|{b}")),
        values: |ctx, s, u, a| stub(ctx, format!("values {s}|{u}|{a}")),
        release: |ctx, n, t, b| stub(ctx, format!("release {n}|{t}|{b}")),
        help: |ctx, h| stub(ctx, format!("help {}", h.len())),
        bench: |_s, _c, _x, n| Box::pin(async move { Ok(PathBuf::from(n)) }),
        run: test_runner(),
    }
}

fn pod(ns: &str, name: &str, phase: &str) -> DynamicObject {
    obj(json!({"apiVersion": "v1", "kind": "Pod",
        "metadata": {"name": name, "namespace": ns, "labels": {"app": "web"}, "uid": format!("uid-{name}")},
        "spec": {"nodeName": "node1", "containers": [{"name": "web", "image": "nginx:1", "ports": [{"containerPort": 80, "name": "http"}]}]},
        "status": {"phase": phase, "containerStatuses": [{"name": "web", "ready": true, "state": {"running": {}}}]}}))
}

fn fake() -> Arc<FakeAccessor> {
    let f = FakeAccessor::new();
    f.add("v1/pods", pod("default", "a", "Running"));
    f.add("v1/pods", pod("default", "b", "Failed"));
    f.add("v1/pods", pod("kube-system", "c", "Running"));
    f.add(
        "v1/namespaces",
        obj(json!({"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": "default"}})),
    );
    f.add(
        "v1/namespaces",
        obj(json!({"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": "kube-system"}})),
    );
    f.add("apps/v1/deployments", obj(json!({"apiVersion": "apps/v1", "kind": "Deployment",
        "metadata": {"name": "web", "namespace": "default", "creationTimestamp": "2024-01-01T00:00:00Z"},
        "spec": {"replicas": 3, "selector": {"matchLabels": {"app": "web"}},
                 "template": {"metadata": {"labels": {"app": "web"}}, "spec": {"containers": [{"name": "web", "image": "nginx:1"}]}}},
        "status": {"readyReplicas": 3, "replicas": 3, "availableReplicas": 3, "updatedReplicas": 3}})));
    f.add("v1/nodes", obj(json!({"apiVersion": "v1", "kind": "Node", "metadata": {"name": "node1", "labels": {"kubernetes.io/os": "linux"}}})));
    f.add("v1/configmaps", obj(json!({"apiVersion": "v1", "kind": "ConfigMap", "metadata": {"name": "cfg", "namespace": "default"}})));
    f.add("v1/secrets", obj(json!({"apiVersion": "v1", "kind": "Secret", "metadata": {"name": "sec", "namespace": "default"}, "data": {"pw": "aHVudGVyMg=="}, "type": "Opaque"})));
    Arc::new(f)
}

fn context(acc: Arc<FakeAccessor>) -> (Ctx, UnboundedReceiver<AppEvent>) {
    let (tx, rx) = unbounded_channel();
    let mut ctx = Ctx::new(
        acc,
        Arc::new(RwLock::new(Config::default())),
        Styles::default(),
        "default".into(),
        tx,
    );
    ctx.resources = crate::k8s::fake::default_resources();
    ctx.cluster_info.cluster = "fake-cluster".into();
    ctx.cluster_info.context = "fake".into();
    (ctx, rx)
}

fn open(ctx: &mut Ctx, gvr: &str) -> Browser {
    open_opts(ctx, gvr, Opts::default())
}

fn open_opts(ctx: &mut Ctx, gvr: &str, opts: Opts) -> Browser {
    let g = Gvr::parse(gvr);
    let kind = kind_for(&g);
    let meta = meta_for(kind, &g, &ctx.resources).unwrap_or_else(|| fallback_meta(&g));
    Browser::new(ctx, g, kind, meta, deps(), opts)
}

/// A browser whose model already delivered data, so refreshActions ran.
fn ready(mut b: Browser, ctx: &Ctx) -> Browser {
    b.actions_ready = true;
    b.refresh_actions(ctx);
    b
}

fn key(c: char) -> KeyEvent {
    if c.is_ascii_uppercase() {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::SHIFT)
    } else {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }
}

fn ctrl_key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn code(c: KeyCode) -> KeyEvent {
    KeyEvent::new(c, KeyModifiers::NONE)
}

/// Feed events to `b` until `pred` matches one; return that event's cmds.
async fn until(
    b: &mut Browser,
    ctx: &mut Ctx,
    rx: &mut UnboundedReceiver<AppEvent>,
    pred: impl Fn(&AppEvent) -> bool,
) -> Vec<ViewCmd> {
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for event")
            .expect("channel closed");
        let mine = match &ev {
            AppEvent::Data { view, .. }
            | AppEvent::Error { view, .. }
            | AppEvent::Info { view, .. }
            | AppEvent::Custom { view, .. } => *view == b.id(),
            _ => false,
        };
        let hit = mine && pred(&ev);
        let cmds = b.on_event(&ev, ctx);
        if hit {
            return cmds;
        }
    }
}

fn is_data(ev: &AppEvent) -> bool {
    matches!(ev, AppEvent::Data { .. })
}

fn is_msg(ev: &AppEvent, f: impl Fn(&Msg) -> bool) -> bool {
    match ev {
        AppEvent::Custom { payload, .. } => payload
            .downcast_ref::<std::sync::Mutex<Option<Msg>>>()
            .and_then(|m| m.lock().ok().map(|g| g.as_ref().map(&f).unwrap_or(false)))
            .unwrap_or(false),
        _ => false,
    }
}

fn is_custom(ev: &AppEvent) -> bool {
    matches!(ev, AppEvent::Custom { .. })
}

async fn loaded(
    b: &mut Browser,
    ctx: &mut Ctx,
    rx: &mut UnboundedReceiver<AppEvent>,
) -> Vec<ViewCmd> {
    let mut out = b.start(ctx);
    out.extend(until(b, ctx, rx, is_data).await);
    out
}

fn ids(b: &Browser) -> Vec<String> {
    b.table().rows.iter().map(|r| r.row.id.clone()).collect()
}

fn flashes(cmds: &[ViewCmd]) -> Vec<(FlashLevel, String)> {
    cmds.iter()
        .filter_map(|c| match c {
            ViewCmd::Flash(l, m) => Some((*l, m.clone())),
            _ => None,
        })
        .collect()
}

fn pushed(cmds: Vec<ViewCmd>) -> Box<dyn View> {
    cmds.into_iter()
        .find_map(|c| match c {
            ViewCmd::Push(v) => Some(v),
            _ => None,
        })
        .expect("expected a pushed view")
}

fn pushed_browser(cmds: Vec<ViewCmd>) -> Box<dyn View> {
    let v = pushed(cmds);
    assert!(
        v.as_any().downcast_ref::<Browser>().is_some(),
        "pushed {} is not a browser",
        v.name()
    );
    v
}

fn as_browser(v: &dyn View) -> &Browser {
    v.as_any().downcast_ref::<Browser>().expect("browser")
}

fn visible_hints(b: &Browser) -> Vec<(String, String)> {
    b.hints()
        .into_iter()
        .filter(|h| h.visible)
        .map(|h| (h.mnemonic, h.description))
        .collect()
}

fn has_hint(b: &Browser, m: &str, d: &str) -> bool {
    b.hints()
        .iter()
        .any(|h| h.mnemonic == m && h.description == d)
}

fn render(b: &mut Browser, ctx: &Ctx, w: u16, h: u16) -> ratatui::buffer::Buffer {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| b.draw(f, f.area(), ctx)).unwrap();
    term.backend().buffer().clone()
}

fn line(buf: &ratatui::buffer::Buffer, y: u16) -> String {
    (0..buf.area.width)
        .map(|x| buf[(x, y)].symbol().to_string())
        .collect()
}

// ---------------------------------------------------------------- tests

#[tokio::test]
async fn loads_rows_title_and_first_flash() {
    let (mut ctx, mut rx) = context(fake());
    let mut b = open(&mut ctx, "v1/pods");
    let cmds = loaded(&mut b, &mut ctx, &mut rx).await;
    assert_eq!(ids(&b), vec!["default/a", "default/b"]);
    assert_eq!(b.title(), " pods(default)[2] ");
    assert_eq!(b.name(), "Pod");
    assert_eq!(
        flashes(&cmds),
        vec![(
            FlashLevel::Info,
            "Viewing v1/pods in namespace default".to_string()
        )]
    );
    assert!(b.table().rows.iter().all(|r| r.kind == ResEvent::Add));
    // A refresh with the same data makes rows unchanged and flashes nothing.
    let cmds = b.on_event(&AppEvent::Tick, &mut ctx);
    assert!(cmds.is_empty());
    let cmds = until(&mut b, &mut ctx, &mut rx, is_data).await;
    assert!(flashes(&cmds).is_empty());
    assert!(b.table().rows.iter().all(|r| r.kind == ResEvent::Unchanged));
}

#[tokio::test]
async fn all_namespaces_sorts_by_namespace_and_adds_sort_key() {
    let (mut ctx, mut rx) = context(fake());
    let mut b = open_opts(
        &mut ctx,
        "v1/pods",
        Opts {
            ns: Some("all".into()),
            ..Default::default()
        },
    );
    let cmds = loaded(&mut b, &mut ctx, &mut rx).await;
    assert!(cmds
        .iter()
        .any(|c| matches!(c, ViewCmd::SetNamespace(n) if n == "all")));
    assert_eq!(b.namespace(), "");
    assert_eq!(b.title(), " pods(all)[3] ");
    assert_eq!(b.table().sort.name, "NAMESPACE");
    assert!(has_hint(&b, "shift-p", "Sort Namespace"));
    assert_eq!(ids(&b), vec!["default/a", "default/b", "kube-system/c"]);
}

/// Model.Watch fails once: a tick landing before the failure is handled
/// must not run the access check again.
#[tokio::test]
async fn watch_denied_checks_once() {
    let f = fake();
    f.denied
        .lock()
        .unwrap()
        .push(("v1/persistentvolumes".into(), "".into()));
    let (mut ctx, mut rx) = context(f);
    let mut b = open(&mut ctx, "v1/persistentvolumes");
    b.start(&mut ctx);
    tokio::time::sleep(Duration::from_millis(100)).await;
    b.on_event(&AppEvent::Tick, &mut ctx);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut n = 0;
    while let Ok(ev) = rx.try_recv() {
        n += matches!(ev, AppEvent::Custom { view, .. } if view == b.id()) as usize;
    }
    assert_eq!(n, 1);
    // No data came, so refreshActions never ran: Help and the kind keys only.
    assert!(has_hint(&b, "?", "Help") && has_hint(&b, "shift-j", "Jump Owner"));
    assert!(!has_hint(&b, "d", "Describe") && !has_hint(&b, "y", "YAML"));
}

/// browser.go switchNamespaceCmd: denied namespaces flash and keep the view;
/// allowed ones flash once (the table already has columns, so no second flash).
#[tokio::test(flavor = "multi_thread")]
async fn switch_ns_checks_access_and_flashes_once() {
    let f = fake();
    f.denied
        .lock()
        .unwrap()
        .push(("v1/pods".into(), "kube-system".into()));
    let (mut ctx, mut rx) = context(f);
    let mut b = open(&mut ctx, "v1/pods");
    loaded(&mut b, &mut ctx, &mut rx).await;
    let cmds = b.run(Act::SwitchNs("kube-system".into()), &mut ctx);
    assert_eq!(
        flashes(&cmds),
        vec![(
            FlashLevel::Err,
            "access denied for user on: kube-system/v1/pods".to_string()
        )]
    );
    assert_eq!(b.namespace(), "default");
    let cmds = b.run(Act::SwitchNs(NAMESPACE_ALL.into()), &mut ctx);
    assert_eq!(
        flashes(&cmds),
        vec![(
            FlashLevel::Info,
            "Viewing v1/pods in namespace `all`...".to_string()
        )]
    );
    let cmds = until(&mut b, &mut ctx, &mut rx, is_data).await;
    assert!(flashes(&cmds).is_empty());
}

/// dao.Policy.List lists bindings through watch.Factory.List, which runs
/// CanForResource first.
#[tokio::test]
async fn policy_list_checks_access() {
    let f = fake();
    f.denied.lock().unwrap().push((
        "rbac.authorization.k8s.io/v1/clusterrolebindings".into(),
        "-".into(),
    ));
    let (mut ctx, mut rx) = context(f);
    let mut b = open_opts(
        &mut ctx,
        "policy",
        Opts {
            subject: Some(("ServiceAccount".into(), "default/fred".into())),
            ..Default::default()
        },
    );
    b.start(&mut ctx);
    let cmds = until(&mut b, &mut ctx, &mut rx, is_custom).await;
    assert_eq!(
        flashes(&cmds),
        vec![(
            FlashLevel::Err,
            "Watcher failed for policy -- [list] access denied on resource \"-\":\"rbac.authorization.k8s.io/v1/clusterrolebindings\""
                .to_string()
        )]
    );
}

/// dao.Rbac.List gets the role through watch.Factory.Get (CanForInstance).
#[tokio::test]
async fn rbac_get_checks_access() {
    let f = fake();
    f.denied_get.lock().unwrap().push((
        "rbac.authorization.k8s.io/v1/roles".into(),
        "default/viewer".into(),
    ));
    let (mut ctx, mut rx) = context(f);
    let mut b = open_opts(
        &mut ctx,
        "rbac",
        Opts {
            source: Some((
                Gvr::parse("rbac.authorization.k8s.io/v1/roles"),
                "default/viewer".into(),
            )),
            ..Default::default()
        },
    );
    b.start(&mut ctx);
    let cmds = until(&mut b, &mut ctx, &mut rx, is_custom).await;
    assert_eq!(
        flashes(&cmds),
        vec![(
            FlashLevel::Err,
            "Watcher failed for rbac -- [get] access denied on resource \"default\":\"rbac.authorization.k8s.io/v1/roles\""
                .to_string()
        )]
    );
}

/// dao.Benchmark.List: a missing dir fails the watch with Go's error text.
#[tokio::test]
async fn benchmarks_missing_dir_fails_watch() {
    let (mut ctx, mut rx) = context(fake());
    let mut b = open(&mut ctx, "benchmarks");
    b.dir = PathBuf::from("/nonexistent/k9rs-bench");
    b.start(&mut ctx);
    let cmds = until(&mut b, &mut ctx, &mut rx, is_custom).await;
    assert_eq!(
        flashes(&cmds),
        vec![(
            FlashLevel::Err,
            "Watcher failed for benchmarks -- open /nonexistent/k9rs-bench: no such file or directory"
                .to_string()
        )]
    );
    b.on_event(&AppEvent::Tick, &mut ctx);
    assert!(b.loading.as_ref().is_none_or(|h| h.is_finished()));
}

#[tokio::test]
async fn empty_result_warns() {
    let (mut ctx, mut rx) = context(fake());
    let mut b = open(&mut ctx, "v1/endpoints");
    // browser.go TableNoData drops the first empty update: no flash, no header.
    let cmds = loaded(&mut b, &mut ctx, &mut rx).await;
    assert!(flashes(&cmds).is_empty());
    assert_eq!(b.title(), "");
    assert!(!has_hint(&b, "d", "Describe"));
    // The updater refreshes initRefreshRate (300ms) after Watch, no tick needed.
    let t = std::time::Instant::now();
    let cmds = until(&mut b, &mut ctx, &mut rx, is_data).await;
    assert!(t.elapsed() >= INIT_REFRESH_RATE - Duration::from_millis(50));
    assert!(has_hint(&b, "d", "Describe"));
    assert_eq!(
        flashes(&cmds),
        vec![(
            FlashLevel::Warn,
            "No resources found for v1/endpoints in \"default\" namespace".to_string()
        )]
    );
}

#[tokio::test]
async fn filter_and_reset() {
    let (mut ctx, mut rx) = context(fake());
    let mut b = open(&mut ctx, "v1/pods");
    loaded(&mut b, &mut ctx, &mut rx).await;
    assert!(matches!(
        b.handle_key(key('/'), &mut ctx)[..],
        [ViewCmd::OpenFilterPrompt]
    ));
    b.on_filter("b", &mut ctx);
    assert_eq!(ids(&b), vec!["default/b"]);
    assert_eq!(b.title(), " pods(default)[1] </b> ");
    assert_eq!(b.filter_text(), "b");
    b.on_filter("!b", &mut ctx);
    assert_eq!(ids(&b), vec!["default/a"]);
    let cmds = b.handle_key(code(KeyCode::Esc), &mut ctx);
    assert!(matches!(cmds[..], [ViewCmd::Pop]));
    assert_eq!(b.filter_text(), "");
    assert_eq!(ids(&b), vec!["default/a", "default/b"]);
}

#[tokio::test]
async fn label_filter_goes_server_side() {
    let (mut ctx, mut rx) = context(fake());
    let mut b = open(&mut ctx, "v1/pods");
    loaded(&mut b, &mut ctx, &mut rx).await;
    b.on_filter("-l app=nope", &mut ctx);
    assert_eq!(b.label(), Some("app=nope"));
    // The first empty update is dropped (browser.go firstView); the next one lands.
    until(&mut b, &mut ctx, &mut rx, is_data).await;
    assert!(!ids(&b).is_empty());
    b.on_event(&AppEvent::Tick, &mut ctx);
    until(&mut b, &mut ctx, &mut rx, is_data).await;
    assert!(ids(&b).is_empty());
}

#[tokio::test]
async fn sorting_keys_and_columns() {
    let (mut ctx, mut rx) = context(fake());
    let mut b = open(&mut ctx, "v1/pods");
    loaded(&mut b, &mut ctx, &mut rx).await;
    assert_eq!(
        b.table().sort,
        SortColumn {
            name: "NAME".into(),
            asc: true
        }
    );
    b.handle_key(key('N'), &mut ctx);
    assert_eq!(
        b.table().sort,
        SortColumn {
            name: "NAME".into(),
            asc: false
        }
    );
    assert_eq!(ids(&b), vec!["default/b", "default/a"]);
    b.handle_key(key('S'), &mut ctx);
    assert_eq!(
        b.table().sort,
        SortColumn {
            name: "STATUS".into(),
            asc: true
        }
    );
    assert_eq!(ids(&b), vec!["default/b", "default/a"]);
    // shift-right then shift-o sorts the selected column.
    let sel = b.table().selected_col;
    b.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT), &mut ctx);
    assert_eq!(b.table().selected_col, sel + 1);
    b.handle_key(key('O'), &mut ctx);
    assert_eq!(b.table().sort.name, "RESTARTS");
}

#[tokio::test]
async fn marks_and_navigation() {
    let (mut ctx, mut rx) = context(fake());
    let mut b = open_opts(
        &mut ctx,
        "v1/pods",
        Opts {
            ns: Some("all".into()),
            ..Default::default()
        },
    );
    loaded(&mut b, &mut ctx, &mut rx).await;
    b.handle_key(key(' '), &mut ctx);
    assert_eq!(b.table().selected_items(), vec!["default/a"]);
    b.handle_key(key('G'), &mut ctx);
    assert_eq!(b.table().cursor, 2);
    b.handle_key(ctrl_key(' '), &mut ctx);
    assert_eq!(
        b.table().selected_items(),
        vec!["default/a", "default/b", "kube-system/c"]
    );
    b.handle_key(ctrl_key('\\'), &mut ctx);
    assert_eq!(b.table().selected_items(), vec!["kube-system/c"]);
    b.handle_key(key('g'), &mut ctx);
    assert_eq!(b.table().cursor, 0);
    b.handle_key(key('j'), &mut ctx);
    b.handle_key(code(KeyCode::Down), &mut ctx);
    assert_eq!(b.table().cursor, 2);
    b.handle_key(key('k'), &mut ctx);
    assert_eq!(b.table().cursor, 1);
}

#[tokio::test]
async fn wide_and_faults_toggles() {
    let (mut ctx, mut rx) = context(fake());
    let mut b = open(&mut ctx, "v1/pods");
    loaded(&mut b, &mut ctx, &mut rx).await;
    let buf = render(&mut b, &ctx, 100, 6);
    assert!(!line(&buf, 1).contains("IP"));
    b.handle_key(ctrl_key('w'), &mut ctx);
    let buf = render(&mut b, &ctx, 100, 6);
    assert!(line(&buf, 1).contains("IP"), "{}", line(&buf, 1));
    b.handle_key(ctrl_key('z'), &mut ctx);
    assert_eq!(ids(&b), vec!["default/b"]);
    b.handle_key(ctrl_key('z'), &mut ctx);
    assert_eq!(ids(&b).len(), 2);
}

#[tokio::test]
async fn namespace_favorites_and_switching() {
    let (mut ctx, mut rx) = context(fake());
    let mut b = open(&mut ctx, "v1/pods");
    loaded(&mut b, &mut ctx, &mut rx).await;
    let h = visible_hints(&b);
    assert!(h.contains(&("0".into(), "all".into())));
    assert!(h.contains(&("1".into(), "default".into())));
    assert!(h.contains(&("2".into(), "kube-system".into())));
    assert!(!h.iter().any(|(m, _)| m == "3"));
    let cmds = b.handle_key(key('2'), &mut ctx);
    assert!(cmds
        .iter()
        .any(|c| matches!(c, ViewCmd::SetNamespace(n) if n == "kube-system")));
    assert_eq!(
        flashes(&cmds),
        vec![(
            FlashLevel::Info,
            "Viewing v1/pods in namespace `kube-system`...".to_string()
        )]
    );
    assert_eq!(b.namespace(), "kube-system");
    ctx.namespace = "kube-system".into();
    until(&mut b, &mut ctx, &mut rx, is_data).await;
    assert_eq!(ids(&b), vec!["kube-system/c"]);
    let cmds = b.handle_key(key('0'), &mut ctx);
    assert!(cmds
        .iter()
        .any(|c| matches!(c, ViewCmd::SetNamespace(n) if n == "all")));
    assert_eq!(b.namespace(), "");
    // warp
    until(&mut b, &mut ctx, &mut rx, is_data).await;
    b.handle_key(key('G'), &mut ctx);
    let cmds = b.handle_key(key('w'), &mut ctx);
    assert!(matches!(&cmds[..], [ViewCmd::Command(c)] if c == "v1/pods kube-system"));
}

#[tokio::test]
async fn pod_hints_match_k9s() {
    let (mut ctx, mut rx) = context(fake());
    let mut b = open(&mut ctx, "v1/pods");
    loaded(&mut b, &mut ctx, &mut rx).await;
    let got = visible_hints(&b);
    let want: Vec<(String, String)> = [
        ("ctrl-d", "Delete"),
        ("ctrl-k", "Kill"),
        ("0", "all"),
        ("1", "default"),
        ("2", "kube-system"),
        ("?", "Help"),
        ("shift-f", "Port-Forward"),
        ("shift-j", "Jump Owner"),
        ("a", "Attach"),
        ("d", "Describe"),
        ("e", "Edit"),
        ("f", "Show PortForward"),
        ("l", "Logs"),
        ("o", "Show Node"),
        ("p", "Logs Previous"),
        ("s", "Shell"),
        ("t", "Transfer"),
        ("w", "Warp To Namespace"),
        ("y", "YAML"),
        ("z", "Sanitize"),
    ]
    .iter()
    .map(|(a, b)| (a.to_string(), b.to_string()))
    .collect();
    assert_eq!(got, want);
    // shared actions are not hints; hidden ones are present but invisible
    assert!(!b.hints().iter().any(|h| h.description == "Mark"));
    assert!(b
        .hints()
        .iter()
        .any(|h| h.mnemonic == "ctrl-w" && h.description == "Toggle Wide" && !h.visible));
    assert!(b
        .hints()
        .iter()
        .any(|h| h.mnemonic == "i" && h.description == "Set Image" && !h.visible));
}

#[tokio::test]
async fn readonly_hides_mutations() {
    let (mut ctx, mut rx) = context(fake());
    ctx.readonly = true;
    let mut b = open(&mut ctx, "v1/pods");
    loaded(&mut b, &mut ctx, &mut rx).await;
    for d in [
        "Delete",
        "Edit",
        "Kill",
        "Shell",
        "Attach",
        "Transfer",
        "Sanitize",
        "Set Image",
    ] {
        assert!(
            !b.hints().iter().any(|h| h.description == d),
            "{d} should be hidden"
        );
    }
    assert!(has_hint(&b, "l", "Logs"));
    assert!(b.handle_key(ctrl_key('d'), &mut ctx).is_empty());
    let mut d = open(&mut ctx, "apps/v1/deployments");
    d.start(&mut ctx);
    assert!(!d
        .hints()
        .iter()
        .any(|h| h.description == "Scale" || h.description == "Restart"));
}

#[tokio::test]
async fn resource_hints() {
    let (mut ctx, _rx) = context(fake());
    let d = ready(open(&mut ctx, "apps/v1/deployments"), &ctx);
    for (m, desc) in [
        ("s", "Scale"),
        ("r", "Restart"),
        ("z", "ReplicaSets"),
        ("l", "Logs"),
        ("shift-f", "Port-Forward"),
    ] {
        assert!(has_hint(&d, m, desc), "deploy {m} {desc}");
    }
    let n = ready(open(&mut ctx, "v1/nodes"), &ctx);
    for (m, desc) in [
        ("c", "Cordon"),
        ("u", "Uncordon"),
        ("r", "Drain"),
        ("s", "Shell"),
        ("y", "YAML"),
    ] {
        assert!(has_hint(&n, m, desc), "node {m} {desc}");
    }
    assert!(
        !has_hint(&n, "w", "Warp To Namespace"),
        "nodes are cluster scoped"
    );
    let cj = ready(open(&mut ctx, "batch/v1/cronjobs"), &ctx);
    assert!(has_hint(&cj, "t", "Trigger") && has_hint(&cj, "s", "Suspend/Resume"));
    let rs = ready(open(&mut ctx, "apps/v1/replicasets"), &ctx);
    assert!(has_hint(&rs, "ctrl-l", "Rollback") && !has_hint(&rs, "s", "Scale"));
    let sec = ready(open(&mut ctx, "v1/secrets"), &ctx);
    assert!(has_hint(&sec, "x", "Decode") && has_hint(&sec, "u", "UsedBy"));
    let ctxs = ready(open(&mut ctx, "contexts"), &ctx);
    // context.go bindDangerousKeys: rename and delete unless read-only.
    assert!(!has_hint(&ctxs, "y", "YAML"));
    assert!(has_hint(&ctxs, "ctrl-d", "Delete") && has_hint(&ctxs, "r", "Rename"));
    assert!(!ctxs.actions().contains_key(&KEY_SPACE));
    ctx.readonly = true;
    let ro = ready(open(&mut ctx, "contexts"), &ctx);
    assert!(!has_hint(&ro, "ctrl-d", "Delete") && !has_hint(&ro, "r", "Rename"));
    ctx.readonly = false;
    let al = ready(open(&mut ctx, "aliases"), &ctx);
    assert!(has_hint(&al, "enter", "Goto"));
    assert!(!al.actions().contains_key(&ctrl('s')));
    let pf = ready(open(&mut ctx, "portforwards"), &ctx);
    assert!(
        has_hint(&pf, "enter", "View Benchmarks")
            && has_hint(&pf, "ctrl-d", "Delete")
            && has_hint(&pf, "b", "Benchmark Run/Stop")
    );
    let co = ready(
        open_opts(
            &mut ctx,
            "containers",
            Opts {
                path: "default/a".into(),
                ..Default::default()
            },
        ),
        &ctx,
    );
    assert!(has_hint(&co, "shift-f", "PortForward") && has_hint(&co, "s", "Shell"));
    assert!(!co.actions().contains_key(&KEY_SPACE));
    assert!(!has_hint(&co, "e", "Edit") && !has_hint(&co, "y", "YAML"));
    let ev = ready(open(&mut ctx, "v1/events"), &ctx);
    assert!(!has_hint(&ev, "ctrl-d", "Delete") && !has_hint(&ev, "e", "Edit"));
    let gen = ready(open(&mut ctx, "networking.k8s.io/v1/ingresses"), &ctx);
    assert!(has_hint(&gen, "shift-j", "Jump Owner") && !has_hint(&gen, "s", "Scale"));
    let wk = ready(open(&mut ctx, "workloads"), &ctx);
    assert!(has_hint(&wk, "y", "YAML") && has_hint(&wk, "ctrl-d", "Delete"));
    let helm = ready(open(&mut ctx, "helm"), &ctx);
    assert!(
        has_hint(&helm, "v", "Values")
            && has_hint(&helm, "r", "Releases")
            && has_hint(&helm, "y", "YAML")
    );
}

#[tokio::test]
async fn plugins_and_hotkeys() {
    let (mut ctx, mut rx) = context(fake());
    ctx.plugins.plugins.insert(
        "dbg".into(),
        crate::config::Plugin {
            scopes: vec!["po".into()],
            short_cut: "Shift-D".into(),
            description: "Debug".into(),
            command: "echo".into(),
            args: vec!["$NAMESPACE".into(), "$NAME".into()],
            background: true,
            ..Default::default()
        },
    );
    ctx.plugins.plugins.insert(
        "other".into(),
        crate::config::Plugin {
            scopes: vec!["svc".into()],
            short_cut: "Shift-X".into(),
            description: "Other".into(),
            command: "true".into(),
            ..Default::default()
        },
    );
    ctx.aliases.aliases.insert("po".into(), "v1/pods".into());
    ctx.hotkeys.hot_keys.insert(
        "dp".into(),
        crate::config::HotKey {
            short_cut: "Shift-H".into(),
            description: "Deploys".into(),
            command: "dp".into(),
            ..Default::default()
        },
    );
    let mut b = open(&mut ctx, "v1/pods");
    loaded(&mut b, &mut ctx, &mut rx).await;
    assert!(has_hint(&b, "shift-d", "Debug"));
    assert!(!has_hint(&b, "shift-x", "Other"));
    assert!(b.handle_key(key('D'), &mut ctx).is_empty());
    let cmds = until(&mut b, &mut ctx, &mut rx, is_custom).await;
    // exec.go pipe: the flashed status is the completion line (with the
    // resolved, truncated command), not the plugin description.
    let ff = flashes(&cmds);
    assert_eq!(ff.len(), 1, "{ff:?}");
    assert_eq!(ff[0].0, FlashLevel::Info);
    assert!(
        ff[0].1.starts_with(
            "Plugin command launched successfully: \"Command completed successfully: \\\""
        ),
        "{ff:?}"
    );
    assert!(!runs_matching("echo default a").is_empty());
    let cmds = b.handle_key(key('H'), &mut ctx);
    assert!(
        matches!(&cmds[..], [ViewCmd::Command(c)] if c == "dp"),
        "hotkey"
    );
    // keepHistory: gotoCmd(clearStack=false).
    ctx.hotkeys.hot_keys.get_mut("dp").unwrap().keep_history = true;
    b.refresh_actions(&ctx);
    let cmds = b.handle_key(key('H'), &mut ctx);
    assert!(
        matches!(&cmds[..], [ViewCmd::Goto { cmd, path }] if cmd == "dp" && path.is_empty()),
        "keepHistory"
    );
}

#[tokio::test]
async fn drill_downs() {
    let (mut ctx, mut rx) = context(fake());
    // pods -> containers
    let mut b = open(&mut ctx, "v1/pods");
    loaded(&mut b, &mut ctx, &mut rx).await;
    let v = pushed_browser(b.handle_key(code(KeyCode::Enter), &mut ctx));
    let co = as_browser(v.as_ref());
    assert_eq!(co.gvr().to_string(), "containers");
    assert_eq!(co.path(), "default/a");
    // No title before the first data update (ui/table.go styleTitle runs
    // in UpdateUI).
    assert_eq!(co.title(), "");
    assert_eq!(co.title_parts().1, "default/a");
    // deploy -> pods by selector
    let mut d = open(&mut ctx, "apps/v1/deployments");
    loaded(&mut d, &mut ctx, &mut rx).await;
    assert!(d.handle_key(code(KeyCode::Enter), &mut ctx).is_empty());
    let v = pushed_browser(until(&mut d, &mut ctx, &mut rx, is_custom).await);
    let p = as_browser(v.as_ref());
    assert_eq!(
        (p.gvr().to_string(), p.path(), p.label()),
        ("v1/pods".to_string(), "default/web", Some("app=web"))
    );
    assert_eq!(p.filter_text(), "app=web");
    assert!(
        !p.actions().contains_key(&rune('w')),
        "no namespace keys with a path"
    );
    // node -> pods on node (all namespaces, field selector)
    let mut n = open(&mut ctx, "v1/nodes");
    loaded(&mut n, &mut ctx, &mut rx).await;
    assert_eq!(n.title(), " nodes(all)[1] ");
    let v = pushed_browser(n.handle_key(code(KeyCode::Enter), &mut ctx));
    let p = as_browser(v.as_ref());
    assert_eq!(p.field(), Some("spec.nodeName=node1"));
    assert_eq!(p.namespace(), "");
    assert_eq!(p.title(), "");
    assert_eq!(p.title_parts().1, "node1");
    // configmap -> describe
    let mut c = open(&mut ctx, "v1/configmaps");
    loaded(&mut c, &mut ctx, &mut rx).await;
    assert_eq!(
        pushed(c.handle_key(code(KeyCode::Enter), &mut ctx)).name(),
        "describe v1/configmaps default/cfg"
    );
    assert_eq!(
        pushed(c.handle_key(key('y'), &mut ctx)).name(),
        "yaml v1/configmaps default/cfg"
    );
    // namespace -> pods in that namespace
    let mut ns = open(&mut ctx, "v1/namespaces");
    loaded(&mut ns, &mut ctx, &mut rx).await;
    assert!(ids(&ns).contains(&"all".to_string()));
    let names: Vec<String> = ns
        .table()
        .rows
        .iter()
        .map(|r| r.row.fields[0].clone())
        .collect();
    assert!(names.contains(&"default+(*)".to_string()), "{names:?}");
    assert!(names.contains(&"kube-system+".to_string()), "{names:?}");
    ns.handle_key(key('G'), &mut ctx);
    let cmds = ns.handle_key(code(KeyCode::Enter), &mut ctx);
    assert!(cmds
        .iter()
        .any(|c| matches!(c, ViewCmd::SetNamespace(n) if n == "kube-system")));
    // ns.go switchNs runs it as a command: pods view plus a history entry.
    assert!(cmds.iter().any(
        |c| matches!(c, ViewCmd::Goto { cmd, path } if cmd == "v1/pods kube-system" && path.is_empty())
    ));
}

#[tokio::test]
async fn containers_logs_and_shell() {
    let (mut ctx, mut rx) = context(fake());
    let mut co = open_opts(
        &mut ctx,
        "containers",
        Opts {
            path: "default/a".into(),
            ..Default::default()
        },
    );
    loaded(&mut co, &mut ctx, &mut rx).await;
    assert_eq!(ids(&co), vec!["web"]);
    assert_eq!(co.table().sort.name, "IDX");
    // showLogs runs the pods list access check first (async).
    co.handle_key(code(KeyCode::Enter), &mut ctx);
    let cmds = until(&mut co, &mut ctx, &mut rx, is_custom).await;
    assert_eq!(
        pushed(cmds).name(),
        "logs default|a|Some(\"web\")|false|false"
    );
    co.handle_key(key('p'), &mut ctx);
    let cmds = until(&mut co, &mut ctx, &mut rx, is_custom).await;
    assert_eq!(
        pushed(cmds).name(),
        "logs default|a|Some(\"web\")|false|true"
    );
    co.handle_key(key('s'), &mut ctx);
    let cmds = until(&mut co, &mut ctx, &mut rx, is_custom).await;
    match &cmds[..] {
        [ViewCmd::Exec {
            fallback: argv,
            banner,
            command,
            container,
            attach: false,
            ..
        }] => {
            // In-process exec of kubectl's command, kubectl argv as fallback.
            assert_eq!(
                command,
                &["sh", "-c", SHELL_CHECK_FOR_TEST].map(String::from)
            );
            assert_eq!(container, "web");
            assert_eq!(
                argv[..6],
                ["kubectl", "exec", "-it", "-n", "default", "a"].map(String::from)
            );
            // view/exec.go bannerFmt, green/black/bold via fatih/color.
            assert!(
                banner.ends_with("<<K9rs-Shell>> Pod: default/a | Container: web \n\x1b[0m")
                    || std::env::var_os("NO_COLOR").is_some(),
                "{banner:?}"
            );
            assert_eq!(
                argv[argv.len() - 3..],
                ["sh", "-c", SHELL_CHECK_FOR_TEST].map(String::from)
            );
        }
        _ => panic!("expected shell suspend"),
    }
    let cmds = co.handle_key(key('a'), &mut ctx);
    match &cmds[..] {
        [ViewCmd::Exec {
            fallback: argv,
            banner,
            attach: true,
            ..
        }] => {
            assert_eq!(argv.join(" "), "kubectl attach -it -n default a -c web");
            assert!(banner.contains("<<K9rs-Shell>> Pod: default/a | Container: web \n"));
        }
        _ => panic!("expected attach suspend"),
    }
}

const SHELL_CHECK_FOR_TEST: &str = "command -v bash >/dev/null && exec bash || exec sh";

#[tokio::test]
async fn pod_logs_shell_node_and_owner() {
    let f = fake();
    f.add("v1/pods", obj(json!({"apiVersion": "v1", "kind": "Pod",
        "metadata": {"name": "owned", "namespace": "default", "ownerReferences": [{"apiVersion": "apps/v1", "kind": "ReplicaSet", "name": "web-1", "uid": "u1"}]},
        "spec": {"nodeName": "node1", "containers": [{"name": "a"}, {"name": "b"}]},
        "status": {"phase": "Running", "containerStatuses": [{"name": "a", "ready": true, "state": {"running": {}}}, {"name": "b", "ready": true, "state": {"running": {}}}]}})));
    let (mut ctx, mut rx) = context(f);
    let mut b = open(&mut ctx, "v1/pods");
    loaded(&mut b, &mut ctx, &mut rx).await;
    // l on pod a: single container
    b.handle_key(key('l'), &mut ctx);
    let v = pushed(until(&mut b, &mut ctx, &mut rx, is_custom).await);
    assert_eq!(v.name(), "logs default|a|Some(\"web\")|false|false");
    // o: show node
    b.handle_key(key('o'), &mut ctx);
    let v = pushed_browser(until(&mut b, &mut ctx, &mut rx, is_custom).await);
    assert_eq!(as_browser(v.as_ref()).instance(), "node1");
    // b is Failed: shell refused
    b.handle_key(key('j'), &mut ctx);
    b.handle_key(key('s'), &mut ctx);
    let cmds = until(&mut b, &mut ctx, &mut rx, is_custom).await;
    assert_eq!(
        flashes(&cmds),
        vec![(
            FlashLevel::Err,
            "default/b is not in a running state".to_string()
        )]
    );
    // owned pod: two containers -> picker; logs -> all containers; owner jump
    b.handle_key(key('j'), &mut ctx);
    assert_eq!(b.table().selected_id().as_deref(), Some("default/owned"));
    b.handle_key(key('s'), &mut ctx);
    let v = pushed(until(&mut b, &mut ctx, &mut rx, is_custom).await);
    let picker = v.as_any().downcast_ref::<Picker>().expect("picker");
    assert_eq!(picker.items(), ["a", "b"]);
    // view/picker.go: tview list box, border in the frame border color, centered title.
    let mut v = v;
    let mut t = Terminal::new(TestBackend::new(60, 6)).unwrap();
    t.draw(|f| v.draw(f, f.area(), &ctx)).unwrap();
    let buf = t.backend().buffer().clone();
    assert_eq!(buf[(0, 0)].fg, ctx.styles.border);
    let top: String = (0..60).map(|x| buf[(x, 0)].symbol().to_string()).collect();
    assert!(top.find("Containers Picker").unwrap() > 15, "{top}");
    b.handle_key(key('l'), &mut ctx);
    assert_eq!(
        pushed(until(&mut b, &mut ctx, &mut rx, is_custom).await).name(),
        "logs default|owned|None|true|false"
    );
    b.handle_key(key('J'), &mut ctx);
    let v = pushed_browser(until(&mut b, &mut ctx, &mut rx, is_custom).await);
    let rs = as_browser(v.as_ref());
    assert_eq!(
        (rs.gvr().to_string(), rs.instance()),
        ("apps/v1/replicasets".to_string(), "default/web-1")
    );
    // pod a has no owner
    b.handle_key(key('g'), &mut ctx);
    b.handle_key(key('J'), &mut ctx);
    let cmds = until(&mut b, &mut ctx, &mut rx, is_custom).await;
    assert_eq!(
        flashes(&cmds),
        vec![(
            FlashLevel::Warn,
            "Unable to jump owner: no owner found".to_string()
        )]
    );
}

use super::picker::Picker;

#[tokio::test]
async fn delete_dialog_flow() {
    let f = fake();
    let (mut ctx, mut rx) = context(f.clone());
    let mut b = open(&mut ctx, "v1/pods");
    loaded(&mut b, &mut ctx, &mut rx).await;
    let cmds = b.handle_key(ctrl_key('d'), &mut ctx);
    let on_ok = match cmds.into_iter().next() {
        Some(ViewCmd::Dialog(Dialog::Delete { msg, on_ok })) => {
            assert_eq!(msg, "Delete pods default/a?");
            on_ok
        }
        _ => panic!("expected delete dialog"),
    };
    let cmds = on_ok(
        &mut ctx,
        DeleteOptions {
            force: true,
            ..Default::default()
        },
    );
    assert_eq!(
        flashes(&cmds),
        vec![(
            FlashLevel::Info,
            "Delete resource v1/pods default/a".to_string()
        )]
    );
    until(&mut b, &mut ctx, &mut rx, |e| {
        is_msg(e, |m| matches!(m, Msg::Deleted(_)))
    })
    .await;
    assert_eq!(f.calls(), vec!["delete v1/pods default/a force=true"]);
    until(&mut b, &mut ctx, &mut rx, is_data).await;
    assert_eq!(ids(&b), vec!["default/b"]);
    // marked multi delete
    f.add("v1/pods", pod("default", "z", "Running"));
    b.handle_key(ctrl_key('r'), &mut ctx);
    until(&mut b, &mut ctx, &mut rx, is_data).await;
    b.handle_key(key(' '), &mut ctx);
    b.handle_key(key('j'), &mut ctx);
    b.handle_key(key(' '), &mut ctx);
    let cmds = b.handle_key(ctrl_key('d'), &mut ctx);
    let on_ok = match cmds.into_iter().next() {
        Some(ViewCmd::Dialog(Dialog::Delete { msg, on_ok })) => {
            assert_eq!(msg, "Delete 2 marked v1/pods?");
            on_ok
        }
        _ => panic!("expected delete dialog"),
    };
    on_ok(&mut ctx, DeleteOptions::default());
    until(&mut b, &mut ctx, &mut rx, |e| {
        is_msg(e, |m| matches!(m, Msg::Deleted(_)))
    })
    .await;
    assert!(b.table().marks.is_empty());
    assert_eq!(
        f.calls()[1..],
        [
            "delete v1/pods default/b force=false",
            "delete v1/pods default/z force=false"
        ]
        .map(String::from)
    );
}

#[tokio::test]
async fn kill_edit_and_copy() {
    let f = fake();
    let (mut ctx, mut rx) = context(f.clone());
    let mut b = open(&mut ctx, "v1/pods");
    loaded(&mut b, &mut ctx, &mut rx).await;
    let cmds = b.handle_key(ctrl_key('k'), &mut ctx);
    assert_eq!(
        flashes(&cmds),
        vec![(
            FlashLevel::Info,
            "Delete resource v1/pods default/a".to_string()
        )]
    );
    until(&mut b, &mut ctx, &mut rx, |e| {
        is_msg(e, |m| matches!(m, Msg::Deleted(_)))
    })
    .await;
    assert_eq!(f.calls(), vec!["delete v1/pods default/a force=false"]);
    let cmds = b.handle_key(key('e'), &mut ctx);
    assert!(
        matches!(&cmds[..], [ViewCmd::Edit { gvr, path, .. }] if gvr.to_string() == "v1/pods" && path == "default/a")
    );
    assert_eq!(
        flashes(&b.handle_key(key('c'), &mut ctx)),
        vec![(
            FlashLevel::Info,
            "Resource name copied to clipboard...".to_string()
        )]
    );
    assert_eq!(
        flashes(&b.handle_key(key('n'), &mut ctx)),
        vec![(
            FlashLevel::Info,
            "Resource namespace copied to clipboard...".to_string()
        )]
    );
    assert_eq!(
        pushed(b.handle_key(key('?'), &mut ctx))
            .name()
            .split(' ')
            .next(),
        Some("help")
    );
}

#[tokio::test]
async fn scale_restart_and_set_image() {
    let f = fake();
    let (mut ctx, mut rx) = context(f.clone());
    let mut d = open(&mut ctx, "apps/v1/deployments");
    loaded(&mut d, &mut ctx, &mut rx).await;
    d.handle_key(key('s'), &mut ctx);
    let cmds = until(&mut d, &mut ctx, &mut rx, is_custom).await;
    let on_ok = match cmds.into_iter().next() {
        Some(ViewCmd::Dialog(Dialog::Scale {
            msg,
            current,
            on_ok,
        })) => {
            assert_eq!(
                (msg.as_str(), current),
                ("Scale deployment default/web?", 3)
            );
            on_ok
        }
        _ => panic!("expected scale dialog"),
    };
    on_ok(&mut ctx, 5);
    let cmds = until(&mut d, &mut ctx, &mut rx, is_custom).await;
    assert_eq!(
        flashes(&cmds),
        vec![(
            FlashLevel::Info,
            "deployments default/web scaled successfully".to_string()
        )]
    );
    assert!(f
        .calls()
        .contains(&"scale apps/v1/deployments default/web 5".to_string()));
    let cmds = d.handle_key(key('r'), &mut ctx);
    let on_ok = match cmds.into_iter().next() {
        Some(ViewCmd::Dialog(Dialog::Restart {
            title,
            msg,
            field_manager,
            on_ok,
        })) => {
            assert_eq!(
                (title.as_str(), msg.as_str(), field_manager.as_str()),
                (
                    "Confirm Restart",
                    "Restart deployment default/web?",
                    "kubectl-rollout"
                )
            );
            on_ok
        }
        _ => panic!("expected restart confirm"),
    };
    on_ok(&mut ctx, "mine".into());
    let cmds = until(&mut d, &mut ctx, &mut rx, is_custom).await;
    assert_eq!(
        flashes(&cmds),
        vec![(
            FlashLevel::Info,
            "Restart in progress for `default/web...".to_string()
        )]
    );
    // The edited FieldManager reaches the patch.
    assert!(f
        .calls()
        .contains(&"restart apps/v1/deployments default/web mine".to_string()));
    d.handle_key(key('i'), &mut ctx);
    let cmds = until(&mut d, &mut ctx, &mut rx, is_custom).await;
    let on_ok = match cmds.into_iter().next() {
        Some(ViewCmd::Dialog(Dialog::SetImage {
            msg,
            containers,
            on_ok,
        })) => {
            assert_eq!(msg, "Set image apps/v1/deployments default/web");
            assert_eq!(containers, vec![("web".into(), "nginx:1".into(), false)]);
            on_ok
        }
        _ => panic!("expected image form"),
    };
    on_ok(
        &mut ctx,
        vec![crate::k8s::ImageSpec {
            name: "web".into(),
            docker_image: "nginx:2".into(),
            init: false,
        }],
    );
    until(&mut d, &mut ctx, &mut rx, is_custom).await;
    assert!(f
        .calls()
        .contains(&"set_images apps/v1/deployments default/web web=nginx:2".to_string()));
    // z: replicasets of the deployment
    d.handle_key(key('z'), &mut ctx);
    let v = pushed_browser(until(&mut d, &mut ctx, &mut rx, is_custom).await);
    assert_eq!(as_browser(v.as_ref()).label(), Some("app=web"));
}

/// browser.go editRes: no patch permission, no editor.
#[tokio::test(flavor = "multi_thread")]
async fn edit_needs_patch_access() {
    let f = fake();
    f.denied_patch
        .lock()
        .unwrap()
        .push(("v1/pods".into(), "default/a".into()));
    let (mut ctx, mut rx) = context(f);
    let mut b = open(&mut ctx, "v1/pods");
    loaded(&mut b, &mut ctx, &mut rx).await;
    let cmds = b.handle_key(key('e'), &mut ctx);
    assert_eq!(
        flashes(&cmds),
        vec![(
            FlashLevel::Err,
            "current user can't edit resource v1/pods".to_string()
        )]
    );
    b.handle_key(key('j'), &mut ctx);
    assert!(matches!(
        &b.handle_key(key('e'), &mut ctx)[..],
        [ViewCmd::Edit { .. }]
    ));
}

/// exec.go launchNodeShell: a cancellable "<Launching>" prompt while the
/// shell pod starts; Cancel stops waiting and nukes the pod.
#[tokio::test]
async fn node_shell_launching_prompt_cancels() {
    let f = fake();
    let (mut ctx, mut rx) = context(f.clone());
    let mut n = open(&mut ctx, "v1/nodes");
    loaded(&mut n, &mut ctx, &mut rx).await;
    n.handle_key(key('s'), &mut ctx);
    let cmds = until(&mut n, &mut ctx, &mut rx, is_custom).await;
    let (cancel, on_cancel) = match cmds.into_iter().next() {
        Some(ViewCmd::Dialog(Dialog::Launching {
            title,
            msg,
            cancel,
            on_cancel,
        })) => {
            assert_eq!(
                (title.as_str(), msg.as_str()),
                ("Launching", "Launching node shell on node1...")
            );
            (cancel, on_cancel)
        }
        _ => panic!("expected the launching prompt"),
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    let pod = format!("default/k9rs-shell-{}", std::process::id());
    assert!(
        f.calls().contains(&format!("create v1/pods {pod}")),
        "{:?}",
        f.calls()
    );
    cancel.cancel();
    on_cancel(&mut ctx);
    // The launch ends quietly (context.Canceled is not flashed).
    let cmds = until(&mut n, &mut ctx, &mut rx, is_custom).await;
    assert!(flashes(&cmds).is_empty());
}

#[tokio::test]
async fn nodes_cordon_and_drain() {
    let f = fake();
    let (mut ctx, mut rx) = context(f.clone());
    let mut n = open(&mut ctx, "v1/nodes");
    loaded(&mut n, &mut ctx, &mut rx).await;
    let cmds = n.handle_key(key('c'), &mut ctx);
    let on_ok = match cmds.into_iter().next() {
        Some(ViewCmd::Dialog(Dialog::Confirm { title, msg, on_ok })) => {
            assert_eq!(
                (title.as_str(), msg.as_str()),
                ("Confirm Cordon", "Cordon node1?")
            );
            on_ok
        }
        _ => panic!("expected cordon confirm"),
    };
    on_ok(&mut ctx);
    until(&mut n, &mut ctx, &mut rx, is_custom).await;
    assert_eq!(f.calls(), vec!["cordon node1 true"]);
    let on_ok = match n.handle_key(key('r'), &mut ctx).into_iter().next() {
        Some(ViewCmd::Dialog(Dialog::Drain { node, on_ok })) => {
            assert_eq!(node, "node1");
            on_ok
        }
        _ => panic!("expected drain dialog"),
    };
    // node.go drainNode: the progress view opens first and the drain
    // output streams into it.
    let v = pushed(on_ok(&mut ctx, Default::default()));
    assert_eq!(v.name(), "text Drain Progress|nodes|");
    let text = loop {
        match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
            Ok(Some(AppEvent::Text { view, text })) if view == v.id() => break text,
            Ok(Some(_)) => continue,
            other => panic!("no progress: {other:?}"),
        }
    };
    assert_eq!(text, "node/node1 drained\n");
}

#[tokio::test]
async fn cronjob_trigger_and_secret_decode() {
    let f = fake();
    f.add("batch/v1/cronjobs", obj(json!({"apiVersion": "batch/v1", "kind": "CronJob", "metadata": {"name": "cj", "namespace": "default", "uid": "cj-uid"},
        "spec": {"schedule": "* * * * *", "suspend": false, "jobTemplate": {"spec": {"template": {"spec": {"containers": [{"name": "c"}]}}}}}})));
    f.add("batch/v1/jobs", obj(json!({"apiVersion": "batch/v1", "kind": "Job", "metadata": {"name": "j1", "namespace": "default", "ownerReferences": [{"apiVersion": "batch/v1", "kind": "CronJob", "name": "cj", "uid": "cj-uid"}]}})));
    f.add("batch/v1/jobs", obj(json!({"apiVersion": "batch/v1", "kind": "Job", "metadata": {"name": "j2", "namespace": "default"}})));
    let (mut ctx, mut rx) = context(f.clone());
    let mut cj = open(&mut ctx, "batch/v1/cronjobs");
    loaded(&mut cj, &mut ctx, &mut rx).await;
    let on_ok = match cj.handle_key(key('t'), &mut ctx).into_iter().next() {
        Some(ViewCmd::Dialog(Dialog::Confirm { title, msg, on_ok })) => {
            assert_eq!(
                (title.as_str(), msg.as_str()),
                ("Confirm Job Trigger", "Trigger CronJob: default/cj?")
            );
            on_ok
        }
        _ => panic!("expected trigger confirm"),
    };
    on_ok(&mut ctx);
    let cmds = until(&mut cj, &mut ctx, &mut rx, is_custom).await;
    assert_eq!(
        flashes(&cmds),
        vec![(
            FlashLevel::Info,
            "Triggered Job batch/v1/cronjobs default/cj".to_string()
        )]
    );
    match cj.handle_key(key('s'), &mut ctx).into_iter().next() {
        Some(ViewCmd::Dialog(Dialog::Confirm { title, msg, .. })) => {
            assert_eq!((title.as_str(), msg.as_str()), ("Suspend", "default/cj"))
        }
        _ => panic!("expected suspend confirm"),
    }
    // enter: jobs owned by the cronjob
    cj.handle_key(code(KeyCode::Enter), &mut ctx);
    let v = pushed_browser(until(&mut cj, &mut ctx, &mut rx, is_custom).await);
    let mut jobs = v
        .as_any()
        .downcast_ref::<Browser>()
        .map(|_| ())
        .map(|_| v)
        .unwrap();
    jobs.start(&mut ctx);
    let jb = jobs.as_any_mut().downcast_mut::<Browser>().unwrap();
    until(jb, &mut ctx, &mut rx, is_data).await;
    assert_eq!(ids(jb), vec!["default/j1"]);
    // secret decode
    let mut s = open(&mut ctx, "v1/secrets");
    loaded(&mut s, &mut ctx, &mut rx).await;
    s.handle_key(key('x'), &mut ctx);
    let v = pushed(until(&mut s, &mut ctx, &mut rx, is_custom).await);
    assert_eq!(v.name(), "text Secret Decoder|default/sec|pw: hunter2\n");
}

#[tokio::test]
async fn used_by_references() {
    let f = fake();
    f.add("apps/v1/deployments", obj(json!({"apiVersion": "apps/v1", "kind": "Deployment", "metadata": {"name": "uses", "namespace": "default"},
        "spec": {"template": {"spec": {"containers": [{"name": "a", "envFrom": [{"configMapRef": {"name": "cfg"}}]}]}}}})));
    let (mut ctx, mut rx) = context(f);
    let mut c = open(&mut ctx, "v1/configmaps");
    loaded(&mut c, &mut ctx, &mut rx).await;
    c.handle_key(key('u'), &mut ctx);
    let cmds = until(&mut c, &mut ctx, &mut rx, is_custom).await;
    assert_eq!(
        flashes(&cmds),
        vec![(
            FlashLevel::Info,
            "Viewing references for v1/configmaps::default/cfg".to_string()
        )]
    );
    let mut v = pushed_browser(cmds);
    v.start(&mut ctx);
    let r = v.as_any_mut().downcast_mut::<Browser>().unwrap();
    until(r, &mut ctx, &mut rx, is_data).await;
    assert_eq!(ids(r), vec!["default/uses"]);
    assert_eq!(r.title(), " references(default/cfg)[1] ");
    let v = pushed_browser(r.handle_key(code(KeyCode::Enter), &mut ctx));
    let d = as_browser(v.as_ref());
    assert_eq!(
        (d.gvr().to_string(), d.instance()),
        ("apps/v1/deployments".to_string(), "default/uses")
    );
}

/// StatefulSet.Scan: claims stamped from volumeClaimTemplates reference the set.
#[tokio::test]
async fn used_by_pvc_from_sts_claim_template() {
    let f = fake();
    f.add("apps/v1/statefulsets", obj(json!({"apiVersion": "apps/v1", "kind": "StatefulSet", "metadata": {"name": "am", "namespace": "default"},
        "spec": {"volumeClaimTemplates": [{"metadata": {"name": "db"}}],
            "template": {"spec": {"containers": [{"name": "a"}]}}}})));
    let acc: crate::k8s::SharedAccessor = f;
    let pvc = Gvr::parse("v1/persistentvolumeclaims");
    let refs = super::loader::scan_refs(&acc, &pvc, "default/db-am-0")
        .await
        .unwrap();
    assert_eq!(
        refs,
        vec![("apps/v1/statefulsets".to_string(), "default/am".to_string())]
    );
    assert!(super::loader::scan_refs(&acc, &pvc, "default/other-0")
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn pseudo_aliases_contexts_and_portforwards() {
    let f = fake();
    let (mut ctx, mut rx) = context(f.clone());
    ctx.aliases.aliases.insert("po".into(), "v1/pods".into());
    ctx.aliases.aliases.insert("pod".into(), "v1/pods".into());
    ctx.aliases
        .aliases
        .insert("dp".into(), "apps/v1/deployments".into());
    let mut a = open(&mut ctx, "aliases");
    loaded(&mut a, &mut ctx, &mut rx).await;
    assert_eq!(a.title(), " aliases(all)[2] ");
    a.handle_key(key('G'), &mut ctx);
    let cmds = a.handle_key(code(KeyCode::Enter), &mut ctx);
    assert!(
        matches!(&cmds[..], [ViewCmd::Command(c)] if c == "v1/pods"),
        "alias goto"
    );
    // contexts: enter switches
    let mut c = open(&mut ctx, "contexts");
    loaded(&mut c, &mut ctx, &mut rx).await;
    assert_eq!(ids(&c), vec!["fake"]);
    // k9s useContext hands the switch to the shell (app.switchContext).
    let cmds = c.handle_key(code(KeyCode::Enter), &mut ctx);
    // useCtx also clears the histories once switched (ViewCmd::UseContext).
    assert!(matches!(cmds.as_slice(), [ViewCmd::UseContext(c)] if c == "fake"));
    // port forwards
    let spec = PortForwardSpec {
        namespace: "default".into(),
        pod: "a".into(),
        container: "web".into(),
        address: "localhost".into(),
        local_port: 8080,
        container_port: 80,
    };
    ctx.port_forwards.insert(
        spec.id(),
        crate::k8s::PortForwardHandle {
            info: PortForwardInfo {
                spec: spec.clone(),
                started_at: chrono::Utc::now(),
                active: true,
            },
            cancel: Default::default(),
            died: None,
        },
    );
    let mut pf = open(&mut ctx, "portforwards");
    loaded(&mut pf, &mut ctx, &mut rx).await;
    assert_eq!(ids(&pf), vec![spec.id()]);
    let on_ok = match pf.handle_key(ctrl_key('d'), &mut ctx).into_iter().next() {
        Some(ViewCmd::Dialog(Dialog::Confirm { title, msg, on_ok })) => {
            assert_eq!(
                (title.as_str(), msg.as_str()),
                ("Delete", "Delete portforwards a::web 8080->80?")
            );
            on_ok
        }
        _ => panic!("expected pf delete confirm"),
    };
    let cmds = on_ok(&mut ctx);
    assert_eq!(
        flashes(&cmds),
        vec![(
            FlashLevel::Info,
            "Successfully deleted 1 PortForward!".to_string()
        )]
    );
    assert!(ctx.port_forwards.is_empty());
}

#[tokio::test]
async fn pod_pf_indicator_and_port_forward_dialog() {
    let f = fake();
    let (mut ctx, mut rx) = context(f.clone());
    let mut b = open(&mut ctx, "v1/pods");
    loaded(&mut b, &mut ctx, &mut rx).await;
    b.handle_key(key('F'), &mut ctx);
    let cmds = until(&mut b, &mut ctx, &mut rx, is_custom).await;
    let on_ok = match cmds.into_iter().next() {
        Some(ViewCmd::Dialog(Dialog::PortForward {
            spec,
            ports,
            prefill,
            on_ok,
        })) => {
            assert_eq!(ports, vec!["web::80(http)"]);
            assert_eq!(spec.container, "web");
            // PFAnn.AsSpec: no port name in the prefill.
            assert_eq!(prefill, ("web::80".to_string(), "80".to_string()));
            on_ok
        }
        _ => panic!("expected pf dialog"),
    };
    let mut spec = PortForwardSpec {
        namespace: "default".into(),
        pod: "a".into(),
        container: "web".into(),
        ..Default::default()
    };
    spec.local_port = 9090;
    spec.container_port = 80;
    on_ok(&mut ctx, vec![spec.clone()]);
    let cmds = until(&mut b, &mut ctx, &mut rx, is_custom).await;
    assert_eq!(
        flashes(&cmds),
        vec![(FlashLevel::Info, "PortForward activated 9090".to_string())]
    );
    assert!(ctx.port_forwards.contains_key(&spec.id()));
    // runForward: a forward dying later is reported to the shell.
    let tx = f.pf_kill.lock().unwrap().pop().expect("kill switch");
    tx.send("lost connection to pod".into()).unwrap();
    let died = loop {
        let ev = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("died event")
            .expect("open");
        if let AppEvent::Custom { view, payload } = ev {
            if view == crate::ui::shell::SHELL_VIEW_ID {
                break payload
                    .downcast::<crate::ui::app::PortForwardDied>()
                    .expect("died payload");
            }
        }
    };
    assert_eq!(
        (died.id.as_str(), died.err.as_str()),
        (spec.id().as_str(), "lost connection to pod")
    );
    // f now opens the port-forward view for the pod
    b.handle_key(key('f'), &mut ctx);
    let v = pushed_browser(until(&mut b, &mut ctx, &mut rx, is_custom).await);
    assert_eq!(as_browser(v.as_ref()).path(), "default/a");
    // pf on a failed pod is refused
    b.handle_key(key('j'), &mut ctx);
    b.handle_key(key('F'), &mut ctx);
    let cmds = until(&mut b, &mut ctx, &mut rx, |e| {
        matches!(e, AppEvent::Error { .. })
    })
    .await;
    assert_eq!(
        flashes(&cmds),
        vec![(
            FlashLevel::Err,
            "pod must be running. Current status=Failed".to_string()
        )]
    );
}

#[tokio::test]
async fn screendumps_save_and_dir() {
    let (mut ctx, mut rx) = context(fake());
    let dir = test_dumps(&ctx);
    let _ = std::fs::remove_dir_all(&dir);
    let mut b = open(&mut ctx, "v1/pods");
    loaded(&mut b, &mut ctx, &mut rx).await;
    let cmds = b.handle_key(ctrl_key('s'), &mut ctx);
    let fl = flashes(&cmds);
    assert!(
        fl[0]
            .1
            .starts_with("File saved successfully: \"pods-default-"),
        "{fl:?}"
    );
    let files: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
    assert_eq!(files.len(), 1);
    let body = std::fs::read_to_string(files[0].path()).unwrap();
    assert!(
        // k9s saves the full header, NAMESPACE included, in a namespaced view.
        body.starts_with(
            "NAMESPACE,NAME,READY,STATUS,RESTARTS,IP,VALID,AGE\ndefault,a,1/1,Running"
        ),
        "{body}"
    );
    let mut sd = open(&mut ctx, "screendumps");
    loaded(&mut sd, &mut ctx, &mut rx).await;
    assert_eq!(ids(&sd).len(), 1);
    let on_ok = match sd.handle_key(ctrl_key('d'), &mut ctx).into_iter().next() {
        Some(ViewCmd::Dialog(Dialog::Confirm { title, on_ok, .. })) => {
            assert_eq!(title, "Confirm Delete");
            on_ok
        }
        _ => panic!("expected confirm"),
    };
    on_ok(&mut ctx);
    until(&mut sd, &mut ctx, &mut rx, |e| {
        is_msg(e, |m| matches!(m, Msg::Deleted(_)))
    })
    .await;
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    // dir view
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.yaml"), "kind: Pod\n").unwrap();
    std::fs::write(tmp.path().join("notes.txt"), "x").unwrap();
    std::fs::create_dir(tmp.path().join("sub")).unwrap();
    std::fs::write(tmp.path().join("sub/c.yaml"), "kind: Pod\n").unwrap();
    let cmd = Command {
        gvr: "dir".into(),
        args: vec![tmp.path().to_string_lossy().to_string()],
        ..Default::default()
    };
    let mut v = open_with(&mut ctx, &cmd, deps()).unwrap();
    v.start(&mut ctx);
    let d = v.as_any_mut().downcast_mut::<Browser>().unwrap();
    until(d, &mut ctx, &mut rx, is_data).await;
    assert_eq!(ids(d).len(), 2, "{:?}", ids(d));
    assert!(d.title().starts_with(" dirs("));
    let yaml_row = ids(d)
        .into_iter()
        .position(|p| p.ends_with("a.yaml"))
        .unwrap();
    d.table.cursor = yaml_row;
    assert_eq!(
        pushed(d.handle_key(key('y'), &mut ctx))
            .name()
            .rsplit('|')
            .next(),
        Some("kind: Pod\n")
    );
    let cmds = d.handle_key(code(KeyCode::Enter), &mut ctx);
    assert_eq!(
        flashes(&cmds),
        vec![(FlashLevel::Err, "you must select a directory".to_string())]
    );
    d.handle_key(key('a'), &mut ctx);
    until(d, &mut ctx, &mut rx, is_custom).await;
    assert!(!runs_matching("kubectl apply -f").is_empty());
    // dir.go gotoCmd: NewDir(sel) is a client.DirGVR ("dirs") browser,
    // keeping the path as listed (no symlink resolution: /tmp stays /tmp).
    d.table.cursor = ids(d).iter().position(|p| p.ends_with("sub")).unwrap();
    let mut sub = pushed_browser(d.handle_key(code(KeyCode::Enter), &mut ctx));
    sub.start(&mut ctx);
    let sub = sub.as_any_mut().downcast_mut::<Browser>().unwrap();
    until(sub, &mut ctx, &mut rx, is_data).await;
    assert_eq!(sub.gvr().to_string(), "dirs");
    let want = tmp.path().join("sub").to_string_lossy().to_string();
    assert!(sub.title().contains(&want), "{} vs {want}", sub.title());
}

#[tokio::test]
async fn helm_releases_and_history() {
    // helm's secret storage: base64 of the gzipped release JSON.
    let release = {
        use base64::Engine as _;
        use std::io::Write as _;
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(br#"{"chart":{"values":{"replicas":1,"greeting":"hello"}}}"#)
            .unwrap();
        let b64 = base64::engine::general_purpose::STANDARD;
        b64.encode(b64.encode(gz.finish().unwrap()))
    };
    let f = fake();
    f.add(
        "v1/secrets",
        obj(json!({"apiVersion": "v1", "kind": "Secret",
            "metadata": {"name": "sh.helm.release.v1.web.v2", "namespace": "default"},
            "data": {"release": release}})),
    );
    let (mut ctx, mut rx) = context(f);
    let mut h = open(&mut ctx, "helm");
    loaded(&mut h, &mut ctx, &mut rx).await;
    assert_eq!(ids(&h), vec!["default/web"]);
    assert!(
        !runs_matching("helm list -o json --all --max 0 -n default --kube-context fake").is_empty()
    );
    let mut v = pushed_browser(h.handle_key(code(KeyCode::Enter), &mut ctx));
    v.start(&mut ctx);
    let hist = v.as_any_mut().downcast_mut::<Browser>().unwrap();
    until(hist, &mut ctx, &mut rx, is_data).await;
    assert_eq!(
        hist.table().sort,
        SortColumn {
            name: "REVISION".into(),
            asc: false
        }
    );
    assert_eq!(ids(hist)[0], "default/web:2");
    hist.handle_key(code(KeyCode::Enter), &mut ctx);
    let t = pushed(until(hist, &mut ctx, &mut rx, is_custom).await);
    // History values: the revision's user values, toggling to the chart's own.
    assert_eq!(
        t.name(),
        "values default/web:2|greeting: hola\n|greeting: hello\nreplicas: 1\n"
    );
    assert!(!runs_matching("helm get values web -o json -n default --revision 2").is_empty());
    let t = pushed(
        h.handle_key(key('v'), &mut ctx)
            .into_iter()
            .chain(until(&mut h, &mut ctx, &mut rx, is_custom).await)
            .collect(),
    );
    assert_eq!(
        t.name(),
        "values default/web|greeting: hola\n|greeting: hola\nreplicas: 1\n"
    );
    match hist.handle_key(key('r'), &mut ctx).into_iter().next() {
        Some(ViewCmd::Dialog(Dialog::Confirm { title, .. })) => {
            assert_eq!(title, "Confirm Rollback")
        }
        _ => panic!("expected rollback confirm"),
    }
    let on_ok = match h.handle_key(ctrl_key('d'), &mut ctx).into_iter().next() {
        Some(ViewCmd::Dialog(Dialog::Confirm { msg, on_ok, .. })) => {
            assert_eq!(msg, "Delete helm default/web?");
            on_ok
        }
        _ => panic!("expected helm delete confirm"),
    };
    on_ok(&mut ctx);
    until(&mut h, &mut ctx, &mut rx, |e| {
        is_msg(e, |m| matches!(m, Msg::Deleted(_)))
    })
    .await;
    assert!(!runs_matching("helm uninstall web -n default").is_empty());
}

#[tokio::test]
async fn rbac_rules_policy_and_subjects() {
    let f = fake();
    f.add("rbac.authorization.k8s.io/v1/clusterroles", obj(json!({"apiVersion": "rbac.authorization.k8s.io/v1", "kind": "ClusterRole", "metadata": {"name": "viewer"},
        "rules": [{"apiGroups": [""], "resources": ["pods"], "verbs": ["get", "list"]}]})));
    f.add("rbac.authorization.k8s.io/v1/clusterrolebindings", obj(json!({"apiVersion": "rbac.authorization.k8s.io/v1", "kind": "ClusterRoleBinding", "metadata": {"name": "vb"},
        "roleRef": {"kind": "ClusterRole", "name": "viewer"}, "subjects": [{"kind": "User", "name": "fred"}, {"kind": "Group", "name": "devs"}]})));
    let (mut ctx, mut rx) = context(f);
    let mut cr = open(&mut ctx, "rbac.authorization.k8s.io/v1/clusterroles");
    loaded(&mut cr, &mut ctx, &mut rx).await;
    let mut v = pushed_browser(cr.handle_key(code(KeyCode::Enter), &mut ctx));
    v.start(&mut ctx);
    let rules = v.as_any_mut().downcast_mut::<Browser>().unwrap();
    until(rules, &mut ctx, &mut rx, is_data).await;
    assert_eq!(rules.table().rows.len(), 1);
    assert_eq!(rules.title(), " rbac(viewer)[1] ");
    let mut u = open(&mut ctx, "users");
    loaded(&mut u, &mut ctx, &mut rx).await;
    assert_eq!(ids(&u), vec!["fred"]);
    assert!(has_hint(&u, "enter", "Rules") && !has_hint(&u, "ctrl-d", "Delete"));
    let mut v = pushed_browser(u.handle_key(code(KeyCode::Enter), &mut ctx));
    v.start(&mut ctx);
    let pol = v.as_any_mut().downcast_mut::<Browser>().unwrap();
    until(pol, &mut ctx, &mut rx, is_data).await;
    assert_eq!(pol.title(), " policy(User:fred)[1] ");
    let cmd = Command {
        gvr: "policy".into(),
        args: vec!["g:devs".into()],
        ..Default::default()
    };
    let mut v = open_with(&mut ctx, &cmd, deps()).unwrap();
    v.start(&mut ctx);
    let g = v.as_any_mut().downcast_mut::<Browser>().unwrap();
    until(g, &mut ctx, &mut rx, is_data).await;
    assert_eq!(g.table().rows.len(), 1);
    assert!(open_with(
        &mut ctx,
        &Command {
            gvr: "policy".into(),
            ..Default::default()
        },
        deps()
    )
    .is_err());
}

#[tokio::test]
async fn workloads_use_shared_renderer() {
    let (mut ctx, mut rx) = context(fake());
    let mut w = open(&mut ctx, "workloads");
    loaded(&mut w, &mut ctx, &mut rx).await;
    assert!(
        ids(&w).contains(&"apps/v1/deployments|default|web".to_string()),
        "{:?}",
        ids(&w)
    );
    assert_eq!(w.table().sort.name, "KIND");
    let pos = ids(&w)
        .iter()
        .position(|i| i == "apps/v1/deployments|default|web")
        .unwrap();
    w.table.cursor = pos;
    let v = pushed_browser(w.handle_key(code(KeyCode::Enter), &mut ctx));
    assert_eq!(as_browser(v.as_ref()).instance(), "default/web");
    assert_eq!(
        pushed(w.handle_key(key('d'), &mut ctx)).name(),
        "describe apps/v1/deployments default/web"
    );
}

#[tokio::test]
async fn open_errors_for_unknown_resources() {
    let (mut ctx, _rx) = context(fake());
    let cmd = Command {
        gvr: "foo.example.com/v1/widgets".into(),
        ..Default::default()
    };
    assert!(open_with(&mut ctx, &cmd, deps()).is_err());
    let cmd = Command {
        gvr: "v1/pods".into(),
        namespace: Some("kube-system".into()),
        filter: Some("c".into()),
        ..Default::default()
    };
    let v = open_with(&mut ctx, &cmd, deps()).unwrap();
    let b = as_browser(v.as_ref());
    assert_eq!(
        (b.namespace(), b.filter_text().as_str()),
        ("kube-system", "c")
    );
}

#[tokio::test]
async fn draws_header_sort_cursor_and_marks() {
    let (mut ctx, mut rx) = context(fake());
    let mut b = open(&mut ctx, "v1/pods");
    loaded(&mut b, &mut ctx, &mut rx).await;
    b.handle_key(key('j'), &mut ctx);
    b.handle_key(key(' '), &mut ctx);
    b.handle_key(key('k'), &mut ctx);
    let s = Styles::default();
    let buf = render(&mut b, &ctx, 80, 6);
    let top = line(&buf, 0);
    assert!(
        top.starts_with('┌') && top.contains(" pods(default)[2] "),
        "{top}"
    );
    let header = line(&buf, 1);
    assert!(header.starts_with("│ NAME↑"), "{header}");
    let arrow = header.chars().position(|c| c == '↑').unwrap() as u16;
    assert_eq!(buf[(arrow, 1)].fg, s.table_sorter);
    assert!(
        buf[(2, 1)].modifier.contains(Modifier::BOLD),
        "sorted column is selected → bold"
    );
    assert_eq!(buf[(2, 1)].fg, s.table_header_fg);
    // cursor row: cursor fg on the row color, bold, across the row
    let row = line(&buf, 2);
    assert!(row.contains('a') && row.contains("Running"), "{row}");
    let added = s.palette.add;
    assert_eq!((buf[(2, 2)].fg, buf[(2, 2)].bg), (s.table_cursor_fg, added));
    assert!(buf[(2, 2)].modifier.contains(Modifier::BOLD));
    assert_eq!(buf[(70, 2)].bg, added);
    // marked row in mark color
    assert_eq!(buf[(2, 3)].fg, s.table_mark);
    // title spans: count in counter color
    let cnt = top.find("[2]").unwrap();
    let x = top[..cnt].chars().count() as u16 + 1;
    assert_eq!(buf[(x, 0)].fg, s.title_counter);
    // right aligned RESTARTS column ends where its header ends
    let rh = header.find("RESTARTS").unwrap();
    let rend = header[..rh].chars().count() + "RESTARTS".len() - 1;
    assert_eq!(buf[(rend as u16, 3)].symbol(), "0");
    let _ = Color::Reset;
}

#[tokio::test]
async fn deltas_and_update_colors() {
    let f = fake();
    let (mut ctx, mut rx) = context(f.clone());
    let mut b = open(&mut ctx, "v1/pods");
    loaded(&mut b, &mut ctx, &mut rx).await;
    {
        let mut objs = f.objects.lock().unwrap();
        let pods = objs.get_mut(&Gvr::parse("v1/pods")).unwrap();
        pods[0] = pod("default", "a", "Pending");
    }
    b.on_event(&AppEvent::Tick, &mut ctx);
    until(&mut b, &mut ctx, &mut rx, is_data).await;
    let re = b
        .table()
        .rows
        .iter()
        .find(|r| r.row.id == "default/a")
        .unwrap()
        .clone();
    assert_eq!(re.kind, ResEvent::Update);
    b.handle_key(key('j'), &mut ctx);
    let buf = render(&mut b, &ctx, 80, 6);
    let row = line(&buf, 2);
    assert!(row.contains("PendingΔ"), "{row}");
    assert_eq!(buf[(2, 2)].fg, ctx.styles.palette.modify);
}

#[test]
fn hints_are_in_tcell_order() {
    let (tx, _rx) = unbounded_channel();
    let mut ctx = Ctx::new(
        fake(),
        Arc::new(RwLock::new(Config::default())),
        Styles::default(),
        "default".into(),
        tx,
    );
    ctx.resources = crate::k8s::fake::default_resources();
    let b = ready(open(&mut ctx, "v1/configmaps"), &ctx);
    let m: Vec<String> = b.hints().into_iter().map(|h| h.mnemonic).collect();
    let pos = |k: &str| {
        m.iter()
            .position(|x| x == k)
            .unwrap_or_else(|| panic!("{k} in {m:?}"))
    };
    assert!(pos("ctrl-d") < pos("enter"));
    assert!(pos("enter") < pos("0"));
    assert!(pos("0") < pos("?"));
    assert!(pos("?") < pos("shift-a"));
    assert!(pos("shift-a") < pos("c"));
    let _ = HashMap::<u8, u8>::new();
}

#[tokio::test]
async fn ns_switch_keeps_live_table_and_skips_no_data_warning() {
    let f = FakeAccessor::new();
    f.add("v1/pods", pod("default", "a", "Running"));
    let (mut ctx, mut rx) = context(Arc::new(f));
    let mut b = open(&mut ctx, "v1/pods");
    loaded(&mut b, &mut ctx, &mut rx).await;
    // browser.go switchNamespaceCmd: model.SetNamespace resets the model only;
    // the UI keeps the old rows, the title shows the new ns with the old count.
    let cmds = b.handle_key(key('2'), &mut ctx);
    assert_eq!(flashes(&cmds).len(), 1);
    assert_eq!(b.namespace(), "kube-system");
    assert_eq!(ids(&b), vec!["default/a"]);
    assert_eq!(b.title(), " pods(kube-system)[1] ");
    // Start resets firstView: the first empty refresh is skipped...
    let c1 = until(&mut b, &mut ctx, &mut rx, is_data).await;
    assert_eq!(ids(&b), vec!["default/a"]);
    // ...the initRefreshRate one paints the empty table, without a warning
    // since the table already has columns.
    let c2 = until(&mut b, &mut ctx, &mut rx, is_data).await;
    assert!(ids(&b).is_empty());
    assert!(b.table().data.header.index_of("NAME").is_some());
    assert!(flashes(&c1).is_empty() && flashes(&c2).is_empty());
    assert_eq!(b.title(), " pods(kube-system)[0] ");
}

#[tokio::test]
async fn favorites_unknown_to_cluster_are_dropped() {
    let (mut ctx, mut rx) = context(fake());
    // Config.FavNamespaces validates favorites against the cluster list.
    ctx.namespaces = vec!["default".into()];
    ctx.namespaces_known = true;
    let mut b = open(&mut ctx, "v1/pods");
    loaded(&mut b, &mut ctx, &mut rx).await;
    let h = visible_hints(&b);
    assert!(h.contains(&("1".into(), "default".into())));
    assert!(!h.iter().any(|(m, _)| m == "2"), "{h:?}");
    // No namespace list yet: keep them all.
    let (mut ctx, mut rx) = context(fake());
    let mut b = open(&mut ctx, "v1/pods");
    loaded(&mut b, &mut ctx, &mut rx).await;
    assert!(visible_hints(&b).contains(&("2".into(), "kube-system".into())));
    // The list failed (e.g. no list access): nothing validates, but pruning
    // only runs when the active namespace itself checks out ("all" does).
    let (mut ctx, mut rx) = context(fake());
    ctx.namespaces_known = true;
    let mut b = open(&mut ctx, "v1/pods");
    loaded(&mut b, &mut ctx, &mut rx).await;
    // Active "default" is not valid either: nothing pruned.
    assert!(visible_hints(&b).contains(&("2".into(), "kube-system".into())));
    b.handle_key(key('0'), &mut ctx);
    until(&mut b, &mut ctx, &mut rx, is_data).await;
    until(&mut b, &mut ctx, &mut rx, is_data).await;
    let h = visible_hints(&b);
    assert!(h.contains(&("0".into(), "all".into())));
    assert!(!h.iter().any(|(m, _)| m == "1"), "{h:?}");
}

#[tokio::test]
async fn use_namespace_marks_it_favorite() {
    let f = fake();
    f.add(
        "v1/namespaces",
        obj(json!({"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": "extra"}})),
    );
    let (mut ctx, mut rx) = context(f);
    let mut ns = open(&mut ctx, "v1/namespaces");
    loaded(&mut ns, &mut ctx, &mut rx).await;
    let row = ns
        .table()
        .rows
        .iter()
        .position(|r| r.row.id == "-/extra")
        .unwrap();
    for _ in 0..row {
        ns.handle_key(code(KeyCode::Down), &mut ctx);
    }
    let cmds = ns.handle_key(key('u'), &mut ctx);
    assert!(cmds
        .iter()
        .any(|c| matches!(c, ViewCmd::SetNamespace(n) if n == "extra")));
    ctx.namespace = "extra".into();
    ns.on_filter("", &mut ctx);
    let names: Vec<String> = ns
        .table()
        .rows
        .iter()
        .map(|r| r.row.fields[0].clone())
        .collect();
    assert!(names.contains(&"extra+(*)".to_string()), "{names:?}");
}

#[tokio::test]
async fn filter_history_needs_rows() {
    let (mut ctx, mut rx) = context(fake());
    let mut b = open(&mut ctx, "v1/pods");
    loaded(&mut b, &mut ctx, &mut rx).await;
    // browser.go BufferActive pushes only while GetRowCount() > 1.
    b.on_filter("zzz", &mut ctx);
    assert!(!b.keeps_filter_history());
    b.on_filter("a", &mut ctx);
    assert!(b.keeps_filter_history());
}

#[tokio::test]
async fn context_rename_and_delete() {
    let f = fake();
    let acc = f.clone();
    let (mut ctx, mut rx) = context(f);
    let mut c = open(&mut ctx, "contexts");
    loaded(&mut c, &mut ctx, &mut rx).await;
    // context.go renameCmd: modal prefilled with the selected name.
    let cmds = c.handle_key(key('r'), &mut ctx);
    let on_ok = match cmds.into_iter().next() {
        Some(ViewCmd::Dialog(Dialog::Rename { name, on_ok })) => {
            assert_eq!(name, "fake");
            on_ok
        }
        _ => panic!("expected the rename dialog"),
    };
    assert!(on_ok(&mut ctx, "fake2".into()).is_empty());
    until(&mut c, &mut ctx, &mut rx, is_custom).await;
    assert!(acc
        .calls()
        .contains(&"rename_context fake fake2".to_string()));
    // The view picks the change up on its next refresh.
    c.handle_key(ctrl_key('r'), &mut ctx);
    until(&mut c, &mut ctx, &mut rx, is_data).await;
    assert_eq!(ids(&c), vec!["fake2"]);
    // deleteCmd: confirm, then DelContext.
    let cmds = c.handle_key(ctrl_key('d'), &mut ctx);
    let on_ok = match cmds.into_iter().next() {
        Some(ViewCmd::Dialog(Dialog::Confirm { title, msg, on_ok })) => {
            assert_eq!(
                (title.as_str(), msg.as_str()),
                ("Delete", "Delete context \"fake2\"?")
            );
            on_ok
        }
        _ => panic!("expected the delete confirm"),
    };
    on_ok(&mut ctx);
    until(&mut c, &mut ctx, &mut rx, is_custom).await;
    assert!(acc.calls().contains(&"delete_context fake2".to_string()));
}

#[tokio::test]
async fn zero_key_needs_cluster_wide_namespace_list() {
    let f = FakeAccessor::new();
    f.add("v1/pods", pod("default", "a", "Running"));
    f.denied
        .lock()
        .unwrap()
        .push(("v1/namespaces".into(), String::new()));
    let (mut ctx, mut rx) = context(Arc::new(f));
    let mut b = open(&mut ctx, "v1/pods");
    loaded(&mut b, &mut ctx, &mut rx).await;
    while visible_hints(&b).contains(&("0".into(), "all".into())) {
        until(&mut b, &mut ctx, &mut rx, is_custom).await;
    }
    // browser.go namespaceActions: no `0` all; favorites start at `0`.
    let h = visible_hints(&b);
    assert!(h.contains(&("0".into(), "default".into())), "{h:?}");
    assert!(h.contains(&("1".into(), "kube-system".into())), "{h:?}");
}

#[tokio::test]
async fn typed_filter_lands_on_the_next_refresh() {
    let (mut ctx, mut rx) = context(fake());
    let mut b = open(&mut ctx, "v1/pods");
    loaded(&mut b, &mut ctx, &mut rx).await;
    let all = ids(&b).len();
    // ui.Table filters on its next model refresh, not per keystroke.
    b.on_filter_typing("b", &mut ctx);
    assert_eq!(ids(&b).len(), all);
    assert_eq!(b.filter_text(), "");
    assert_eq!(b.title(), format!(" pods(default)[{all}] "));
    // keyEntryDelay (100ms) later Table.Filter retitles with the old count.
    let t = std::time::Instant::now();
    until(&mut b, &mut ctx, &mut rx, is_custom).await;
    assert!(t.elapsed() >= Duration::from_millis(90));
    assert_eq!(b.title(), format!(" pods(default)[{all}] </b> "));
    assert_eq!(ids(&b).len(), all);
    b.on_event(&AppEvent::Tick, &mut ctx);
    assert_eq!(ids(&b), vec!["default/b"]);
    assert_eq!(b.title(), " pods(default)[1] </b> ");
    // Enter applies at once.
    b.on_filter("running", &mut ctx);
    assert_eq!(ids(&b), vec!["default/a"]);
}

#[tokio::test]
async fn copy_flashes_clipboard_errors() {
    let (mut ctx, mut rx) = context(fake());
    let g = Gvr::parse("v1/pods");
    let kind = kind_for(&g);
    let meta = meta_for(kind, &g, &ctx.resources).unwrap();
    let mut d = deps();
    d.clipboard = |_| Err(anyhow::anyhow!("clipboard unavailable"));
    let mut b = Browser::new(&mut ctx, g, kind, meta, d, Opts::default());
    loaded(&mut b, &mut ctx, &mut rx).await;
    // table.go cpCmd: flash.Err on a failed write, no success flash.
    let cmds = b.handle_key(key('c'), &mut ctx);
    assert_eq!(
        flashes(&cmds),
        vec![(FlashLevel::Err, "clipboard unavailable".to_string())]
    );
}

#[tokio::test]
async fn namespace_and_danger_keys_need_a_connection() {
    let (mut ctx, mut rx) = context(fake());
    ctx.con_ok = false;
    let mut b = open(&mut ctx, "v1/pods");
    loaded(&mut b, &mut ctx, &mut rx).await;
    // browser.go refreshActions: namespaceActions and edit/delete need ConOK.
    let h = visible_hints(&b);
    assert!(
        !h.iter()
            .any(|(m, _)| m == "0" || m == "w" || m == "e" || m == "ctrl-d"),
        "{h:?}"
    );
    assert!(has_hint(&b, "d", "Describe"));
}

/// views.go getVS via table.go Start: `gvr@ns-regex` keys win for matching
/// namespaces, the alias form is tried before the gvr, and the setting is
/// re-resolved when the view starts again in another namespace.
#[tokio::test]
async fn namespace_scoped_custom_views() {
    let (mut ctx, _rx) = context(fake());
    let vs = |c: &[&str]| crate::config::ViewSetting {
        columns: c.iter().map(|s| s.to_string()).collect(),
        sort_column: String::new(),
    };
    ctx.views.views.insert("v1/pods".into(), vs(&["NAME"]));
    ctx.views
        .views
        .insert("v1/pods@kube-.*".into(), vs(&["NAME", "IP"]));
    let b = open_opts(
        &mut ctx,
        "v1/pods",
        Opts {
            ns: Some("default".into()),
            ..Default::default()
        },
    );
    assert_eq!(b.view_setting.as_ref().unwrap().columns, ["NAME"]);
    let mut b = open_opts(
        &mut ctx,
        "v1/pods",
        Opts {
            ns: Some("kube-system".into()),
            ..Default::default()
        },
    );
    assert_eq!(b.view_setting.as_ref().unwrap().columns, ["NAME", "IP"]);
    b.set_namespace("default");
    assert!(b.apply_view_setting(&ctx));
    assert_eq!(b.view_setting.as_ref().unwrap().columns, ["NAME"]);
    // A setting keyed by the typed alias.
    ctx.views.views.insert("po".into(), vs(&["AGE"]));
    let b = open_opts(
        &mut ctx,
        "v1/pods",
        Opts {
            alias: "po".into(),
            ..Default::default()
        },
    );
    assert_eq!(b.view_setting.as_ref().unwrap().columns, ["AGE"]);
}

/// ui.useFullGVRTitle shows `apps/v1/deployments` instead of `deployments`.
#[tokio::test]
async fn full_gvr_title() {
    let (mut ctx, _rx) = context(fake());
    let b = open(&mut ctx, "apps/v1/deployments");
    assert_eq!(b.title_parts().0, "deployments");
    ctx.config.write().unwrap().k9s.ui.use_full_gvr_name = true;
    let b = open(&mut ctx, "apps/v1/deployments");
    assert_eq!(b.title_parts().0, "apps/v1/deployments");
    let b = open(&mut ctx, "v1/pods");
    assert_eq!(b.title_parts().0, "v1/pods");
}

/// exec.go k9sShellPod: imagePullPolicy and imagePullSecrets from shellPod.
#[test]
fn shell_pod_pull_settings() {
    let mut cfg = crate::config::ShellPod {
        image: "busybox:1.37.0".into(),
        namespace: "default".into(),
        ..Default::default()
    };
    let m = super::actions::shell_pod_manifest("n1", "k9rs-shell-1", &cfg);
    assert!(m.pointer("/spec/imagePullSecrets").is_none());
    assert!(m.pointer("/spec/containers/0/imagePullPolicy").is_none());
    cfg.image_pull_policy = "IfNotPresent".into();
    cfg.image_pull_secrets = vec![crate::config::k9s::LocalObjectReference { name: "reg".into() }];
    let m = super::actions::shell_pod_manifest("n1", "k9rs-shell-1", &cfg);
    assert_eq!(m.pointer("/spec/imagePullSecrets/0/name").unwrap(), "reg");
    assert_eq!(
        m.pointer("/spec/containers/0/imagePullPolicy").unwrap(),
        "IfNotPresent"
    );
    assert_eq!(
        m.pointer("/spec/containers/0/resources/limits/cpu")
            .unwrap(),
        "100m"
    );
}

/// vul_extender.go: `v`/`shift-v` only with imageScans.enable; `v` opens the
/// scans view for the selection (no rows without a scanner).
#[tokio::test]
async fn vulnerability_keys_and_scans_view() {
    let (mut ctx, mut rx) = context(fake());
    let mut p = open(&mut ctx, "v1/pods");
    loaded(&mut p, &mut ctx, &mut rx).await;
    assert!(!has_hint(&p, "v", "Show Vulnerabilities"));
    ctx.config.write().unwrap().k9s.image_scans.enable = true;
    let mut p = open(&mut ctx, "v1/pods");
    loaded(&mut p, &mut ctx, &mut rx).await;
    assert!(visible_hints(&p).contains(&("v".into(), "Show Vulnerabilities".into())));
    assert!(has_hint(&p, "shift-v", "Sort Vulnerabilities"));
    assert!(!visible_hints(&p).iter().any(|(m, _)| m == "shift-v"));
    let mut v = pushed_browser(p.handle_key(key('v'), &mut ctx));
    v.start(&mut ctx);
    let sc = v.as_any_mut().downcast_mut::<Browser>().unwrap();
    assert_eq!(sc.gvr().to_string(), "scans");
    until(sc, &mut ctx, &mut rx, is_data).await;
    let names: Vec<String> = sc
        .renderer
        .header("")
        .iter()
        .map(|h| h.name.clone())
        .collect();
    assert_eq!(
        names,
        [
            "SEVERITY",
            "VULNERABILITY",
            "IMAGE",
            "LIBRARY",
            "VERSION",
            "FIXED-IN",
            "TYPE"
        ]
    );
    assert_eq!(sc.table.sort.name, "SEVERITY");
    assert_eq!(sc.path, "default/a", "the title shows the scanned resource");
    assert!(has_hint(sc, "shift-l", "Sort Lib") && has_hint(sc, "shift-f", "Sort Fixed-in"));
    assert!(!has_hint(sc, "shift-a", "Sort Age"));
}

#[test]
fn cve_urls() {
    assert_eq!(
        super::actions::cve_url("img|lib|1|2|apk|CVE-1|SEV-1").as_deref(),
        Some("https://nvd.nist.gov/vuln/detail/CVE-1")
    );
    assert_eq!(
        super::actions::cve_url("img|lib|1|2|npm|GHSA-x|SEV-2").as_deref(),
        Some("https://github.com/advisories/GHSA-x")
    );
    assert_eq!(super::actions::cve_url("img|lib"), None);
}

/// logs_extender.go showLogs: CanForResource(ns, pods, list) gates the view.
#[tokio::test]
async fn logs_need_pod_list_access() {
    let f = fake();
    f.denied
        .lock()
        .unwrap()
        .push(("v1/pods".into(), "default".into()));
    let (mut ctx, mut rx) = context(f);
    let mut p = open(&mut ctx, "apps/v1/deployments");
    loaded(&mut p, &mut ctx, &mut rx).await;
    p.handle_key(key('l'), &mut ctx);
    let cmds = until(&mut p, &mut ctx, &mut rx, |e| {
        matches!(e, AppEvent::Error { .. })
    })
    .await;
    assert_eq!(
        flashes(&cmds),
        vec![(
            FlashLevel::Err,
            "[list] access denied on resource \"default\":\"v1/pods\"".to_string()
        )]
    );
}

/// exec.go pipe errors as k9s flashes them after a shell/attach/edit.
#[cfg(unix)]
#[test]
fn exec_failure_text_like_go() {
    use std::os::unix::process::ExitStatusExt;
    let st = std::process::ExitStatus::from_raw(1 << 8);
    assert_eq!(
        super::exec_failure(&st).as_deref(),
        Some("command failed. Check k9rs logs: exit status 1")
    );
    assert_eq!(
        super::exec_failure(&std::process::ExitStatus::from_raw(0)),
        None
    );
    // Killed by a signal: no error.
    assert_eq!(
        super::exec_failure(&std::process::ExitStatus::from_raw(9)),
        None
    );
}

/// helm_history.go: ids without `:revision` flash on Enter; a non-numeric
/// revision fails the rollback like strconv.Atoi.
#[tokio::test]
async fn helm_history_bad_revision_ids() {
    let (mut ctx, mut rx) = context(fake());
    let mut h = open_opts(
        &mut ctx,
        "helm-history",
        Opts {
            path: "default/web".into(),
            ..Default::default()
        },
    );
    let cmds = h.helm_values(Some("default/web".into()), &ctx);
    assert_eq!(
        flashes(&cmds),
        vec![(
            FlashLevel::Err,
            "unable to parse version in \"default/web\"".to_string()
        )]
    );
    assert_eq!(h.helm_target("default/web:x:y").1.as_deref(), Some(""));
    let on_ok = match h
        .helm_rollback(Some("default/web".into()), &ctx)
        .into_iter()
        .next()
    {
        Some(ViewCmd::Dialog(Dialog::Confirm { on_ok, .. })) => on_ok,
        _ => panic!("expected rollback confirm"),
    };
    on_ok(&mut ctx);
    let cmds = until(&mut h, &mut ctx, &mut rx, |e| {
        matches!(e, AppEvent::Error { .. })
    })
    .await;
    assert_eq!(
        flashes(&cmds),
        vec![(
            FlashLevel::Err,
            "could not convert revision to a number: strconv.Atoi: parsing \"\": invalid syntax"
                .to_string()
        )]
    );
}

/// crd.go showCRD: Enter runs the CRD name as a command.
#[tokio::test]
async fn crd_enter_runs_name_as_command() {
    let f = fake();
    f.add(
        "apiextensions.k8s.io/v1/customresourcedefinitions",
        obj(json!({"apiVersion": "apiextensions.k8s.io/v1", "kind": "CustomResourceDefinition",
            "metadata": {"name": "certificates.cert-manager.io"},
            "spec": {"group": "cert-manager.io", "names": {"plural": "certificates", "kind": "Certificate"},
                "scope": "Namespaced", "versions": [{"name": "v1", "served": true, "storage": true}]}})),
    );
    let (mut ctx, mut rx) = context(f);
    let mut c = open(
        &mut ctx,
        "apiextensions.k8s.io/v1/customresourcedefinitions",
    );
    loaded(&mut c, &mut ctx, &mut rx).await;
    let cmds = c.handle_key(code(KeyCode::Enter), &mut ctx);
    assert!(
        matches!(&cmds[..], [ViewCmd::Goto { cmd, path }] if cmd == "certificates.cert-manager.io" && path.is_empty()),
        "{:?}",
        cmds.len()
    );
}

#[tokio::test]
async fn clashing_hotkey_warns_on_the_logo() {
    let (mut ctx, mut rx) = context(fake());
    ctx.hotkeys.hot_keys.insert(
        "dup".into(),
        crate::config::HotKey {
            short_cut: "d".into(),
            command: "svc".into(),
            ..Default::default()
        },
    );
    let mut b = open(&mut ctx, "v1/pods");
    // hotKeyActions: "duplicate hotkey" -> Logo().Warn on refreshActions.
    let cmds = loaded(&mut b, &mut ctx, &mut rx).await;
    assert!(cmds
        .iter()
        .any(|c| matches!(c, ViewCmd::LogoWarn(m) if m == "HotKeys load failed!")));
    assert!(has_hint(&b, "d", "Describe"), "the view key stays");
    ctx.hotkeys.hot_keys.get_mut("dup").unwrap().override_ = true;
    b.handle_key(ctrl_key('r'), &mut ctx);
    let cmds = until(&mut b, &mut ctx, &mut rx, is_data).await;
    assert!(!cmds.iter().any(|c| matches!(c, ViewCmd::LogoWarn(_))));
}

/// Table-view plugins share exec.go's pipe handling: each pipe is split on
/// spaces and quoted (no shell expansion); pipes under 2 tokens are dropped.
#[tokio::test]
async fn plugin_pipes_like_k9s() {
    let (mut ctx, mut rx) = context(fake());
    ctx.plugins.plugins.insert(
        "p".into(),
        crate::config::Plugin {
            scopes: vec!["po".into()],
            short_cut: "Shift-T".into(),
            description: "Pipe".into(),
            command: "echo".into(),
            args: vec!["$NAME".into()],
            pipes: vec!["grep $HOME".into(), "bad".into()],
            ..Default::default()
        },
    );
    ctx.aliases.aliases.insert("po".into(), "v1/pods".into());
    let mut b = open(&mut ctx, "v1/pods");
    loaded(&mut b, &mut ctx, &mut rx).await;
    let cmds = b.handle_key(key('T'), &mut ctx);
    let [ViewCmd::Suspend { argv, .. }] = cmds.as_slice() else {
        panic!("expected suspend");
    };
    assert_eq!(argv[0], "sh");
    assert_eq!(argv[2], "echo a | 'grep' '$HOME'");
}

/// exec.go oneShoot: a failed command keeps its output and Go's exit text
/// (dir.go shows both in "Applied Manifest").
#[tokio::test]
async fn runner_failure_keeps_output() {
    let run = super::default_runner();
    let e = run(
        vec![
            "sh".into(),
            "-c".into(),
            "echo out; echo err >&2; exit 3".into(),
        ],
        vec![],
    )
    .await
    .unwrap_err();
    assert_eq!(e.to_string(), "err");
    let f = e.downcast_ref::<super::RunFailed>().unwrap();
    assert_eq!(
        (f.status.as_str(), f.output.as_str()),
        ("exit status 3", "out\nerr")
    );
}

/// dao.ReplicaSet.Rollback -> kubectl DeploymentRollbacker: JSON patch of the
/// template (hash label dropped) and annotations; skip when current.
#[tokio::test]
async fn rs_rollback_like_kubectl() {
    let f = FakeAccessor::new();
    let owner = json!([{"apiVersion": "apps/v1", "kind": "Deployment", "name": "web", "uid": "dp-1", "controller": true}]);
    let tpl = |img: &str, hash: &str| {
        json!({"metadata": {"labels": {"app": "web", "pod-template-hash": hash}},
        "spec": {"containers": [{"name": "c", "image": img}]}})
    };
    f.add("apps/v1/deployments", obj(json!({"apiVersion": "apps/v1", "kind": "Deployment",
        "metadata": {"name": "web", "namespace": "default", "uid": "dp-1",
            "annotations": {"deployment.kubernetes.io/revision": "2", "keep": "dp"}},
        "spec": {"selector": {"matchLabels": {"app": "web"}},
            "template": {"metadata": {"labels": {"app": "web"}}, "spec": {"containers": [{"name": "c", "image": "v2"}]}}}})));
    for (n, rev, img, replicas) in [
        ("web-1", "1", "v1", 0),
        ("web-2", "2", "v2", 0),
        ("web-3", "3", "v3", 1),
    ] {
        f.add("apps/v1/replicasets", obj(json!({"apiVersion": "apps/v1", "kind": "ReplicaSet",
            "metadata": {"name": n, "namespace": "default", "labels": {"app": "web"}, "ownerReferences": owner,
                "annotations": {"deployment.kubernetes.io/revision": rev, "from": n}},
            "spec": {"template": tpl(img, n)}, "status": {"replicas": replicas}})));
    }
    let f = Arc::new(f);
    let acc = || -> crate::k8s::SharedAccessor { f.clone() };
    super::actions::rollback_rs(acc(), "default/web-1".into())
        .await
        .unwrap();
    let calls = f.calls();
    assert_eq!(calls.len(), 1, "{calls:?}");
    let patch: serde_json::Value = serde_json::from_str(
        calls[0]
            .strip_prefix("patch_json apps/v1/deployments default/web ")
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        patch,
        json!([
            {"op": "replace", "path": "/spec/template", "value":
                {"metadata": {"labels": {"app": "web"}}, "spec": {"containers": [{"name": "c", "image": "v1"}]}}},
            {"op": "replace", "path": "/metadata/annotations", "value":
                {"deployment.kubernetes.io/revision": "2", "from": "web-1"}},
        ])
    );
    // Revision 2 already matches the deployment template: skipped, no patch.
    super::actions::rollback_rs(acc(), "default/web-2".into())
        .await
        .unwrap();
    assert_eq!(f.calls().len(), 1);
    // A replica set with live replicas is the current one.
    let e = super::actions::rollback_rs(acc(), "default/web-3".into())
        .await
        .unwrap_err();
    assert_eq!(e.to_string(), "can not rollback current replica");
}
