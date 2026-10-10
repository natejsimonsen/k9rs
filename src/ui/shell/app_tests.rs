//! App shell tests: dummy views, pre-built commands, the fake accessor and a
//! ratatui `TestBackend`.

use std::any::Any;
use std::sync::{Arc, Mutex};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::{Frame, Terminal};

use super::*;
use crate::config::{HotKey, Styles};
use crate::k8s::fake::FakeAccessor;
use crate::ui::shell::text::testutil::{fg, find, row};
use crate::ui::{Hint, ViewId};

type Log = Arc<Mutex<Vec<String>>>;

/// A scriptable view: records lifecycle calls and returns canned commands.
struct Dummy {
    id: ViewId,
    name: String,
    title: String,
    hints: Vec<Hint>,
    log: Log,
    on_key: Vec<(char, Box<dyn Fn() -> Vec<ViewCmd> + Send>)>,
    filter: String,
    full: bool,
}

impl Dummy {
    fn new(ctx: &mut Ctx, name: &str, log: &Log) -> Dummy {
        Dummy {
            id: ctx.new_view_id(),
            name: name.into(),
            title: format!("{name}(default)[0]"),
            hints: vec![Hint::new("d", "Describe"), Hint::new("0", "all")],
            log: log.clone(),
            on_key: Vec::new(),
            filter: String::new(),
            full: false,
        }
    }
    fn with_key(mut self, c: char, f: impl Fn() -> Vec<ViewCmd> + Send + 'static) -> Self {
        self.on_key.push((c, Box::new(f)));
        self
    }
    fn rec(&self, s: String) {
        self.log.lock().unwrap().push(format!("{}:{s}", self.name));
    }
}

