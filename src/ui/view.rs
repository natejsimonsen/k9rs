//! View contract shared by every screen.

use std::any::Any;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::Frame;
use tokio::sync::mpsc;

use crate::config::{SharedConfig, Styles};
use crate::k8s::{ClusterInfo, SharedAccessor};
use crate::model::TableData;

/// Unique id of a view instance; async results are routed back by id.
pub type ViewId = u64;

/// A menu hint (k9s ui.MenuHint): shown in the header and the help view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hint {
    /// Key label as k9s prints it, e.g. `ctrl-d`, `shift-f`, `enter`, `0`.
    pub mnemonic: String,
    pub description: String,
    /// Shared actions are shown only in help, not in the header menu.
    pub visible: bool,
}

impl Hint {
    pub fn new(mnemonic: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            mnemonic: mnemonic.into(),
            description: description.into(),
            visible: true,
        }
    }
    pub fn hidden(mut self) -> Self {
        self.visible = false;
        self
    }
}

/// Flash message level (ui.FlashLevel).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlashLevel {
    Info,
    Warn,
    Err,
}

/// Dialog kinds the shell renders on top of the current view.
pub enum Dialog {
    /// Yes/No confirm (k9s dialog.ShowConfirm). `on_ok` runs when accepted.
    Confirm {
        title: String,
        msg: String,
        on_ok: Box<dyn FnOnce(&mut Ctx) -> Vec<ViewCmd> + Send>,
    },
    /// Restart confirm with an editable field manager (dialog.ShowRestart).
    Restart {
        title: String,
        msg: String,
        field_manager: String,
        on_ok: Box<dyn FnOnce(&mut Ctx, String) -> Vec<ViewCmd> + Send>,
    },
    /// Delete dialog with propagation + force options (dialog.ShowDelete).
    Delete {
        msg: String,
        on_ok: Box<dyn FnOnce(&mut Ctx, crate::k8s::DeleteOptions) -> Vec<ViewCmd> + Send>,
    },
    /// Error popup (dialog.ShowError).
    Error { msg: String },
    /// Scale prompt (view.scaleDialog).
    Scale {
        /// Dialog message, e.g. `Scale Deployment default/nginx?` (k9s scale_extender).
        msg: String,
        current: i32,
        on_ok: Box<dyn FnOnce(&mut Ctx, i32) -> Vec<ViewCmd> + Send>,
    },
    /// Port-forward form (view.pf_dialog).
    PortForward {
        spec: crate::k8s::PortForwardSpec,
        /// Candidate container ports to pick from (`container::port`).
        ports: Vec<String>,
        /// PFAnns.ToPortSpec prefill: (container ports, local ports), comma lists.
        prefill: (String, String),
        on_ok: Box<dyn FnOnce(&mut Ctx, Vec<crate::k8s::PortForwardSpec>) -> Vec<ViewCmd> + Send>,
    },
    /// Confirm that needs `accept` typed into a "Confirm:" field before OK
    /// does anything (dialog.ShowConfirmAck with override).
    ConfirmAck {
        title: String,
        msg: String,
        accept: String,
        on_ok: Box<dyn FnOnce(&mut Ctx) -> Vec<ViewCmd> + Send>,
    },
    /// Busy prompt with a Cancel button (dialog.ShowPrompt): Cancel/Esc
    /// cancels `cancel` and runs `on_cancel`; the action closes it with
    /// ViewCmd::CloseDialog when done.
    Launching {
        title: String,
        msg: String,
        cancel: tokio_util::sync::CancellationToken,
        on_cancel: Box<dyn FnOnce(&mut Ctx) -> Vec<ViewCmd> + Send>,
    },
    /// File transfer form (dialog.ShowUploads).
    Transfer {
        /// `ns/pod:` prefix the From field starts with.
        pod: String,
        containers: Vec<String>,
        retries: i64,
        /// Err keeps the form open and flashes (the Ack returned false).
        on_ok: Box<dyn FnMut(&mut Ctx, TransferArgs) -> Result<Vec<ViewCmd>, String> + Send>,
    },
    /// Drain form (view.drain_dialog).
    Drain {
        node: String,
        on_ok: Box<dyn FnOnce(&mut Ctx, crate::k8s::DrainOptions) -> Vec<ViewCmd> + Send>,
    },
    /// Set image form: one input per container (view.ImageExtender).
    SetImage {
        msg: String,
        /// (container, image, init container)
        containers: Vec<(String, String, bool)>,
        on_ok: Box<dyn FnOnce(&mut Ctx, Vec<crate::k8s::ImageSpec>) -> Vec<ViewCmd> + Send>,
    },
    /// Context rename form (view/context.go showRenameModal).
    Rename {
        name: String,
        on_ok: Box<dyn FnOnce(&mut Ctx, String) -> Vec<ViewCmd> + Send>,
    },
    /// Modal list (dialog.ShowSelection, e.g. "Jump To" owners). `on_select`
    /// gets the picked list index.
    Selection {
        title: String,
        options: Vec<String>,
        on_select: Box<dyn FnOnce(&mut Ctx, usize) -> Vec<ViewCmd> + Send>,
    },
    /// Generic text prompt (used by `set image`, cow, etc).
    Prompt {
        title: String,
        label: String,
        value: String,
        on_ok: Box<dyn FnOnce(&mut Ctx, String) -> Vec<ViewCmd> + Send>,
    },
}

