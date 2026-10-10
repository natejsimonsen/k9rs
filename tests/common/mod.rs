//! End-to-end harness: a seeded `FakeAccessor`, a `Ctx` built the way
//! `cli::run_app` builds it, and a thin view stack that applies the
//! `ViewCmd`s views return (push/pop/replace/reset, namespace changes) and
//! records everything else (flashes, dialogs, suspends, commands) so tests
//! can assert on it. Screens render to a 160x45 ratatui `TestBackend`.
//!
//! The real `App` shell can't be driven from outside the crate (its key,
//! event and draw entry points are crate-private), so this harness talks to
//! the public view layer directly.

#![allow(dead_code)]

pub mod fixtures;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use k9rs::config::paths::{Locations, ProcessEnv};
use k9rs::config::{Aliases, Config, CustomView, HotKeys, Plugins, Styles};
use k9rs::k8s::fake::FakeAccessor;
use k9rs::k8s::Accessor;
use k9rs::ui::{AppEvent, Ctx, Dialog, FlashLevel, View, ViewCmd};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Color;
use ratatui::Terminal;
use tokio::sync::mpsc::UnboundedReceiver;

pub const W: u16 = 160;
pub const H: u16 = 45;
/// The fake accessor's kubeconfig context.
pub const CONTEXT: &str = "fake";

/// One K9S_CONFIG_DIR for the whole test binary (env vars are process wide).
/// Tests stay isolated by using a unique cluster name per harness, which
/// keys the per-context config dir.
pub fn config_root() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        // The static is never dropped, so clear dirs left by earlier runs.
        let tmp = std::env::temp_dir();
        let mine = format!("k9rs-e2e-{}", std::process::id());
        if let Ok(rd) = std::fs::read_dir(&tmp) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with("k9rs-e2e-") && name != mine {
                    let _ = std::fs::remove_dir_all(e.path());
                }
            }
        }
        let d = tmp.join(mine);
        std::fs::create_dir_all(&d).unwrap();
        std::env::set_var("K9S_CONFIG_DIR", &d);
        std::env::set_var("K9S_LOGS_DIR", d.join("logs"));
        d
    })
}

fn unique_cluster() -> String {
    static N: AtomicUsize = AtomicUsize::new(0);
    format!("e2e-cluster-{}", N.fetch_add(1, Ordering::SeqCst))
}

/// What the test wants loaded from the config dir.
#[derive(Default)]
pub struct Setup {
    pub namespace: Option<String>,
    pub readonly: bool,
    pub hotkeys_yaml: Option<&'static str>,
    pub plugins_yaml: Option<&'static str>,
    pub views_yaml: Option<&'static str>,
    pub skin_yaml: Option<&'static str>,
    /// Context config (`clusters/<cluster>/fake/config.yaml`) body.
    pub context_yaml: Option<String>,
}

pub struct Suspended {
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
}

pub struct Harness {
    pub acc: Arc<FakeAccessor>,
    pub ctx: Ctx,
    rx: UnboundedReceiver<AppEvent>,
    pub stack: Vec<Box<dyn View>>,
    pub flashes: Vec<(FlashLevel, String)>,
    /// Logo statuses (app.Status); `None` marks a ClearStatus.
    pub statuses: Vec<Option<(FlashLevel, String)>>,
    pub dialogs: Vec<Dialog>,
    pub suspends: Vec<Suspended>,
    pub commands: Vec<String>,
    pub quit: bool,
    pub dir: PathBuf,
}

impl Harness {
    pub async fn new() -> Harness {
        Harness::with(Setup::default()).await
    }