impl View for Dummy {
    fn id(&self) -> ViewId {
        self.id
    }
    fn name(&self) -> String {
        self.name.clone()
    }
    fn title(&self) -> String {
        self.title.clone()
    }
    fn hints(&self) -> Vec<Hint> {
        self.hints.clone()
    }
    fn start(&mut self, _ctx: &mut Ctx) -> Vec<ViewCmd> {
        self.rec("start".into());
        Vec::new()
    }
    fn stop(&mut self, _ctx: &mut Ctx) {
        self.rec("stop".into());
    }
    fn handle_key(&mut self, key: KeyEvent, _ctx: &mut Ctx) -> Vec<ViewCmd> {
        self.rec(format!("key {}", crate::ui::keys::key_name(&key)));
        if let KeyCode::Char(c) = key.code {
            if let Some((_, f)) = self.on_key.iter().find(|(k, _)| *k == c) {
                return f();
            }
        }
        if key.code == KeyCode::Esc {
            return vec![ViewCmd::Pop];
        }
        Vec::new()
    }
    fn on_event(&mut self, ev: &AppEvent, _ctx: &mut Ctx) -> Vec<ViewCmd> {
        let name = match ev {
            AppEvent::Tick => "tick",
            AppEvent::Data { .. } => "data",
            AppEvent::Text { .. } => "text",
            AppEvent::ClusterInfo(_) => "cluster",
            AppEvent::Resources(_) => "resources",
            AppEvent::Namespaces(_) => "namespaces",
            _ => "other",
        };
        self.rec(format!("event {name}"));
        Vec::new()
    }
    fn on_filter(&mut self, text: &str, _ctx: &mut Ctx) -> Vec<ViewCmd> {
        self.filter = text.to_string();
        self.rec(format!("filter {text}"));
        Vec::new()
    }
    fn filter_text(&self) -> String {
        self.filter.clone()
    }
    // "nope" stands in for a filter that leaves no rows.
    fn keeps_filter_history(&self) -> bool {
        self.filter != "nope"
    }
    fn is_fullscreen(&self) -> bool {
        self.full
    }
    fn owns_frame(&self) -> bool {
        self.full
    }
    fn draw(&mut self, frame: &mut Frame<'_>, area: Rect, _ctx: &Ctx) {
        let buf = frame.buffer_mut();
        if area.width > 0 && area.height > 0 {
            if self.full {
                let last = area.bottom() - 1;
                buf.set_string(area.x, last, "last row", ratatui::style::Style::default());
            }
            buf.set_string(
                area.x,
                area.y,
                format!("body of {}", self.name),
                ratatui::style::Style::default(),
            );
        }
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

struct Harness {
    app: App,
    log: Log,
    opened: Arc<Mutex<Vec<Command>>>,
    persisted: Arc<Mutex<Vec<(String, String, Option<String>, Option<String>)>>>,
    fake: Arc<FakeAccessor>,
}

fn harness_with(fake: FakeAccessor) -> Harness {
    let fake = Arc::new(fake);
    let acc: SharedAccessor = fake.clone();
    let config: SharedConfig = Default::default();
    let mut app = App::bootstrap(
        acc,
        config,
        Styles::default(),
        "default".into(),
        AppOptions {
            splashless: true,
            // The kubeconfig contexts run_app reads at startup.
            contexts: fake.contexts.lock().unwrap().clone(),
            ..Default::default()
        },
    );
    let log: Log = Default::default();
    let opened: Arc<Mutex<Vec<Command>>> = Default::default();
    let persisted: Arc<Mutex<Vec<(String, String, Option<String>, Option<String>)>>> =
        Default::default();
    let (l2, o2) = (log.clone(), opened.clone());
    app.hooks.browser = Box::new(move |ctx, cmd| {
        o2.lock().unwrap().push(cmd.clone());
        let name = Gvr::parse(&cmd.gvr).resource;
        if name == "bogus" {
            return Err(anyhow!("unknown resource bogus"));
        }
        let mut d = Dummy::new(ctx, &name, &l2);
        d.title = match &cmd.namespace {
            Some(ns) => format!("{name}({ns})[0]"),
            None => format!("{name}[0]"),
        };
        Ok(Box::new(d) as Box<dyn View>)
    });
    let l3 = log.clone();
    app.hooks.xray = Box::new(move |ctx, gvr, ns| {
        let mut d = Dummy::new(ctx, "xray", &l3);
        d.title = format!("Xray-{}({ns})", gvr.resource);
        Box::new(d)
    });
    let l4 = log.clone();
    app.hooks.pulses = Box::new(move |ctx, _ns| Box::new(Dummy::new(ctx, "pulses", &l4)));
    let l5 = log.clone();
    app.hooks.add_resources = Box::new(move |a, r| {
        l5.lock()
            .unwrap()
            .push(format!("add_resources {}", r.len()));
        a.define("v1/pods", &["po"]);
    });
    app.hooks.load_context = Box::new(|_, ctx| {
        let mut cc = ContextConfig::default();
        cc.namespace.active = format!("{ctx}-ns");
        cc.view.active = "svc".into();
        Ok(cc)
    });
    let p2 = persisted.clone();
    app.hooks.persist = Box::new(move |cl, ct, cc| {
        p2.lock().unwrap().push((
            cl.into(),
            ct.into(),
            Some(cc.view.active.clone()),
            Some(cc.namespace.active.clone()),
        ));
        Ok(())
    });
    app.hooks.save_config = Box::new(|_| Ok(()));
    app.ctx.resources = crate::k8s::fake::default_resources();
    for (g, aa) in [
        ("v1/pods", &["po", "pod", "pods", "v1/pods"][..]),
        ("v1/services", &["svc", "service", "v1/services"][..]),
        ("v1/configmaps", &["cm", "v1/configmaps"][..]),
        (
            "v1/namespaces",
            &["ns", "namespace", "namespaces", "v1/namespaces"][..],
        ),
        (
            "apps/v1/deployments",
            &["dp", "deploy", "apps/v1/deployments"][..],
        ),
    ] {
        app.ctx.aliases.define(g, aa);
    }
    Harness {
        app,
        log,
        opened,
        persisted,
        fake,
    }
}

fn harness() -> Harness {
    harness_with(FakeAccessor::new())
}

fn key(c: KeyCode) -> KeyEvent {
    KeyEvent::new(c, KeyModifiers::NONE)
}
fn ch(c: char) -> KeyEvent {
    key(KeyCode::Char(c))
}
fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

impl Harness {
    fn typ(&mut self, s: &str) {
        for c in s.chars() {
            self.app.handle_key(ch(c));
        }
    }
    fn render(&mut self, w: u16, h: u16) -> Buffer {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| self.app.draw(f)).unwrap();
        t.backend().buffer().clone()
    }
    fn names(&self) -> Vec<String> {
        self.app.stack.iter().map(|v| v.name()).collect()
    }
    fn log(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
    fn clear_log(&self) {
        self.log.lock().unwrap().clear();
    }
    fn goto(&mut self, line: &str) {
        self.app.goto(line, true, true);
    }
    fn flash(&self) -> Option<(FlashLevel, String)> {
        self.app.flash.current().map(|(l, s)| (l, s.to_string()))
    }
    fn dialog_text(&self) -> Option<String> {
        self.app.dialog.as_ref().map(|d| d.text.clone())
    }
}

#[test]
fn command_prompt_navigates_and_persists() {
    let mut h = harness();
    h.app.handle_key(ch(':'));
    assert!(h.app.prompt.is_some());
    h.typ("po");
    h.app.handle_key(key(KeyCode::Enter));
    assert!(h.app.prompt.is_none());
    assert_eq!(h.names(), ["pods"]);
    let opened = h.opened.lock().unwrap().clone();
    assert_eq!(opened[0].gvr, "v1/pods");
    assert_eq!(opened[0].namespace.as_deref(), Some("default"));
    // command.go run: p.SwitchNS(ns) puts the namespace in history and view.active.
    assert_eq!(h.app.cmd_history.list(), ["v1/pods default"]);
    // k9s only records the view in memory (Config.SetActiveView); it is
    // saved at the next save point (was: saved on every navigation).
    assert!(h.persisted.lock().unwrap().is_empty());
    let cc = h.app.ctx.context_cfg.clone().unwrap();
    assert_eq!(cc.view.active, "v1/pods default");
    h.app.save_cmd();
    let p = h.persisted.lock().unwrap().clone();
    assert_eq!(p.last().unwrap().2.as_deref(), Some("v1/pods default"));
    assert_eq!(p.last().unwrap().1, "fake");
    assert!(h.log().contains(&"pods:start".to_string()));
}

#[test]
fn command_args_follow_k9s_merge_and_switch_ns() {
    let mut h = harness();
    // Merge prints the fuzzy arg bare; SwitchNS re-groks it into the ns slot,
    // so k9s drops `-f` from commands.
    h.goto("po -f cdns");
    let last = h.opened.lock().unwrap().last().cloned().unwrap();
    assert_eq!(last.fuzzy, None);
    assert_eq!(last.namespace.as_deref(), Some("default"));
    h.goto("po kube-system /Core");
    let last = h.opened.lock().unwrap().last().cloned().unwrap();
    assert_eq!(last.filter.as_deref(), Some("core"));
    assert_eq!(last.namespace.as_deref(), Some("kube-system"));
    assert_eq!(
        h.app.cmd_history.list(),
        ["v1/pods cdns default", "v1/pods /core kube-system"]
    );
}

/// tview Flex.SetFullScreen: a fullscreen view covers the whole screen,
/// flash row included (k9s flashes are hidden there).
#[test]
fn fullscreen_view_covers_flash_row() {
    let mut h = harness();
    let log = h.log.clone();
    let mut d = Dummy::new(&mut h.app.ctx, "logs", &log);
    d.full = true;
    h.app.apply(vec![
        ViewCmd::Push(Box::new(d)),
        ViewCmd::Flash(FlashLevel::Warn, "careful".into()),
    ]);
    let buf = h.render(80, 10);
    assert!(row(&buf, 0).starts_with("body of logs"), "{}", row(&buf, 0));
    assert!(row(&buf, 9).starts_with("last row"), "{}", row(&buf, 9));
}

#[test]
fn renders_full_layout() {
    let mut h = harness();
    h.app.ctx.cluster_info = ClusterInfo {
        context: "fake".into(),
        cluster: "fake-cluster".into(),
        user: "fake-user".into(),
        k9s_version: "v0.50.18".into(),
        k9s_latest: String::new(),
        k8s_version: "v1.34.0".into(),
        cpu_pct: Some(10),
        mem_pct: Some(20),
    };
    h.app.ctx.has_metrics = true;
    h.app.node_mx_ok = true;
    h.goto("po");
    h.goto("svc");
    let log = h.log.clone();
    let d = Dummy::new(&mut h.app.ctx, "describe", &log);
    h.app.apply(vec![
        ViewCmd::Push(Box::new(d)),
        ViewCmd::Flash(FlashLevel::Warn, "careful".into()),
    ]);
    let st = Styles::default();
    let buf = h.render(130, 24);
    assert!(
        row(&buf, 0).starts_with(" Context:  fake [RW]"),
        "{}",
        row(&buf, 0)
    );
    assert!(row(&buf, 5).starts_with(" CPU:      10%"));
    // Menu from the top view.
    let (x, y) = find(&buf, "<0>").unwrap();
    assert_eq!(fg(&buf, x, y), st.menu_num_key);
    let (x, y) = find(&buf, "<d>").unwrap();
    assert_eq!(fg(&buf, x, y), st.menu_key);
    // Logo.
    assert!(find(&buf, crate::logo::LOGO_SMALL[4].trim_end()).is_some());
    // Frame with the styled title right under the header.
    let (x, y) = find(&buf, " describe(default)[0] ").unwrap();
    assert_eq!(y, 7);
    assert_eq!(fg(&buf, x + 10, y), st.title_highlight);
    assert_eq!(fg(&buf, 0, 7), st.border_focus);
    assert!(find(&buf, "body of describe").is_some());
    // Crumbs: only the views on the stack (svc reset the stack).
    let crumbs = row(&buf, 22);
    assert!(crumbs.starts_with("  <services>   <describe> "), "{crumbs}");
    let (x, y) = find(&buf, "<describe>").unwrap();
    assert_eq!(buf[(x, y)].bg, st.crumbs_active);
    // Flash.
    let (x, y) = find(&buf, "careful").unwrap();
    assert_eq!(y, 23);
    // tcell.ColorOrange is a named color: palette 214, not RGB.
    assert_eq!(fg(&buf, x, y), ratatui::style::Color::Indexed(214));
}

#[test]
fn header_and_crumb_toggles_and_headless() {
    let mut h = harness();
    h.goto("po");
    h.app.handle_key(ctrl('e'));
    let buf = h.render(100, 20);
    assert!(
        row(&buf, 0).contains("K9rs "),
        "indicator row: {}",
        row(&buf, 0)
    );
    assert!(find(&buf, " pods(default)[0] ").unwrap().1 == 1);
    h.app.handle_key(ctrl('g'));
    let buf = h.render(100, 20);
    assert!(!row(&buf, 18).contains("<pods>"));
    h.app.handle_key(ctrl('e'));
    h.app.handle_key(ctrl('g'));
    let buf = h.render(100, 20);
    assert!(row(&buf, 0).starts_with(" Context:"));
    assert!(row(&buf, 18).contains("<pods>"));

    // Fullscreen hides header and crumbs.
    h.app.apply(vec![ViewCmd::ToggleFullScreen]);
    let buf = h.render(100, 20);
    assert_eq!(find(&buf, " pods(default)[0] ").unwrap().1, 0);
    assert!(find(&buf, "<pods>").is_none());
}

#[test]
fn headless_logoless_crumbsless_options() {
    let fake = Arc::new(FakeAccessor::new());
    let mut app = App::bootstrap(
        fake,
        Default::default(),
        Styles::default(),
        "default".into(),
        AppOptions {
            headless: false,
            logoless: true,
            crumbsless: true,
            splashless: false,
            ..Default::default()
        },
    );
    assert!(app.show_header && !app.show_logo && !app.show_crumbs && app.show_splash);
    app.splash_until = Some(Instant::now() + Duration::from_secs(5));
    let mut t = Terminal::new(TestBackend::new(80, 15)).unwrap();
    t.draw(|f| app.draw(f)).unwrap();
    let rev = format!("Revision {}", crate::version::VERSION);
    assert!(find(t.backend().buffer(), &rev).is_some());
    assert!(app.expire(Instant::now() + Duration::from_secs(6)));
    t.draw(|f| app.draw(f)).unwrap();
    let buf = t.backend().buffer().clone();
    assert!(find(&buf, "Revision").is_none());
    assert!(find(&buf, "|____|").is_none(), "logoless");
}

#[test]
fn help_toggles_and_esc_pops() {
    let mut h = harness();
    h.goto("po");
    h.app.handle_key(ch('?'));
    assert_eq!(h.names(), ["pods", "help"]);
    let buf = h.render(160, 30);
    assert!(find(&buf, "RESOURCE").is_some());
    assert!(find(&buf, "<d>").is_some());
    assert!(find(&buf, " Help ").is_some());
    h.app.handle_key(ch('?'));
    assert_eq!(h.names(), ["pods"]);
    h.app.handle_key(ch('?'));
    h.app.handle_key(key(KeyCode::Esc));
    assert_eq!(h.names(), ["pods"]);
    // :help from the prompt
    h.app.handle_key(ch(':'));
    h.typ("help");
    h.app.handle_key(key(KeyCode::Enter));
    assert_eq!(h.names(), ["pods", "help"]);
    // `?` inside the prompt is text, not help.
    h.app.handle_key(key(KeyCode::Esc));
    h.app.handle_key(ch(':'));
    h.app.handle_key(ch('?'));
    assert_eq!(h.app.prompt.as_ref().unwrap().text, "?");
}

#[test]
fn ctrl_c_and_quit_commands() {
    let mut h = harness();
    h.app.handle_key(ctrl('c'));
    assert!(h.app.quit);

    let mut h = harness();
    h.app.ctx.config.write().unwrap().k9s.no_exit_on_ctrl_c = true;
    h.app.handle_key(ctrl('c'));
    assert!(!h.app.quit);
    h.goto("q");
    assert!(h.app.quit);

    let mut h = harness();
    h.goto("po");
    h.app.apply(vec![ViewCmd::Quit]);
    assert!(h.app.quit);
}

#[test]
fn unknown_and_failing_commands_show_error_dialog() {
    let mut h = harness();
    h.goto("bozo");
    let t = h.dialog_text().unwrap();
    assert!(t.starts_with("< Ruroh? `bozo` command not found >"), "{t}");
    let buf = h.render(120, 30);
    assert!(find(&buf, "<error>").is_some());
    // Keys go to the dialog, not the view; Enter dismisses.
    h.app.handle_key(key(KeyCode::Enter));
    assert!(h.app.dialog.is_none());
    // Browser failure.
    let c = Command {
        gvr: "x/v1/bogus".into(),
        alias: "bogus".into(),
        ..Default::default()
    };
    let r = h
        .app
        .dispatch(c, crate::cmd::Interpreter::new("bogus"), true, true);
    assert!(r
        .unwrap_err()
        .to_string()
        .contains("unknown resource bogus"));
}

#[test]
fn cow_command() {
    let mut h = harness();
    h.goto("cow hello");
    assert_eq!(
        h.dialog_text().unwrap(),
        format!(
            "< Ruroh? hello >\n{}",
            super::super::shell::cow::COW.join("\n")
        )
    );
    h.app.handle_key(key(KeyCode::Esc));
    h.goto("cow");
    assert_eq!(
        h.flash(),
        Some((FlashLevel::Err, "Invalid command. Use `cow xxx`".into()))
    );
}

#[test]
fn view_stack_lifecycle() {
    let mut h = harness();
    let log = h.log.clone();
    let a = Dummy::new(&mut h.app.ctx, "a", &log);
    let b = Dummy::new(&mut h.app.ctx, "b", &log);
    let c = Dummy::new(&mut h.app.ctx, "c", &log);
    let d = Dummy::new(&mut h.app.ctx, "d", &log);
    h.app
        .apply(vec![ViewCmd::Push(Box::new(a)), ViewCmd::Push(Box::new(b))]);
    assert_eq!(h.log(), ["a:start", "a:stop", "b:start"]);
    h.clear_log();
    h.app.apply(vec![ViewCmd::Replace(Box::new(c))]);
    assert_eq!(h.names(), ["a", "c"]);
    assert_eq!(h.log(), ["b:stop", "c:start"]);
    h.clear_log();
    h.app.apply(vec![ViewCmd::Pop]);
    assert_eq!(h.log(), ["c:stop", "a:start"]);
    h.clear_log();
    h.app.apply(vec![ViewCmd::Pop]);
    assert_eq!(h.names(), ["a"], "root never pops");
    assert!(h.log().is_empty());
    h.app.apply(vec![ViewCmd::Reset(Box::new(d))]);
    assert_eq!(h.names(), ["d"]);
    assert_eq!(h.log(), ["a:stop", "d:start"]);
    // Esc in a view returns Pop -> no-op on root.
    h.app.handle_key(key(KeyCode::Esc));
    assert_eq!(h.names(), ["d"]);
}

#[test]
fn events_route_by_view_id() {
    let mut h = harness();
    h.goto("po");
    let log = h.log.clone();
    let d = Dummy::new(&mut h.app.ctx, "describe", &log);
    let pods_id = h.app.stack[0].id();
    h.app.apply(vec![ViewCmd::Push(Box::new(d))]);
    h.clear_log();
    h.app.handle_event(AppEvent::Text {
        view: pods_id,
        text: "x".into(),
    });
    assert_eq!(h.log(), ["pods:event text"]);
    h.clear_log();
    h.app.handle_event(AppEvent::Tick);
    assert_eq!(h.log(), ["describe:event tick"]);
    h.clear_log();
    h.app.handle_event(AppEvent::Text {
        view: 999,
        text: "x".into(),
    });
    assert!(h.log().is_empty());
    h.app.handle_event(AppEvent::Info {
        view: SHELL_VIEW_ID,
        msg: "done".into(),
    });
    assert_eq!(h.flash(), Some((FlashLevel::Info, "done".into())));
    h.app.handle_event(AppEvent::Error {
        view: SHELL_VIEW_ID,
        msg: "bad".into(),
    });
    assert_eq!(h.flash(), Some((FlashLevel::Err, "bad".into())));
    h.app.handle_event(AppEvent::Resources(vec![]));
    assert!(h.log().contains(&"add_resources 0".to_string()));
    assert!(h.app.ctx.aliases.aliases.contains_key("po"));
    h.app.handle_event(AppEvent::Namespaces(vec![
        "default".into(),
        "kube-system".into(),
    ]));
    assert_eq!(h.app.ctx.namespaces.len(), 2);
}

#[test]
fn cluster_info_defcon_and_retries() {
    let mut h = harness();
    h.goto("po");
    h.app
        .handle_event(shell_event(ShellEvent::HasMetrics(true)));
    assert!(h.app.ctx.has_metrics);
    // cluster_info.go hasMetrics: without node metrics access, no defcon.
    h.app.handle_event(AppEvent::ClusterInfo(ClusterInfo {
        cpu_pct: Some(95),
        mem_pct: Some(75),
        ..Default::default()
    }));
    assert_eq!(h.app.logo_status, None);
    h.app
        .handle_event(shell_event(ShellEvent::NodeMetricsAccess(true)));
    h.app.handle_event(AppEvent::ClusterInfo(ClusterInfo {
        cpu_pct: Some(95),
        mem_pct: Some(75),
        ..Default::default()
    }));
    // Memory warning is reported last and wins.
    assert_eq!(
        h.app.logo_status,
        Some((FlashLevel::Warn, "Warning Memory level!".into()))
    );
    let buf = h.render(120, 20);
    let (x, y) = find(&buf, "Warning Memory level!").unwrap();
    assert_eq!(buf[(x, y)].bg, Styles::default().logo_warn);
    h.app.handle_event(AppEvent::ClusterInfo(ClusterInfo {
        cpu_pct: Some(10),
        mem_pct: Some(10),
        ..Default::default()
    }));
    assert!(h.app.logo_status.is_none());
    assert_eq!(h.app.prev_info.cpu_pct, Some(95));

    h.app
        .handle_event(shell_event(ShellEvent::ClusterInfoFailed("down".into())));
    assert_eq!(
        h.app.logo_status,
        Some((FlashLevel::Warn, "Dial K8s Toast [1/5]".into()))
    );
    assert!(h.log().contains(&"pods:stop".to_string()));
    // With metrics the defcon check clears the status right away (k9s
    // ClearStatus); without metrics the reconnect message stays.
    h.app.ctx.has_metrics = false;
    // Reconnect is the connectivity check (ConnOk), ahead of cluster info.
    h.app.handle_event(shell_event(ShellEvent::ConnOk));
    h.app
        .handle_event(AppEvent::ClusterInfo(ClusterInfo::default()));
    assert_eq!(h.app.con_retry, 0);
    assert_eq!(
        h.app.logo_status,
        Some((FlashLevel::Info, "K8s connectivity OK".into()))
    );
    for _ in 0..5 {
        h.app
            .handle_event(shell_event(ShellEvent::ClusterInfoFailed("down".into())));
    }
    assert!(h.app.quit);
    assert_eq!(
        h.app.exit_status.as_deref(),
        Some("Lost K8s connection (5). Bailing out!")
    );
}

/// refreshCluster clears the flash at the connectivity check, so a slow
/// cluster info refresh no longer wipes a flash posted meanwhile.
#[test]
fn cluster_info_keeps_later_flash() {
    let mut h = harness();
    h.app.handle_event(shell_event(ShellEvent::ConnOk));
    h.app.flash.set(FlashLevel::Info, "Viewing v1/pods...");
    h.app
        .handle_event(AppEvent::ClusterInfo(ClusterInfo::default()));
    assert_eq!(
        h.flash(),
        Some((FlashLevel::Info, "Viewing v1/pods...".into()))
    );
}

#[test]
fn filter_prompt_live_and_history() {
    let mut h = harness();
    h.goto("po");
    let log = h.log.clone();
    let d =
        Dummy::new(&mut h.app.ctx, "pods2", &log).with_key('/', || vec![ViewCmd::OpenFilterPrompt]);
    h.app.apply(vec![ViewCmd::Push(Box::new(d))]);
    h.app.handle_key(ch('/'));
    assert_eq!(h.app.prompt.as_ref().unwrap().kind, PromptKind::Filter);
    h.clear_log();
    h.typ("ng");
    assert_eq!(h.log(), ["pods2:filter n", "pods2:filter ng"]);
    let st = Styles::default();
    let buf = h.render(120, 25);
    let (x, y) = find(&buf, "🐩/ ng").unwrap();
    assert_eq!(fg(&buf, 0, y), st.prompt_border_default);
    assert!(x > 0);
    // Title shows the filter in the frame.
    assert!(find(&buf, "</ng>").is_some());
    // Unfocused border while the prompt is active.
    let (_, ty) = find(&buf, " pods2(default)[0] ").unwrap();
    assert_eq!(fg(&buf, 0, ty), st.border);
    h.app.handle_key(key(KeyCode::Enter));
    assert!(h.app.prompt.is_none());
    assert_eq!(h.app.filter_history.list(), ["ng"]);
    // A filter that matches nothing stays out of the history.
    h.app.handle_key(ch('/'));
    h.typ("nope");
    h.app.handle_key(key(KeyCode::Enter));
    assert_eq!(h.app.filter_history.list(), ["ng"]);
    h.app.handle_key(ch('/'));
    h.typ("ng");
    h.app.handle_key(key(KeyCode::Enter));
    // Re-open: ResetPrompt drops the live filter; history suggested; esc clears.
    h.clear_log();
    h.app.handle_key(ch('/'));
    assert_eq!(h.log(), ["pods2:key /", "pods2:filter "]);
    assert!(find(&h.render(120, 25), "</ng>").is_none());
    h.app.handle_key(ch('n'));
    assert_eq!(h.app.prompt.as_ref().unwrap().suggestion(), "g");
    h.clear_log();
    h.app.handle_key(key(KeyCode::Esc));
    assert_eq!(h.log(), ["pods2:filter "]);
}

#[test]
fn command_prompt_suggestions_and_rendering() {
    let mut h = harness();
    h.goto("po");
    h.app.handle_key(ch(':'));
    // k9s activateCmd clears the shown history suggestion; Up walks the list.
    assert_eq!(h.app.prompt.as_ref().unwrap().suggestion(), "");
    h.app.handle_key(key(KeyCode::Up));
    assert_eq!(
        h.app.prompt.as_ref().unwrap().suggestion(),
        "v1/pods default"
    );
    h.typ("po");
    // Real cmd::suggest: sorted completion suffixes ("pod", "pods").
    assert_eq!(h.app.prompt.as_ref().unwrap().suggestion(), "d");
    h.app.handle_key(key(KeyCode::Up));
    assert_eq!(h.app.prompt.as_ref().unwrap().suggestion(), "ds");
    let st = Styles::default();
    let buf = h.render(120, 25);
    let (x, y) = find(&buf, "🐶> pods").unwrap();
    assert_eq!(y, 8, "prompt sits below the header");
    assert_eq!(fg(&buf, x - 2, y), st.prompt_border_command);
    h.app.handle_key(key(KeyCode::Tab));
    assert_eq!(h.app.prompt.as_ref().unwrap().text, "pods");
    // Namespace completion after a resource.
    h.app.ctx.namespaces = vec!["kube-system".into()];
    h.typ(" k");
    assert_eq!(h.app.prompt.as_ref().unwrap().suggestion(), "ube-system");
    h.app.handle_key(ctrl('u'));
    assert_eq!(h.app.prompt.as_ref().unwrap().text, "");
    // Enter on empty closes without running.
    h.app.handle_key(key(KeyCode::Enter));
    assert!(h.app.prompt.is_none());
    // noIcons prompt.
    h.app.ctx.config.write().unwrap().k9s.ui.no_icons = true;
    h.app.handle_key(ch(':'));
    let buf = h.render(120, 25);
    assert!(find(&buf, "  > ").is_some());
    // ctrl-e closes without running.
    h.typ("svc");
    h.app.handle_key(ctrl('e'));
    assert!(h.app.prompt.is_none());
    assert_eq!(h.names(), ["pods"]);
}

#[test]
fn history_navigation_keys() {
    let mut h = harness();
    h.app.handle_key(ch('['));
    assert_eq!(
        h.flash(),
        Some((FlashLevel::Warn, "Can't go back any further".into()))
    );
    h.goto("po");
    h.goto("svc");
    h.app.handle_key(ch('['));
    assert_eq!(h.names(), ["pods"]);
    h.app.handle_key(ch(']'));
    assert_eq!(h.names(), ["services"]);
    h.app.handle_key(ch(']'));
    assert_eq!(
        h.flash(),
        Some((FlashLevel::Warn, "Can't go forward any further".into()))
    );
    h.app.handle_key(ch('-'));
    assert_eq!(h.names(), ["services"]);
    // History lines carry the namespace (p.SwitchNS).
    assert_eq!(
        h.app.cmd_history.list(),
        ["v1/pods default", "v1/services default"]
    );
    h.goto("-");
    assert_eq!(h.names(), ["services"]);
}

#[test]
fn hotkeys_run_commands() {
    let mut h = harness();
    h.app.ctx.hotkeys.hot_keys.insert(
        "svc".into(),
        HotKey {
            short_cut: "Shift-1".into(),
            description: "Services".into(),
            command: "svc -n $NAMESPACE".into(),
            keep_history: false,
            ..Default::default()
        },
    );
    h.goto("po");
    // hotKeyActions binds hotkeys on browser and xray views only (was app
    // wide); this stub view does not bind them, so the key is not taken.
    h.app.handle_key(ch('!'));
    assert_eq!(h.names(), ["pods"]);
    // The commands they emit: keepHistory pushes instead of resetting.
    h.app.apply(vec![crate::ui::shell::plugins::hotkey_goto(
        "svc".into(),
        false,
    )]);
    assert_eq!(h.names(), ["services"]);
    h.app.apply(vec![crate::ui::shell::plugins::hotkey_goto(
        "cm".into(),
        true,
    )]);
    assert_eq!(h.names(), ["services", "configmaps"]);
}

#[test]
fn alt_runes_match_no_binding() {
    let mut h = harness();
    h.goto("po");
    let log = h.log.clone();
    let d = Dummy::new(&mut h.app.ctx, "tbl", &log).with_key('d', Vec::new);
    h.app.apply(vec![ViewCmd::Push(Box::new(d))]);
    h.clear_log();
    let alt = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT);
    // ui.AsKey: alt-d is d*ModAlt, bound nowhere; alt-: opens no prompt.
    h.app.handle_key(alt('d'));
    h.app.handle_key(alt(':'));
    assert!(h.app.prompt.is_none());
    assert!(h.log().is_empty(), "{:?}", h.log());
    // tview's own navigation runes still move (alt-j is j: down).
    h.app.handle_key(alt('j'));
    assert_eq!(h.log(), ["tbl:key down"]);
    // Alt on non-rune keys is dropped.
    h.clear_log();
    h.app
        .handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::ALT));
    assert_eq!(h.log(), ["tbl:key up"]);
}