/// tview mouse actions the views react to (screen coordinates).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mouse {
    /// MouseLeftClick (button released where it was pressed).
    Click {
        x: u16,
        y: u16,
    },
    ScrollUp {
        x: u16,
        y: u16,
    },
    ScrollDown {
        x: u16,
        y: u16,
    },
}

/// dialog.TransferArgs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TransferArgs {
    pub from: String,
    pub to: String,
    pub container: String,
    pub download: bool,
    pub no_preserve: bool,
    pub retries: i64,
}

/// Instructions a view returns to the app shell.
pub enum ViewCmd {
    /// Push a new view on the stack.
    Push(Box<dyn View>),
    /// Pop the current view (no-op on the root view).
    Pop,
    /// Replace the current view.
    Replace(Box<dyn View>),
    /// Reset the stack to a single view (used by `:` navigation).
    Reset(Box<dyn View>),
    /// Exit the application.
    Quit,
    /// Show a flash message.
    Flash(FlashLevel, String),
    /// app.Status: logo status (indicator when the header is hidden).
    Status(FlashLevel, String),
    /// app.ClearStatus(flash): reset the logo status (and the flash).
    ClearStatus(bool),
    /// Run a `:` command string.
    Command(String),
    /// Logo().Warn: a warning in the logo slot (status, not flash).
    LogoWarn(String),
    /// context.go useCtx: switch to the context, then clear the command
    /// and filter histories.
    UseContext(String),
    /// app.gotoResource(cmd, path, clearStack=false, pushCmd=true): push the
    /// command's view on the stack, scoped to instance `path` if not empty.
    Goto { cmd: String, path: String },
    /// Open the `:` prompt (optionally pre-filled).
    OpenCommandPrompt(String),
    /// Open the `/` filter prompt.
    OpenFilterPrompt,
    /// Show a dialog.
    Dialog(Dialog),
    /// Suspend the TUI, run `argv` with `env` in the foreground terminal, then
    /// resume (used for `kubectl exec`, `$EDITOR`, plugins). `on_done` runs
    /// after the process exits.
    Suspend {
        argv: Vec<String>,
        env: Vec<(String, String)>,
        /// Printed before the command runs (k9s shellOpts.banner).
        banner: String,
        on_done: Option<Box<dyn FnOnce(&mut Ctx, std::process::ExitStatus) -> Vec<ViewCmd> + Send>>,
    },
    /// Change the active namespace (persisted to the context config).
    SetNamespace(String),
    /// Dismiss the current dialog (dialog.dismiss).
    CloseDialog,
    /// Toggle header visibility (ctrl-e).
    ToggleHeader,
    /// Toggle crumbs visibility (ctrl-g).
    ToggleCrumbs,
    /// Toggle fullscreen for the current view (f).
    ToggleFullScreen,
    /// Refresh the header cluster info.
    RefreshClusterInfo,
}

