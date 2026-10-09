//! Application shell (k9s view/app.go + ui/app.go + view/command.go):
//! terminal setup, event loop, view stack, header/crumbs/prompt/flash,
//! dialogs, command dispatch and global key bindings.
//!
//! Construction: `App::new(ctx, opts)` keeps working. Because `Ctx` holds the
//! only sender of the app event channel, `new` rebuilds the `Ctx` around a
//! fresh channel (copying every public field) so the app owns the receiver;
//! do this before allocating view ids. `App::bootstrap` builds the `Ctx`
//! and channel in one go, and `App::with_receiver` accepts a `Ctx` plus the
//! receiver of the channel it was created with.

use std::collections::VecDeque;
use std::io::{self, Stdout};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::{Frame, Terminal};
use tokio::sync::mpsc;

use super::keys::{parse_key, same_key};
use super::shell::dialog::DialogState;
use super::shell::flash::Flash;
use super::shell::header::{self, HeaderModel, HEADER_HEIGHT};
use super::shell::help::HELP_NAME;
use super::shell::prompt::{Prompt, PromptAction, PromptKind, PROMPT_HEIGHT};
use super::shell::text::{fill, go_quote};
use super::shell::{crumbs, plugins, splash, title, SHELL_VIEW_ID};
use super::{AppEvent, Ctx, Dialog, FlashLevel, View, ViewCmd};
use crate::cmd::Command;
use crate::cmd::History;
use crate::config::{Aliases, Config, ContextConfig, SharedConfig, Styles};
use crate::k8s::{ClusterInfo, ContextInfo, Gvr, ResourceMeta, SharedAccessor, CLUSTER_SCOPE};

/// Startup options derived from the CLI and config.
#[derive(Clone, Debug, Default)]
pub struct AppOptions {
    /// Initial `:` command (CLI `-c`, else context view.active, else `po`).
    pub command: String,
    pub headless: bool,
    pub logoless: bool,
    pub crumbsless: bool,
    pub splashless: bool,
    pub readonly: bool,
    pub refresh_rate: f32,
    /// No API connection at startup: skip the cluster updater until a
    /// context switch succeeds (k9s clusterUpdater).
    pub disconnected: bool,
}

/// view/app.go clusterRefresh.
const CLUSTER_REFRESH: Duration = Duration::from_secs(15);
/// ui/indicator.go status reset delay.
const INDICATOR_DELAY: Duration = Duration::from_secs(5);
/// view/command.go ctxCmd (root default command).
const CTX_CMD: &str = "ctx";

/// Commands whose argument errors k9s flashes (specialCmd) instead of
/// showing the error dialog.
const FLASHED_CMDS: [&str; 15] = [
    "cow", "x", "xr", "xray", "can", "ctx", "context", "contexts", "dir", "dirs", "d", "ls", "a",
    "alias", "aliases",
];
const CONTEXT_CMDS: [&str; 3] = ["ctx", "context", "contexts"];

type BrowserFn = Box<dyn Fn(&mut Ctx, &Command) -> Result<Box<dyn View>> + Send>;
type XrayFn = Box<dyn Fn(&mut Ctx, Gvr, String) -> Box<dyn View> + Send>;
type PulsesFn = Box<dyn Fn(&mut Ctx, String) -> Box<dyn View> + Send>;
type AddResourcesFn = Box<dyn Fn(&mut Aliases, &[ResourceMeta]) + Send>;
type LoadContextFn = Box<dyn Fn(&str, &str) -> Result<ContextConfig> + Send>;
type PersistFn = Box<dyn Fn(&str, &str, Option<&str>, Option<&str>) -> Result<()> + Send>;
type SaveFn = Box<dyn Fn(&Config) -> Result<()> + Send>;

/// Calls into other slices (command parsing, views, config persistence).
/// Indirected so tests can substitute them.
pub(crate) struct Hooks {
    pub browser: BrowserFn,
    pub xray: XrayFn,
    pub pulses: PulsesFn,
    pub add_resources: AddResourcesFn,
    pub load_context: LoadContextFn,
    /// `(cluster, context, view.active, namespace)` → save the context config.
    pub persist: PersistFn,
    pub save_config: SaveFn,
}

impl Default for Hooks {
    fn default() -> Self {
        Hooks {
            browser: Box::new(super::browser::open),
            xray: Box::new(super::views::xray),
            pulses: Box::new(super::views::pulses),
            add_resources: Box::new(|a, r| a.add_resources(r)),
            load_context: Box::new(ContextConfig::load),
            persist: Box::new(|cluster, context, view, ns| {
                let mut cc = ContextConfig::load(cluster, context)?;
                if let Some(v) = view {
                    cc.view.active = v.to_string();
                }
                if let Some(ns) = ns {
                    cc.set_active_namespace(ns);
                }
                cc.save(cluster, context)
            }),
            save_config: Box::new(|c| c.save()),
        }
    }
}

/// Shell-internal async results (sent as `AppEvent::Custom` to `SHELL_VIEW_ID`).
enum ShellEvent {
    HasMetrics(bool),
    Contexts(Vec<ContextInfo>),
    ClusterInfoFailed(String),
    ContextSwitched {
        name: String,
        result: std::result::Result<String, String>,
        ns: Option<String>,
        then: Option<String>,
    },
}

struct Suspend {
    argv: Vec<String>,
    env: Vec<(String, String)>,
    on_done: Option<Box<dyn FnOnce(&mut Ctx, std::process::ExitStatus) -> Vec<ViewCmd> + Send>>,
}

pub struct App {
    pub ctx: Ctx,
    pub opts: AppOptions,
    rx: Option<mpsc::UnboundedReceiver<AppEvent>>,
    pub(crate) hooks: Hooks,
    stack: Vec<Box<dyn View>>,
    dialog: Option<DialogState>,
    prompt: Option<Prompt>,
    flash: Flash,
    logo_status: Option<(FlashLevel, String)>,
    indicator_status: Option<(FlashLevel, String, Instant)>,
    show_header: bool,
    show_logo: bool,
    show_crumbs: bool,
    fullscreen: bool,
    show_splash: bool,
    splash_until: Option<Instant>,
    cmd_history: History,
    filter_history: History,
    quit: bool,
    exit_status: Option<String>,
    suspends: VecDeque<Suspend>,
    prev_info: ClusterInfo,
    /// Context switched: like k9s ClusterInfo.Reset, the next refresh diffs
    /// against an empty meta (zero CPU/MEM).
    info_reset: bool,
    contexts: Vec<ContextInfo>,
    con_retry: i32,
}