#[test]
fn aliases_toggle() {
    let mut h = harness();
    h.goto("po");
    h.app.handle_key(ctrl('a'));
    assert_eq!(h.names(), ["pods", "aliases"]);
    h.app.handle_key(ctrl('a'));
    assert_eq!(h.names(), ["pods"]);
    h.goto("alias");
    assert_eq!(
        h.names(),
        ["pods", "aliases"],
        ":alias pushes without clearing"
    );
}

#[test]
fn namespace_and_context_cmds() {
    let mut h = harness();
    h.goto("ns kube-system");
    assert_eq!(h.app.ctx.namespace, "kube-system");
    assert_eq!(h.names(), ["pods"]);
    // In memory only until a save point (k9s switchNS / SetActiveView).
    assert!(h.persisted.lock().unwrap().is_empty());
    let cc = h.app.ctx.context_cfg.clone().unwrap();
    assert_eq!(cc.namespace.active, "kube-system");
    assert!(
        cc.view.active.starts_with("v1/pods") && cc.view.active.contains("kube-system"),
        "{}",
        cc.view.active
    );
    h.goto("ns");
    assert_eq!(h.names(), ["namespaces"]);
    assert_eq!(
        h.opened.lock().unwrap().last().unwrap().namespace,
        None,
        "cluster scoped"
    );
    h.goto("po -n default");
    assert_eq!(h.app.ctx.namespace, "default");
    h.goto("ctx");
    assert_eq!(h.names(), ["contexts"]);
    h.app.apply(vec![ViewCmd::SetNamespace("-".into())]);
    assert_eq!(h.app.ctx.namespace, "");
}

