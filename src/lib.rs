//! k9rs: a Rust port of k9s (https://github.com/derailed/k9s), v0.50.18.
//!
//! Module map (mirrors the Go layout where it helps):
//! - `model`  : table data model (header, rows, events), filtering, sorting.
//! - `render` : per-resource renderers (k8s object -> table row) and colorers.
//! - `k8s`    : cluster access (GVR, discovery, the `Accessor` trait, real + fake impls).
//! - `config` : k9s config dir parsing: config.yaml, aliases, hotkeys, plugins, views, skins.
//! - `cmd`    : the `:` command interpreter (aliases, suggestions, flags like `-n`, `@ctx`).
//! - `ui`     : ratatui application, views, header/menu/crumbs/prompt/flash, key bindings.
//! - `cli`    : clap command line matching k9s flags, `info` and `version` subcommands.
//! - `logo`   : the k9s ASCII logo and splash.

#![allow(clippy::type_complexity)]

pub mod cli;
pub mod cmd;
pub mod config;
pub mod k8s;
pub mod logo;
pub mod model;
pub mod render;
pub mod ui;
pub mod version;