impl App {
    /// Build the app around an existing `Ctx` (see module docs: the `Ctx` is
    /// rebuilt around a new event channel owned by the app).
    pub fn new(ctx: Ctx, opts: AppOptions) -> App {
        let (tx, rx) = mpsc::unbounded_channel();
        App::with_receiver(rewire(ctx, tx), opts, rx)
    }

    /// Create the event channel, the `Ctx` and the app.
    pub fn bootstrap(
        accessor: SharedAccessor,
        config: SharedConfig,
        styles: Styles,
        namespace: String,
        opts: AppOptions,
    ) -> App {
        let (tx, rx) = mpsc::unbounded_channel();
        let ctx = Ctx::new(accessor, config, styles, namespace, tx);
        App::with_receiver(ctx, opts, rx)
    }

    /// Build the app from a `Ctx` and the receiver paired with its sender.
    pub fn with_receiver(
        mut ctx: Ctx,
        opts: AppOptions,
        rx: mpsc::UnboundedReceiver<AppEvent>,
    ) -> App {
        let ui = ctx
            .config
            .read()
            .map(|c| c.k9s.ui.clone())
            .unwrap_or_default();
        ctx.readonly |= opts.readonly;
        App {
            show_header: !(opts.headless || ui.headless),
            show_logo: !(opts.logoless || ui.logoless),
            show_crumbs: !(opts.crumbsless || ui.crumbsless),
            show_splash: !(opts.splashless || ui.splashless),
            fullscreen: ui.defaults_to_full_screen,
            ctx,
            opts,
            rx: Some(rx),
            hooks: Hooks::default(),
            stack: Vec::new(),
            dialog: None,
            prompt: None,
            flash: Flash::default(),
            logo_status: None,
            indicator_status: None,
            splash_until: None,
            cmd_history: History::default(),
            filter_history: History::default(),
            quit: false,
            exit_status: None,
            suspends: VecDeque::new(),
            prev_info: ClusterInfo::default(),
            info_reset: false,
            contexts: Vec::new(),
            con_retry: 0,
        }
    }

    /// Run the TUI until quit.
    pub async fn run(mut self) -> Result<()> {
        let mouse = self.cfg(|c| c.k9s.ui.enable_mouse);
        install_panic_hook(mouse);
        let mut tui = Tui::enter(mouse)?;
        if self.show_splash {
            self.splash_until = Some(Instant::now() + splash::SPLASH_DELAY);
        }
        self.start_background();
        self.default_command();
        let res = self.event_loop(&mut tui).await;
        drop(tui);
        self.save_config();
        res?;
        if let Some(s) = self.exit_status.take() {
            return Err(anyhow!("view exit status {s}"));
        }
        Ok(())
    }