#[tokio::test]
async fn context_switch_flow() {
    let fake = FakeAccessor::new();
    fake.contexts.lock().unwrap().push(crate::k8s::ContextInfo {
        name: "other".into(),
        cluster: "other-cluster".into(),
        user: "u".into(),
        // No kubeconfig namespace: the context config's applies (a
        // kubeconfig one would win, K9s.ActivateContext).
        namespace: String::new(),
        active: false,
    });
    let mut h = harness_with(fake);
    let mut rx = h.app.rx.take().unwrap();
    h.goto("po");
    h.goto("svc @other");
    // Wait for the switch result.
    let ev = loop {
        let ev = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        if matches!(&ev, AppEvent::Custom { payload, .. } if payload.is::<ShellEvent>()) {
            if let AppEvent::Custom { payload, .. } = &ev {
                if matches!(
                    payload.downcast_ref::<ShellEvent>(),
                    Some(ShellEvent::ContextSwitched { .. })
                ) {
                    break ev;
                }
            }
        }
    };
    h.app.handle_event(ev);
    // Config.Save(true) before the switch (old context) and once the new
    // one is active (view/context.go useContext, view/app.go switchContext).
    let saved: Vec<String> = h
        .persisted
        .lock()
        .unwrap()
        .iter()
        .map(|p| p.1.clone())
        .collect();
    assert_eq!(saved, ["fake", "other"]);
    assert!(h.fake.calls().contains(&"switch_context other".to_string()));
    assert_eq!(h.app.ctx.namespace, "other-ns", "from the context config");
    assert_eq!(h.names(), ["services"]);
    assert_eq!(
        h.flash(),
        Some((
            FlashLevel::Info,
            "Switching context to \"other\"::\"other-ns\"".into()
        ))
    );
    // `ctx missing` fails -> flash error, view restarted.
    h.goto("ctx missing");
    let ev = loop {
        let ev = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        if let AppEvent::Custom { payload, .. } = &ev {
            if matches!(
                payload.downcast_ref::<ShellEvent>(),
                Some(ShellEvent::ContextSwitched { .. })
            ) {
                break ev;
            }
        }
    };
    h.app.handle_event(ev);
    assert_eq!(
        h.flash(),
        Some((FlashLevel::Err, "no context named missing".into()))
    );
    // `:ctx`/`@ctx` keep the histories; the context view's Enter (useCtx)
    // clears them once the switch lands.
    assert!(!h.app.cmd_history.is_empty());
    h.app.filter_history.push("web");
    h.app.apply(vec![ViewCmd::UseContext("fake".into())]);
    let ev = loop {
        let ev = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        if let AppEvent::Custom { payload, .. } = &ev {
            if matches!(
                payload.downcast_ref::<ShellEvent>(),
                Some(ShellEvent::ContextSwitched { .. })
            ) {
                break ev;
            }
        }
    };
    h.app.handle_event(ev);
    assert!(h.app.cmd_history.is_empty() && h.app.filter_history.is_empty());
}