    pub async fn with(s: Setup) -> Harness {
        let root = config_root();
        let cluster = unique_cluster();
        let dir = root.join("per-test").join(&cluster);
        std::fs::create_dir_all(&dir).unwrap();
        let acc = Arc::new(fixtures::cluster());

        // Files go where k9s looks for them: context scoped hotkeys/plugins
        // and context config under clusters/<cluster>/<context>, skins under
        // skins/, views.yaml in a per-test dir (the global one is shared).
        let locs = Locations::current();
        let put = |p: PathBuf, body: &str| {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, body).unwrap();
            p
        };
        if let Some(y) = &s.context_yaml {
            put(locs.context_config(&cluster, CONTEXT), y);
        }
        let hotkeys = match s.hotkeys_yaml {
            Some(b) => {
                let p = put(locs.context_hotkeys_file(&cluster, CONTEXT), b);
                HotKeys::load_with(&locs, Some(&p)).expect("hotkeys.yaml")
            }
            None => HotKeys::default(),
        };
        let mut plugins = Plugins::default();
        if let Some(b) = s.plugins_yaml {
            let p = put(locs.context_plugins_file(&cluster, CONTEXT), b);
            plugins
                .load_all(&locs, Some(&p), false)
                .expect("plugins.yaml");
        }
        let views = match s.views_yaml {
            Some(b) => CustomView::load_from(&put(dir.join("views.yaml"), b)).expect("views.yaml"),
            None => CustomView::default(),
        };
        let styles = match s.skin_yaml {
            Some(b) => {
                put(locs.skin_file_from_name(&cluster), b);
                Styles::load_active(&locs, &ProcessEnv, &cluster, None, false)
            }
            None => Styles::load_active(&locs, &ProcessEnv, "", None, false),
        };

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut cfg = Config::default();
        cfg.k9s.read_only = s.readonly;
        // The shell's latest-release check would call GitHub.
        cfg.k9s.skip_latest_rev_check = true;
        let ns = s.namespace.clone().unwrap_or_else(|| "default".into());
        let mut ctx = Ctx::new(
            acc.clone() as k9rs::k8s::SharedAccessor,
            Arc::new(RwLock::new(cfg)),
            styles,
            ns,
            tx,
        );
        let resources = acc.resources().await.unwrap();
        let mut aliases = Aliases::defaults();
        aliases.add_resources(&resources);
        ctx.readonly = s.readonly;
        ctx.aliases = aliases;
        ctx.hotkeys = hotkeys;
        ctx.plugins = plugins;
        ctx.views = views;
        ctx.resources = resources;
        ctx.namespaces = acc.namespaces().await.unwrap();
        let mut ci = acc.cluster_info().await.unwrap();
        ci.cluster = cluster;
        ctx.cluster_info = ci;
        ctx.has_metrics = acc.has_metrics().await;
        Harness {
            acc,
            ctx,
            rx,
            stack: Vec::new(),
            flashes: Vec::new(),
            statuses: Vec::new(),
            dialogs: Vec::new(),
            suspends: Vec::new(),
            commands: Vec::new(),
            quit: false,
            dir,
        }
    }

    /// Parse a `:` command and open its view as the new root (k9s
    /// gotoResource for plain resource commands).
    pub async fn goto(&mut self, line: &str) -> anyhow::Result<()> {
        let cmd = k9rs::cmd::parse(line, &self.ctx.aliases)?;
        if cmd.gvr.is_empty() {
            anyhow::bail!("`{}` command not found", cmd.alias);
        }
        if let Some(ns) = cmd.namespace.clone().filter(|n| !n.is_empty()) {
            self.ctx.namespace = ns;
        }
        let v = k9rs::ui::browser::open(&mut self.ctx, &cmd)?;
        self.apply(vec![ViewCmd::Reset(v)]);
        self.settle().await;
        Ok(())
    }

    pub fn apply(&mut self, cmds: Vec<ViewCmd>) {
        let mut q: std::collections::VecDeque<ViewCmd> = cmds.into();
        while let Some(c) = q.pop_front() {
            let more = self.apply_one(c);
            for m in more.into_iter().rev() {
                q.push_front(m);
            }
        }
    }

    fn apply_one(&mut self, c: ViewCmd) -> Vec<ViewCmd> {
        match c {
            ViewCmd::Push(v) => {
                if let Some(t) = self.stack.last_mut() {
                    t.stop(&mut self.ctx);
                }
                self.stack.push(v);
                return self.stack.last_mut().unwrap().start(&mut self.ctx);
            }
            ViewCmd::Pop => {
                if self.stack.len() > 1 {
                    let mut v = self.stack.pop().unwrap();
                    v.stop(&mut self.ctx);
                    return self.stack.last_mut().unwrap().start(&mut self.ctx);
                }
            }
            ViewCmd::Replace(v) => {
                if let Some(mut o) = self.stack.pop() {
                    o.stop(&mut self.ctx);
                }
                self.stack.push(v);
                return self.stack.last_mut().unwrap().start(&mut self.ctx);
            }
            ViewCmd::Reset(v) => {
                while let Some(mut o) = self.stack.pop() {
                    o.stop(&mut self.ctx);
                }
                self.stack.push(v);
                return self.stack.last_mut().unwrap().start(&mut self.ctx);
            }
            ViewCmd::Quit => self.quit = true,
            ViewCmd::Flash(l, m) => self.flashes.push((l, m)),
            ViewCmd::Status(l, m) => self.statuses.push(Some((l, m))),
            ViewCmd::ClearStatus(_) => self.statuses.push(None),
            ViewCmd::Command(s) => self.commands.push(s),
            ViewCmd::UseContext(n) => self.commands.push(format!("ctx {n}")),
            ViewCmd::LogoWarn(m) => self.flashes.push((FlashLevel::Warn, m)),
            ViewCmd::Goto { cmd, path } => {
                self.commands.push(format!("goto:{cmd}|{path}"));
                // app.gotoResource(cmd, path, clearStack=false): push the view.
                if let Ok(mut c) = k9rs::cmd::parse(&cmd, &self.ctx.aliases) {
                    c.instance = path;
                    if let Some(ns) = c.namespace.clone().filter(|n| !n.is_empty()) {
                        self.ctx.namespace = ns;
                    }
                    if let Ok(v) = k9rs::ui::browser::open(&mut self.ctx, &c) {
                        return vec![ViewCmd::Push(v)];
                    }
                }
            }
            ViewCmd::OpenCommandPrompt(s) => self.commands.push(format!("prompt:{s}")),
            ViewCmd::OpenFilterPrompt => self.commands.push("prompt:/".into()),
            ViewCmd::Dialog(d) => self.dialogs.push(d),
            ViewCmd::Suspend { argv, env, .. } => self.suspends.push(Suspended { argv, env }),
            ViewCmd::SetNamespace(ns) => {
                self.ctx.namespace = if ns == "-" { String::new() } else { ns };
            }
            ViewCmd::CloseDialog => {
                self.dialogs.pop();
            }
            ViewCmd::ToggleHeader
            | ViewCmd::ToggleCrumbs
            | ViewCmd::ToggleFullScreen
            | ViewCmd::RefreshClusterInfo => {}
        }
        Vec::new()
    }

    /// Deliver pending async events until nothing arrives for a short while.
    pub async fn settle(&mut self) {
        while let Ok(Some(ev)) =
            tokio::time::timeout(Duration::from_millis(60), self.rx.recv()).await
        {
            self.deliver(ev);
        }
    }

    fn deliver(&mut self, ev: AppEvent) {
        let id = match &ev {
            AppEvent::Data { view, .. }
            | AppEvent::Text { view, .. }
            | AppEvent::LogLines { view, .. }
            | AppEvent::LogEnd { view, .. }
            | AppEvent::Error { view, .. }
            | AppEvent::Info { view, .. }
            | AppEvent::Custom { view, .. } => Some(*view),
            _ => None,
        };
        let cmds = match id {
            Some(id) => match self.stack.iter_mut().find(|v| v.id() == id) {
                Some(v) => v.on_event(&ev, &mut self.ctx),
                None => Vec::new(),
            },
            None => match self.stack.last_mut() {
                Some(v) => v.on_event(&ev, &mut self.ctx),
                None => Vec::new(),
            },
        };
        self.apply(cmds);
    }

    /// Refresh tick (k9s refreshRate) then settle.
    pub async fn tick(&mut self) {
        self.deliver(AppEvent::Tick);
        self.settle().await;
    }

    pub fn top(&self) -> &dyn View {
        self.stack.last().expect("a view").as_ref()
    }

    /// Send a key to the top view, apply its commands and settle.
    pub async fn key(&mut self, k: KeyEvent) {
        let cmds = self
            .stack
            .last_mut()
            .expect("a view")
            .handle_key(k, &mut self.ctx);
        self.apply(cmds);
        self.settle().await;
    }

    pub async fn press(&mut self, c: char) {
        let m = if c.is_ascii_uppercase() {
            KeyModifiers::SHIFT
        } else {
            KeyModifiers::NONE
        };
        self.key(KeyEvent::new(KeyCode::Char(c), m)).await;
    }

    pub async fn ctrl(&mut self, c: char) {
        self.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
            .await;
    }

    pub async fn code(&mut self, c: KeyCode) {
        self.key(KeyEvent::new(c, KeyModifiers::NONE)).await;
    }

    /// Live `/` filter as typed into the prompt.
    pub async fn filter(&mut self, text: &str) {
        let cmds = self
            .stack
            .last_mut()
            .expect("a view")
            .on_filter(text, &mut self.ctx);
        self.apply(cmds);
        self.settle().await;
    }

    pub fn render(&mut self) -> Screen {
        let mut t = Terminal::new(TestBackend::new(W, H)).unwrap();
        let ctx = &self.ctx;
        let top = self.stack.last_mut().expect("a view");
        t.draw(|f| top.draw(f, f.area(), ctx)).unwrap();
        Screen(t.backend().buffer().clone())
    }

    pub fn take_dialog(&mut self) -> Dialog {
        assert!(!self.dialogs.is_empty(), "expected a dialog");
        self.dialogs.remove(0)
    }

    pub fn last_flash(&self) -> Option<&(FlashLevel, String)> {
        self.flashes.last()
    }

    pub fn crumbs(&self) -> Vec<String> {
        self.stack.iter().map(|v| v.name()).collect()
    }
}