    async fn event_loop(&mut self, tui: &mut Tui) -> Result<()> {
        let mut rx = self
            .rx
            .take()
            .ok_or_else(|| anyhow!("app event receiver already taken"))?;
        let mut events = EventStream::new();
        let mut tick = tokio::time::interval(self.refresh_rate());
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tick.tick().await;
        let mut timers = tokio::time::interval(Duration::from_millis(100));
        let mut dirty = true;
        loop {
            if self.quit {
                break;
            }
            if let Some(s) = self.suspends.pop_front() {
                drop(events);
                let res = tui.suspend(|| run_foreground(&s.argv, &s.env));
                events = EventStream::new();
                self.after_suspend(s, res?);
                dirty = true;
                continue;
            }
            if dirty {
                let colors = &tui.colors;
                tui.term.draw(|f| {
                    self.draw(f);
                    colors.apply(f.buffer_mut());
                })?;
                dirty = false;
            }
            tokio::select! {
                ev = events.next() => match ev {
                    Some(Ok(e)) => { self.handle_term_event(e); dirty = true; }
                    Some(Err(e)) => return Err(e.into()),
                    None => break,
                },
                ev = rx.recv() => {
                    if let Some(ev) = ev {
                        self.handle_event(ev);
                        dirty = true;
                    }
                }
                _ = tick.tick() => {
                    self.handle_event(AppEvent::Tick);
                    dirty = true;
                }
                _ = timers.tick() => {
                    if self.expire(Instant::now()) {
                        dirty = true;
                    }
                }
            }
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Test API: lets integration tests drive the real shell without a TTY.

    /// Start background work and run the default command, like `run`.
    #[doc(hidden)]
    pub fn boot(&mut self) {
        self.start_background();
        self.default_command();
    }

    /// Feed every queued app event to `handle_event`; returns how many.
    #[doc(hidden)]
    pub fn pump(&mut self) -> usize {
        let mut evs = Vec::new();
        if let Some(rx) = self.rx.as_mut() {
            while let Ok(ev) = rx.try_recv() {
                evs.push(ev);
            }
        }
        let n = evs.len();
        for ev in evs {
            self.handle_event(ev);
        }
        n
    }

    /// Queued foreground commands `(argv, env)`, removed without running.
    #[doc(hidden)]
    pub fn take_suspends(&mut self) -> Vec<(Vec<String>, Vec<(String, String)>)> {
        self.suspends.drain(..).map(|s| (s.argv, s.env)).collect()
    }

    /// True once the app was asked to quit.
    #[doc(hidden)]
    pub fn is_quitting(&self) -> bool {
        self.quit
    }

    fn refresh_rate(&self) -> Duration {
        let mut r = self.opts.refresh_rate;
        if r <= 0.0 {
            r = self.cfg(|c| c.k9s.refresh_rate);
        }
        if r <= 0.0 {
            r = 2.0;
        }
        Duration::from_secs_f32(r)
    }

    fn cfg<T>(&self, f: impl FnOnce(&Config) -> T) -> T
    where
        T: Default,
    {
        self.ctx.config.read().map(|c| f(&c)).unwrap_or_default()
    }

    fn no_icons(&self) -> bool {
        self.cfg(|c| c.k9s.ui.no_icons)
    }

    // -----------------------------------------------------------------------
    // Background work

    fn start_background(&mut self) {
        let acc = self.ctx.accessor.clone();
        let tx = self.ctx.sender();
        self.ctx.spawn(async move {
            match acc.resources().await {
                Ok(r) => {
                    let _ = tx.send(AppEvent::Resources(r));
                }
                Err(e) => tracing::warn!(error = %e, "discovery failed"),
            }
            if let Ok(c) = acc.contexts().await {
                let _ = tx.send(shell_event(ShellEvent::Contexts(c)));
            }
        });
        if self.opts.disconnected {
            tracing::debug!("Skipping cluster updater - no valid connection");
            return;
        }
        let acc = self.ctx.accessor.clone();
        let tx = self.ctx.sender();
        let cfg = self.ctx.config.clone();
        self.ctx.spawn(async move {
            loop {
                refresh_cluster(&acc, &tx, &cfg).await;
                if tx.is_closed() {
                    return;
                }
                tokio::time::sleep(CLUSTER_REFRESH).await;
            }
        });
    }

    fn refresh_cluster_now(&self) {
        let acc = self.ctx.accessor.clone();
        let tx = self.ctx.sender();
        let cfg = self.ctx.config.clone();
        self.ctx
            .spawn(async move { refresh_cluster(&acc, &tx, &cfg).await });
    }

    // -----------------------------------------------------------------------
    // Terminal events

    fn handle_term_event(&mut self, e: Event) {
        match e {
            Event::Key(k) if k.kind != KeyEventKind::Release => self.handle_key(k),
            Event::Resize(_, _) => self.handle_event(AppEvent::Resize),
            Event::Paste(s) if self.prompt.is_some() => {
                for c in s.chars() {
                    self.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
                }
            }
            _ => {}
        }
    }

    fn expire(&mut self, now: Instant) -> bool {
        let mut changed = self.flash.expire(now);
        if let Some(t) = self.splash_until {
            if now >= t {
                self.splash_until = None;
                changed = true;
            }
        }
        if let Some((_, _, at)) = &self.indicator_status {
            if now.duration_since(*at) >= INDICATOR_DELAY {
                self.indicator_status = None;
                changed = true;
            }
        }
        changed
    }

    // -----------------------------------------------------------------------
    // Keys (view/app.go bindAppKeys + ui/app.go bindKeys + prompt.go)

    /// Handle one key press (test API for integration tests).
    #[doc(hidden)]
    pub fn handle_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C')) {
            let no_exit = self.cfg(|c| c.k9s.no_exit_on_ctrl_c);
            if !no_exit || self.dialog.is_some() {
                self.bail_out();
            }
            return;
        }
        if let Some(mut d) = self.dialog.take() {
            let out = d.handle_key(key, &mut self.ctx);
            if let Some((l, msg)) = d.pending_flash.take() {
                self.flash.set(l, msg);
            }
            match out {
                Some(cmds) => self.apply(cmds),
                None => self.dialog = Some(d),
            }
            return;
        }
        if self.prompt.is_some() {
            self.prompt_key(key);
            return;
        }
        let is = |c: char| key.code == KeyCode::Char(c) && !ctrl;
        let ctrl_is = |c: char| ctrl && key.code == KeyCode::Char(c);
        if is(':') {
            self.open_prompt(PromptKind::Command, String::new());
        } else if is('?') {
            self.help_cmd();
        } else if ctrl_is('e') {
            self.show_header = !self.show_header;
        } else if ctrl_is('g') {
            self.show_crumbs = !self.show_crumbs;
        } else if ctrl_is('a') {
            self.alias_cmd();
        } else if ctrl_is('p') {
            self.save_cmd();
        } else if is('[') {
            match self.cmd_history.back() {
                Some(c) => self.goto(&c, true, false),
                None => self
                    .flash
                    .set(FlashLevel::Warn, "Can't go back any further"),
            }
        } else if is(']') {
            match self.cmd_history.forward() {
                Some(c) => self.goto(&c, true, false),
                None => self
                    .flash
                    .set(FlashLevel::Warn, "Can't go forward any further"),
            }
        } else if is('-') {
            match self.cmd_history.top() {
                Some(c) => self.goto(&c, true, false),
                None => self
                    .flash
                    .set(FlashLevel::Warn, "No previous view to switch to"),
            }
        } else if let Some(hk) = self.hotkey_for(&key) {
            let cmd = plugins::hotkey_command(&self.ctx, &hk.command);
            self.goto(&cmd, !hk.keep_history, true);
        } else if let Some(top) = self.stack.last_mut() {
            let cmds = top.handle_key(key, &mut self.ctx);
            self.apply(cmds);
        }
    }

    fn hotkey_for(&self, key: &KeyEvent) -> Option<crate::config::HotKey> {
        self.ctx
            .hotkeys
            .hot_keys
            .values()
            .find(|hk| parse_key(&hk.short_cut).is_some_and(|k| same_key(&k, key)))
            .cloned()
    }

