//! The ratatui application (k9s internal/ui + internal/view).
//!
//! Layout (k9s default): header row = [cluster info | menu hints | logo],
//! then the active view (table/details/logs/...), then crumbs. The `:` and `/`
//! prompts appear above the view while active. Flash messages overlay the
//! logo status line.
//!
//! Slices:
//! - `ui-shell`  : app loop, header, menu, crumbs, prompt, flash, dialogs,
//!   help view, aliases view, styles, key names.
//! - `ui-table`  : the resource browser (table view), drill-down navigation,
//!   namespace switching, sorting, marks, actions
//!   (describe/yaml/edit/delete/logs/shell/pf/scale/restart...).
//! - `ui-details`: details (describe/yaml), logs, container view, xray,
//!   pulses, port-forward and screen dump views.

pub mod app;
pub mod browser;
pub mod keys;
pub mod shell;
pub mod view;
pub mod views;

pub use app::App;
pub use view::*;