pub fn shift(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::SHIFT)
}

/// A rendered screen.
pub struct Screen(pub Buffer);

impl Screen {
    pub fn line(&self, y: u16) -> String {
        (0..self.0.area.width)
            .map(|x| self.0[(x, y)].symbol().to_string())
            .collect::<String>()
    }
    pub fn lines(&self) -> Vec<String> {
        (0..self.0.area.height).map(|y| self.line(y)).collect()
    }
    pub fn text(&self) -> String {
        self.lines().join("\n")
    }
    pub fn contains(&self, s: &str) -> bool {
        self.lines().iter().any(|l| l.contains(s))
    }
    /// (x, y) of the first occurrence of `s`.
    pub fn find(&self, s: &str) -> Option<(u16, u16)> {
        self.lines().iter().enumerate().find_map(|(y, l)| {
            l.find(s).map(|bx| {
                let x = l[..bx].chars().count();
                (x as u16, y as u16)
            })
        })
    }
    pub fn row_of(&self, s: &str) -> Option<u16> {
        self.find(s).map(|(_, y)| y)
    }
    pub fn fg_at(&self, x: u16, y: u16) -> Color {
        self.0[(x, y)].fg
    }
    pub fn bg_at(&self, x: u16, y: u16) -> Color {
        self.0[(x, y)].bg
    }
    /// Foreground of the first char of `s`.
    pub fn fg_of(&self, s: &str) -> Color {
        let (x, y) = self
            .find(s)
            .unwrap_or_else(|| panic!("{s:?} not on screen:\n{}", self.text()));
        self.fg_at(x, y)
    }
    pub fn bg_of(&self, s: &str) -> Color {
        let (x, y) = self
            .find(s)
            .unwrap_or_else(|| panic!("{s:?} not on screen:\n{}", self.text()));
        self.bg_at(x, y)
    }
    /// Table body rows (trimmed) between the header row containing `hdr` and
    /// the bottom border.
    pub fn table_names(&self, hdr: &str) -> Vec<String> {
        let ls = self.lines();
        let Some(start) = ls.iter().position(|l| l.contains(hdr)) else {
            return Vec::new();
        };
        ls[start + 1..]
            .iter()
            .map(|l| {
                l.trim_matches(|c: char| c == '│' || c.is_whitespace())
                    .to_string()
            })
            .take_while(|l| !l.starts_with('└') && !l.is_empty())
            .collect()
    }
}