/// Next ClusterInfo event within `wait`, skipping everything else.
async fn next_cluster_info(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    wait: Duration,
) -> Option<AppEvent> {
    tokio::time::timeout(wait, async {
        loop {
            match rx.recv().await {
                Some(ev @ AppEvent::ClusterInfo(_)) => return Some(ev),
                Some(_) => {}
                None => return None,
            }
        }
    })
    .await
    .ok()
    .flatten()
}

/// Next shell event matching `f` within `wait`.
async fn next_shell_event(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    wait: Duration,
    f: impl Fn(&ShellEvent) -> bool,
) -> Option<AppEvent> {
    tokio::time::timeout(wait, async {
        loop {
            match rx.recv().await {
                Some(ev) => {
                    if let AppEvent::Custom { payload, .. } = &ev {
                        if payload.downcast_ref::<ShellEvent>().is_some_and(&f) {
                            return Some(ev);
                        }
                    }
                }
                None => return None,
            }
        }
    })
    .await
    .ok()
    .flatten()
}

/// view/app.go: disconnected, the header still gets one refresh (k9s
/// version, n/a meta) and no padding; a context switch reloads the
/// resources before the first view and shows the new context meta.
#[tokio::test]
async fn disconnected_header_and_switch() {
    let fake = FakeAccessor::new();
    *fake.unreachable.lock().unwrap() = Some("dial tcp: connection refused".into());
    let mut h = harness_with(fake);
    h.app.opts.disconnected = true;
    h.app.info_padded = false;
    let mut rx = h.app.rx.take().unwrap();
    h.app.boot();
    let ev = next_shell_event(&mut rx, Duration::from_secs(5), |e| {
        matches!(e, ShellEvent::Meta(_))
    })
    .await
    .expect("meta");
    h.app.handle_event(ev);
    assert_eq!(h.app.ctx.cluster_info.k9s_version, crate::version::VERSION);
    assert_eq!(h.app.ctx.cluster_info.context, "");
    let pods = h.fake.resources.lock().unwrap().clone();
    h.app.handle_event(shell_event(ShellEvent::ContextSwitched {
        name: "fake".into(),
        result: Ok("default".into()),
        ns: None,
        then: None,
        resources: Some(pods.clone()),
    }));
    assert_eq!(h.app.ctx.resources, pods);
    assert_eq!(h.app.ctx.cluster_info.cluster, "fake-cluster");
    // The updater's first connectivity check fails: Dial K8s Toast.
    let ev = next_shell_event(&mut rx, Duration::from_secs(5), |e| {
        matches!(e, ShellEvent::ClusterInfoFailed(_))
    })
    .await
    .expect("conn check");
    h.app.handle_event(ev);
    assert_eq!(h.app.con_retry, 1);
    assert!(!h.app.info_padded);
}

/// k9s Config.Save: nothing is saved without an active context's cluster.
#[test]
fn no_persist_without_context() {
    let mut h = harness_with(FakeAccessor::default());
    h.app.contexts.clear();
    h.goto("po");
    assert!(h.persisted.lock().unwrap().is_empty());
    assert!(crate::config::ContextConfig::load("", "").is_err());
}

#[tokio::test]
async fn disconnected_start_waits_for_context_switch() {
    let mut h = harness();
    h.app.opts.disconnected = true;
    let mut rx = h.app.rx.take().unwrap();
    h.app.boot();
    // No cluster updater, so no retries that would bail out.
    assert!(next_cluster_info(&mut rx, Duration::from_millis(300))
        .await
        .is_none());
    // A successful switch reconnects and starts the updater.
    h.app.handle_event(shell_event(ShellEvent::ContextSwitched {
        name: "fake".into(),
        result: Ok("default".into()),
        ns: None,
        then: None,
        resources: None,
    }));
    assert!(!h.app.opts.disconnected);
    assert!(next_cluster_info(&mut rx, Duration::from_secs(5))
        .await
        .is_some());
}

#[test]
fn xray_pulses_dir_and_can() {
    let mut h = harness();
    h.goto("xray po kube-system");
    assert_eq!(h.names(), ["xray"]);
    assert_eq!(h.app.ctx.namespace, "kube-system");
    h.goto("xray cm");
    assert_eq!(
        h.flash(),
        Some((FlashLevel::Err, "unsupported resource \"cm\"".into()))
    );
    h.goto("xray bozo");
    assert_eq!(
        h.flash(),
        Some((FlashLevel::Err, "invalid resource name: \"bozo\"".into()))
    );
    h.goto("xray");
    assert_eq!(
        h.flash(),
        Some((FlashLevel::Err, "invalid command. use `xray xxx`".into()))
    );
    h.goto("pulses");
    assert_eq!(h.names(), ["pulses"]);
    let tmp = std::env::temp_dir().display().to_string();
    h.goto(&format!("dir {tmp}"));
    assert_eq!(h.names(), ["dirs"]);
    assert_eq!(
        h.opened.lock().unwrap().last().unwrap().args,
        std::slice::from_ref(&tmp)
    );
    assert!(h
        .app
        .cmd_history
        .list()
        .contains(&format!("dir {tmp}").to_lowercase()));
    h.goto("dir /definitely/not/here");
    assert_eq!(
        h.flash(),
        Some((
            FlashLevel::Err,
            "stat /definitely/not/here: no such file or directory".into()
        ))
    );
    h.goto("dir");
    assert_eq!(
        h.flash(),
        Some((FlashLevel::Err, "Invalid command. Use `dir xxx`".into()))
    );
    h.goto("can u:fred");
    assert_eq!(h.names(), ["policy"]);
    h.goto("can x:fred");
    assert_eq!(
        h.flash(),
        Some((
            FlashLevel::Err,
            "Invalid command. Use `can [u|g|s]:xxx`".into()
        ))
    );
}

#[test]
fn dialogs_capture_keys() {
    let mut h = harness();
    h.goto("po");
    let hit = Arc::new(Mutex::new(false));
    let h2 = hit.clone();
    h.app.apply(vec![ViewCmd::Dialog(Dialog::Confirm {
        title: "Kill".into(),
        msg: "Kill pod?".into(),
        on_ok: Box::new(move |_| {
            *h2.lock().unwrap() = true;
            vec![ViewCmd::Flash(FlashLevel::Info, "killed".into())]
        }),
    })]);
    let st = Styles::default();
    let buf = h.render(120, 30);
    let (x, y) = find(&buf, "<Kill>").unwrap();
    assert!(x > 30 && y > 5, "centered");
    let (_, ty) = find(&buf, " pods(default)[0] ").unwrap();
    assert_eq!(fg(&buf, 0, ty), st.border, "view unfocused under a dialog");
    h.clear_log();
    h.app.handle_key(ch('?'));
    assert!(h.log().is_empty());
    assert_eq!(h.names(), ["pods"]);
    h.app.handle_key(key(KeyCode::Tab));
    h.app.handle_key(key(KeyCode::Enter));
    assert!(*hit.lock().unwrap());
    assert!(h.app.dialog.is_none());
    assert_eq!(h.flash(), Some((FlashLevel::Info, "killed".into())));
    // ctrl-c inside a dialog quits even with noExitOnCtrlC.
    h.app.ctx.config.write().unwrap().k9s.no_exit_on_ctrl_c = true;
    h.app
        .apply(vec![ViewCmd::Dialog(Dialog::Error { msg: "x".into() })]);
    h.app.handle_key(ctrl('c'));
    assert!(h.app.quit);
}

