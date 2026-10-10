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

use super::shell::dialog::DialogState;
use super::shell::flash::Flash;
use super::shell::header::{self, HeaderModel, HEADER_HEIGHT};
use super::shell::help::HELP_NAME;
use super::shell::prompt::{Prompt, PromptAction, PromptKind, PROMPT_HEIGHT};
use super::shell::text::{fill, go_quote};
use super::shell::watch::{self, Change, Targets, Watch};
use super::shell::{crumbs, splash, title, SHELL_VIEW_ID};
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
    /// Kubeconfig contexts read at startup (the context -> cluster map
    /// before the first async refresh).
    pub contexts: Vec<ContextInfo>,
    /// Config locations; enables skin reloads and the `ui.reactive` file
    /// watchers (none in tests).
    pub locations: Option<crate::config::paths::Locations>,
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
type PersistFn = Box<dyn Fn(&str, &str, &ContextConfig) -> Result<()> + Send>;
type SaveFn = Box<dyn Fn(&Config) -> Result<()> + Send>;

/// Calls into other slices (command parsing, views, config persistence).
/// Indirected so tests can substitute them.
pub(crate) struct Hooks {
    pub browser: BrowserFn,
    pub xray: XrayFn,
    pub pulses: PulsesFn,
    pub add_resources: AddResourcesFn,
    pub load_context: LoadContextFn,
    /// `(cluster, context, config)` → save the context config (K9s.Save).
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
            persist: Box::new(|cluster, context, cc| {
                tracing::debug!(cluster, context, "[CONFIG] Saving context config to disk");
                cc.save(cluster, context)
            }),
            // Config.Save: config.yaml is only written when missing.
            save_config: Box::new(|c| {
                if crate::config::paths::config_file().exists() {
                    return Ok(());
                }
                c.save()
            }),
        }
    }
}

/// Shell-internal async results (sent as `AppEvent::Custom` to `SHELL_VIEW_ID`).
enum ShellEvent {
    HasMetrics(bool),
    ConnOk,
    Contexts(Vec<ContextInfo>),
    ClusterInfoFailed(String),
    /// CanI list metrics.k8s.io nodes (header CPU/MEM).
    NodeMetricsAccess(bool),
    /// clusterUpdater: the backoff budget ran out (BailOut).
    UpdaterGaveUp,
    /// The in-process exec/attach connected.
    TtyReady {
        proc: Box<crate::k8s::TtyProcess>,
        attach: bool,
        banner: String,
        on_done: Option<DoneFn>,
    },
    /// In-process exec isn't available: run kubectl instead.
    TtyFallback {
        argv: Vec<String>,
        env: Vec<(String, String)>,
        banner: String,
        on_done: Option<DoneFn>,
    },
    /// The edit target was fetched and written to a temp file.
    EditReady(std::result::Result<(tempfile::TempPath, String, serde_json::Value), String>),
    /// The edited file was handled.
    EditOutcome(crate::ui::shell::session::EditOutcome),
    /// ValidatePortForwards: forwards whose pod vanished or was recreated.
    StalePortForwards(Vec<String>),
    /// Cluster meta refreshed without a connection.
    Meta(ClusterInfo),
    /// ui.reactive watchers saw config/skin/views changes.
    Watched(Vec<(Watch, Change)>),
    ContextSwitched {
        name: String,
        result: std::result::Result<String, String>,
        ns: Option<String>,
        then: Option<String>,
        /// The new cluster's resources (view/command.go Reset reloads them
        /// before the first view), when they could be listed.
        resources: Option<Vec<ResourceMeta>>,
    },
}

/// A port forward ended with an error after it started.
pub(crate) struct PortForwardDied {
    pub id: String,
    pub err: String,
}

struct Suspend {
    argv: Vec<String>,
    env: Vec<(String, String)>,
    banner: String,
    on_done: Option<Box<dyn FnOnce(&mut Ctx, std::process::ExitStatus) -> Vec<ViewCmd> + Send>>,
    kind: SuspendKind,
}

/// What runs while the TUI is suspended.
enum SuspendKind {
    /// A foreground process (argv).
    Process,
    /// The in-process edit loop's editor (argv) on the temp file.
    Editor,
    /// A connected in-process TTY session; `true` for attach.
    Tty(Box<crate::k8s::TtyProcess>, bool),
}

/// The in-process `kubectl edit` loop.
struct EditState {
    gvr: crate::k8s::Gvr,
    path: String,
    file: Option<tempfile::TempPath>,
    original_text: String,
    original: serde_json::Value,
    /// The previous attempt's file, when it is being retried after an error.
    retry_of: Option<String>,
    on_done: Option<Box<dyn FnOnce(&mut Ctx, std::process::ExitStatus) -> Vec<ViewCmd> + Send>>,
}

type DoneFn = Box<dyn FnOnce(&mut Ctx, std::process::ExitStatus) -> Vec<ViewCmd> + Send>;