/// Events delivered to views (results of async work, ticks, resizes).
#[derive(Debug)]
pub enum AppEvent {
    /// Periodic refresh tick (config refreshRate).
    Tick,
    Resize,
    /// Table data ready for view `view`.
    Data {
        view: ViewId,
        data: TableData,
    },
    /// Free text (describe/yaml/etc) ready for view `view`.
    Text {
        view: ViewId,
        text: String,
    },
    /// Log lines for view `view`.
    LogLines {
        view: ViewId,
        lines: Vec<crate::k8s::LogLine>,
    },
    /// Log stream ended (EOF or error).
    LogEnd {
        view: ViewId,
        error: Option<String>,
    },
    /// An async operation failed.
    Error {
        view: ViewId,
        msg: String,
    },
    /// An async operation succeeded with an informational message.
    Info {
        view: ViewId,
        msg: String,
    },
    /// Updated header info.
    ClusterInfo(ClusterInfo),
    /// Namespaces list refreshed.
    Namespaces(Vec<String>),
    /// Discovery refreshed.
    Resources(Vec<crate::k8s::ResourceMeta>),
    /// Arbitrary payload for a view (downcast by the receiver).
    Custom {
        view: ViewId,
        payload: Box<dyn Any + Send>,
    },
}

/// Shared services handed to views on every call.
pub struct Ctx {
    pub accessor: SharedAccessor,
    pub config: SharedConfig,
    pub styles: Styles,
    /// Active namespace (`NAMESPACE_ALL` for all).
    pub namespace: String,
    pub readonly: bool,
    /// Discovered API resources (filled in once discovery completes).
    pub resources: Vec<crate::k8s::ResourceMeta>,
    pub namespaces: Vec<String>,
    /// A namespace list answer arrived (empty on failure, like k9s
    /// ValidNamespaceNames erroring).
    pub namespaces_known: bool,
    /// APIClient.ConnectionOK: the last connectivity check passed.
    pub con_ok: bool,
    pub aliases: crate::config::Aliases,
    pub hotkeys: crate::config::HotKeys,
    pub plugins: crate::config::Plugins,
    pub views: crate::config::CustomView,
    pub cluster_info: ClusterInfo,
    /// The active context config (K9s.activeConfig), saved only at k9s's
    /// save points.
    pub context_cfg: Option<crate::config::ContextConfig>,
    /// Active port forwards, keyed by `PortForwardSpec::id()`.
    pub port_forwards: std::collections::BTreeMap<String, crate::k8s::PortForwardHandle>,
    pub has_metrics: bool,
    /// Views are being started by the boot command (k9s App not running yet).
    pub booting: bool,
    tx: mpsc::UnboundedSender<AppEvent>,
    next_view_id: ViewId,
}

impl Ctx {
    pub fn new(
        accessor: SharedAccessor,
        config: SharedConfig,
        styles: Styles,
        namespace: String,
        tx: mpsc::UnboundedSender<AppEvent>,
    ) -> Self {
        Self {
            accessor,
            config,
            styles,
            namespace,
            readonly: false,
            resources: Vec::new(),
            namespaces: Vec::new(),
            namespaces_known: false,
            con_ok: true,
            aliases: Default::default(),
            hotkeys: Default::default(),
            plugins: Default::default(),
            views: Default::default(),
            cluster_info: Default::default(),
            context_cfg: None,
            port_forwards: Default::default(),
            has_metrics: false,
            booting: false,
            tx,
            next_view_id: 1,
        }
    }
    /// Allocate a view id.
    pub fn new_view_id(&mut self) -> ViewId {
        let id = self.next_view_id;
        self.next_view_id += 1;
        id
    }
    /// Sender for async tasks to post events back to the app.
    pub fn sender(&self) -> mpsc::UnboundedSender<AppEvent> {
        self.tx.clone()
    }
    /// Spawn an async task on the runtime.
    pub fn spawn<F>(&self, fut: F) -> tokio::task::JoinHandle<()>
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        tokio::spawn(fut)
    }
}

