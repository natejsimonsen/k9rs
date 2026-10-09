//! k9s configuration (internal/config): config.yaml, per-context config,
//! aliases.yaml, hotkeys.yaml, plugins.yaml, views.yaml, skins/*.yaml.
//! Implemented by the `config` slice. The types below are the contract the UI
//! relies on; the slice fills in parsing, defaults, validation and saving.

pub mod aliases;
pub mod hotkeys;
pub mod k9s;
pub mod paths;
pub mod plugins;
pub mod skin;
pub mod styles;
pub mod views;

pub use aliases::Aliases;
pub use hotkeys::{HotKey, HotKeys};
pub use k9s::*;
pub use plugins::{Plugin, Plugins};
pub use skin::{Skin, Styles};
pub use views::{CustomView, ViewSetting};

use std::sync::{Arc, RwLock};

/// Shared handle to the live configuration.
pub type SharedConfig = Arc<RwLock<Config>>;