    fn prompt_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let Some(kind) = self.prompt.as_ref().map(|p| p.kind) else {
            return;
        };
        // App-level actions that stay live in command mode.
        if ctrl && key.code == KeyCode::Char('a') {
            self.alias_cmd();
            return;
        }
        if ctrl && key.code == KeyCode::Char('p') {
            self.save_cmd();
            return;
        }
        if kind == PromptKind::Command {
            if ctrl && key.code == KeyCode::Char('q') {
                self.set_prompt_text(String::new());
                return;
            }
            if ctrl && key.code == KeyCode::Char('e') {
                // tview prompt: ctrl-e accepts and deactivates the buffer
                // without running it (only Enter triggers gotoCmd).
                self.prompt = None;
                return;
            }
            if key.code == KeyCode::Enter {
                let text = self
                    .prompt
                    .as_ref()
                    .map(|p| p.text.clone())
                    .unwrap_or_default();
                if !text.is_empty() {
                    self.prompt = None;
                    self.goto(&text, true, true);
                    return;
                }
            }
        }
        let Some(p) = self.prompt.as_mut() else {
            return;
        };
        match p.handle_key(key) {
            PromptAction::None | PromptAction::Suggested => {}
            PromptAction::Edited { suggest } => {
                let text = p.text.clone();
                if suggest {
                    self.refresh_suggestions();
                }
                if kind == PromptKind::Filter {
                    self.filter_view(&text);
                }
            }
            PromptAction::Submit(text) => {
                self.prompt = None;
                if kind == PromptKind::Filter {
                    self.filter_history.push(&text);
                    self.filter_view(&text);
                }
            }
            PromptAction::Cancel => {
                self.prompt = None;
                if kind == PromptKind::Filter {
                    self.filter_view("");
                }
            }
        }
    }

    fn set_prompt_text(&mut self, text: String) {
        if let Some(p) = self.prompt.as_mut() {
            p.text = text;
            p.set_suggestions(Vec::new());
        }
    }

    fn filter_view(&mut self, text: &str) {
        if let Some(top) = self.stack.last_mut() {
            let cmds = top.on_filter(text, &mut self.ctx);
            self.apply(cmds);
        }
    }

    fn open_prompt(&mut self, kind: PromptKind, text: String) {
        self.prompt = Some(Prompt::new(kind, text));
        self.refresh_suggestions();
    }

    fn refresh_suggestions(&mut self) {
        let Some(p) = self.prompt.as_ref() else {
            return;
        };
        let text = p.text.clone();
        let ss = match p.kind {
            PromptKind::Command => {
                let contexts: Vec<String> = self.contexts.iter().map(|c| c.name.clone()).collect();
                crate::cmd::suggest_command(
                    &text,
                    &self.ctx.aliases,
                    &self.cmd_history,
                    &self.ctx.namespaces,
                    &contexts,
                )
            }
            PromptKind::Filter => crate::cmd::suggest_filter(&text, &self.filter_history),
        };
        if let Some(p) = self.prompt.as_mut() {
            p.set_suggestions(ss);
        }
    }

    // -----------------------------------------------------------------------
    // App events

    pub(crate) fn handle_event(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::ClusterInfo(ci) => {
                if self.con_retry > 0 {
                    self.con_retry = 0;
                    self.status(FlashLevel::Info, "K8s connectivity OK");
                    if let Some(top) = self.stack.last_mut() {
                        let cmds = top.start(&mut self.ctx);
                        self.apply(cmds);
                    }
                } else {
                    self.logo_status = None;
                    self.flash.clear();
                }
                self.prev_info = std::mem::replace(&mut self.ctx.cluster_info, ci.clone());
                if std::mem::take(&mut self.info_reset) {
                    self.prev_info = ClusterInfo {
                        cpu_pct: Some(0),
                        mem_pct: Some(0),
                        ..Default::default()
                    };
                }
                self.defcon();
                self.forward_top(AppEvent::ClusterInfo(ci));
            }
            AppEvent::Namespaces(ns) => {
                self.ctx.namespaces = ns.clone();
                self.forward_top(AppEvent::Namespaces(ns));
            }
            AppEvent::Resources(r) => {
                (self.hooks.add_resources)(&mut self.ctx.aliases, &r);
                self.ctx.resources = r.clone();
                self.forward_top(AppEvent::Resources(r));
            }
            AppEvent::Info { view, msg } if view == SHELL_VIEW_ID => {
                self.flash.set(FlashLevel::Info, msg)
            }
            AppEvent::Error { view, msg } if view == SHELL_VIEW_ID => {
                self.flash.set(FlashLevel::Err, msg)
            }
            AppEvent::Custom { view, payload } if view == SHELL_VIEW_ID => {
                if let Ok(ev) = payload.downcast::<ShellEvent>() {
                    self.shell_event(*ev);
                }
            }
            AppEvent::Tick | AppEvent::Resize => self.forward_top(ev),
            other => {
                let id = match &other {
                    AppEvent::Data { view, .. }
                    | AppEvent::Text { view, .. }
                    | AppEvent::LogLines { view, .. }
                    | AppEvent::LogEnd { view, .. }
                    | AppEvent::Error { view, .. }
                    | AppEvent::Info { view, .. }
                    | AppEvent::Custom { view, .. } => *view,
                    _ => return,
                };
                if let Some(v) = self.stack.iter_mut().find(|v| v.id() == id) {
                    let cmds = v.on_event(&other, &mut self.ctx);
                    self.apply(cmds);
                }
            }
        }
    }

    fn forward_top(&mut self, ev: AppEvent) {
        if let Some(top) = self.stack.last_mut() {
            let cmds = top.on_event(&ev, &mut self.ctx);
            self.apply(cmds);
        }
    }

    fn shell_event(&mut self, ev: ShellEvent) {
        match ev {
            ShellEvent::HasMetrics(b) => self.ctx.has_metrics = b,
            ShellEvent::Contexts(c) => self.contexts = c,
            ShellEvent::ClusterInfoFailed(e) => {
                tracing::error!(error = %e, "cluster info refresh failed");
                if let Some(top) = self.stack.last_mut() {
                    top.stop(&mut self.ctx);
                }
                self.con_retry += 1;
                let max = self.cfg(|c| c.k9s.max_conn_retry);
                if self.con_retry >= max {
                    self.exit_status = Some(format!(
                        "Lost K8s connection ({}). Bailing out!",
                        self.con_retry
                    ));
                    self.bail_out();
                    return;
                }
                self.status(
                    FlashLevel::Warn,
                    format!("Dial K8s Toast [{}/{}]", self.con_retry, max),
                );
            }
            ShellEvent::ContextSwitched {
                name,
                result,
                ns,
                then,
            } => self.context_switched(name, result, ns, then),
        }
    }

    /// view/cluster_info.go setDefCon.
    fn defcon(&mut self) {
        if !self.ctx.has_metrics {
            return;
        }
        let th = self.cfg(|c| c.k9s.thresholds.clone());
        let mut set = false;
        for (sev, v, what) in [
            (&th.cpu, self.ctx.cluster_info.cpu_pct, "CPU"),
            (&th.memory, self.ctx.cluster_info.mem_pct, "Memory"),
        ] {
            let Some(v) = v else { continue };
            if !(0..=100).contains(&v) {
                continue;
            }
            let lvl = if v >= sev.critical as i64 {
                Some((FlashLevel::Err, "Critical"))
            } else if v >= sev.warn as i64 {
                Some((FlashLevel::Warn, "Warning"))
            } else {
                None
            };
            if let Some((l, m)) = lvl {
                self.status(l, format!("{m} {what} level!"));
                set = true;
            }
        }
        if !set {
            self.logo_status = None;
            self.flash.clear();
        }
    }

    /// view/app.go Status: logo status with header, indicator otherwise.
    fn status(&mut self, l: FlashLevel, msg: impl Into<String>) {
        let msg = msg.into();
        if self.show_header {
            self.logo_status = Some((l, msg));
        } else {
            self.indicator_status = Some((l, msg, Instant::now()));
        }
    }

    // -----------------------------------------------------------------------
    // View commands

    pub(crate) fn apply(&mut self, cmds: Vec<ViewCmd>) {
        let mut q: VecDeque<ViewCmd> = cmds.into();
        while let Some(c) = q.pop_front() {
            let more = self.apply_one(c);
            for m in more.into_iter().rev() {
                q.push_front(m);
            }
        }
    }

    fn apply_one(&mut self, c: ViewCmd) -> Vec<ViewCmd> {
        match c {
            ViewCmd::Push(v) => return self.push(v),
            ViewCmd::Pop => {
                if self.stack.len() > 1 {
                    if let Some(mut v) = self.stack.pop() {
                        v.stop(&mut self.ctx);
                    }
                    if let Some(top) = self.stack.last_mut() {
                        return top.start(&mut self.ctx);
                    }
                }
            }
            ViewCmd::Replace(v) => {
                if let Some(mut old) = self.stack.pop() {
                    old.stop(&mut self.ctx);
                }
                self.stack.push(v);
                return self.start_top();
            }
            ViewCmd::Reset(v) => return self.inject(v, true),
            ViewCmd::Quit => self.bail_out(),
            ViewCmd::Flash(l, s) => self.flash.set(l, s),
            ViewCmd::Command(s) => self.goto(&s, true, true),
            ViewCmd::OpenCommandPrompt(s) => self.open_prompt(PromptKind::Command, s),
            ViewCmd::OpenFilterPrompt => self.open_prompt(PromptKind::Filter, String::new()),
            ViewCmd::Dialog(d) => self.dialog = Some(DialogState::new(d, &self.ctx)),
            ViewCmd::Suspend { argv, env, on_done } => {
                self.suspends.push_back(Suspend { argv, env, on_done })
            }
            ViewCmd::SetNamespace(ns) => self.switch_ns(&ns),
            ViewCmd::ToggleHeader => self.show_header = !self.show_header,
            ViewCmd::ToggleCrumbs => self.show_crumbs = !self.show_crumbs,
            ViewCmd::ToggleFullScreen => self.fullscreen = !self.fullscreen,
            ViewCmd::RefreshClusterInfo => self.refresh_cluster_now(),
        }
        Vec::new()
    }

    fn push(&mut self, v: Box<dyn View>) -> Vec<ViewCmd> {
        if let Some(top) = self.stack.last_mut() {
            top.stop(&mut self.ctx);
        }
        self.stack.push(v);
        self.start_top()
    }

    fn start_top(&mut self) -> Vec<ViewCmd> {
        match self.stack.last_mut() {
            Some(top) => top.start(&mut self.ctx),
            None => Vec::new(),
        }
    }

    /// view/app.go inject.
    fn inject(&mut self, v: Box<dyn View>, clear: bool) -> Vec<ViewCmd> {
        if clear {
            while let Some(mut old) = self.stack.pop() {
                old.stop(&mut self.ctx);
            }
        }
        self.push(v)
    }

    fn after_suspend(&mut self, s: Suspend, res: io::Result<std::process::ExitStatus>) {
        let status = match res {
            Ok(st) => st,
            Err(e) => {
                self.flash.set(
                    FlashLevel::Err,
                    format!("Exec failed {}: {e}", go_quote(&s.argv.join(" "))),
                );
                failed_status()
            }
        };
        if let Some(f) = s.on_done {
            let cmds = f(&mut self.ctx, status);
            self.apply(cmds);
        }
    }

    // -----------------------------------------------------------------------
    // Commands (view/command.go)

    fn bail_out(&mut self) {
        self.quit = true;
    }

    fn save_config(&self) {
        if let Ok(c) = self.ctx.config.read() {
            if let Err(e) = (self.hooks.save_config)(&c) {
                tracing::error!(error = %e, "config save failed");
            }
        }
    }

    fn save_cmd(&mut self) {
        let res = self
            .ctx
            .config
            .read()
            .map_err(|e| anyhow!("{e}"))
            .and_then(|c| (self.hooks.save_config)(&c));
        if let Err(e) = res {
            self.flash.set(FlashLevel::Err, e.to_string());
        }
        self.flash
            .set(FlashLevel::Info, "current context config saved");
    }

    fn top_name(&self) -> Option<String> {
        self.stack.last().map(|v| v.name())
    }

    fn help_cmd(&mut self) {
        if self.top_name().as_deref() == Some(HELP_NAME) {
            self.apply(vec![ViewCmd::Pop]);
            return;
        }
        let hints = self.stack.last().map(|v| v.hints()).unwrap_or_default();
        let v = super::shell::help(&mut self.ctx, hints);
        let cmds = self.inject(v, false);
        self.apply(cmds);
        self.prompt = None;
    }

    fn alias_cmd(&mut self) {
        if self.top_name().as_deref() == Some("aliases") {
            self.apply(vec![ViewCmd::Pop]);
            return;
        }
        let c = Command {
            gvr: "aliases".into(),
            alias: "aliases".into(),
            ..Default::default()
        };
        match (self.hooks.browser)(&mut self.ctx, &c) {
            Ok(v) => {
                let cmds = self.inject(v, false);
                self.apply(cmds);
            }
            Err(e) => self.flash.set(FlashLevel::Err, e.to_string()),
        }
    }

    /// view/app.go gotoResource: run a command line, errors pop a dialog.
    pub(crate) fn goto(&mut self, line: &str, clear_stack: bool, push_cmd: bool) {
        if let Err(e) = self.run_line(line, clear_stack, push_cmd) {
            self.show_error(e.to_string());
        }
    }

    fn show_error(&mut self, msg: String) {
        self.dialog = Some(DialogState::new(Dialog::Error { msg }, &self.ctx));
    }

    /// view/command.go defaultCmd(isRoot = true).
    fn default_command(&mut self) {
        let line = self.opts.command.trim().to_string();
        if line.is_empty() {
            self.goto(CTX_CMD, true, true);
            return;
        }
        if let Err(e) = self.run_line(&line, true, true) {
            tracing::error!(command = %line, error = %e, "command exec failed, using default command");
            self.goto(CTX_CMD, true, true);
        }
    }

    fn run_line(&mut self, line: &str, clear_stack: bool, push_cmd: bool) -> Result<()> {
        let line = line.trim();
        let (cmd, p) = match crate::cmd::interpret(line, &self.ctx.aliases) {
            Ok(r) => r,
            Err(e) => {
                // specialCmd failures are flashed; resource lookups pop a dialog.
                let first = line.split_whitespace().next().unwrap_or_default();
                if FLASHED_CMDS.contains(&first) {
                    self.flash.set(FlashLevel::Err, e.to_string());
                    return Ok(());
                }
                return Err(e);
            }
        };
        self.dispatch(cmd, &p.get_line(), clear_stack, push_cmd)
    }

    /// view/command.go run: special commands first, then resource views.
    /// `line` is the canonical command line (history / view.active).
    pub(crate) fn dispatch(
        &mut self,
        cmd: Command,
        line: &str,
        clear_stack: bool,
        push_cmd: bool,
    ) -> Result<()> {
        match cmd.gvr.as_str() {
            crate::cmd::COW_CMD => {
                self.show_error(cmd.args.join(" "));
            }
            crate::cmd::QUIT_CMD => self.bail_out(),
            crate::cmd::HELP_CMD => self.help_cmd(),
            crate::cmd::LAST_CMD => match self.cmd_history.top() {
                Some(c) => return self.run_line(&c, true, false),
                None => self
                    .flash
                    .set(FlashLevel::Warn, "No previous view to switch to"),
            },
            crate::cmd::ALIASES_CMD => match (self.hooks.browser)(&mut self.ctx, &cmd) {
                Ok(v) => self.exec(line, v, false, push_cmd),
                Err(e) => self.flash.set(FlashLevel::Err, e.to_string()),
            },
            crate::cmd::XRAY_CMD => self.xray_cmd(&cmd, line, push_cmd),
            crate::cmd::POLICY_CMD => match (self.hooks.browser)(&mut self.ctx, &cmd) {
                Ok(v) => {
                    let cmds = self.inject(v, true);
                    self.apply(cmds);
                }
                Err(e) => self.flash.set(FlashLevel::Err, e.to_string()),
            },
            crate::cmd::CONTEXTS_CMD => match cmd.context.clone().filter(|c| !c.is_empty()) {
                Some(n) => self.use_context(&n, None, None),
                None => match (self.hooks.browser)(&mut self.ctx, &cmd) {
                    Ok(v) => self.exec(line, v, true, push_cmd),
                    Err(e) => self.flash.set(FlashLevel::Err, e.to_string()),
                },
            },
            crate::cmd::DIRS_CMD => {
                let path = cmd.args.first().cloned().unwrap_or_default();
                if let Err(e) = self.dir_cmd(&cmd, &path, push_cmd) {
                    self.flash.set(FlashLevel::Err, e.to_string());
                }
            }
            _ => return self.resource_cmd(cmd, line, clear_stack, push_cmd),
        }
        Ok(())
    }

    fn resource_cmd(
        &mut self,
        mut cmd: Command,
        line: &str,
        clear_stack: bool,
        push_cmd: bool,
    ) -> Result<()> {
        if cmd.gvr.is_empty() {
            return Err(anyhow!("`{}` command not found", cmd.alias));
        }
        if let Some(c) = cmd.context.clone().filter(|c| !c.is_empty()) {
            if c != self.ctx.accessor.active_context() {
                self.save_config();
                let then = strip_context(line);
                self.use_context(&c, cmd.namespace.clone(), Some(then));
                return Ok(());
            }
        }
        let mut ns = self.ctx.namespace.clone();
        if let Some(cns) = cmd.namespace.clone().filter(|s| !s.is_empty()) {
            ns = cns;
        }
        let namespaced = self
            .ctx
            .resources
            .iter()
            .find(|m| m.gvr.to_string() == cmd.gvr)
            .map(|m| m.namespaced)
            .unwrap_or(false);
        if namespaced {
            self.switch_ns(&ns);
            cmd.namespace = Some(self.ctx.namespace.clone());
        } else {
            cmd.namespace = None;
        }
        let view = if cmd.gvr == "pulses" {
            let ns = self.ctx.namespace.clone();
            (self.hooks.pulses)(&mut self.ctx, ns)
        } else {
            (self.hooks.browser)(&mut self.ctx, &cmd)?
        };
        self.exec(line, view, clear_stack, push_cmd);
        Ok(())
    }

    /// view/command.go exec.
    fn exec(&mut self, line: &str, v: Box<dyn View>, clear_stack: bool, push_cmd: bool) {
        if clear_stack {
            let active = strip_context(line);
            self.persist(Some(&active), None);
        }
        let cmds = self.inject(v, clear_stack);
        self.apply(cmds);
        if push_cmd {
            self.cmd_history.push(line);
        }
    }

    /// view/command.go xrayCmd (resource already resolved and validated by parse).
    fn xray_cmd(&mut self, cmd: &Command, line: &str, push_cmd: bool) {
        let Some(gvr) = cmd.args.first().cloned() else {
            self.flash
                .set(FlashLevel::Err, "invalid command. use `xray xxx`");
            return;
        };
        let ns = cmd
            .namespace
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| self.ctx.namespace.clone());
        self.switch_ns(&ns);
        let ns = self.ctx.namespace.clone();
        let v = (self.hooks.xray)(&mut self.ctx, Gvr::parse(&gvr), ns);
        self.exec(line, v, true, push_cmd);
    }

    fn dir_cmd(&mut self, cmd: &Command, path: &str, push_cmd: bool) -> Result<()> {
        let mut path = path.to_string();
        std::fs::metadata(&path).map_err(|e| anyhow!("stat {path}: {}", go_io_error(&e)))?;
        if path == "." {
            if let Ok(d) = std::env::current_dir() {
                path = d.display().to_string();
            }
        }
        if push_cmd {
            self.cmd_history.push(&format!("dir {path}"));
        }
        let mut c = cmd.clone();
        c.args = vec![path];
        let v = (self.hooks.browser)(&mut self.ctx, &c)?;
        let cmds = self.inject(v, true);
        self.apply(cmds);
        Ok(())
    }

    /// view/app.go switchNS.
    fn switch_ns(&mut self, ns: &str) {
        if self.ctx.namespace == ns {
            return;
        }
        let ns = if ns == CLUSTER_SCOPE { "" } else { ns };
        self.ctx.namespace = ns.to_string();
        self.persist(None, Some(ns));
    }

    fn current_cluster(&self) -> String {
        let active = self.ctx.accessor.active_context();
        self.contexts
            .iter()
            .find(|c| c.name == active)
            .map(|c| c.cluster.clone())
            .unwrap_or_else(|| self.ctx.cluster_info.cluster.clone())
    }

    fn persist(&self, view: Option<&str>, ns: Option<&str>) {
        let cluster = self.current_cluster();
        let context = self.ctx.accessor.active_context();
        if let Err(e) = (self.hooks.persist)(&cluster, &context, view, ns) {
            tracing::warn!(error = %e, "unable to persist context config");
        }
    }

    /// view/context.go useContext: switch asynchronously, then navigate.
    fn use_context(&mut self, name: &str, ns: Option<String>, then: Option<String>) {
        if let Some(top) = self.stack.last_mut() {
            top.stop(&mut self.ctx);
        }
        self.save_config();
        let acc = self.ctx.accessor.clone();
        let tx = self.ctx.sender();
        let name = name.to_string();
        self.ctx.spawn(async move {
            let result = acc.switch_context(&name).await.map_err(|e| e.to_string());
            let _ = tx.send(shell_event(ShellEvent::ContextSwitched {
                name,
                result,
                ns,
                then,
            }));
        });
    }

    /// view/app.go switchContext (after the accessor reconnected).
    fn context_switched(
        &mut self,
        name: String,
        result: std::result::Result<String, String>,
        ns: Option<String>,
        then: Option<String>,
    ) {
        let default_ns = match result {
            Ok(ns) => ns,
            Err(e) => {
                self.flash.set(FlashLevel::Err, e);
                if let Some(top) = self.stack.last_mut() {
                    let cmds = top.start(&mut self.ctx);
                    self.apply(cmds);
                }
                return;
            }
        };
        let (cluster, kube_ns) = self
            .contexts
            .iter()
            .find(|c| c.name == name)
            .map(|c| (c.cluster.clone(), c.namespace.clone()))
            .unwrap_or_default();
        let cc = (self.hooks.load_context)(&cluster, &name).ok();
        // K9s.ActivateContext: the kubeconfig context namespace wins over
        // the saved one.
        let mut target_ns = if kube_ns.is_empty() {
            cc.as_ref()
                .map(|c| c.namespace.active.clone())
                .filter(|s| !s.is_empty())
                .unwrap_or(default_ns)
        } else {
            kube_ns
        };
        if let Some(n) = ns.filter(|s| !s.is_empty()) {
            target_ns = n;
        }
        let mut view = then
            .or_else(|| cc.as_ref().map(|c| c.view.active.clone()))
            .unwrap_or_default();
        let first = view.split_whitespace().next().unwrap_or_default();
        if view.is_empty() || CONTEXT_CMDS.contains(&first) {
            view = crate::k8s::known::pods().to_string();
        }
        self.ctx.namespace.clear();
        self.switch_ns(&target_ns);
        self.flash.set(
            FlashLevel::Info,
            format!(
                "Switching context to {}::{}",
                go_quote(&name),
                go_quote(&target_ns)
            ),
        );
        self.ctx.resources.clear();
        self.ctx.cluster_info = ClusterInfo::default();
        self.info_reset = true;
        if std::mem::take(&mut self.opts.disconnected) {
            self.start_background();
        } else {
            self.start_background_once();
        }
        self.goto(&view, true, true);
    }

    fn start_background_once(&mut self) {
        let acc = self.ctx.accessor.clone();
        let tx = self.ctx.sender();
        let cfg = self.ctx.config.clone();
        self.ctx.spawn(async move {
            if let Ok(r) = acc.resources().await {
                let _ = tx.send(AppEvent::Resources(r));
            }
            if let Ok(c) = acc.contexts().await {
                let _ = tx.send(shell_event(ShellEvent::Contexts(c)));
            }
            refresh_cluster(&acc, &tx, &cfg).await;
        });
    }

    // -----------------------------------------------------------------------
    // Drawing

    /// Draw the whole screen (test API for integration tests).
    #[doc(hidden)]
    pub fn draw(&mut self, f: &mut Frame<'_>) {
        let area = f.area();
        let styles = self.ctx.styles.clone();
        fill(
            f.buffer_mut(),
            area,
            Style::default().fg(styles.fg).bg(styles.bg),
        );
        if self.splash_until.is_some() {
            splash::draw(f.buffer_mut(), area, crate::version::VERSION, &styles);
            return;
        }
        let no_icons = self.no_icons();
        let full = self.fullscreen || self.stack.last().is_some_and(|v| v.is_fullscreen());
        let mut y = area.y;
        let bottom = area.bottom();
        let take = |y: &mut u16, h: u16| -> Rect {
            let h = h.min(bottom.saturating_sub(*y));
            let r = Rect::new(area.x, *y, area.width, h);
            *y += h;
            r
        };
        let hints = self.stack.last().map(|v| v.hints()).unwrap_or_default();
        let model = HeaderModel {
            info: &self.ctx.cluster_info,
            prev: &self.prev_info,
            has_metrics: self.ctx.has_metrics,
            readonly: self.ctx.readonly,
            no_icons,
            hints: &hints,
            show_logo: self.show_logo,
            status: self.logo_status.as_ref(),
        };
        if !full {
            if self.show_header {
                let r = take(&mut y, HEADER_HEIGHT);
                header::draw_header(f.buffer_mut(), r, &model, &styles);
            } else {
                let r = take(&mut y, 1);
                let temp = self
                    .indicator_status
                    .as_ref()
                    .map(|(l, m, _)| (*l, m.clone()));
                header::draw_indicator(f.buffer_mut(), r, &model, temp.as_ref(), &styles);
            }
        }
        let mut cursor = None;
        if let Some(p) = &self.prompt {
            let r = take(&mut y, PROMPT_HEIGHT);
            cursor = Some(p.draw(f.buffer_mut(), r, &styles, no_icons));
        }
        let crumbs_h = if self.show_crumbs && !full { 1 } else { 0 };
        let flash_h = 1;
        let content_h = bottom.saturating_sub(y).saturating_sub(crumbs_h + flash_h);
        let content = take(&mut y, content_h);
        let focused = self.prompt.is_none() && self.dialog.is_none();
        match self.stack.last_mut() {
            Some(top) if top.owns_frame() => {
                top.draw(f, content, &self.ctx);
                if !focused {
                    title::blur_frame(f.buffer_mut(), content, &styles);
                }
            }
            Some(top) => {
                let t = title::title_segs(&top.title(), &top.filter_text(), &styles);
                let inner = title::draw_frame(f.buffer_mut(), content, &t, focused, &styles);
                top.draw(f, inner, &self.ctx);
            }
            None => {
                title::draw_frame(f.buffer_mut(), content, &[], focused, &styles);
            }
        }
        if crumbs_h > 0 {
            let r = take(&mut y, 1);
            let names: Vec<String> = self.stack.iter().map(|v| v.name()).collect();
            crumbs::draw(f.buffer_mut(), r, &names, &styles);
        }
        let r = take(&mut y, flash_h);
        self.flash.draw(f.buffer_mut(), r, &styles, no_icons);
        if let Some(d) = &self.dialog {
            cursor = d.draw(f.buffer_mut(), area, &styles);
        }
        if let Some(c) = cursor {
            f.set_cursor_position(c);
        }
    }
}

