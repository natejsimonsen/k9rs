//! Shell widgets (owned by the `ui-shell` slice): header (cluster info, menu,
//! logo), crumbs, prompt, flash, dialogs, splash, help view, plugins.
//!
//! Async tasks that want the shell itself to flash a message send
//! `AppEvent::Info` / `AppEvent::Error` with `view: SHELL_VIEW_ID`.

pub(crate) mod cow;
pub(crate) mod crumbs;
pub(crate) mod dialog;
pub(crate) mod flash;
pub(crate) mod header;
pub(crate) mod help;
pub(crate) mod palette;
pub mod plugins;
pub(crate) mod prompt;
pub(crate) mod session;
pub(crate) mod splash;
pub(crate) mod text;
pub(crate) mod title;
pub(crate) mod watch;

use super::{Ctx, Hint, View, ViewId};

pub use cow::cow;
pub use plugins::{plugin_for_key, plugin_hints, run_plugin, Selection};

/// Events addressed to this id are handled by the shell (`Info`/`Error`
/// are flashed). `Ctx::new_view_id` never hands it out.
pub const SHELL_VIEW_ID: ViewId = 0;

/// The help view (`?`): general keys, resource keys (`hints`), navigation, hotkeys.
pub fn help(ctx: &mut Ctx, hints: Vec<Hint>) -> Box<dyn View> {
    help::new_help(ctx, hints, Vec::new())
}

/// The help view with the top view's ExtraHints (xray emoji legend).
pub fn help_with_extras(
    ctx: &mut Ctx,
    hints: Vec<Hint>,
    extras: Vec<(String, String)>,
) -> Box<dyn View> {
    help::new_help(ctx, hints, extras)
}