#[test]
fn suspend_queue_and_completion() {
    let mut h = harness();
    h.goto("po");
    let done = Arc::new(Mutex::new(None));
    let d2 = done.clone();
    h.app.apply(vec![ViewCmd::Suspend {
        argv: vec!["true".into()],
        banner: String::new(),
        env: vec![("A".into(), "1".into())],
        on_done: Some(Box::new(move |_, st| {
            *d2.lock().unwrap() = Some(st.success());
            vec![ViewCmd::Flash(FlashLevel::Info, "back".into())]
        })),
    }]);
    let s = h.app.suspends.pop_front().unwrap();
    assert_eq!(s.argv, ["true"]);
    #[cfg(unix)]
    {
        let res = run_foreground(&s.argv, &s.env);
        h.app.after_suspend(s, res);
        assert_eq!(*done.lock().unwrap(), Some(true));
        assert_eq!(h.flash(), Some((FlashLevel::Info, "back".into())));
    }
    let s = Suspend {
        argv: vec!["/definitely/not/a/binary".into()],
        env: vec![],
        banner: String::new(),
        on_done: None,
    };
    let res = run_foreground(&s.argv, &s.env);
    assert!(res.is_err());
    h.app.after_suspend(s, res);
    assert!(h
        .flash()
        .unwrap()
        .1
        .starts_with("Exec failed \"/definitely/not/a/binary\""));
    assert!(run_foreground(&[], &[]).is_err());
}

#[test]
fn misc_view_cmds() {
    let mut h = harness();
    h.goto("po");
    h.app.apply(vec![ViewCmd::OpenCommandPrompt("svc".into())]);
    assert_eq!(h.app.prompt.as_ref().unwrap().text, "svc");
    h.app.handle_key(key(KeyCode::Enter));
    assert_eq!(h.names(), ["services"]);
    h.app.apply(vec![ViewCmd::Command("po".into())]);
    assert_eq!(h.names(), ["pods"]);
    h.app
        .apply(vec![ViewCmd::ToggleHeader, ViewCmd::ToggleCrumbs]);
    assert!(!h.app.show_header && !h.app.show_crumbs);
    h.app.handle_key(ctrl('p'));
    assert_eq!(
        h.flash(),
        Some((FlashLevel::Info, "current context config saved".into()))
    );
    // Keys not bound by the shell reach the view.
    h.clear_log();
    h.app.handle_key(ch('d'));
    assert_eq!(h.log(), ["pods:key d"]);
}

#[test]
fn default_command_falls_back_to_contexts() {
    let mut h = harness();
    h.app.default_command();
    assert_eq!(h.names(), ["contexts"]);
    let mut h = harness();
    h.app.opts.command = "bozo".into();
    h.app.default_command();
    assert_eq!(h.names(), ["contexts"]);
    let mut h = harness();
    h.app.opts.command = "svc".into();
    h.app.default_command();
    assert_eq!(h.names(), ["services"]);
}

#[tokio::test]
async fn new_rewires_channel() {
    let (tx, _old_rx) = mpsc::unbounded_channel();
    let acc: SharedAccessor = Arc::new(FakeAccessor::new());
    let mut ctx = Ctx::new(
        acc,
        Default::default(),
        Styles::default(),
        "default".into(),
        tx,
    );
    ctx.readonly = true;
    ctx.namespaces = vec!["a".into()];
    let mut app = App::new(ctx, AppOptions::default());
    assert!(app.ctx.readonly);
    assert_eq!(app.ctx.namespaces, ["a"]);
    app.ctx.sender().send(AppEvent::Tick).unwrap();
    let mut rx = app.rx.take().unwrap();
    assert!(matches!(rx.recv().await, Some(AppEvent::Tick)));
    // Background refresh posts cluster info through the same channel.
    app.refresh_cluster_now();
    let mut got_info = false;
    // ConnOk, HasMetrics, NodeMetricsAccess, then the info.
    for _ in 0..6 {
        if let Ok(Some(AppEvent::ClusterInfo(ci))) =
            tokio::time::timeout(Duration::from_secs(5), rx.recv()).await
        {
            assert_eq!(ci.cluster, "fake-cluster");
            got_info = true;
            break;
        }
    }
    assert!(got_info);
}

#[test]
fn helpers() {
    assert_eq!(strip_context("po @dev -n x"), "po -n x");
    assert!(!failed_status().success());
}

fn temp_locations() -> (tempfile::TempDir, crate::config::paths::Locations) {
    use crate::config::paths::{Locations, MapEnv, Xdg, K9S_ENV_CONFIG_DIR};
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_str().unwrap().to_string();
    let env = MapEnv::new(&[(K9S_ENV_CONFIG_DIR, &dir)]);
    let locs = Locations::resolve(&env, &Xdg::resolve(&env, std::path::Path::new("/h"), false));
    std::fs::create_dir_all(&locs.skins_dir).unwrap();
    (tmp, locs)
}

/// ConfigWatcher: a broken config.yaml warns in the flash and the logo;
/// a valid one is merged and the skin it names is loaded.
#[tokio::test]
async fn config_watcher_reloads_config_and_skin() {
    let (_tmp, locs) = temp_locations();
    let mut h = harness();
    h.app.opts.locations = Some(locs.clone());
    std::fs::write(&locs.config_file, "k9s:\n  refreshRate: [oops\n").unwrap();
    h.app.watched(vec![(Watch::Config, Change::Written)]);
    assert_eq!(
        h.flash(),
        Some((
            FlashLevel::Warn,
            "k9rs config reload failed. Check k9rs logs!".into()
        ))
    );
    assert_eq!(
        h.app.logo_status,
        Some((FlashLevel::Warn, "K9rs config reload failed!".into()))
    );
    std::fs::write(
        locs.skins_dir.join("red.yaml"),
        "k9s:\n  body:\n    fgColor: red\n",
    )
    .unwrap();
    std::fs::write(
        &locs.config_file,
        "k9s:\n  readOnly: true\n  ui:\n    skin: red\n",
    )
    .unwrap();
    h.app.watched(vec![(Watch::Config, Change::Written)]);
    assert_eq!(h.app.ctx.styles.skin.k9s.body.fg_color, "red");
    assert!(h.app.ctx.readonly, "IsReadOnly reads the reloaded config");
    assert_eq!(h.app.skin_file, Some(locs.skins_dir.join("red.yaml")));
    // SkinsDirWatcher: only the active skin file triggers a reload.
    std::fs::write(
        locs.skins_dir.join("red.yaml"),
        "k9s:\n  body:\n    fgColor: green\n",
    )
    .unwrap();
    h.app.watched(vec![(
        Watch::Skin(locs.skins_dir.join("other.yaml")),
        Change::Written,
    )]);
    assert_eq!(h.app.ctx.styles.skin.k9s.body.fg_color, "red");
    h.app.watched(vec![(
        Watch::Skin(locs.skins_dir.join("red.yaml")),
        Change::Written,
    )]);
    assert_eq!(h.app.ctx.styles.skin.k9s.body.fg_color, "green");
}

/// CustomViewsWatcher: views.yaml is reset, reloaded and the top view told.
#[test]
fn views_watcher_reloads_and_notifies_top_view() {
    let (_tmp, locs) = temp_locations();
    let mut h = harness();
    h.app.opts.locations = Some(locs.clone());
    let log = h.log.clone();
    let d = Dummy::new(&mut h.app.ctx, "svc", &log);
    h.app.apply(vec![ViewCmd::Push(Box::new(d))]);
    h.clear_log();
    std::fs::write(
        &locs.views_file,
        "views:\n  v1/services:\n    columns: [NAME]\n",
    )
    .unwrap();
    h.app.watched(vec![(Watch::Views, Change::Written)]);
    assert!(h.app.ctx.views.views.contains_key("v1/services"));
    assert!(
        h.log().contains(&"svc:event other".to_string()),
        "{:?}",
        h.log()
    );
    std::fs::remove_file(&locs.views_file).unwrap();
    h.app.watched(vec![(Watch::Views, Change::Removed)]);
    assert!(h.app.ctx.views.views.is_empty());
}

/// hotKeyActions re-reads hotkeys.yaml whenever a view builds its actions.
#[test]
fn hotkeys_file_is_reread_for_new_views() {
    let (_tmp, locs) = temp_locations();
    let mut h = harness();
    h.app.opts.locations = Some(locs.clone());
    h.goto("po");
    assert!(h.app.ctx.hotkeys.hot_keys.is_empty());
    std::fs::write(
        &locs.hotkeys_file,
        "hotKeys:\n  pods:\n    shortCut: Shift-0\n    description: Pods\n    command: pods\n",
    )
    .unwrap();
    h.goto("svc");
    assert!(h.app.ctx.hotkeys.hot_keys.contains_key("pods"));
}

/// Go syscall.Errno text: `open <dir>: not a directory` (dir.go List).
#[test]
fn go_io_errors_use_go_errno_strings() {
    let f = tempfile::NamedTempFile::new().unwrap();
    let e = std::fs::read_dir(f.path()).unwrap_err();
    assert_eq!(super::go_io_error(&e), "not a directory");
    let e = std::fs::metadata("/definitely/not/here").unwrap_err();
    assert_eq!(super::go_io_error(&e), "no such file or directory");
}