/// Rebuild `old` around `tx`, keeping every public field.
fn rewire(old: Ctx, tx: mpsc::UnboundedSender<AppEvent>) -> Ctx {
    let mut c = Ctx::new(
        old.accessor.clone(),
        old.config.clone(),
        old.styles.clone(),
        old.namespace.clone(),
        tx,
    );
    c.readonly = old.readonly;
    c.resources = old.resources;
    c.namespaces = old.namespaces;
    c.aliases = old.aliases;
    c.hotkeys = old.hotkeys;
    c.plugins = old.plugins;
    c.views = old.views;
    c.cluster_info = old.cluster_info;
    c.port_forwards = old.port_forwards;
    c.has_metrics = old.has_metrics;
    c
}

fn shell_event(ev: ShellEvent) -> AppEvent {
    AppEvent::Custom {
        view: SHELL_VIEW_ID,
        payload: Box::new(ev),
    }
}

/// ClusterInfo.Refresh: cluster meta plus the latest k9s release check.
async fn refresh_cluster(
    acc: &SharedAccessor,
    tx: &mpsc::UnboundedSender<AppEvent>,
    cfg: &SharedConfig,
) {
    let mx = acc.has_metrics().await;
    let _ = tx.send(shell_event(ShellEvent::HasMetrics(mx)));
    match acc.cluster_info().await {
        Ok(mut ci) => {
            // Unit tests never reach out to GitHub.
            let skip = cfg!(test) || cfg.read().is_ok_and(|c| c.k9s.skip_latest_rev_check);
            let latest = if skip {
                String::new()
            } else {
                crate::version::latest_rev().await
            };
            (ci.k9s_version, ci.k9s_latest) = crate::version::k9s_revs(&ci.k9s_version, &latest);
            let _ = tx.send(AppEvent::ClusterInfo(ci));
        }
        Err(e) => {
            let _ = tx.send(shell_event(ShellEvent::ClusterInfoFailed(e.to_string())));
            return;
        }
    }
    if let Ok(ns) = acc.namespaces().await {
        let _ = tx.send(AppEvent::Namespaces(ns));
    }
}