/// A screen in the view stack (k9s view.Component / model.Component).
pub trait View: Send {
    /// Stable view id (from `Ctx::new_view_id`).
    fn id(&self) -> ViewId;
    /// Short name used in crumbs (`pods`, `describe`, `logs`).
    fn name(&self) -> String;
    /// Title shown in the view's frame, e.g. ` Pods(default)[12] `.
    fn title(&self) -> String {
        self.name()
    }
    /// Menu hints for the header and the help view.
    fn hints(&self) -> Vec<Hint>;
    /// Called when the view becomes active (pushed or revealed).
    fn start(&mut self, _ctx: &mut Ctx) -> Vec<ViewCmd> {
        Vec::new()
    }
    /// Called when the view is hidden or popped.
    fn stop(&mut self, _ctx: &mut Ctx) {}
    /// Handle a key while the view has focus.
    fn handle_key(&mut self, key: KeyEvent, ctx: &mut Ctx) -> Vec<ViewCmd>;
    /// tview mouse action at a screen position inside the view's `area`
    /// (only with `ui.enableMouse`).
    fn on_mouse(&mut self, _m: Mouse, _area: Rect, _ctx: &mut Ctx) -> Vec<ViewCmd> {
        Vec::new()
    }
    /// Handle an app event (async result, tick).
    fn on_event(&mut self, _ev: &AppEvent, _ctx: &mut Ctx) -> Vec<ViewCmd> {
        Vec::new()
    }
    /// Handle the text submitted from the `/` filter prompt (live, on every keystroke).
    fn on_filter(&mut self, _text: &str, _ctx: &mut Ctx) -> Vec<ViewCmd> {
        Vec::new()
    }
    /// The `/` buffer changed while typing (BufferChanged/BufferCompleted);
    /// views that filter live treat it like `on_filter`.
    fn on_filter_typing(&mut self, text: &str, ctx: &mut Ctx) -> Vec<ViewCmd> {
        self.on_filter(text, ctx)
    }
    /// Draw the view.
    fn draw(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &Ctx);
    /// True when the view draws its own border and title (the table and
    /// picker do); the shell then hands it the full content area unframed.
    fn owns_frame(&self) -> bool {
        false
    }
    /// ExtraHints for the help view, sorted by key (help.go addExtras).
    fn extra_hints(&self, _ctx: &Ctx) -> Vec<(String, String)> {
        Vec::new()
    }
    /// Told before each draw whether the content has tview focus (no prompt,
    /// dialog or clicked header box holds it).
    fn set_focused(&mut self, _focused: bool) {}
    /// Border color an unfocused (blurred) self-framed view falls back to
    /// (tview Box borderColor).
    fn blur_border(&self, styles: &crate::config::Styles) -> ratatui::style::Color {
        styles.border
    }
    /// Whether the view currently wants fullscreen (hides header/crumbs).
    fn is_fullscreen(&self) -> bool {
        false
    }
    /// Current filter text (shown in the title).
    /// browser.go BufferActive: an accepted filter enters the filter history
    /// only from a resource table that still shows rows.
    fn keeps_filter_history(&self) -> bool {
        false
    }
    fn filter_text(&self) -> String {
        String::new()
    }
    /// tview box title color: what `-` (a `default` skin color) in the
    /// title markup falls back to. Box default is the body fg.
    fn title_color(&self, styles: &Styles) -> ratatui::style::Color {
        styles.fg
    }
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}
