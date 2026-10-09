//! Black-box end-to-end scenarios for k9rs against the fake cluster.
//! Expected values come from the k9s v0.50.18 Go sources (cited per test).

#[path = "../common/mod.rs"]
mod common;

mod actions;
mod app;
mod commands;
mod config_files;
mod contexts;
mod details;
mod drill;
mod logs;
mod pods;
mod startup;
mod table;