pub fn color(name: &str) -> Color {
    k9rs::config::skin::parse_color(name)
}

/// Dump the screen when a test needs eyeballing (`E2E_DUMP=1`).
pub fn dump(s: &Screen) {
    if std::env::var("E2E_DUMP").is_ok() {
        eprintln!("{}", s.text());
    }
}

/// The real `App` shell around a harness' `Ctx` (header, prompt, dialogs,
/// crumbs, hotkeys, context switching), driven through its test API.
pub struct Shell {
    pub app: k9rs::ui::App,
    pub acc: Arc<FakeAccessor>,
}

impl Harness {
    /// Hand the harness' context to the real shell; `command` is the
    /// startup command (empty = k9s default).
    pub fn into_shell(self, command: &str) -> Shell {
        let opts = k9rs::ui::app::AppOptions {
            command: command.into(),
            splashless: true,
            ..Default::default()
        };
        Shell {
            app: k9rs::ui::App::with_receiver(self.ctx, opts, self.rx),
            acc: self.acc,
        }
    }
}

impl Shell {
    /// Boot like `App::run` (background work + default command), then settle.
    pub async fn boot(&mut self) {
        self.app.boot();
        self.settle().await;
    }

    /// Pump app events until the queue stays empty for a few rounds.
    pub async fn settle(&mut self) {
        let mut idle = 0;
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(15)).await;
            if self.app.pump() == 0 {
                idle += 1;
                if idle >= 4 {
                    break;
                }
            } else {
                idle = 0;
            }
        }
    }

    pub async fn key(&mut self, k: KeyEvent) {
        self.app.handle_key(k);
        self.settle().await;
    }

    pub async fn press(&mut self, c: char) {
        let m = if c.is_ascii_uppercase() {
            KeyModifiers::SHIFT
        } else {
            KeyModifiers::NONE
        };
        self.key(KeyEvent::new(KeyCode::Char(c), m)).await;
    }

    pub async fn ctrl(&mut self, c: char) {
        self.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
            .await;
    }

    pub async fn code(&mut self, c: KeyCode) {
        self.key(KeyEvent::new(c, KeyModifiers::NONE)).await;
    }

    /// Refresh tick then settle.
    pub async fn tick(&mut self) {
        self.app.tick();
        self.settle().await;
    }

    /// Type text without settling between characters.
    pub fn typ(&mut self, s: &str) {
        for c in s.chars() {
            self.app
                .handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
    }

    /// `:` + line + Enter.
    pub async fn command(&mut self, line: &str) {
        self.app
            .handle_key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::NONE));
        self.typ(line);
        self.code(KeyCode::Enter).await;
    }

    pub fn render(&mut self) -> Screen {
        self.render_size(W, H)
    }

    pub fn render_size(&mut self, w: u16, h: u16) -> Screen {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        let app = &mut self.app;
        t.draw(|f| app.draw(f)).unwrap();
        Screen(t.backend().buffer().clone())
    }
}