/// tview mouse focus (ui.enableMouse): clicking a header box takes focus
/// from the content, so view keys stop reaching it while app keys (`:`)
/// still work; clicking the content refocuses it.
#[test]
fn mouse_click_moves_focus_between_header_and_content() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let click = |h: &mut Harness, x: u16, y: u16| {
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            h.app.handle_mouse(MouseEvent {
                kind,
                column: x,
                row: y,
                modifiers: KeyModifiers::NONE,
            });
        }
    };
    let mut h = harness();
    h.goto("po");
    h.render(80, 30);
    h.clear_log();
    click(&mut h, 40, 2);
    h.app.handle_key(ch('j'));
    assert!(
        !h.log().iter().any(|l| l.contains("key j")),
        "{:?}",
        h.log()
    );
    // The content frame is drawn blurred.
    let buf = h.render(80, 30);
    assert_eq!(buf[(0, 7)].fg, h.app.ctx.styles.border);
    click(&mut h, 10, 12);
    h.app.handle_key(ch('j'));
    assert!(h.log().iter().any(|l| l.contains("key j")), "{:?}", h.log());
}

/// After shelling out the screen is cleared without asking the terminal for
/// its cursor; forgetting both frames makes the next draw repaint it all.
#[test]
fn forget_frames_forces_full_repaint() {
    let mut t = Terminal::new(TestBackend::new(10, 2)).unwrap();
    let draw = |t: &mut Terminal<TestBackend>| {
        t.draw(|f| {
            f.buffer_mut()
                .set_string(0, 0, "hi", ratatui::style::Style::default())
        })
        .unwrap();
    };
    draw(&mut t);
    // Simulate the external clear.
    ratatui::backend::Backend::clear(t.backend_mut()).unwrap();
    draw(&mut t);
    assert_eq!(
        t.backend().buffer()[(0, 0)].symbol(),
        " ",
        "unchanged cells are skipped"
    );
    forget_frames(&mut t);
    draw(&mut t);
    assert_eq!(t.backend().buffer()[(0, 0)].symbol(), "h");
}

/// A backend that refuses cursor queries: after a suspend the crossterm input
/// thread may still own stdin, so a query times out and kills the app.
struct NoCursorQuery(TestBackend);

impl ratatui::backend::Backend for NoCursorQuery {
    type Error = <TestBackend as ratatui::backend::Backend>::Error;
    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a ratatui::buffer::Cell)>,
    {
        self.0.draw(content)
    }
    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.0.hide_cursor()
    }
    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.0.show_cursor()
    }
    fn get_cursor_position(&mut self) -> Result<ratatui::layout::Position, Self::Error> {
        panic!("cursor position queried")
    }
    fn set_cursor_position<P: Into<ratatui::layout::Position>>(
        &mut self,
        p: P,
    ) -> Result<(), Self::Error> {
        self.0.set_cursor_position(p)
    }
    fn clear(&mut self) -> Result<(), Self::Error> {
        self.0.clear()
    }
    fn clear_region(&mut self, t: ratatui::backend::ClearType) -> Result<(), Self::Error> {
        self.0.clear_region(t)
    }
    fn size(&self) -> Result<ratatui::layout::Size, Self::Error> {
        self.0.size()
    }
    fn window_size(&mut self) -> Result<ratatui::backend::WindowSize, Self::Error> {
        self.0.window_size()
    }
    fn flush(&mut self) -> Result<(), Self::Error> {
        self.0.flush()
    }
}

#[test]
fn full_redraw_after_suspend_skips_cursor_query() {
    let mut term = Terminal::new(NoCursorQuery(TestBackend::new(10, 2))).unwrap();
    let paint =
        |f: &mut Frame| f.render_widget(ratatui::widgets::Paragraph::new("hello"), f.area());
    term.draw(paint).unwrap();
    // The shell cleared the real screen; the TUI must repaint every cell.
    ratatui::backend::Backend::clear(&mut term.backend_mut().0).unwrap();
    forget_frames(&mut term);
    term.draw(paint).unwrap();
    assert_eq!(row(term.backend().0.buffer(), 0), "hello     ");
}

/// tview: a click outside a dialog or the prompt moves focus to the box
/// under it; they stay drawn but keys reach the content. Clicking them
/// again gives the keys back.
#[test]
fn mouse_click_outside_dialog_and_prompt_moves_keys_to_content() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let click = |h: &mut Harness, x: u16, y: u16| {
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            h.app.handle_mouse(MouseEvent {
                kind,
                column: x,
                row: y,
                modifiers: KeyModifiers::NONE,
            });
        }
    };
    let mut h = harness();
    h.goto("po");
    // Prompt: type, click the table, keys go to the view.
    h.app.handle_key(ch(':'));
    h.typ("dp");
    h.render(80, 30);
    h.clear_log();
    click(&mut h, 10, 20);
    h.app.handle_key(ch('j'));
    assert!(h.log().iter().any(|l| l.contains("key j")), "{:?}", h.log());
    assert!(h.app.prompt.is_some(), "prompt stays");
    // ':' is a no-op while the blurred prompt is still in command mode.
    h.app.handle_key(ch(':'));
    assert_eq!(h.app.prompt.as_ref().unwrap().text, "dp");
    // Clicking the prompt refocuses it.
    let r = h.app.prompt_area.unwrap();
    click(&mut h, r.x + 3, r.y + 1);
    h.typ("x");
    assert_eq!(h.app.prompt.as_ref().unwrap().text, "dpx");
    h.app.handle_key(key(KeyCode::Esc));
    // Dialog: a click outside sends keys to the view; it stays up.
    h.app
        .apply(vec![ViewCmd::Dialog(Dialog::Error { msg: "boom".into() })]);
    h.render(80, 30);
    h.clear_log();
    click(&mut h, 2, 15);
    // Nothing in the modal is drawn focused any more.
    let buf = h.render(80, 30);
    let row = (0..30u16)
        .find(|&y| {
            (0..80u16)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
                .contains("Dismiss")
        })
        .unwrap();
    let x = (0..80u16).find(|&x| buf[(x, row)].symbol() == "D").unwrap();
    assert_eq!(buf[(x, row)].bg, h.app.ctx.styles.bg);
    h.app.handle_key(ch('j'));
    assert!(h.app.dialog.is_some());
    assert!(h.log().iter().any(|l| l.contains("key j")), "{:?}", h.log());
}

/// k9s live: closing a dialog with a mouse click leaves no view focused.
#[test]
fn mouse_closing_dialog_leaves_content_unfocused() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    let mut h = harness();
    h.goto("po");
    h.app
        .apply(vec![ViewCmd::Dialog(Dialog::Error { msg: "boom".into() })]);
    let buf = h.render(80, 30);
    let (x, y) = (0..30u16)
        .find_map(|y| {
            let l: String = (0..80u16).map(|x| buf[(x, y)].symbol()).collect();
            l.find("Dismiss")
                .map(|i| (l[..i].chars().count() as u16, y))
        })
        .unwrap();
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        h.app.handle_mouse(MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        });
    }
    assert!(h.app.dialog.is_none());
    h.clear_log();
    h.app.handle_key(ch('j'));
    assert!(
        !h.log().iter().any(|l| l.contains("key j")),
        "{:?}",
        h.log()
    );
}

/// view/command.go Reset: the active context's aliases.yaml loads on a
/// switch (nuke) and on refresh (merged); a broken file warns in the logo.
#[test]
fn aliases_reload_per_context() {
    let (_tmp, locs) = temp_locations();
    let mut h = harness();
    h.app.opts.locations = Some(locs.clone());
    let f = locs.context_aliases_file("fake-cluster", "fake");
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    std::fs::write(&f, "aliases:\n  zz: v1/configmaps\n").unwrap();
    std::fs::write(&locs.aliases_file, "aliases:\n  gg: v1/secrets\n").unwrap();
    h.app.ctx.aliases.define("stale/v1/old", &["old"]);
    h.app.reload_aliases(false);
    assert_eq!(h.app.ctx.aliases.get("zz"), Some("v1/configmaps"));
    assert_eq!(h.app.ctx.aliases.get("gg"), Some("v1/secrets"));
    assert_eq!(
        h.app.ctx.aliases.get("old"),
        Some("stale/v1/old"),
        "kept without nuke"
    );
    h.app.reload_aliases(true);
    assert_eq!(h.app.ctx.aliases.get("old"), None, "nuked on switch");
    assert_eq!(h.app.ctx.aliases.get("zz"), Some("v1/configmaps"));
    assert_eq!(h.app.ctx.aliases.get("ctx"), Some("contexts"), "defaults");
    std::fs::write(&f, "aliases: [1]\n").unwrap();
    h.app.reload_aliases(false);
    assert_eq!(
        h.app.logo_status,
        Some((FlashLevel::Warn, "Aliases load failed!".into()))
    );
}

