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
    fn draw(&mut self, frame: &mut Frame<'_>, area: Rect, _ctx: &Ctx) {
        let buf = frame.buffer_mut();
        if area.width > 0 && area.height > 0 {
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
    app.hooks.persist = Box::new(move |cl, ct, v, ns| {
        p2.lock().unwrap().push((
            cl.into(),
            ct.into(),
            v.map(String::from),
            ns.map(String::from),
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
    assert_eq!(h.app.cmd_history.list(), ["v1/pods"]);
    let p = h.persisted.lock().unwrap().clone();
    assert_eq!(p.last().unwrap().2.as_deref(), Some("v1/pods"));
    assert_eq!(p.last().unwrap().1, "fake");
    assert!(h.log().contains(&"pods:start".to_string()));
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
        row(&buf, 0).starts_with(" Context: fake [RW]"),
        "{}",
        row(&buf, 0)
    );
    assert!(row(&buf, 5).starts_with(" CPU:     10%"));
    // Menu from the top view.
    let (x, y) = find(&buf, "<0>").unwrap();
    assert_eq!(fg(&buf, x, y), st.menu_num_key);
    let (x, y) = find(&buf, "<d>").unwrap();
    assert_eq!(fg(&buf, x, y), st.menu_key);
    // Logo.
    assert!(find(&buf, "|____|\\__ \\/____//____  /").is_some());
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
    assert_eq!(fg(&buf, x, y), ratatui::style::Color::Rgb(0xFF, 0xA5, 0x00));
}

#[test]
fn header_and_crumb_toggles_and_headless() {
    let mut h = harness();
    h.goto("po");
    h.app.handle_key(ctrl('e'));
    let buf = h.render(100, 20);
    assert!(
        row(&buf, 0).contains("K9s "),
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
    assert!(find(t.backend().buffer(), "Revision v0.50.18").is_some());
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
    let r = h.app.dispatch(c, "bogus", true, true);
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
    // Re-open: history suggested; esc clears the filter.
    h.app.handle_key(ch('/'));
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
    // Empty buffer suggests history.
    assert_eq!(h.app.prompt.as_ref().unwrap().suggestion(), "v1/pods");
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
    assert_eq!(h.app.cmd_history.list(), ["v1/pods", "v1/services"]);
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
    h.app.handle_key(ch('!'));
    assert_eq!(h.names(), ["services"]);
    let last = h.opened.lock().unwrap().last().cloned().unwrap();
    assert_eq!(last.namespace.as_deref(), Some("default"));
    // keepHistory keeps the stack.
    h.app.ctx.hotkeys.hot_keys.insert(
        "cm".into(),
        HotKey {
            short_cut: "F2".into(),
            description: "CM".into(),
            command: "cm".into(),
            keep_history: true,
            ..Default::default()
        },
    );
    h.app.handle_key(key(KeyCode::F(2)));
    assert_eq!(h.names(), ["services", "configmaps"]);
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
    let p = h.persisted.lock().unwrap().clone();
    assert!(p.iter().any(|x| x.3.as_deref() == Some("kube-system")));
    assert!(
        p.iter().any(|x| x
            .2
            .as_deref()
            .is_some_and(|v| v.starts_with("v1/pods") && v.contains("kube-system"))),
        "{p:?}"
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
        namespace: "other-default".into(),
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
    for _ in 0..3 {
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