/// dao/registry.go: pseudo resources registered as Namespaced, so
/// `:workloads kube-system` switches namespace like a real resource.
const NAMESPACED_PSEUDO: &[&str] = &[
    "workloads",
    "portforwards",
    "helm",
    "helm-history",
    "policy",
];

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
    /// useCtx pending: clear the histories once the switch lands.
    clear_history_on_switch: bool,
    quit: bool,
    /// command.go exec is falling back after a panic.
    exec_recovering: bool,
    /// Node metrics list access (cluster_info.go hasMetrics CanI).
    node_mx_ok: bool,
    exit_status: Option<String>,
    suspends: VecDeque<Suspend>,
    edit: Option<EditState>,
    prev_info: ClusterInfo,
    /// Context switched: like k9s ClusterInfo.Reset, the next refresh diffs
    /// against an empty meta (zero CPU/MEM).
    info_reset: bool,
    /// view/cluster_info.go Init (border padding) only runs when k9s starts
    /// connected.
    info_padded: bool,
    contexts: Vec<ContextInfo>,
    con_retry: i32,
    /// Configurator.skinFile: the loaded skin file, if any.
    skin_file: Option<std::path::PathBuf>,
    /// The context config the ConfigWatcher follows.
    watch_ctx: std::sync::Arc<std::sync::Mutex<Option<std::path::PathBuf>>>,
    /// tview focus: a click outside the content (header, crumbs, flash)
    /// focuses that box, so view keys stop reaching the view.
    content_focused: bool,
    /// Content area of the last draw (mouse hit testing).
    content_area: Rect,
    /// Where the left button went down (a release there is a click).
    mouse_down: Option<(u16, u16)>,
    /// A click outside the dialog / prompt moved tview focus away: they stay
    /// on screen but keys go to the newly focused box.
    dialog_blurred: bool,
    prompt_blurred: bool,
    /// Prompt area of the last draw.
    prompt_area: Option<Rect>,
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
        ctx.con_ok = !opts.disconnected;
        App {
            show_header: !(opts.headless || ui.headless),
            show_logo: !(opts.logoless || ui.logoless),
            show_crumbs: !(opts.crumbsless || ui.crumbsless),
            show_splash: !(opts.splashless || ui.splashless),
            fullscreen: ui.defaults_to_full_screen,
            ctx,
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
            clear_history_on_switch: false,
            quit: false,
            exec_recovering: false,
            node_mx_ok: false,
            exit_status: None,
            suspends: VecDeque::new(),
            edit: None,
            prev_info: ClusterInfo::default(),
            info_reset: false,
            info_padded: !opts.disconnected,
            contexts: opts.contexts.clone(),
            con_retry: 0,
            skin_file: None,
            watch_ctx: Default::default(),
            content_focused: true,
            content_area: Rect::default(),
            mouse_down: None,
            dialog_blurred: false,
            prompt_blurred: false,
            prompt_area: None,
            opts,
        }
    }

    /// Run the TUI until quit.
    pub async fn run(mut self) -> Result<()> {
        let mouse = self.cfg(|c| c.k9s.ui.enable_mouse);
        install_panic_hook(mouse);
        let mut tui = Tui::enter(mouse)?;
        init_signals();
        // view/app.go Init: initImgScanner when imageScans.enable.
        let scans = self.cfg(|c| c.k9s.image_scans.clone());
        if scans.enable {
            let cache = crate::config::paths::Xdg::current().cache_home;
            crate::vul::install(std::sync::Arc::new(crate::vul::new_for(scans, &cache)));
        }
        if self.show_splash {
            self.splash_until = Some(Instant::now() + splash::SPLASH_DELAY);
        }
        self.start_background();
        self.start_watchers();
        self.default_command();
        let res = self.event_loop(&mut tui).await;
        drop(tui);
        self.nuke_shell_pod().await;
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
            if let Some(mut s) = self.suspends.pop_front() {
                drop(events);
                let res = match std::mem::replace(&mut s.kind, SuspendKind::Process) {
                    SuspendKind::Tty(p, attach) => tui
                        .suspend_tty(&s.banner, attach, *p)
                        .await
                        .map(|code| Ok(crate::ui::shell::session::exit_status(code))),
                    kind => {
                        s.kind = kind;
                        tui.suspend(|| {
                            print!("{}", s.banner);
                            let _ = io::Write::flush(&mut io::stdout());
                            run_foreground(&s.argv, &s.env)
                        })
                    }
                };
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
        self.start_watchers();
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

    /// One refresh tick (k9s refreshRate).
    #[doc(hidden)]
    pub fn tick(&mut self) {
        self.handle_event(AppEvent::Tick);
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
        if r.is_nan() || r <= 0.0 {
            r = self.cfg(|c| c.k9s.refresh_rate);
        }
        if r.is_nan() || r <= 0.0 {
            r = 2.0;
        }
        // tokio intervals panic on a zero period.
        crate::config::k9s::secs(r as f64).max(Duration::from_millis(100))
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

    /// The active skin file (Configurator.activeSkin + SkinFileFromName).
    fn active_skin_file(&self) -> Option<std::path::PathBuf> {
        let locs = self.opts.locations.as_ref()?;
        let ui_skin = self.cfg(|c| c.k9s.ui.skin.clone());
        let ctx_skin = self
            .ctx
            .context_cfg
            .as_ref()
            .and_then(|c| c.skin_name().map(String::from));
        crate::config::skin::active_skin(
            locs,
            &crate::config::paths::ProcessEnv,
            &ui_skin,
            ctx_skin.as_deref(),
        )
        .map(|n| locs.skin_file_from_name(&n))
    }

    /// Configurator.RefreshStyles: refresh the cluster info and reload the
    /// skin (the context's, else the config's, else stock).
    fn refresh_styles(&mut self) {
        let Some(locs) = self.opts.locations.clone() else {
            return;
        };
        if !self.opts.disconnected {
            let (acc, tx, cfg) = (
                self.ctx.accessor.clone(),
                self.ctx.sender(),
                self.ctx.config.clone(),
            );
            self.ctx.spawn(async move {
                refresh_cluster(&acc, &tx, &cfg).await;
            });
        }
        let (ui_skin, invert) = self.cfg(|c| (c.k9s.ui.skin.clone(), c.k9s.is_invert()));
        let ctx_skin = self
            .ctx
            .context_cfg
            .as_ref()
            .and_then(|c| c.skin_name().map(String::from));
        self.ctx.styles = Styles::reload_active(
            &self.ctx.styles,
            &locs,
            &crate::config::paths::ProcessEnv,
            &ui_skin,
            ctx_skin.as_deref(),
            invert,
        );
        self.skin_file = self.active_skin_file();
    }

    /// The active context's config.yaml path.
    fn context_config_path(&self) -> Option<std::path::PathBuf> {
        let locs = self.opts.locations.as_ref()?;
        let (cluster, context) = (self.current_cluster(), self.ctx.accessor.active_context());
        (!cluster.is_empty() && !context.is_empty())
            .then(|| locs.context_config(&cluster, &context))
    }

    /// view/app.go Resume: start the watchers when `ui.reactive` is set.
    fn start_watchers(&mut self) {
        self.skin_file = self.active_skin_file();
        if !self.cfg(|c| c.k9s.ui.reactive) {
            return;
        }
        let Some(locs) = self.opts.locations.clone() else {
            return;
        };
        if let Ok(mut c) = self.watch_ctx.lock() {
            *c = self.context_config_path();
        }
        let targets = Targets {
            config: locs.config_file.clone(),
            context: self.watch_ctx.clone(),
            skins_dir: locs.skins_dir.is_dir().then(|| locs.skins_dir.clone()),
            views: locs.views_file.is_file().then(|| locs.views_file.clone()),
        };
        let tx = self.ctx.sender();
        self.ctx.spawn(async move {
            let mut p = watch::Poller::default();
            loop {
                let changes: Vec<(Watch, Change)> = p
                    .poll(&targets)
                    .into_iter()
                    .filter(|(w, c)| watch::wanted(w, *c))
                    .collect();
                if !changes.is_empty()
                    && tx.send(shell_event(ShellEvent::Watched(changes))).is_err()
                {
                    return;
                }
                if tx.is_closed() {
                    return;
                }
                tokio::time::sleep(watch::POLL).await;
            }
        });
    }

    /// ConfigWatcher / SkinsDirWatcher / CustomViewsWatcher reactions.
    fn watched(&mut self, changes: Vec<(Watch, Change)>) {
        for (w, _) in changes {
            match w {
                Watch::Config => {
                    let Some(path) = self.opts.locations.as_ref().map(|l| l.config_file.clone())
                    else {
                        continue;
                    };
                    let res = match self.ctx.config.write() {
                        Ok(mut c) => {
                            let r = c.load_file(&path);
                            // K9s.Merge replaces the UI struct and with it the
                            // flag overrides it carries.
                            let m = &mut c.k9s.manual;
                            m.headless = None;
                            m.logoless = None;
                            m.crumbsless = None;
                            m.splashless = None;
                            m.invert = None;
                            r
                        }
                        Err(e) => Err(anyhow!("{e}")),
                    };
                    if let Err(e) = res {
                        tracing::error!(error = %e, "K9rs config reload failed");
                        self.flash.set(
                            FlashLevel::Warn,
                            "k9rs config reload failed. Check k9rs logs!",
                        );
                        self.logo_status =
                            Some((FlashLevel::Warn, "K9rs config reload failed!".into()));
                    }
                    self.config_reloaded();
                }
                Watch::ContextConfig => {
                    let (cluster, context) =
                        (self.current_cluster(), self.ctx.accessor.active_context());
                    match (self.hooks.load_context)(&cluster, &context) {
                        Ok(cc) => self.ctx.context_cfg = Some(cc),
                        Err(e) => {
                            tracing::error!(error = %e, "K9rs context config reload failed");
                            self.flash.set(
                                FlashLevel::Warn,
                                "Context config reload failed. Check k9rs logs!",
                            );
                            self.logo_status =
                                Some((FlashLevel::Warn, "Context config reload failed!".into()));
                        }
                    }
                    self.config_reloaded();
                }
                Watch::Skin(p) => {
                    let same = self
                        .skin_file
                        .as_ref()
                        .is_some_and(|f| f.file_name() == p.file_name());
                    if same {
                        tracing::debug!(file = %p.display(), "Skin file changed detected");
                        self.refresh_styles();
                    }
                }
                Watch::Views => {
                    let Some(path) = self.opts.locations.as_ref().map(|l| l.views_file.clone())
                    else {
                        continue;
                    };
                    // RefreshCustomViews: Reset, then Load (errors only logged).
                    self.ctx.views.reset();
                    if path.exists() {
                        if let Err(e) = self.ctx.views.load_file(&path) {
                            tracing::warn!(error = %e, "Custom views refresh failed");
                            continue;
                        }
                    }
                    if let Some(top) = self.stack.last_mut() {
                        let ev = AppEvent::Custom {
                            view: top.id(),
                            payload: Box::new(super::browser::ViewSettingsChanged),
                        };
                        let cmds = top.on_event(&ev, &mut self.ctx);
                        self.apply(cmds);
                    }
                }
            }
        }
    }

    /// hotKeyActions / pluginActions re-read their files whenever a view
    /// builds its actions, so edits apply to the next view.
    fn reload_action_configs(&mut self) {
        let Some(locs) = self.opts.locations.clone() else {
            return;
        };
        let (cluster, context) = (self.current_cluster(), self.ctx.accessor.active_context());
        let has_ctx = !cluster.is_empty() && !context.is_empty();
        match crate::config::HotKeys::load_with(
            &locs,
            has_ctx
                .then(|| locs.context_hotkeys_file(&cluster, &context))
                .as_deref(),
        ) {
            Ok(h) => self.ctx.hotkeys = h,
            Err(e) => {
                tracing::warn!(error = %e, "Hotkeys load failed");
                self.logo_status = Some((FlashLevel::Warn, "HotKeys load failed!".into()));
            }
        }
        // pluginActions: skipped without a connection.
        if self.opts.disconnected {
            return;
        }
        let mut pp = crate::config::Plugins::default();
        match pp.load_all(
            &locs,
            has_ctx
                .then(|| locs.context_plugins_file(&cluster, &context))
                .as_deref(),
            true,
        ) {
            Ok(()) => self.ctx.plugins = pp,
            Err(e) => {
                tracing::warn!(error = %e, "Plugins load failed");
                self.logo_status = Some((FlashLevel::Warn, "Plugins load failed!".into()));
            }
        }
    }

    /// After a config reload: settings read live (K9s.IsReadOnly) and the
    /// styles.
    fn config_reloaded(&mut self) {
        let cc = self.ctx.context_cfg.clone();
        self.ctx.readonly = self.cfg(|c| c.k9s.is_read_only(cc.as_ref()));
        self.refresh_styles();
    }

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
            // ReloadStyles -> UpdateClusterInfo: one refresh without a
            // connection (n/a cluster meta, k9s release check).
            let tx = self.ctx.sender();
            let cfg = self.ctx.config.clone();
            self.ctx.spawn(async move {
                let ci = ClusterInfo {
                    k9s_version: crate::version::VERSION.into(),
                    ..Default::default()
                };
                let ci = with_latest_rev(ci, &cfg).await;
                let _ = tx.send(shell_event(ShellEvent::Meta(ci)));
            });
            return;
        }
        let acc = self.ctx.accessor.clone();
        let tx = self.ctx.sender();
        let cfg = self.ctx.config.clone();
        self.ctx.spawn(async move {
            // view/app.go clusterUpdater: a failed first refresh stops it.
            if !refresh_cluster(&acc, &tx, &cfg).await {
                tracing::error!("Cluster updater failed!");
                return;
            }
            // Failures back off exponentially (15s start, 2m budget since the
            // last success), then k9s bails out.
            let mut bf = ExpBackoff::new(CLUSTER_REFRESH, Duration::from_secs(120));
            let mut delay = CLUSTER_REFRESH;
            loop {
                if tx.is_closed() {
                    return;
                }
                tokio::time::sleep(delay).await;
                if refresh_cluster(&acc, &tx, &cfg).await {
                    bf.reset();
                    delay = CLUSTER_REFRESH;
                    continue;
                }
                match bf.next() {
                    Some(d) => delay = d,
                    None => {
                        let _ = tx.send(shell_event(ShellEvent::UpdaterGaveUp));
                        return;
                    }
                }
            }
        });
    }

    fn refresh_cluster_now(&self) {
        let acc = self.ctx.accessor.clone();
        let tx = self.ctx.sender();
        let cfg = self.ctx.config.clone();
        self.ctx.spawn(async move {
            refresh_cluster(&acc, &tx, &cfg).await;
        });
    }

    // -----------------------------------------------------------------------
    // Terminal events

    fn handle_term_event(&mut self, e: Event) {
        match e {
            Event::Key(k) if k.kind != KeyEventKind::Release => self.handle_key(k),
            Event::Resize(_, _) => self.handle_event(AppEvent::Resize),
            Event::Mouse(m) => self.handle_mouse(m),
            Event::Paste(s) if self.prompt.is_some() => {
                for c in s.chars() {
                    self.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
                }
            }
            _ => {}
        }
    }

    /// tview Application mouse routing (ui.enableMouse): left click = release
    /// where pressed; the box under it takes focus, the wheel goes to the
    /// primitive under the pointer.
    pub fn handle_mouse(&mut self, m: crossterm::event::MouseEvent) {
        use crate::ui::Mouse;
        use crossterm::event::{MouseButton, MouseEventKind};
        let (x, y) = (m.column, m.row);
        let pos = ratatui::layout::Position::new(x, y);
        let inside = self.content_area.contains(pos);
        let on_dialog = self.dialog.as_ref().is_some_and(|d| d.contains(x, y));
        let on_prompt = self.prompt.is_some() && self.prompt_area.is_some_and(|r| r.contains(pos));
        let ev = match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.mouse_down = Some((x, y));
                return;
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if self.mouse_down.take() != Some((x, y)) {
                    return;
                }
                // The modal page is on top: it takes the click first.
                if on_dialog {
                    self.dialog_blurred = false;
                    if let Some(mut d) = self.dialog.take() {
                        let out = d.click(x, y, &mut self.ctx);
                        if let Some((l, msg)) = d.pending_flash.take() {
                            self.flash.set(l, msg);
                        }
                        match out {
                            Some(cmds) => {
                                // Pages.RemovePage hands focus to a page
                                // that is not the view: k9s keys go dead
                                // until the content is clicked (live).
                                self.content_focused = false;
                                self.apply(cmds);
                            }
                            None => self.dialog = Some(d),
                        }
                    }
                    return;
                }
                if self.dialog.is_some() {
                    self.dialog_blurred = true;
                }
                if on_prompt {
                    self.prompt_blurred = false;
                    return;
                }
                if self.prompt.is_some() {
                    self.prompt_blurred = true;
                }
                self.content_focused = inside;
                Mouse::Click { x, y }
            }
            MouseEventKind::ScrollUp => Mouse::ScrollUp { x, y },
            MouseEventKind::ScrollDown => Mouse::ScrollDown { x, y },
            _ => return,
        };
        if !inside || on_dialog {
            return;
        }
        let area = self.content_area;
        if let Some(top) = self.stack.last_mut() {
            let cmds = top.on_mouse(ev, area, &mut self.ctx);
            self.apply(cmds);
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
        let dialog = if self.dialog_blurred {
            None
        } else {
            self.dialog.take()
        };
        if let Some(mut d) = dialog {
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
        if self.prompt.is_some() && !self.prompt_blurred {
            self.prompt_key(key);
            return;
        }
        let Some(key) = as_key(key) else {
            return;
        };
        let is = |c: char| key.code == KeyCode::Char(c) && !ctrl;
        let ctrl_is = |c: char| ctrl && key.code == KeyCode::Char(c);
        if is(':') {
            self.open_prompt(PromptKind::Command, String::new());
            // ui/app.go activateCmd: ClearText(true) after activation wipes
            // the shown history suggestion; Tab/Up still use the list.
            if let Some(p) = self.prompt.as_mut() {
                p.hide_suggestion();
            }
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
        } else if !self.content_focused {
            // The focused header/crumbs box swallows view keys and hotkeys.
        } else if let Some(top) = self.stack.last_mut() {
            // Hotkeys are view actions (browser and xray only, hotKeyActions).
            let cmds = top.handle_key(key, &mut self.ctx);
            self.apply(cmds);
        }
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
                    self.filter_view_typing(&text);
                }
            }
            PromptAction::Submit(text) => {
                self.prompt = None;
                if kind == PromptKind::Filter {
                    self.filter_view(&text);
                    if self.stack.last().is_some_and(|v| v.keeps_filter_history()) {
                        self.filter_history.push(&text);
                    }
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

    /// watch.Factory.ValidatePortForwards (each cluster refresh): stop
    /// forwards whose pod is gone or newer than the forward.
    fn validate_port_forwards(&self) {
        if self.ctx.port_forwards.is_empty() {
            return;
        }
        let pfs: Vec<(String, String, chrono::DateTime<chrono::Utc>)> = self
            .ctx
            .port_forwards
            .iter()
            .map(|(id, h)| {
                let s = &h.info.spec;
                (
                    id.clone(),
                    crate::k8s::fqn(&s.namespace, &s.pod),
                    h.info.started_at,
                )
            })
            .collect();
        let acc = self.ctx.accessor.clone();
        let tx = self.ctx.sender();
        self.ctx.spawn(async move {
            let pods = crate::k8s::Gvr::parse("v1/pods");
            let mut stale = Vec::new();
            for (id, path, started) in pfs {
                match acc.get(&pods, &path).await {
                    Err(_) => stale.push(id),
                    Ok(o) => {
                        let created = o
                            .metadata
                            .creation_timestamp
                            .as_ref()
                            .map(|t| t.0.as_second());
                        if created.is_some_and(|c| c > started.timestamp()) {
                            stale.push(id);
                        }
                    }
                }
            }
            if !stale.is_empty() {
                let _ = tx.send(shell_event(ShellEvent::StalePortForwards(stale)));
            }
        });
    }

    fn set_prompt_text(&mut self, text: String) {
        if let Some(p) = self.prompt.as_mut() {
            p.text = text;
            p.set_suggestions(Vec::new());
        }
    }

    fn filter_view_typing(&mut self, text: &str) {
        if let Some(top) = self.stack.last_mut() {
            let cmds = top.on_filter_typing(text, &mut self.ctx);
            self.apply(cmds);
        }
    }

    fn filter_view(&mut self, text: &str) {
        if let Some(top) = self.stack.last_mut() {
            let cmds = top.on_filter(text, &mut self.ctx);
            self.apply(cmds);
        }
    }

    fn open_prompt(&mut self, kind: PromptKind, text: String) {
        // A blurred prompt is still in command mode: activateCmd is a no-op.
        if self.prompt.is_some() && self.prompt_blurred {
            return;
        }
        self.prompt_blurred = false;
        // Closing the prompt hands focus back to the content.
        self.content_focused = true;
        self.prompt = Some(Prompt::new(kind, text));
        self.refresh_suggestions();
        // ResetPrompt clears the view's buffer: a live filter drops as `/` opens.
        if kind == PromptKind::Filter
            && self
                .stack
                .last()
                .is_some_and(|v| !v.filter_text().is_empty())
        {
            self.filter_view_typing("");
        }
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
                self.prev_info = std::mem::replace(&mut self.ctx.cluster_info, ci.clone());
                // After a context switch k9s refreshes twice in a row
                // (ClusterInfo.Reset, then the updater): no deltas shown.
                if std::mem::take(&mut self.info_reset) {
                    self.prev_info = ci.clone();
                }
                self.defcon();
                self.forward_top(AppEvent::ClusterInfo(ci));
            }
            AppEvent::Namespaces(ns) => {
                self.ctx.namespaces = ns.clone();
                self.ctx.namespaces_known = true;
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
                match payload.downcast::<ShellEvent>() {
                    Ok(ev) => self.shell_event(*ev),
                    Err(p) => {
                        if let Ok(d) = p.downcast::<PortForwardDied>() {
                            self.port_forward_died(*d);
                        }
                    }
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

    /// pf_extender.go runForward: a forward that dies is flashed and removed.
    fn port_forward_died(&mut self, d: PortForwardDied) {
        self.flash.set(
            FlashLevel::Warn,
            format!("PortForward failed for {}: {}. Deleting!", d.id, d.err),
        );
        self.ctx.port_forwards.remove(&d.id);
    }

    fn shell_event(&mut self, ev: ShellEvent) {
        match ev {
            ShellEvent::HasMetrics(b) => self.ctx.has_metrics = b,
            ShellEvent::NodeMetricsAccess(b) => self.node_mx_ok = b,
            ShellEvent::Contexts(c) => self.contexts = c,
            // refreshCluster: connectivity is checked (and the status
            // cleared) before the slower cluster info refresh.
            ShellEvent::TtyReady {
                proc,
                attach,
                banner,
                on_done,
            } => self.suspends.push_back(Suspend {
                argv: Vec::new(),
                env: Vec::new(),
                banner,
                on_done,
                kind: SuspendKind::Tty(proc, attach),
            }),
            ShellEvent::TtyFallback {
                argv,
                env,
                banner,
                on_done,
            } => self.suspends.push_back(Suspend {
                argv,
                env,
                banner,
                on_done,
                kind: SuspendKind::Process,
            }),
            ShellEvent::EditReady(res) => self.edit_ready(res),
            ShellEvent::EditOutcome(o) => self.edit_outcome(o),
            ShellEvent::StalePortForwards(ids) => {
                for id in ids {
                    tracing::warn!(path = %id, "Deleted portforward");
                    self.ctx.port_forwards.remove(&id);
                }
            }
            ShellEvent::ConnOk => {
                self.ctx.con_ok = true;
                self.validate_port_forwards();
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
                // refreshCluster: "Reload alias".
                self.reload_aliases(false);
            }
            ShellEvent::Meta(ci) => {
                // Stale once the updater refreshed after a switch.
                if self.info_reset || self.opts.disconnected {
                    self.ctx.cluster_info = ci;
                }
            }
            ShellEvent::UpdaterGaveUp => {
                tracing::error!("Cluster updates failed. Giving up ;(");
                self.bail_out();
            }
            ShellEvent::ClusterInfoFailed(e) => {
                tracing::error!(error = %e, "cluster info refresh failed");
                self.ctx.con_ok = false;
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
            ShellEvent::Watched(changes) => self.watched(changes),
            ShellEvent::ContextSwitched {
                name,
                result,
                ns,
                then,
                resources,
            } => {
                let ok = result.is_ok();
                if ok {
                    // watch.Factory.Terminate: forwarders.DeleteAll.
                    self.ctx.port_forwards.clear();
                    self.ctx.resources.clear();
                    if let Some(r) = resources {
                        self.ctx.resources = r;
                    }
                }
                self.context_switched(name, result, ns, then);
            }
        }
    }

    /// view/cluster_info.go setDefCon.
    fn defcon(&mut self) {
        if !self.ctx.has_metrics || !self.node_mx_ok {
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
        // setDefCon: a running benchmark keeps its logo status.
        let benching = self
            .logo_status
            .as_ref()
            .is_some_and(|(_, m)| m.contains("Bench"));
        if !set && !benching {
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
                    return self.start_top();
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
            ViewCmd::Status(l, s) => self.status(l, s),
            ViewCmd::ClearStatus(flash) => {
                self.logo_status = None;
                if flash {
                    self.flash.clear();
                }
            }
            ViewCmd::Command(s) => self.goto(&s, true, true),
            ViewCmd::LogoWarn(m) => self.logo_status = Some((FlashLevel::Warn, m)),
            ViewCmd::UseContext(n) => {
                self.clear_history_on_switch = true;
                self.goto(&format!("ctx {n}"), true, true);
            }
            ViewCmd::Goto { cmd, path } => self.goto_path(&cmd, &path),
            ViewCmd::OpenCommandPrompt(s) => self.open_prompt(PromptKind::Command, s),
            ViewCmd::OpenFilterPrompt => self.open_prompt(PromptKind::Filter, String::new()),
            ViewCmd::Dialog(d) => {
                self.dialog_blurred = false;
                self.dialog = Some(DialogState::new(d, &self.ctx));
            }
            ViewCmd::Suspend {
                argv,
                env,
                banner,
                on_done,
            } => self.suspends.push_back(Suspend {
                argv,
                env,
                banner,
                on_done,
                kind: SuspendKind::Process,
            }),
            ViewCmd::Exec {
                ns,
                pod,
                container,
                command,
                attach,
                banner,
                label,
                fallback,
                env,
                on_done,
            } => self.start_exec(
                ns, pod, container, command, attach, banner, label, fallback, env, on_done,
            ),
            ViewCmd::Edit { gvr, path, on_done } => self.start_edit(gvr, path, on_done),
            ViewCmd::SetNamespace(ns) => self.switch_ns(&ns),
            ViewCmd::CloseDialog => self.dialog = None,
            ViewCmd::ToggleHeader => self.show_header = !self.show_header,
            ViewCmd::ToggleCrumbs => self.show_crumbs = !self.show_crumbs,
            ViewCmd::ToggleFullScreen => self.fullscreen = !self.fullscreen,
            ViewCmd::RefreshClusterInfo => self.refresh_cluster_now(),
        }
        Vec::new()
    }

    fn push(&mut self, v: Box<dyn View>) -> Vec<ViewCmd> {
        self.reload_action_configs();
        if let Some(top) = self.stack.last_mut() {
            top.stop(&mut self.ctx);
        }
        self.stack.push(v);
        self.start_top()
    }

    fn start_top(&mut self) -> Vec<ViewCmd> {
        self.content_focused = true;
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
        if matches!(s.kind, SuspendKind::Editor) {
            return self.editor_done(&s.argv, res);
        }
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

    /// Connect an exec/attach session in the background; the TUI suspends only
    /// once the remote TTY is ready (or kubectl takes over).
    #[allow(clippy::too_many_arguments)]
    fn start_exec(
        &mut self,
        ns: String,
        pod: String,
        container: String,
        command: Vec<String>,
        attach: bool,
        banner: String,
        label: String,
        fallback: Vec<String>,
        env: Vec<(String, String)>,
        on_done: Option<DoneFn>,
    ) {
        let (acc, tx) = (self.ctx.accessor.clone(), self.ctx.sender());
        self.ctx.spawn(async move {
            let ev = match acc.exec_tty(&ns, &pod, &container, command, attach).await {
                Ok(p) => ShellEvent::TtyReady {
                    proc: Box::new(p),
                    attach,
                    banner,
                    on_done,
                },
                Err(e) if e.is::<crate::k8s::TtyUnsupported>() => ShellEvent::TtyFallback {
                    argv: fallback,
                    env,
                    banner,
                    on_done,
                },
                Err(e) => {
                    let _ = tx.send(AppEvent::Error {
                        view: SHELL_VIEW_ID,
                        msg: format!("{label} exec failed: {e:#}"),
                    });
                    return;
                }
            };
            let _ = tx.send(shell_event(ev));
        });
    }

    /// kubectl edit, in-process: fetch the object into a private temp file.
    fn start_edit(&mut self, gvr: crate::k8s::Gvr, path: String, on_done: Option<DoneFn>) {
        if self.edit.is_some() {
            return;
        }
        self.edit = Some(EditState {
            gvr: gvr.clone(),
            path: path.clone(),
            file: None,
            original_text: String::new(),
            original: serde_json::Value::Null,
            retry_of: None,
            on_done,
        });
        let (acc, tx) = (self.ctx.accessor.clone(), self.ctx.sender());
        self.ctx.spawn(async move {
            let res = crate::ui::shell::session::edit_prepare(&acc, &gvr, &path).await;
            let _ = tx.send(shell_event(ShellEvent::EditReady(
                res.map_err(|e| format!("{e:#}")),
            )));
        });
    }

    fn edit_ready(
        &mut self,
        res: std::result::Result<(tempfile::TempPath, String, serde_json::Value), String>,
    ) {
        let Some(st) = self.edit.as_mut() else {
            return;
        };
        match res {
            Ok((file, text, original)) => {
                st.original_text = text;
                st.original = original;
                let f = file.to_string_lossy().to_string();
                st.file = Some(file);
                self.open_editor(&f);
            }
            Err(e) => self.edit_finish(Some(e)),
        }
    }

    fn open_editor(&mut self, file: &str) {
        let argv = crate::ui::shell::session::editor_argv(|k| std::env::var(k).ok(), file);
        self.suspends.push_back(Suspend {
            argv,
            env: Vec::new(),
            banner: String::new(),
            on_done: None,
            kind: SuspendKind::Editor,
        });
    }

    /// The editor exited: diff and patch in the background.
    fn editor_done(&mut self, argv: &[String], res: io::Result<std::process::ExitStatus>) {
        let Some(st) = self.edit.as_ref() else {
            return;
        };
        let editor = argv.first().cloned().unwrap_or_default();
        if !res.as_ref().is_ok_and(|s| s.success()) {
            return self.edit_finish(Some(format!(
                "there was a problem with the editor {}",
                go_quote(&editor)
            )));
        }
        let Some(file) = st.file.as_ref().map(|f| f.to_path_buf()) else {
            return;
        };
        let job = crate::ui::shell::session::EditJob {
            gvr: st.gvr.clone(),
            path: st.path.clone(),
            file,
            original_text: st.original_text.clone(),
            original: st.original.clone(),
            retry_of: st.retry_of.clone(),
        };
        let (acc, tx) = (self.ctx.accessor.clone(), self.ctx.sender());
        self.ctx.spawn(async move {
            let o = crate::ui::shell::session::edit_apply(&acc, job).await;
            let _ = tx.send(shell_event(ShellEvent::EditOutcome(o)));
        });
    }

    fn edit_outcome(&mut self, o: crate::ui::shell::session::EditOutcome) {
        use crate::ui::shell::session::EditOutcome;
        match o {
            EditOutcome::Reopen(edited) => {
                let Some(st) = self.edit.as_mut() else {
                    return;
                };
                st.retry_of = Some(edited);
                let f = st.file.as_ref().map(|f| f.to_string_lossy().to_string());
                if let Some(f) = f {
                    self.open_editor(&f);
                }
            }
            EditOutcome::Done(err) => self.edit_finish(err),
        }
    }

    fn edit_finish(&mut self, err: Option<String>) {
        let Some(mut st) = self.edit.take() else {
            return;
        };
        let mut err = err;
        // preservedFile: keep the changes when the edit failed after editing.
        if let (Some(e), Some(f)) = (err.as_mut(), st.file.take()) {
            if st.retry_of.is_some() || !st.original_text.is_empty() {
                if let Ok(p) = f.keep() {
                    e.push_str(&format!(
                        ". A copy of your changes has been stored to {}",
                        go_quote(&p.to_string_lossy())
                    ));
                }
            }
        }
        let status = crate::ui::shell::session::exit_status(i32::from(err.is_some()));
        if let Some(f) = st.on_done.take() {
            let cmds = f(&mut self.ctx, status);
            self.apply(cmds);
        }
        if let Some(e) = err {
            self.flash
                .set(FlashLevel::Err, format!("Edit command failed: {e}"));
        }
    }

    /// app.go BailOut -> nukeK9sShell: remove this process' node shell pod.
    async fn nuke_shell_pod(&self) {
        let gate = self
            .ctx
            .context_cfg
            .as_ref()
            .is_some_and(|c| c.feature_gates.node_shell);
        if !gate {
            return;
        }
        let ns = self.cfg(|c| c.k9s.shell_pod.namespace.clone());
        let path = crate::k8s::fqn(&ns, &format!("k9rs-shell-{}", std::process::id()));
        let pods = crate::k8s::Gvr::parse("v1/pods");
        let del = self
            .ctx
            .accessor
            .delete(&pods, &path, crate::k8s::DeleteOptions::default());
        match tokio::time::timeout(Duration::from_millis(500), del).await {
            Ok(Err(e)) if !format!("{e:#}").contains("not found") => {
                tracing::error!(error = %e, "Unable to nuke k9rs shell pod")
            }
            Err(_) => tracing::error!("Unable to nuke k9rs shell pod: context deadline exceeded"),
            _ => {}
        }
    }

    fn bail_out(&mut self) {
        self.quit = true;
    }

    /// Config.Save(true): config.yaml (when missing) and the active context
    /// config. k9s only saves at startup, context switches, `ctrl-s` and quit.
    fn save_config(&self) {
        if let Err(e) = self.save_all() {
            tracing::error!(error = %e, "Config save failed!");
        }
    }

    fn save_all(&self) -> Result<()> {
        let c = self.ctx.config.read().map_err(|e| anyhow!("{e}"))?;
        (self.hooks.save_config)(&c)?;
        let (cluster, context) = (self.current_cluster(), self.ctx.accessor.active_context());
        if cluster.is_empty() || context.is_empty() {
            return Ok(());
        }
        match &self.ctx.context_cfg {
            Some(cc) => (self.hooks.persist)(&cluster, &context, cc),
            None => Ok(()),
        }
    }

    fn save_cmd(&mut self) {
        if let Err(e) = self.save_all() {
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
        let extras = self
            .stack
            .last()
            .map(|v| v.extra_hints(&self.ctx))
            .unwrap_or_default();
        let v = super::shell::help_with_extras(&mut self.ctx, hints, extras);
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

    /// app.gotoResource(c, path, false, true).
    fn goto_path(&mut self, line: &str, path: &str) {
        let res = crate::cmd::interpret(line.trim(), &self.ctx.aliases).and_then(|(mut cmd, p)| {
            cmd.instance = path.to_string();
            self.dispatch(cmd, p, false, true)
        });
        if let Err(e) = res {
            self.show_error(e.to_string());
        }
    }

    fn show_error(&mut self, msg: String) {
        self.dialog_blurred = false;
        self.dialog = Some(DialogState::new(Dialog::Error { msg }, &self.ctx));
    }

    /// view/command.go defaultCmd(isRoot = true).
    fn default_command(&mut self) {
        self.ctx.booting = true;
        let line = self.opts.command.trim().to_string();
        if line.is_empty() {
            self.goto(CTX_CMD, true, true);
        } else if let Err(e) = self.run_line(&line, true, true) {
            tracing::error!(cmd = %line, error = %e, "Command exec failed. Using default command");
            self.goto(CTX_CMD, true, true);
        }
        self.ctx.booting = false;
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
        self.dispatch(cmd, p, clear_stack, push_cmd)
    }

    /// view/command.go run: special commands first, then resource views.
    /// `line` is the canonical command line (history / view.active).
    pub(crate) fn dispatch(
        &mut self,
        cmd: Command,
        p: crate::cmd::Interpreter,
        clear_stack: bool,
        push_cmd: bool,
    ) -> Result<()> {
        self.reload_action_configs();
        let line = &p.get_line();
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
            _ => return self.resource_cmd(cmd, p, clear_stack, push_cmd),
        }
        Ok(())
    }

    fn resource_cmd(
        &mut self,
        mut cmd: Command,
        mut p: crate::cmd::Interpreter,
        clear_stack: bool,
        push_cmd: bool,
    ) -> Result<()> {
        let line = &p.get_line();
        if cmd.gvr.is_empty() {
            return Err(anyhow!("`{}` command not found", cmd.alias));
        }
        if let Some(c) = cmd.context.clone().filter(|c| !c.is_empty()) {
            if c != self.ctx.accessor.active_context() {
                // command.go run: Config.Save(true), done by use_context.
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
            .unwrap_or_else(|| NAMESPACED_PSEUDO.contains(&cmd.gvr.as_str()));
        // p.SwitchNS / p.ClearNS re-grok the line: history and view.active
        // carry the namespace, and the view args are read afterwards.
        if namespaced {
            self.switch_ns(&ns);
            cmd.namespace = Some(self.ctx.namespace.clone());
            p.switch_ns(&ns);
        } else {
            cmd.namespace = None;
            p.clear_ns();
        }
        crate::cmd::apply_view_args(&mut cmd, &p);
        let line = &p.get_line();
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
        // command.go exec recover: a view that panics while opening falls
        // back to the last command (or pods), with the stack cleared.
        let res = catching(|| {
            let cmds = self.inject(v, clear_stack);
            self.apply(cmds);
        });
        if let Err(msg) = res {
            tracing::error!(error = %msg, "Failure detected during command exec");
            tracing::debug!(history = ?self.cmd_history.list(), "Dumping history buffer");
            if self.exec_recovering {
                return;
            }
            self.exec_recovering = true;
            let fallback = self.cmd_history.top().unwrap_or_else(|| "pod".into());
            self.goto(&fallback, true, true);
            self.exec_recovering = false;
            return;
        }
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

    /// view/command.go Reset: aliases.yaml (global, then the active
    /// context's) and the cluster resources, over the current aliases or a
    /// cleared map (`nuke`, on context switch).
    fn reload_aliases(&mut self, nuke: bool) {
        let Some(locs) = self.opts.locations.clone() else {
            return;
        };
        let (cluster, context) = (self.current_cluster(), self.ctx.accessor.active_context());
        let file = (!cluster.is_empty() && !context.is_empty())
            .then(|| locs.context_aliases_file(&cluster, &context));
        let mut a = if nuke {
            crate::config::Aliases::default()
        } else {
            self.ctx.aliases.clone()
        };
        match a.reload_with(&locs, file.as_deref()) {
            Ok(()) => {
                (self.hooks.add_resources)(&mut a, &self.ctx.resources);
                self.ctx.aliases = a;
            }
            Err(e) => {
                tracing::warn!(error = %e, "Command reset failed");
                self.logo_status = Some((FlashLevel::Warn, "Aliases load failed!".into()));
            }
        }
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

    /// Record the active view/namespace in the in-memory context config
    /// (Config.SetActiveView / SetActiveNamespace); saved by `save_config`.
    fn persist(&mut self, view: Option<&str>, ns: Option<&str>) {
        let cluster = self.current_cluster();
        let context = self.ctx.accessor.active_context();
        // No active context: nothing to record.
        if cluster.is_empty() || context.is_empty() {
            return;
        }
        if self.ctx.context_cfg.is_none() {
            self.ctx.context_cfg = (self.hooks.load_context)(&cluster, &context).ok();
        }
        if let Some(cc) = self.ctx.context_cfg.as_mut() {
            if let Some(v) = view {
                cc.view.active = v.to_string();
            }
            if let Some(ns) = ns {
                cc.set_active_namespace(ns);
            }
        }
    }

    /// view/context.go useContext: switch asynchronously, then navigate.
    fn use_context(&mut self, name: &str, ns: Option<String>, then: Option<String>) {
        if let Some(top) = self.stack.last_mut() {
            top.stop(&mut self.ctx);
        }
        // Config.Save(true): the context config too, with the namespace in use.
        self.save_config();
        let cur = self.ctx.namespace.clone();
        self.persist(None, Some(&cur));
        // K9s.ActivateContext: the new context's proxy, if any.
        let proxy = self
            .contexts
            .iter()
            .find(|c| c.name == name)
            .and_then(|c| (self.hooks.load_context)(&c.cluster, name).ok())
            .and_then(|cc| cc.proxy)
            .map(|p| p.address)
            .filter(|a| !a.is_empty());
        let acc = self.ctx.accessor.clone();
        let tx = self.ctx.sender();
        let name = name.to_string();
        self.ctx.spawn(async move {
            let result = acc.switch_context(&name).await.map_err(|e| e.to_string());
            if result.is_ok() && proxy.is_some() {
                acc.set_proxy(proxy).await;
            }
            let resources = match &result {
                Ok(_) => acc.resources().await.ok(),
                Err(_) => None,
            };
            let _ = tx.send(shell_event(ShellEvent::ContextSwitched {
                name,
                result,
                ns,
                then,
                resources,
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
                self.clear_history_on_switch = false;
                self.flash.set(FlashLevel::Err, e);
                if let Some(top) = self.stack.last_mut() {
                    let cmds = top.start(&mut self.ctx);
                    self.apply(cmds);
                }
                return;
            }
        };
        let (cluster, kube_ns, user) = self
            .contexts
            .iter()
            .find(|c| c.name == name)
            .map(|c| (c.cluster.clone(), c.namespace.clone(), c.user.clone()))
            .unwrap_or_default();
        let cc = (self.hooks.load_context)(&cluster, &name).ok();
        self.ctx.context_cfg = cc.clone();
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
        if let Some(cc) = self.ctx.context_cfg.as_mut() {
            if CONTEXT_CMDS.contains(&cc.view.active.split_whitespace().next().unwrap_or_default())
            {
                cc.view.active = crate::k8s::known::pods().to_string();
            }
        }
        // view/app.go switchContext: Config.Save(true) once activated, then
        // command.Reset(path, true).
        self.save_config();
        self.reload_aliases(true);
        self.flash.set(
            FlashLevel::Info,
            format!(
                "Switching context to {}::{}",
                go_quote(&name),
                go_quote(&target_ns)
            ),
        );
        // ReloadStyles (the new context's skin); Resume re-targets the
        // context config watcher.
        self.refresh_styles();
        if let Ok(mut c) = self.watch_ctx.lock() {
            *c = self.context_config_path();
        }
        // The views opened next read the new context's config.
        self.ctx.cluster_info = ClusterInfo {
            context: name.clone(),
            cluster: cluster.clone(),
            user,
            ..Default::default()
        };
        self.info_reset = true;
        // ClusterInfo.Reset: the new cluster meta even if the connection
        // check then fails.
        let (acc, tx, cfg) = (
            self.ctx.accessor.clone(),
            self.ctx.sender(),
            self.ctx.config.clone(),
        );
        self.ctx.spawn(async move {
            if let Ok(ci) = acc.cluster_info().await {
                let ci = with_latest_rev(ci, &cfg).await;
                let _ = tx.send(shell_event(ShellEvent::Meta(ci)));
            }
        });
        // SwitchContext resets the client: connOK until the next check.
        self.ctx.con_ok = true;
        if std::mem::take(&mut self.opts.disconnected) {
            self.start_background();
        } else {
            self.start_background_once();
        }
        self.goto(&view, true, true);
        // useCtx: clearHistory after the switch (its goto included).
        if std::mem::take(&mut self.clear_history_on_switch) {
            self.cmd_history.clear();
            self.filter_history.clear();
        }
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
        self.draw_screen(f);
        crate::ui::shell::text::style_wide_trailers(f.buffer_mut());
    }

    fn draw_screen(&mut self, f: &mut Frame<'_>) {
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
        // tview Flex.SetFullScreen: the view takes the whole screen and
        // covers the flash too.
        let view_full = self.stack.last().is_some_and(|v| v.is_fullscreen());
        let full = self.fullscreen || view_full;
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
            has_metrics: self.ctx.has_metrics && self.node_mx_ok,
            readonly: self.ctx.readonly,
            no_icons,
            hints: &hints,
            show_logo: self.show_logo,
            status: self.logo_status.as_ref(),
            padded: self.info_padded,
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
        self.prompt_area = None;
        if let Some(p) = &self.prompt {
            let r = take(&mut y, PROMPT_HEIGHT);
            self.prompt_area = Some(r);
            let c = p.draw(f.buffer_mut(), r, &styles, no_icons);
            cursor = (!self.prompt_blurred).then_some(c);
        }
        let crumbs_h = if self.show_crumbs && !full { 1 } else { 0 };
        let flash_h = 1;
        let content_h = bottom.saturating_sub(y).saturating_sub(crumbs_h + flash_h);
        let cover = view_full && self.prompt.is_none();
        let content = if cover { area } else { take(&mut y, content_h) };
        self.content_area = content;
        let focused = (self.prompt.is_none() || self.prompt_blurred)
            && (self.dialog.is_none() || self.dialog_blurred)
            && self.content_focused;
        if let Some(top) = self.stack.last_mut() {
            top.set_focused(focused);
        }
        match self.stack.last_mut() {
            Some(top) if top.owns_frame() => {
                top.draw(f, content, &self.ctx);
                if !focused {
                    let c = top.blur_border(&styles);
                    title::blur_frame(f.buffer_mut(), content, &styles, c);
                }
            }
            Some(top) => {
                let t = title::title_segs(
                    &top.title(),
                    &top.filter_text(),
                    top.title_color(&styles),
                    &styles,
                );
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
        if !cover {
            let r = take(&mut y, flash_h);
            self.flash.draw(f.buffer_mut(), r, &styles, no_icons);
        }
        if let Some(d) = self.dialog.as_mut() {
            d.blurred = self.dialog_blurred;
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
    c.namespaces_known = old.namespaces_known;
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

/// ClusterInfo.Refresh release check: K9s Rev and a newer release.
async fn with_latest_rev(mut ci: ClusterInfo, cfg: &SharedConfig) -> ClusterInfo {
    // Unit tests never reach out to GitHub.
    let skip = cfg!(test) || cfg.read().is_ok_and(|c| c.k9s.skip_latest_rev_check);
    let latest = if skip {
        String::new()
    } else {
        crate::version::latest_rev().await
    };
    (ci.k9s_version, ci.k9s_latest) = crate::version::k9s_revs(&ci.k9s_version, &latest);
    ci
}

/// view/app.go refreshCluster: connectivity check, then ClusterInfo.Refresh
/// (cluster meta plus the latest k9s release check). False when the
/// connection check failed.
/// cenkalti/backoff ExponentialBackOff (k9s model.NewExpBackOff): 0.5
/// randomization, 1.5 multiplier, 60s max interval, `max_elapsed` since the
/// last reset; `None` means stop.
struct ExpBackoff {
    initial: Duration,
    current: Duration,
    max_elapsed: Duration,
    start: Instant,
}

impl ExpBackoff {
    const MAX_INTERVAL: Duration = Duration::from_secs(60);

    fn new(initial: Duration, max_elapsed: Duration) -> ExpBackoff {
        ExpBackoff {
            initial,
            current: initial,
            max_elapsed,
            start: Instant::now(),
        }
    }

    fn reset(&mut self) {
        self.current = self.initial;
        self.start = Instant::now();
    }

    fn next(&mut self) -> Option<Duration> {
        self.next_at(Instant::now(), rand::random::<f64>())
    }

    fn next_at(&mut self, now: Instant, r: f64) -> Option<Duration> {
        let elapsed = now.saturating_duration_since(self.start);
        let cur = self.current.as_secs_f64();
        let delta = 0.5 * cur;
        let d = Duration::from_secs_f64((cur - delta) + r * (2.0 * delta + 1e-9));
        if elapsed + d > self.max_elapsed {
            return None;
        }
        self.current = (self.current.mul_f64(1.5)).min(Self::MAX_INTERVAL);
        Some(d)
    }
}

async fn refresh_cluster(
    acc: &SharedAccessor,
    tx: &mpsc::UnboundedSender<AppEvent>,
    cfg: &SharedConfig,
) -> bool {
    if let Err(e) = acc.check_connectivity().await {
        let _ = tx.send(shell_event(ShellEvent::ClusterInfoFailed(format!("{e:#}"))));
        return false;
    }
    let _ = tx.send(shell_event(ShellEvent::ConnOk));
    let mx = acc.has_metrics().await;
    let _ = tx.send(shell_event(ShellEvent::HasMetrics(mx)));
    // cluster_info.go hasMetrics: CPU/MEM need list access to node metrics.
    let node_mx = mx
        && match acc
            .can(
                &crate::k8s::Gvr::parse("metrics.k8s.io/v1beta1/nodes"),
                "",
                "list",
            )
            .await
        {
            Ok(ok) => ok,
            Err(e) => {
                tracing::warn!(error = %e, "No nodes metrics access");
                false
            }
        };
    let _ = tx.send(shell_event(ShellEvent::NodeMetricsAccess(node_mx)));
    match acc.cluster_info().await {
        Ok(ci) => {
            let ci = with_latest_rev(ci, cfg).await;
            let _ = tx.send(AppEvent::ClusterInfo(ci));
        }
        Err(e) => {
            let _ = tx.send(shell_event(ShellEvent::ClusterInfoFailed(e.to_string())));
            return false;
        }
    }
    // ValidNamespaceNames: a failed list leaves no valid namespaces.
    let ns = acc.namespaces().await.unwrap_or_default();
    let _ = tx.send(AppEvent::Namespaces(ns));
    true
}

/// ui.AsKey: alt+rune becomes rune*ModAlt, which no k9s binding uses, so
/// only tview's own table/text navigation runes still act (g G j k h l).
/// Alt on any other key is ignored.
fn as_key(key: KeyEvent) -> Option<KeyEvent> {
    if !key.modifiers.contains(KeyModifiers::ALT) {
        return Some(key);
    }
    match key.code {
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            let code = match c {
                'j' => KeyCode::Down,
                'k' => KeyCode::Up,
                'g' => KeyCode::Home,
                'G' => KeyCode::End,
                'h' => KeyCode::Left,
                'l' => KeyCode::Right,
                _ => return None,
            };
            Some(KeyEvent::new(code, KeyModifiers::NONE))
        }
        _ => {
            let mut k = key;
            k.modifiers.remove(KeyModifiers::ALT);
            Some(k)
        }
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
pub(crate) fn go_io_error(e: &io::Error) -> String {
    match e.kind() {
        io::ErrorKind::NotFound => "no such file or directory".into(),
        io::ErrorKind::PermissionDenied => "permission denied".into(),
        // Go syscall.Errno strings: lowercased strerror, no "(os error N)".
        _ if e.raw_os_error().is_some() => {
            let s = e.to_string();
            let s = s.split(" (os error").next().unwrap_or(&s);
            let mut cc = s.chars();
            match cc.next() {
                Some(c) => c.to_lowercase().chain(cc).collect(),
                None => String::new(),
            }
        }
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
        // tcell opens the controlling terminal first.
        #[cfg(unix)]
        if let Err(e) = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
        {
            return Err(anyhow!(crate::cli::info::go_path_error(
                "open",
                std::path::Path::new("/dev/tty"),
                &e
            )));
        }
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
        // Terminal::clear asks the terminal for the cursor position, which
        // can time out right after a child program (an editor) exits and
        // abort the app. Clear the screen and drop both frame buffers
        // instead so the next draw repaints everything.
        execute!(
            io::stdout(),
            crossterm::terminal::Clear(crossterm::terminal::ClearType::All)
        )?;
        forget_frames(&mut self.term);
        Ok(r)
    }
}

impl Tui {
    /// Hand the terminal to a connected remote TTY until it ends; returns
    /// the remote exit code.
    async fn suspend_tty(
        &mut self,
        banner: &str,
        attach: bool,
        p: crate::k8s::TtyProcess,
    ) -> io::Result<i32> {
        restore_screen(self.mouse);
        // k9s execute(): clear the screen before/after shelling out.
        print!("\x1b[H\x1b[2J{banner}");
        if attach {
            println!("If you don't see a command prompt, try pressing enter.");
        }
        let _ = io::Write::flush(&mut io::stdout());
        enable_raw_mode()?;
        let code = crate::ui::shell::session::tty_bridge(p).await;
        let _ = disable_raw_mode();
        print!("\x1b[H\x1b[2J");
        enter_screen(self.mouse)?;
        execute!(
            io::stdout(),
            crossterm::terminal::Clear(crossterm::terminal::ClearType::All)
        )?;
        forget_frames(&mut self.term);
        Ok(code)
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        restore_screen(self.mouse);
    }
}

/// Reset both frame buffers so the next draw writes every cell (after the
/// screen was cleared behind ratatui's back).
fn forget_frames<B: ratatui::backend::Backend>(term: &mut Terminal<B>) {
    term.swap_buffers();
    term.swap_buffers();
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

/// view/app.go initSignals: SIGHUP exits right away (status 0) without
/// saving the config.
fn init_signals() {
    #[cfg(unix)]
    tokio::spawn(async {
        use tokio::signal::unix::{signal, SignalKind};
        if let Ok(mut s) = signal(SignalKind::hangup()) {
            s.recv().await;
            std::process::exit(0);
        }
    });
}

/// Panic text, like Go's recovered value.
fn panic_message(info: &std::panic::PanicHookInfo<'_>) -> String {
    let p = info.payload();
    let msg = p
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".into());
    match info.location() {
        Some(l) => format!("{msg} ({}:{})", l.file(), l.line()),
        None => msg,
    }
}

thread_local! {
    /// Depth of [`catching`] sections: their panics are recovered, so the
    /// hook only logs them.
    static CATCHING: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Go `recover()` around `f`: a panic becomes `Err(message)`.
fn catching(f: impl FnOnce()) -> std::result::Result<(), String> {
    CATCHING.with(|c| c.set(c.get() + 1));
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    CATCHING.with(|c| c.set(c.get() - 1));
    res.map_err(|p| {
        p.downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| p.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".into())
    })
}

/// cmd/root.go recover + tview Run: a panic on the UI thread restores the
/// terminal, logs the error and stack, prints the red logo with
/// `Boom!! <err>.` and exits (k9s recovers and returns cleanly). Panics in
/// background tasks only kill that task: log them, keep the screen.
fn install_panic_hook(mouse: bool) {
    let ui = std::thread::current().id();
    std::panic::set_hook(Box::new(move |info| {
        let msg = panic_message(info);
        let stack = std::backtrace::Backtrace::force_capture();
        if CATCHING.with(|c| c.get() > 0) {
            tracing::error!(error = %msg, "Recovered panic");
            tracing::error!(stack = %stack, "Dumping stack");
            return;
        }
        if std::thread::current().id() != ui {
            tracing::error!(error = %msg, "Background task panicked");
            tracing::error!(stack = %stack, "");
            return;
        }
        restore_screen(mouse);
        tracing::error!(error = %msg, "Boom!! k9rs init failed");
        tracing::error!(stack = %stack, "");
        let _ = crate::cli::info::print_boom(&mut io::stdout(), &msg);
        std::process::exit(0);
    }));
}

#[cfg(test)]
#[path = "shell/app_tests.rs"]
mod tests;