/// contextRX: drop ` @ctx` from a command line.
fn strip_context(line: &str) -> String {
    let mut out = String::new();
    for w in line.split_whitespace() {
        if w.starts_with('@') && w.len() > 1 {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(w);
    }
    out
}

/// Go's syscall error text for common stat failures.
fn go_io_error(e: &io::Error) -> String {
    match e.kind() {
        io::ErrorKind::NotFound => "no such file or directory".into(),
        io::ErrorKind::PermissionDenied => "permission denied".into(),
        _ => e.to_string(),
    }
}

fn failed_status() -> std::process::ExitStatus {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(127 << 8)
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(1)
    }
}

/// Run `argv` in the foreground with inherited stdio.
fn run_foreground(
    argv: &[String],
    env: &[(String, String)],
) -> io::Result<std::process::ExitStatus> {
    let Some((bin, args)) = argv.split_first() else {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty command"));
    };
    std::process::Command::new(bin)
        .args(args)
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .status()
}

// ---------------------------------------------------------------------------
// Terminal

struct Tui {
    term: Terminal<CrosstermBackend<Stdout>>,
    mouse: bool,
    /// tcell-style color fitting for the terminal.
    colors: super::shell::palette::Fitter,
}

impl Tui {
    fn enter(mouse: bool) -> Result<Tui> {
        enter_screen(mouse)?;
        let mut term = Terminal::new(CrosstermBackend::new(io::stdout()))?;
        term.clear()?;
        let colors = super::shell::palette::Fitter::new(super::shell::palette::detect_env());
        Ok(Tui {
            term,
            mouse,
            colors,
        })
    }

    /// Leave the alt screen/raw mode, run `f`, then restore the TUI.
    fn suspend<R>(&mut self, f: impl FnOnce() -> R) -> io::Result<R> {
        restore_screen(self.mouse);
        // k9s execute(): clear the screen before/after shelling out.
        print!("\x1b[H\x1b[2J");
        let r = f();
        enter_screen(self.mouse)?;
        self.term.clear()?;
        Ok(r)
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        restore_screen(self.mouse);
    }
}

fn enter_screen(mouse: bool) -> io::Result<()> {
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)?;
    if mouse {
        execute!(io::stdout(), EnableMouseCapture)?;
    }
    Ok(())
}

fn restore_screen(mouse: bool) {
    if mouse {
        let _ = execute!(io::stdout(), DisableMouseCapture);
    }
    let _ = execute!(io::stdout(), LeaveAlternateScreen, crossterm::cursor::Show);
    let _ = disable_raw_mode();
}

fn install_panic_hook(mouse: bool) {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_screen(mouse);
        prev(info);
    }));
}

#[cfg(test)]
#[path = "shell/app_tests.rs"]
mod tests;