/// view/context.go useCtx clears both histories; `:ctx x` does not.
#[tokio::test]
async fn context_view_switch_clears_history() {
    let mut h = harness();
    h.app.cmd_history.push("svc");
    h.app.filter_history.push("nginx");
    let switched = |h: &mut Harness| {
        h.app.handle_event(shell_event(ShellEvent::ContextSwitched {
            name: "fake".into(),
            result: Ok("default".into()),
            ns: None,
            then: None,
            resources: None,
        }));
    };
    h.goto("ctx fake");
    switched(&mut h);
    assert!(!h.app.cmd_history.list().is_empty(), "`:ctx` keeps history");
    h.app.apply(vec![ViewCmd::UseContext("fake".into())]);
    switched(&mut h);
    assert!(
        h.app.cmd_history.list().is_empty(),
        "{:?}",
        h.app.cmd_history.list()
    );
    assert!(h.app.filter_history.list().is_empty());
}

/// clusterUpdater backoff: 15s doubling by 1.5 (r = 0.5 removes the
/// jitter) until the 2 minute budget since the last success runs out.
#[test]
fn cluster_updater_backoff() {
    let mut bf = ExpBackoff::new(Duration::from_secs(15), Duration::from_secs(120));
    let t0 = bf.start;
    let s = |x: f64| t0 + Duration::from_secs_f64(x);
    let secs = |d: Option<Duration>| d.map(|d| (d.as_secs_f64() * 100.0).round() / 100.0);
    assert_eq!(secs(bf.next_at(s(0.0), 0.5)), Some(15.0));
    assert_eq!(secs(bf.next_at(s(15.0), 0.5)), Some(22.5));
    assert_eq!(secs(bf.next_at(s(37.5), 0.5)), Some(33.75));
    assert_eq!(bf.next_at(s(71.25), 0.5), None);
    bf.reset();
    assert_eq!(secs(bf.next_at(bf.start, 0.0)), Some(7.5));
}

/// watch.Factory.ValidatePortForwards on each connectivity check: forwards
/// to a vanished or recreated pod stop; a context switch stops them all.
#[tokio::test]
async fn port_forwards_validated_and_terminated() {
    use crate::k8s::{PortForwardHandle, PortForwardInfo, PortForwardSpec};
    let fake = FakeAccessor::new();
    let now = chrono::Utc::now();
    let old = (now - chrono::Duration::hours(1)).to_rfc3339();
    let new = (now + chrono::Duration::hours(1)).to_rfc3339();
    for (n, ts) in [("kept", old), ("recreated", new)] {
        fake.add(
            "v1/pods",
            crate::k8s::fake::obj(serde_json::json!({"apiVersion": "v1", "kind": "Pod",
                "metadata": {"name": n, "namespace": "default", "creationTimestamp": ts}})),
        );
    }
    let mut h = harness_with(fake);
    for pod in ["kept", "recreated", "gone"] {
        let spec = PortForwardSpec {
            namespace: "default".into(),
            pod: pod.into(),
            container: "c".into(),
            address: "localhost".into(),
            local_port: 8080,
            container_port: 80,
        };
        h.app.ctx.port_forwards.insert(
            spec.id(),
            PortForwardHandle {
                info: PortForwardInfo {
                    spec,
                    started_at: now,
                    active: true,
                },
                cancel: Default::default(),
                died: None,
            },
        );
    }
    h.app.handle_event(shell_event(ShellEvent::ConnOk));
    tokio::time::sleep(Duration::from_millis(100)).await;
    h.app.pump();
    let left: Vec<&String> = h.app.ctx.port_forwards.keys().collect();
    assert_eq!(left, ["default/kept|c:8080:80"]);
    h.app.handle_event(shell_event(ShellEvent::ContextSwitched {
        name: "fake".into(),
        result: Ok("default".into()),
        ns: None,
        then: None,
        resources: None,
    }));
    assert!(h.app.ctx.port_forwards.is_empty());
}

/// gotoResource(c, "", false, true): the command goes through the
/// interpreter (aliases) and lands in the history.
#[test]
fn goto_cmd_uses_aliases_and_history() {
    let mut h = harness();
    h.goto("svc");
    h.app.apply(vec![ViewCmd::Goto {
        cmd: "po".into(),
        path: String::new(),
    }]);
    let hist = h.app.cmd_history.list();
    assert!(hist.iter().any(|c| c.starts_with("v1/pods")), "{hist:?}");
}

/// app.go BailOut -> nukeK9sShell: quitting deletes this process' shell pod
/// when the node shell gate is on.
#[tokio::test]
async fn quit_nukes_node_shell_pod() {
    let fake = FakeAccessor::new();
    let name = format!("k9rs-shell-{}", std::process::id());
    fake.add(
        "v1/pods",
        crate::k8s::fake::obj(serde_json::json!({"apiVersion": "v1", "kind": "Pod",
            "metadata": {"name": name, "namespace": "default"}})),
    );
    let mut h = harness_with(fake);
    h.app.nuke_shell_pod().await;
    assert!(h.fake.calls().is_empty(), "gate off: {:?}", h.fake.calls());
    let mut cc = ContextConfig::default();
    cc.feature_gates.node_shell = true;
    h.app.ctx.context_cfg = Some(cc);
    h.app.nuke_shell_pod().await;
    assert_eq!(
        h.fake.calls(),
        [format!("delete v1/pods default/{name} force=false")]
    );
    // A pod that is already gone is fine.
    h.app.nuke_shell_pod().await;
}

/// pf_extender.go runForward: a dead forward warns and is removed.
#[test]
fn dead_port_forward_is_flashed_and_removed() {
    let mut h = harness();
    let spec = crate::k8s::PortForwardSpec {
        namespace: "default".into(),
        pod: "p".into(),
        container: "c".into(),
        local_port: 8080,
        container_port: 80,
        ..Default::default()
    };
    let id = spec.id();
    h.app.ctx.port_forwards.insert(
        id.clone(),
        crate::k8s::PortForwardHandle {
            info: crate::k8s::PortForwardInfo {
                spec,
                started_at: chrono::Utc::now(),
                active: true,
            },
            cancel: Default::default(),
            died: None,
        },
    );
    h.app.handle_event(AppEvent::Custom {
        view: SHELL_VIEW_ID,
        payload: Box::new(crate::ui::app::PortForwardDied {
            id: id.clone(),
            err: "lost connection to pod".into(),
        }),
    });
    assert_eq!(
        h.flash(),
        Some((
            FlashLevel::Warn,
            format!("PortForward failed for {id}: lost connection to pod. Deleting!")
        ))
    );
    assert!(h.app.ctx.port_forwards.is_empty());
}

/// cluster_info.go setDefCon: a benchmark's logo status survives a calm
/// cluster refresh.
#[test]
fn defcon_keeps_benchmark_status() {
    let mut h = harness();
    h.app.ctx.has_metrics = true;
    h.app.node_mx_ok = true;
    h.app.apply(vec![ViewCmd::Status(
        FlashLevel::Warn,
        "Benchmark in progress...".into(),
    )]);
    h.app.handle_event(AppEvent::ClusterInfo(ClusterInfo {
        cpu_pct: Some(5),
        mem_pct: Some(5),
        ..Default::default()
    }));
    assert_eq!(
        h.app.logo_status,
        Some((FlashLevel::Warn, "Benchmark in progress...".into()))
    );
    h.app.apply(vec![ViewCmd::ClearStatus(true)]);
    h.app.apply(vec![ViewCmd::Status(
        FlashLevel::Warn,
        "Dial K8s Toast [1/5]".into(),
    )]);
    h.app.handle_event(AppEvent::ClusterInfo(ClusterInfo {
        cpu_pct: Some(5),
        mem_pct: Some(5),
        ..Default::default()
    }));
    assert_eq!(h.app.logo_status, None);
}

/// A view that panics while opening (command.go exec recover test).
struct PanicView(ViewId);

impl View for PanicView {
    fn id(&self) -> ViewId {
        self.0
    }
    fn name(&self) -> String {
        "boom".into()
    }
    fn hints(&self) -> Vec<Hint> {
        Vec::new()
    }
    fn start(&mut self, _ctx: &mut Ctx) -> Vec<ViewCmd> {
        panic!("view start blew up");
    }
    fn handle_key(&mut self, _key: KeyEvent, _ctx: &mut Ctx) -> Vec<ViewCmd> {
        Vec::new()
    }
    fn draw(&mut self, _frame: &mut Frame<'_>, _area: Rect, _ctx: &Ctx) {}
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// command.go exec: a panic while opening a view falls back to the last
/// command, or pods when there is none.
#[test]
fn exec_panic_falls_back_to_last_view() {
    let mut h = harness();
    let inner = std::mem::replace(&mut h.app.hooks.browser, Box::new(|_, _| unreachable!()));
    h.app.hooks.browser = Box::new(move |ctx, cmd| {
        if cmd.gvr.ends_with("configmaps") {
            return Ok(Box::new(PanicView(ctx.new_view_id())) as Box<dyn View>);
        }
        inner(ctx, cmd)
    });
    h.goto("svc");
    assert_eq!(h.names(), ["services"]);
    h.goto("cm");
    assert_eq!(h.names(), ["services"]);
    // No history yet: pods.
    h.app.cmd_history = Default::default();
    h.goto("cm");
    assert_eq!(h.names(), ["pods"]);
}
