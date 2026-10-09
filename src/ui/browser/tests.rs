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
        server_table: crate::render::core::render_server_table,
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

#[tokio::test]
async fn empty_result_warns() {
    let (mut ctx, mut rx) = context(fake());
    let mut b = open(&mut ctx, "v1/endpoints");
    let cmds = loaded(&mut b, &mut ctx, &mut rx).await;
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
    let d = open(&mut ctx, "apps/v1/deployments");
    for (m, desc) in [
        ("s", "Scale"),
        ("r", "Restart"),
        ("z", "ReplicaSets"),
        ("l", "Logs"),
        ("shift-f", "Port-Forward"),
    ] {
        assert!(has_hint(&d, m, desc), "deploy {m} {desc}");
    }
    let n = open(&mut ctx, "v1/nodes");
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
    let cj = open(&mut ctx, "batch/v1/cronjobs");
    assert!(has_hint(&cj, "t", "Trigger") && has_hint(&cj, "s", "Suspend/Resume"));
    let rs = open(&mut ctx, "apps/v1/replicasets");
    assert!(has_hint(&rs, "ctrl-l", "Rollback") && !has_hint(&rs, "s", "Scale"));
    let sec = open(&mut ctx, "v1/secrets");
    assert!(has_hint(&sec, "x", "Decode") && has_hint(&sec, "u", "UsedBy"));
    let ctxs = open(&mut ctx, "contexts");
    assert!(!has_hint(&ctxs, "y", "YAML") && !has_hint(&ctxs, "ctrl-d", "Delete"));
    assert!(!ctxs.actions().contains_key(&KEY_SPACE));
    let al = open(&mut ctx, "aliases");
    assert!(has_hint(&al, "enter", "Goto"));
    assert!(!al.actions().contains_key(&ctrl('s')));
    let pf = open(&mut ctx, "portforwards");
    assert!(
        has_hint(&pf, "enter", "View Benchmarks")
            && has_hint(&pf, "ctrl-d", "Delete")
            && has_hint(&pf, "b", "Benchmark Run/Stop")
    );
    let co = open_opts(
        &mut ctx,
        "containers",
        Opts {
            path: "default/a".into(),
            ..Default::default()
        },
    );
    assert!(has_hint(&co, "shift-f", "PortForward") && has_hint(&co, "s", "Shell"));
    assert!(!co.actions().contains_key(&KEY_SPACE));
    assert!(!has_hint(&co, "e", "Edit") && !has_hint(&co, "y", "YAML"));
    let ev = open(&mut ctx, "v1/events");
    assert!(!has_hint(&ev, "ctrl-d", "Delete") && !has_hint(&ev, "e", "Edit"));
    let gen = open(&mut ctx, "networking.k8s.io/v1/ingresses");
    assert!(has_hint(&gen, "shift-j", "Jump Owner") && !has_hint(&gen, "s", "Scale"));
    let wk = open(&mut ctx, "workloads");
    assert!(has_hint(&wk, "y", "YAML") && has_hint(&wk, "ctrl-d", "Delete"));
    let helm = open(&mut ctx, "helm");
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
    assert_eq!(
        flashes(&cmds),
        vec![(
            FlashLevel::Info,
            "Plugin command launched successfully: \"Debug\"".to_string()
        )]
    );
    assert!(!runs_matching("echo default a").is_empty());
    let cmds = b.handle_key(key('H'), &mut ctx);
    assert!(
        matches!(&cmds[..], [ViewCmd::Command(c)] if c == "dp"),
        "hotkey"
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
    let v = pushed_browser(cmds);
    assert_eq!(as_browser(v.as_ref()).namespace(), "kube-system");
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
    assert_eq!(
        pushed(co.handle_key(code(KeyCode::Enter), &mut ctx)).name(),
        "logs default|a|Some(\"web\")|false|false"
    );
    assert_eq!(
        pushed(co.handle_key(key('p'), &mut ctx)).name(),
        "logs default|a|Some(\"web\")|false|true"
    );
    co.handle_key(key('s'), &mut ctx);
    let cmds = until(&mut co, &mut ctx, &mut rx, is_custom).await;
    match &cmds[..] {
        [ViewCmd::Suspend { argv, .. }] => {
            assert_eq!(
                argv[..6],
                ["kubectl", "exec", "-it", "-n", "default", "a"].map(String::from)
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
        [ViewCmd::Suspend { argv, .. }] => {
            assert_eq!(argv.join(" "), "kubectl attach -it -n default a -c web");
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
        matches!(&cmds[..], [ViewCmd::Suspend { argv, .. }] if argv.join(" ") == "kubectl edit pods/a -n default")
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
        Some(ViewCmd::Dialog(Dialog::Confirm { title, msg, on_ok })) => {
            assert_eq!(
                (title.as_str(), msg.as_str()),
                ("Confirm Restart", "Restart deployment default/web?")
            );
            on_ok
        }
        _ => panic!("expected restart confirm"),
    };
    on_ok(&mut ctx);
    let cmds = until(&mut d, &mut ctx, &mut rx, is_custom).await;
    assert_eq!(
        flashes(&cmds),
        vec![(
            FlashLevel::Info,
            "Restart in progress for `default/web...".to_string()
        )]
    );
    d.handle_key(key('i'), &mut ctx);
    let cmds = until(&mut d, &mut ctx, &mut rx, is_custom).await;
    let on_ok = match cmds.into_iter().next() {
        Some(ViewCmd::Dialog(Dialog::Prompt { value, on_ok, .. })) => {
            assert_eq!(value, "web=nginx:1");
            on_ok
        }
        _ => panic!("expected image prompt"),
    };
    on_ok(&mut ctx, "web=nginx:2".into());
    until(&mut d, &mut ctx, &mut rx, is_custom).await;
    assert!(f
        .calls()
        .contains(&"set_images apps/v1/deployments default/web web=nginx:2".to_string()));
    // z: replicasets of the deployment
    d.handle_key(key('z'), &mut ctx);
    let v = pushed_browser(until(&mut d, &mut ctx, &mut rx, is_custom).await);
    assert_eq!(as_browser(v.as_ref()).label(), Some("app=web"));
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
    on_ok(&mut ctx, Default::default());
    let v = pushed(until(&mut n, &mut ctx, &mut rx, is_custom).await);
    assert_eq!(v.name(), "text Drain Progress|nodes|node/node1 drained\n");
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
    assert!(matches!(cmds.as_slice(), [ViewCmd::Command(c)] if c == "ctx fake"));
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
        Some(ViewCmd::Dialog(Dialog::PortForward { spec, ports, on_ok })) => {
            assert_eq!(ports, vec!["web::80(http)"]);
            assert_eq!(
                (
                    spec.container.as_str(),
                    spec.container_port,
                    spec.local_port
                ),
                ("web", 80, 80)
            );
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
    on_ok(&mut ctx, spec.clone());
    let cmds = until(&mut b, &mut ctx, &mut rx, is_custom).await;
    assert_eq!(
        flashes(&cmds),
        vec![(FlashLevel::Info, "PortForward activated 9090".to_string())]
    );
    assert!(ctx.port_forwards.contains_key(&spec.id()));
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
        body.starts_with("NAME,READY,STATUS,RESTARTS,IP,VALID,AGE\na,1/1,Running"),
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
}

#[tokio::test]
async fn helm_releases_and_history() {
    let (mut ctx, mut rx) = context(fake());
    let mut h = open(&mut ctx, "helm");
    loaded(&mut h, &mut ctx, &mut rx).await;
    assert_eq!(ids(&h), vec!["default/web"]);
    assert!(!runs_matching("helm list -o json -n default --kube-context fake").is_empty());
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
    assert!(
        t.name()
            .starts_with("text Values|default/web|ran helm get values web -n default --revision 2"),
        "{}",
        t.name()
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
    let b = open(&mut ctx, "v1/configmaps");
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
