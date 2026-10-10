//! config.yaml (`k9s:` root, config/k9s.go + config/config.go) and the per
//! context config (`clusters/<cluster>/<context>/config.yaml`, config/data).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

use self::yaml::{de_bool, de_null, de_opt_bool, f32_go, sorted_map};
use super::paths::{self, Env, Locations, ProcessEnv};

#[path = "benchmark.rs"]
pub mod benchmark;
#[path = "json.rs"]
pub mod json;
#[path = "yaml.rs"]
pub mod yaml;

/// Default refresh rate in seconds; also the enforced minimum.
pub const DEFAULT_REFRESH_RATE: f32 = 2.0;
/// Default max api server connection retries.
pub const DEFAULT_MAX_CONN_RETRY: i32 = 5;
/// Default api server call timeout (client.DefaultCallTimeoutDuration).
pub const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(120);
/// Default log level flag value.
pub const DEFAULT_LOG_LEVEL: &str = "info";
/// Default view (data.DefaultView).
pub const DEFAULT_VIEW: &str = "po";
/// Default log tail count.
pub const DEFAULT_LOGGER_TAIL_COUNT: i64 = 100;
/// Max log tail/buffer size.
pub const MAX_LOG_THRESHOLD: i64 = 5_000;
/// Default logs since seconds (tail).
pub const DEFAULT_SINCE_SECONDS: i64 = -1;
/// Default shell pod image.
pub const DEFAULT_SHELL_IMAGE: &str = "busybox:1.37.0";
/// Port forward address env override.
pub const ENV_PF_ADDRESS: &str = "K9S_DEFAULT_PF_ADDRESS";
/// Default port forward address.
pub const DEFAULT_PF_ADDRESS: &str = "localhost";
/// Node shell feature gate env override.
pub const ENV_FG_NODE_SHELL: &str = "K9S_FEATURE_GATE_NODE_SHELL";
/// Max number of namespace favorites (data.MaxFavoritesNS).
pub const MAX_FAVORITES_NS: usize = 9;

/// Known GPU vendors (config.KnownGPUVendors defaults).
pub fn default_gpu_vendors() -> BTreeMap<String, String> {
    [
        ("nvidia", "nvidia.com/gpu"),
        ("nvidia-shared", "nvidia.com/gpu.shared"),
        ("amd", "amd.com/gpu"),
        ("intel", "gpu.intel.com/i915"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

fn default_pf_address(env: &dyn Env) -> String {
    env.get_set(ENV_PF_ADDRESS)
        .unwrap_or_else(|| DEFAULT_PF_ADDRESS.to_string())
}

/// Root of config.yaml.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub k9s: K9s,
}

/// Command line overrides (config.Flags). `None` means not given.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Flags {
    pub refresh_rate: Option<f32>,
    pub log_level: Option<String>,
    pub log_file: Option<String>,
    pub headless: Option<bool>,
    pub logoless: Option<bool>,
    pub command: Option<String>,
    pub all_namespaces: Option<bool>,
    pub read_only: Option<bool>,
    pub write: Option<bool>,
    pub crumbsless: Option<bool>,
    pub splashless: Option<bool>,
    pub invert: Option<bool>,
    pub screen_dump_dir: Option<String>,
}

/// Runtime state set from flags (the unexported `manual*` fields of k9s).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Manual {
    pub refresh_rate: f32,
    pub read_only: Option<bool>,
    pub command: Option<String>,
    pub screen_dump_dir: Option<String>,
    pub headless: Option<bool>,
    pub logoless: Option<bool>,
    pub crumbsless: Option<bool>,
    pub splashless: Option<bool>,
    pub invert: Option<bool>,
    /// GPU vendors merged from config files (config.KnownGPUVendors).
    pub gpu_vendors: BTreeMap<String, String>,
}

/// `k9s:` section (config.K9s).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct K9s {
    #[serde(deserialize_with = "de_bool")]
    pub live_view_auto_refresh: bool,
    #[serde(
        rename = "gpuVendors",
        serialize_with = "sorted_map",
        deserialize_with = "de_null"
    )]
    pub gpu_vendors: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "String::is_empty", deserialize_with = "de_null")]
    pub screen_dump_dir: String,
    #[serde(serialize_with = "f32_go")]
    pub refresh_rate: f32,
    #[serde(rename = "apiServerTimeout", deserialize_with = "de_null")]
    pub api_server_timeout: String,
    pub max_conn_retry: i32,
    #[serde(deserialize_with = "de_bool")]
    pub read_only: bool,
    #[serde(rename = "noExitOnCtrlC", deserialize_with = "de_bool")]
    pub no_exit_on_ctrl_c: bool,
    #[serde(deserialize_with = "de_null")]
    pub port_forward_address: String,
    #[serde(deserialize_with = "de_null")]
    pub ui: Ui,
    #[serde(deserialize_with = "de_bool")]
    pub skip_latest_rev_check: bool,
    #[serde(deserialize_with = "de_bool")]
    pub disable_pod_counting: bool,
    #[serde(deserialize_with = "de_null")]
    pub shell_pod: ShellPod,
    #[serde(deserialize_with = "de_null")]
    pub image_scans: ImageScans,
    #[serde(deserialize_with = "de_null")]
    pub logger: Logger,
    #[serde(deserialize_with = "de_null")]
    pub thresholds: Thresholds,
    #[serde(deserialize_with = "de_null")]
    pub default_view: String,
    /// Flag overrides; never serialized.
    #[serde(skip)]
    pub manual: Manual,
}

impl Default for K9s {
    fn default() -> Self {
        Self {
            live_view_auto_refresh: false,
            gpu_vendors: BTreeMap::new(),
            screen_dump_dir: String::new(),
            refresh_rate: DEFAULT_REFRESH_RATE,
            api_server_timeout: "2m0s".into(),
            max_conn_retry: DEFAULT_MAX_CONN_RETRY,
            read_only: false,
            no_exit_on_ctrl_c: false,
            port_forward_address: DEFAULT_PF_ADDRESS.into(),
            ui: Ui::default(),
            skip_latest_rev_check: false,
            disable_pod_counting: false,
            shell_pod: ShellPod::default(),
            image_scans: ImageScans::default(),
            logger: Logger::default(),
            thresholds: Thresholds::default(),
            default_view: String::new(),
            manual: Manual::default(),
        }
    }
}

/// `k9s.ui` (config.UI).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Ui {
    #[serde(deserialize_with = "de_bool")]
    pub enable_mouse: bool,
    #[serde(deserialize_with = "de_bool")]
    pub headless: bool,
    #[serde(deserialize_with = "de_bool")]
    pub logoless: bool,
    #[serde(deserialize_with = "de_bool")]
    pub crumbsless: bool,
    #[serde(deserialize_with = "de_bool")]
    pub splashless: bool,
    #[serde(deserialize_with = "de_bool")]
    pub reactive: bool,
    #[serde(deserialize_with = "de_bool")]
    pub no_icons: bool,
    #[serde(deserialize_with = "de_bool")]
    pub invert: bool,
    #[serde(skip_serializing_if = "String::is_empty", deserialize_with = "de_null")]
    pub skin: String,
    #[serde(deserialize_with = "de_bool")]
    pub defaults_to_full_screen: bool,
    #[serde(rename = "useFullGVRTitle", deserialize_with = "de_bool")]
    pub use_full_gvr_name: bool,
}

/// `k9s.shellPod` (config.ShellPod).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default = "ShellPod::zero")]
pub struct ShellPod {
    #[serde(deserialize_with = "de_null")]
    pub image: String,
    #[serde(skip_serializing_if = "Vec::is_empty", deserialize_with = "de_null")]
    pub command: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", deserialize_with = "de_null")]
    pub args: Vec<String>,
    #[serde(deserialize_with = "de_null")]
    pub namespace: String,
    #[serde(
        skip_serializing_if = "BTreeMap::is_empty",
        serialize_with = "sorted_map",
        deserialize_with = "de_null"
    )]
    pub limits: BTreeMap<String, String>,
    #[serde(
        skip_serializing_if = "BTreeMap::is_empty",
        serialize_with = "sorted_map",
        deserialize_with = "de_null"
    )]
    pub labels: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Vec::is_empty", deserialize_with = "de_null")]
    pub image_pull_secrets: Vec<LocalObjectReference>,
    #[serde(skip_serializing_if = "String::is_empty", deserialize_with = "de_null")]
    pub image_pull_policy: String,
    #[serde(
        skip_serializing_if = "std::ops::Not::not",
        deserialize_with = "de_bool"
    )]
    pub tty: bool,
    #[serde(skip_serializing_if = "Vec::is_empty", deserialize_with = "de_null")]
    pub host_path_volume: Vec<HostPathVolume>,
}

fn default_limits() -> BTreeMap<String, String> {
    [("cpu", "100m"), ("memory", "100Mi")]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

impl Default for ShellPod {
    /// config.NewShellPod.
    fn default() -> Self {
        Self {
            image: DEFAULT_SHELL_IMAGE.into(),
            namespace: "default".into(),
            limits: default_limits(),
            ..Self::zero()
        }
    }
}

impl ShellPod {
    /// Zero value (fields missing from a YAML shellPod section).
    pub fn zero() -> ShellPod {
        ShellPod {
            image: String::new(),
            command: vec![],
            args: vec![],
            namespace: String::new(),
            limits: BTreeMap::new(),
            labels: BTreeMap::new(),
            image_pull_secrets: vec![],
            image_pull_policy: String::new(),
            tty: false,
            host_path_volume: vec![],
        }
    }

    /// ShellPod.Validate.
    pub fn validate(&mut self) {
        if self.image.is_empty() {
            self.image = DEFAULT_SHELL_IMAGE.into();
        }
        if self.limits.is_empty() {
            self.limits = default_limits();
        }
    }
}

/// v1.LocalObjectReference.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalObjectReference {
    #[serde(deserialize_with = "de_null")]
    pub name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HostPathVolume {
    #[serde(deserialize_with = "de_null")]
    pub name: String,
    #[serde(deserialize_with = "de_null")]
    pub mount_path: String,
    #[serde(deserialize_with = "de_null")]
    pub host_path: String,
    #[serde(
        skip_serializing_if = "std::ops::Not::not",
        deserialize_with = "de_bool"
    )]
    pub read_only: bool,
}

/// `k9s.imageScans` (config.ImageScans).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ImageScans {
    #[serde(deserialize_with = "de_bool")]
    pub enable: bool,
    #[serde(deserialize_with = "de_null")]
    pub exclusions: ScanExclusions,
}

impl ImageScans {
    /// Whether a scan of an image in `ns` with labels `ll` is excluded.
    pub fn should_exclude(&self, ns: &str, ll: &BTreeMap<String, String>) -> bool {
        if !self.enable {
            return false;
        }
        self.exclusions.exclude(ns, ll)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ScanExclusions {
    #[serde(deserialize_with = "de_null")]
    pub namespaces: Vec<String>,
    #[serde(serialize_with = "sorted_map", deserialize_with = "de_null")]
    pub labels: BTreeMap<String, Vec<String>>,
}

impl ScanExclusions {
    fn exclude(&self, ns: &str, ll: &BTreeMap<String, String>) -> bool {
        if self.namespaces.iter().any(|n| n == ns) {
            return true;
        }
        ll.iter()
            .any(|(k, v)| self.labels.get(k).is_some_and(|vv| vv.contains(v)))
    }
}

/// `k9s.logger` (config.Logger).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Logger {
    pub tail: i64,
    pub buffer: i64,
    pub since_seconds: i64,
    #[serde(deserialize_with = "de_bool")]
    pub text_wrap: bool,
    #[serde(deserialize_with = "de_bool")]
    pub disable_autoscroll: bool,
    #[serde(deserialize_with = "de_bool")]
    pub column_lock: bool,
    #[serde(deserialize_with = "de_bool")]
    pub show_time: bool,
}

impl Default for Logger {
    fn default() -> Self {
        Self {
            tail: DEFAULT_LOGGER_TAIL_COUNT,
            buffer: MAX_LOG_THRESHOLD,
            since_seconds: DEFAULT_SINCE_SECONDS,
            text_wrap: false,
            disable_autoscroll: false,
            column_lock: false,
            show_time: false,
        }
    }
}

impl Logger {
    /// Logger.Validate.
    pub fn validate(&mut self) {
        if self.tail <= 0 {
            self.tail = DEFAULT_LOGGER_TAIL_COUNT;
        }
        if self.tail > MAX_LOG_THRESHOLD {
            self.tail = MAX_LOG_THRESHOLD;
        }
        if self.buffer <= 0 || self.buffer > MAX_LOG_THRESHOLD {
            self.buffer = MAX_LOG_THRESHOLD;
        }
        if self.since_seconds == 0 {
            self.since_seconds = DEFAULT_SINCE_SECONDS;
        }
    }
}

/// `k9s.thresholds` (config.Threshold).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Thresholds {
    #[serde(deserialize_with = "de_null")]
    pub cpu: Severity,
    #[serde(deserialize_with = "de_null")]
    pub memory: Severity,
}

/// Severity levels (config.SeverityLevel).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SeverityLevel {
    Low,
    Medium,
    High,
}

impl Thresholds {
    /// Threshold.Validate.
    pub fn validate(&mut self) {
        self.cpu.validate();
        self.memory.validate();
    }

    /// Severity level for `k` (`cpu` or `memory`) at percentage `v`.
    pub fn level_for(&self, k: &str, v: i32) -> SeverityLevel {
        let s = match k {
            "cpu" => &self.cpu,
            "memory" => &self.memory,
            _ => return SeverityLevel::Low,
        };
        if !(0..=100).contains(&v) {
            return SeverityLevel::Low;
        }
        if v >= s.critical {
            return SeverityLevel::High;
        }
        if v >= s.warn {
            return SeverityLevel::Medium;
        }
        SeverityLevel::Low
    }

    /// Color name for the severity of `k` at `v`.
    pub fn severity_color(&self, k: &str, v: i32) -> &'static str {
        match self.level_for(k, v) {
            SeverityLevel::High => "red",
            SeverityLevel::Medium => "orangered",
            SeverityLevel::Low => "green",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default = "Severity::zero")]
pub struct Severity {
    pub critical: i32,
    pub warn: i32,
}

impl Default for Severity {
    fn default() -> Self {
        Self {
            critical: 90,
            warn: 70,
        }
    }
}

impl Severity {
    fn zero() -> Severity {
        Severity {
            critical: 0,
            warn: 0,
        }
    }

    /// Severity.Validate: out of (0,100] values revert to defaults.
    pub fn validate(&mut self) {
        let norm = Severity::default();
        if !(1..=100).contains(&self.warn) {
            self.warn = norm.warn;
        }
        if !(1..=100).contains(&self.critical) {
            self.critical = norm.critical;
        }
    }
}

static REFRESH_RATE_WARNED: AtomicBool = AtomicBool::new(false);

impl K9s {
    /// K9s.Merge: take the loaded settings.
    pub fn merge(&mut self, k1: K9s) {
        for (k, v) in k1.gpu_vendors {
            self.manual.gpu_vendors.insert(k, v);
        }
        // config.KnownGPUVendors is global in k9s; the renderers read it.
        crate::render::helpers::set_gpu_vendors(&self.known_gpu_vendors());
        self.live_view_auto_refresh = k1.live_view_auto_refresh;
        self.default_view = k1.default_view;
        self.screen_dump_dir = k1.screen_dump_dir;
        self.refresh_rate = k1.refresh_rate;
        self.api_server_timeout = k1.api_server_timeout;
        self.max_conn_retry = k1.max_conn_retry;
        self.read_only = k1.read_only;
        self.no_exit_on_ctrl_c = k1.no_exit_on_ctrl_c;
        self.port_forward_address = k1.port_forward_address;
        self.ui = k1.ui;
        self.skip_latest_rev_check = k1.skip_latest_rev_check;
        self.disable_pod_counting = k1.disable_pod_counting;
        self.shell_pod = k1.shell_pod;
        self.logger = k1.logger;
        self.image_scans = k1.image_scans;
        self.thresholds = k1.thresholds;
    }

    /// Known GPU vendors: built-ins plus the ones from config files.
    pub fn known_gpu_vendors(&self) -> BTreeMap<String, String> {
        let mut m = default_gpu_vendors();
        for (k, v) in self
            .manual
            .gpu_vendors
            .iter()
            .chain(self.gpu_vendors.iter())
        {
            m.insert(k.clone(), v.clone());
        }
        m
    }

    /// K9s.Override: apply command line flags.
    pub fn override_flags(&mut self, f: &Flags) {
        if let Some(r) = f.refresh_rate {
            if r != DEFAULT_REFRESH_RATE {
                self.manual.refresh_rate = r;
            }
        }
        self.manual.headless = f.headless;
        self.manual.logoless = f.logoless;
        self.manual.crumbsless = f.crumbsless;
        self.manual.splashless = f.splashless;
        self.manual.invert = f.invert;
        if f.read_only == Some(true) {
            self.manual.read_only = Some(true);
        }
        if f.write == Some(true) {
            self.manual.read_only = Some(false);
        }
        self.manual.command = f.command.clone();
        self.manual.screen_dump_dir = f.screen_dump_dir.clone();
    }

    pub fn is_headless(&self) -> bool {
        self.manual.headless == Some(true) || self.ui.headless
    }
    pub fn is_logoless(&self) -> bool {
        self.manual.logoless == Some(true) || self.ui.logoless
    }
    pub fn is_crumbsless(&self) -> bool {
        self.manual.crumbsless == Some(true) || self.ui.crumbsless
    }
    pub fn is_splashless(&self) -> bool {
        self.manual.splashless == Some(true) || self.ui.splashless
    }
    pub fn is_invert(&self) -> bool {
        self.manual.invert == Some(true) || self.ui.invert
    }

    /// Effective refresh rate (flag over config), never below the minimum.
    pub fn get_refresh_rate(&self) -> f32 {
        let mut rate = self.refresh_rate;
        if self.manual.refresh_rate != 0.0 {
            rate = self.manual.refresh_rate;
        }
        if rate.is_nan() || rate < DEFAULT_REFRESH_RATE {
            if !REFRESH_RATE_WARNED.swap(true, Ordering::Relaxed) {
                tracing::warn!(
                    requested = rate as f64,
                    minimum = DEFAULT_REFRESH_RATE as f64,
                    "Refresh rate is below minimum, capping to minimum value"
                );
            }
            return DEFAULT_REFRESH_RATE;
        }
        rate
    }

    /// Refresh rate as a duration.
    pub fn refresh_duration(&self) -> Duration {
        secs(self.get_refresh_rate() as f64)
    }

    /// Effective read only mode: config, then context config, then flags.
    pub fn is_read_only(&self, ctx: Option<&ContextConfig>) -> bool {
        let mut ro = self.read_only;
        if let Some(r) = ctx.and_then(|c| c.read_only) {
            ro = r;
        }
        if let Some(r) = self.manual.read_only {
            ro = r;
        }
        ro
    }

    /// Screen dumps dir: flag, then config, then `default` (AppScreenDumpDir).
    pub fn app_screen_dump_dir_or(&mut self, default: &Path) -> PathBuf {
        let mut d = self.screen_dump_dir.clone();
        if let Some(m) = self
            .manual
            .screen_dump_dir
            .clone()
            .filter(|m| !m.is_empty())
        {
            d = m.clone();
            self.screen_dump_dir = m;
        }
        if d.is_empty() {
            return default.to_path_buf();
        }
        PathBuf::from(d)
    }

    /// Screen dumps dir with the k9s default location as fallback.
    pub fn app_screen_dump_dir(&mut self) -> PathBuf {
        self.app_screen_dump_dir_or(&paths::dumps_dir())
    }

    /// Context screen dumps dir: `<dumps>/<cluster>/<context>` (`na` when no context).
    pub fn context_screen_dump_dir(&mut self, active: Option<(&str, &str)>) -> PathBuf {
        let base = self.app_screen_dump_dir();
        match active {
            Some((cl, ct)) => base.join(paths::sanitize_context_subpath(cl, ct)),
            None => base.join("na"),
        }
    }

    /// Initial view: `-c` flag (consumed, used once) else `ctx` view.active.
    pub fn active_view(&mut self, ctx: Option<&ContextConfig>) -> String {
        let mut v = ctx
            .map(|c| c.view.active.clone())
            .unwrap_or_else(|| DEFAULT_VIEW.to_string());
        if let Some(cmd) = self.manual.command.as_mut() {
            if !cmd.is_empty() {
                v = std::mem::take(cmd);
            }
        }
        v
    }

    /// Config.ResetActiveView: when `-n`/`-A` is given, drop the namespace
    /// argument from the saved active view.
    pub fn reset_active_view(&mut self, ctx: &mut ContextConfig) {
        if self
            .manual
            .command
            .as_deref()
            .is_some_and(|c| !c.is_empty())
        {
            return;
        }
        let v = self.active_view(Some(ctx));
        if v.is_empty() {
            return;
        }
        let (cmd, has_ns) = view_cmd_has_ns(&v);
        if has_ns {
            ctx.view.active = cmd;
        }
    }

    /// K9s.Validate with the process environment.
    pub fn validate(&mut self) {
        self.validate_with(&ProcessEnv)
    }

    /// K9s.Validate.
    pub fn validate_with(&mut self, env: &dyn Env) {
        if self.refresh_rate <= 0.0 {
            self.refresh_rate = DEFAULT_REFRESH_RATE;
        }
        if self.max_conn_retry <= 0 {
            self.max_conn_retry = DEFAULT_MAX_CONN_RETRY;
        }
        if let Some(a) = env.get_set(ENV_PF_ADDRESS) {
            self.port_forward_address = a;
        }
        if self.port_forward_address.is_empty() {
            self.port_forward_address = default_pf_address(env);
        }
        self.shell_pod.validate();
        self.logger.validate();
        self.thresholds.validate();
    }

    /// API server timeout (`apiServerTimeout`, default 2m).
    pub fn api_server_timeout(&self) -> Duration {
        parse_go_duration(&self.api_server_timeout).unwrap_or(DEFAULT_CALL_TIMEOUT)
    }
}

/// `(cmd, has namespace arg)` of a view command line (cmd.Interpreter).
fn view_cmd_has_ns(line: &str) -> (String, bool) {
    let ff: Vec<&str> = line.split_whitespace().collect();
    let Some(first) = ff.first() else {
        return (String::new(), false);
    };
    let cmd = first.to_lowercase();
    let mut rest = line.replacen(first, "", 1).trim().to_string();
    let mut lbls = String::new();
    if rest.contains('\'') {
        let idx: Vec<usize> = rest.match_indices('\'').map(|(i, _)| i).take(2).collect();
        if idx.len() == 2 {
            lbls = rest[idx[0] + 1..idx[1]].to_string();
            rest = rest
                .replacen(&format!("'{lbls}'"), "", 1)
                .trim()
                .to_string();
        } else {
            rest.clear();
        }
    }
    let mut aa: Vec<String> = rest.split_whitespace().map(String::from).collect();
    if !lbls.is_empty() {
        aa.push(lbls);
    }
    let is_ctx = matches!(cmd.as_str(), "ctx" | "context" | "contexts");
    let is_dir = matches!(cmd.as_str(), "dir" | "dirs" | "d" | "ls");
    let is_xray = matches!(cmd.as_str(), "x" | "xr" | "xray");
    let mut ns = String::new();
    let mut topic = false;
    let mut i = 0;
    while i < aa.len() {
        let a = aa[i].trim();
        let is_label = ["=", "==", "!=", " in ", " notin "]
            .iter()
            .any(|f| a.contains(f));
        if a.starts_with("-f") {
            if a == "-f" {
                i += 1;
            }
        } else if !(a.starts_with('/') || a.starts_with('@') || is_label || is_ctx || is_dir) {
            // xray takes the resource first, then the namespace.
            if is_xray && !topic {
                topic = true;
            } else {
                ns = a.to_lowercase();
            }
        }
        i += 1;
    }
    (cmd, !ns.is_empty())
}

/// Parse a Go duration string (`2m0s`, `1.5h`, `300ms`, `0`).
pub fn parse_go_duration(s: &str) -> Option<Duration> {
    let mut s = s.trim();
    if s.is_empty() {
        return None;
    }
    let neg = s.starts_with('-');
    if s.starts_with('-') || s.starts_with('+') {
        s = &s[1..];
    }
    if s == "0" {
        return Some(Duration::ZERO);
    }
    if s.is_empty() {
        return None;
    }
    let mut total = 0f64;
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let start = i;
        while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
            i += 1;
        }
        let num = &s[start..i];
        if num.is_empty() || num == "." {
            return None;
        }
        let n: f64 = num.parse().ok()?;
        let us = i;
        while i < b.len() && !(b[i].is_ascii_digit() || b[i] == b'.') {
            i += 1;
        }
        let unit = &s[us..i];
        let mult = match unit {
            "ns" => 1e-9,
            "us" | "µs" | "μs" => 1e-6,
            "ms" => 1e-3,
            "s" => 1.0,
            "m" => 60.0,
            "h" => 3600.0,
            _ => return None,
        };
        total += n * mult;
    }
    if neg {
        return None;
    }
    Duration::try_from_secs_f64(total).ok()
}

/// Seconds from config/flags as a duration timers accept: never panics on
/// NaN, infinite or huge values (capped at a day); non-positive is zero.
pub fn secs(r: f64) -> Duration {
    const MAX: f64 = 24.0 * 3600.0;
    if r.is_nan() || r <= 0.0 {
        return Duration::ZERO;
    }
    Duration::from_secs_f64(r.min(MAX))
}

/// Format a duration like Go's `time.Duration.String` (whole units).
pub fn format_go_duration(d: Duration) -> String {
    let total = d.as_nanos();
    if total == 0 {
        return "0s".into();
    }
    if total < 1_000_000_000 {
        if total < 1_000 {
            return format!("{total}ns");
        }
        if total < 1_000_000 {
            return format!("{}µs", total as f64 / 1e3);
        }
        return format!("{}ms", total as f64 / 1e6);
    }
    let secs = d.as_secs();
    let frac = d.subsec_nanos();
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    let mut sec = s.to_string();
    if frac > 0 {
        let f = format!("{frac:09}");
        sec = format!("{s}.{}", f.trim_end_matches('0'));
    }
    if h > 0 {
        format!("{h}h{m}m{sec}s")
    } else if m > 0 {
        format!("{m}m{sec}s")
    } else {
        format!("{sec}s")
    }
}

impl Config {
    /// New default config for the given locations (config.NewConfig).
    pub fn new(locs: &Locations) -> Config {
        Config {
            k9s: K9s {
                screen_dump_dir: locs.dumps_dir.to_string_lossy().into_owned(),
                ..K9s::default()
            },
        }
    }

    /// Load config.yaml from the k9s config dir, writing defaults if missing.
    /// Problems with the file content are logged and the defaults kept,
    /// like k9s.
    pub fn load() -> Result<Config> {
        let (c, err) = Config::load_with(&Locations::current())?;
        if let Some(e) = err {
            tracing::warn!(error = %e, "Fail to load global/context configuration");
        }
        Ok(c)
    }

    /// Load `locs.config_file` (created with defaults when missing). The
    /// second value carries schema/parse problems; the config is still usable.
    pub fn load_with(locs: &Locations) -> Result<(Config, Option<anyhow::Error>)> {
        let mut c = Config::new(locs);
        if !locs.config_file.exists() {
            c.validate();
            c.save_file(&locs.config_file)?;
        }
        let err = c.load_file(&locs.config_file).err();
        c.validate();
        Ok((c, err))
    }

    /// Load from an explicit path (any validation problem is an error).
    pub fn load_from(path: &Path) -> Result<Config> {
        let mut c = Config::default();
        c.load_file(path)?;
        c.validate();
        Ok(c)
    }

    /// Config.Load: merge `path` into `self`. Schema violations are reported
    /// in the error but the settings are still applied.
    pub fn load_file(&mut self, path: &Path) -> Result<()> {
        let bb = std::fs::read_to_string(path)?;
        let mut errs: Vec<String> = Vec::new();
        if let Err(e) = json::validate(json::K9S_SCHEMA, &bb) {
            errs.push(format!(
                "k9rs config file {:?} load failed:\n{e}",
                path.to_string_lossy()
            ));
        }
        match parse_main_config(&bb) {
            Ok(Some(k)) => self.k9s.merge(k),
            Ok(None) => {}
            Err(e) => errs.push(format!("main config.yaml load failed: {e}")),
        }
        if errs.is_empty() {
            Ok(())
        } else {
            Err(anyhow!(errs.join("\n")))
        }
    }

    /// Save to the k9s config file.
    pub fn save(&self) -> Result<()> {
        self.save_file(&paths::config_file())
    }

    /// Config.SaveFile.
    pub fn save_file(&self, path: &Path) -> Result<()> {
        paths::ensure_dir_path(path)?;
        let s = yaml::to_string(self)?;
        paths::write_file(path, s.as_bytes())?;
        tracing::info!(path = %path.display(), "[CONFIG] Saving K9rs config to disk");
        Ok(())
    }

    /// Validate and fill defaults (config.Validate).
    pub fn validate(&mut self) {
        self.k9s.validate();
    }

    /// Config.Refine namespace selection: `-A`, then `-n`, then the context
    /// active namespace; records it on `ctx` and returns it.
    pub fn refine_namespace(
        &mut self,
        ctx: &mut ContextConfig,
        all_namespaces: bool,
        namespace: Option<&str>,
    ) -> String {
        let mut ns = if all_namespaces {
            self.k9s.reset_active_view(ctx);
            crate::k8s::NAMESPACE_ALL.to_string()
        } else if let Some(n) = namespace.filter(|n| !n.is_empty()) {
            self.k9s.reset_active_view(ctx);
            n.to_string()
        } else {
            ctx.namespace.active.clone()
        };
        if ns.is_empty() {
            ns = crate::k8s::DEFAULT_NAMESPACE.to_string();
        }
        if ns != crate::k8s::NOT_NAMESPACED {
            ctx.set_active_namespace(&ns);
        }
        ns
    }
}

fn parse_main_config(bb: &str) -> Result<Option<K9s>> {
    let v = yaml::parse_value(bb)?;
    let Some(k) = v.get("k9s") else {
        return Ok(None);
    };
    if k.is_null() {
        return Ok(None);
    }
    Ok(Some(serde_yaml::from_value(k.clone())?))
}

/// Per context config (`clusters/<cluster>/<context>/config.yaml`, data.Context).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ContextConfig {
    #[serde(skip_serializing_if = "String::is_empty", deserialize_with = "de_null")]
    pub cluster: String,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "de_opt_bool"
    )]
    pub read_only: Option<bool>,
    #[serde(
        skip_serializing_if = "skin_is_empty",
        deserialize_with = "de_opt_string"
    )]
    pub skin: Option<String>,
    #[serde(deserialize_with = "de_null")]
    pub namespace: Namespace,
    #[serde(deserialize_with = "de_null")]
    pub view: View,
    #[serde(deserialize_with = "de_null")]
    pub feature_gates: FeatureGates,
    #[serde(serialize_with = "ser_proxy")]
    pub proxy: Option<Proxy>,
    /// Not part of the k9s file format.
    #[serde(skip)]
    pub port_forward_address: String,
}

fn skin_is_empty(s: &Option<String>) -> bool {
    s.as_deref().is_none_or(str::is_empty)
}

fn de_opt_string<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<String>, D::Error> {
    Ok(Option::<String>::deserialize(d)?.filter(|s| !s.is_empty()))
}

fn ser_proxy<S: serde::Serializer>(
    p: &Option<Proxy>,
    s: S,
) -> std::result::Result<S::Ok, S::Error> {
    match p {
        Some(p) => p.serialize(s),
        None => s.serialize_none(),
    }
}

/// Context proxy (data.Proxy).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Proxy {
    #[serde(deserialize_with = "de_null")]
    pub address: String,
}

/// The on-disk shape of a context config (`k9s:` root).
#[derive(Serialize, Deserialize)]
struct ContextFile {
    #[serde(default)]
    k9s: Option<ContextConfig>,
}

/// Namespace favorites (data.Namespace).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default = "Namespace::zero")]
pub struct Namespace {
    #[serde(deserialize_with = "de_null")]
    pub active: String,
    #[serde(deserialize_with = "de_bool")]
    pub lock_favorites: bool,
    #[serde(deserialize_with = "de_null")]
    pub favorites: Vec<String>,
}

impl Default for Namespace {
    /// data.NewNamespace.
    fn default() -> Self {
        Namespace::new_active(crate::k8s::DEFAULT_NAMESPACE)
    }
}

impl Namespace {
    fn zero() -> Namespace {
        Namespace {
            active: String::new(),
            lock_favorites: false,
            favorites: vec![],
        }
    }

    /// data.NewActiveNamespace.
    pub fn new_active(ns: &str) -> Namespace {
        let ns = if ns.is_empty() {
            crate::k8s::DEFAULT_NAMESPACE
        } else {
            ns
        };
        Namespace {
            active: ns.to_string(),
            lock_favorites: false,
            favorites: vec![crate::k8s::DEFAULT_NAMESPACE.to_string()],
        }
    }

    /// Namespace.SetActive: blank means all namespaces; adds to favorites
    /// (most recent first, capped) unless locked.
    pub fn set_active(&mut self, ns: &str) {
        let ns = if ns.is_empty() {
            crate::k8s::NAMESPACE_ALL
        } else {
            ns
        };
        self.active = ns.to_string();
        if !self.lock_favorites {
            self.add_fav(ns);
        }
    }

    fn add_fav(&mut self, ns: &str) {
        if self.favorites.iter().any(|f| f == ns) {
            return;
        }
        let mut nfv = Vec::with_capacity(MAX_FAVORITES_NS);
        nfv.push(ns.to_string());
        for (i, f) in self.favorites.iter().enumerate() {
            if i + 1 < MAX_FAVORITES_NS {
                nfv.push(f.clone());
            }
        }
        self.favorites = nfv;
    }

    /// Remove a favorite (no-op when locked).
    pub fn rm_fav(&mut self, ns: &str) {
        if self.lock_favorites {
            return;
        }
        if let Some(i) = self.favorites.iter().position(|f| f == ns) {
            self.favorites.remove(i);
        }
    }

    fn trim(&mut self) {
        if self.favorites.len() > MAX_FAVORITES_NS {
            self.favorites.truncate(MAX_FAVORITES_NS);
        }
    }

    /// Namespace.merge: append older favorites not already present.
    pub fn merge(&mut self, old: &Namespace) {
        if self.lock_favorites {
            return;
        }
        for f in &old.favorites {
            if !self.favorites.contains(f) {
                self.favorites.push(f.clone());
            }
        }
        self.trim();
    }

    /// Namespace.Validate: drop favorites the cluster does not know about
    /// (skipped when the active namespace itself is unknown).
    pub fn validate_with(&mut self, is_valid: &dyn Fn(&str) -> bool) {
        if !is_valid(&self.active) {
            return;
        }
        let invalid: Vec<String> = self
            .favorites
            .iter()
            .filter(|f| !is_valid(f))
            .cloned()
            .collect();
        for f in invalid {
            self.rm_fav(&f);
        }
        self.trim();
    }

    /// Whether the active namespace is all namespaces.
    pub fn is_all_namespaces(&self) -> bool {
        self.active == crate::k8s::NAMESPACE_ALL || self.active.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default = "View::zero")]
pub struct View {
    #[serde(deserialize_with = "de_null")]
    pub active: String,
}

impl Default for View {
    fn default() -> Self {
        Self {
            active: DEFAULT_VIEW.into(),
        }
    }
}

impl View {
    fn zero() -> View {
        View {
            active: String::new(),
        }
    }

    /// View.Validate.
    pub fn validate(&mut self) {
        if self.active.is_empty() {
            self.active = DEFAULT_VIEW.into();
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FeatureGates {
    #[serde(deserialize_with = "de_bool")]
    pub node_shell: bool,
}

impl ContextConfig {
    /// data.NewContextFromConfig: fresh config for a kubeconfig context.
    pub fn from_kube(cluster: &str, kube_namespace: &str) -> ContextConfig {
        ContextConfig {
            cluster: cluster.to_string(),
            namespace: Namespace::new_active(kube_namespace),
            ..ContextConfig::default()
        }
    }

    /// Load the per-context config for `cluster`/`context`, with defaults
    /// (generated and saved when missing).
    pub fn load(cluster: &str, context: &str) -> Result<ContextConfig> {
        // K9s.ActiveContext: nothing to load (or generate) without a context.
        if cluster.is_empty() || context.is_empty() {
            return Err(anyhow!("no context found for: {context:?}"));
        }
        ContextConfig::activate(&paths::contexts_dir(), cluster, context, "")
    }

    /// K9s.ActivateContext: load (or generate) the context config under
    /// `root`, validate it and apply the kubeconfig context namespace.
    pub fn activate(
        root: &Path,
        cluster: &str,
        context: &str,
        kube_namespace: &str,
    ) -> Result<ContextConfig> {
        let mut c = ContextConfig::load_dir(root, cluster, context, kube_namespace)?;
        c.validate_with(&ProcessEnv, cluster);
        if !kube_namespace.is_empty() {
            c.namespace.active = kube_namespace.to_string();
        } else if c.namespace.active.is_empty() {
            c.namespace.active = crate::k8s::DEFAULT_NAMESPACE.to_string();
        }
        Ok(c)
    }

    /// data.Dir.Load: read the context config or generate a new one.
    pub fn load_dir(
        root: &Path,
        cluster: &str,
        context: &str,
        kube_namespace: &str,
    ) -> Result<ContextConfig> {
        let path = root
            .join(paths::sanitize_context_subpath(cluster, context))
            .join(paths::MAIN_CONFIG_FILE);
        tracing::debug!(path = %path.display(), cluster, context, "[CONFIG] Loading context config from disk");
        let empty = match std::fs::metadata(&path) {
            Ok(m) => m.len() == 0,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
            Err(e) => return Err(e.into()),
        };
        if empty {
            tracing::debug!(path = %path.display(), "Context config not found! Generating..");
            let cfg = ContextConfig::from_kube(cluster, kube_namespace);
            cfg.save_to(&path)?;
            return Ok(cfg);
        }
        ContextConfig::read(&path)
    }

    /// Read a context config file (data.Dir.loadConfig).
    pub fn read(path: &Path) -> Result<ContextConfig> {
        let bb = std::fs::read_to_string(path)?;
        if let Err(e) = json::validate(json::CONTEXT_SCHEMA, &bb) {
            tracing::warn!(path = %path.display(), error = %e, "Validation failed. Please update your config and restart!");
        }
        ContextConfig::parse(&bb).map_err(|e| anyhow!("context-config yaml load failed: {e}\n{bb}"))
    }

    /// Parse context config YAML (`k9s:` root) without validation.
    pub fn parse(bb: &str) -> Result<ContextConfig> {
        let v = yaml::parse_value(bb)?;
        if v.is_null() {
            return Ok(ContextConfig::default());
        }
        let f: ContextFile = serde_yaml::from_value(v)?;
        Ok(f.k9s.unwrap_or_default())
    }

    /// Save under the k9s contexts dir (data.Dir.Save).
    pub fn save(&self, cluster: &str, context: &str) -> Result<()> {
        let path = Locations::current().context_config(cluster, context);
        self.save_to(&path)
    }

    /// data.Dir.Save: merge favorites from the file on disk, then write.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        let mut c = self.clone();
        if path.exists() {
            if let Ok(old) = ContextConfig::read(path) {
                c.merge(&old);
            }
        }
        paths::ensure_dir_path(path)?;
        let s = yaml::to_string(&ContextFile { k9s: Some(c) })?;
        paths::write_file(path, s.as_bytes())
    }

    /// Context.merge.
    pub fn merge(&mut self, old: &ContextConfig) {
        self.namespace.merge(&old.namespace);
    }

    /// Context.Validate (namespace checks need a cluster, see
    /// `Namespace::validate_with`).
    pub fn validate_with(&mut self, env: &dyn Env, cluster: &str) {
        self.cluster = cluster.to_string();
        if let Some(b) = env.get_set(ENV_FG_NODE_SHELL) {
            self.feature_gates.node_shell = b == "true";
        }
        self.view.validate();
    }

    /// Record `ns` as the active namespace and add it to the favorites
    /// (most recent first, capped at MAX_FAVORITES_NS) unless locked.
    pub fn set_active_namespace(&mut self, ns: &str) {
        if ns == crate::k8s::NOT_NAMESPACED {
            return;
        }
        self.namespace.set_active(ns);
    }

    /// Favorite namespaces.
    pub fn fav_namespaces(&self) -> &[String] {
        &self.namespace.favorites
    }

    /// Context skin override, if any.
    pub fn skin_name(&self) -> Option<&str> {
        self.skin.as_deref().filter(|s| !s.is_empty())
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn secs_never_panics() {
        assert_eq!(secs(f64::NAN), Duration::ZERO);
        assert_eq!(secs(-1.0), Duration::ZERO);
        assert_eq!(secs(f64::INFINITY), Duration::from_secs(86_400));
        assert_eq!(secs(1e300), Duration::from_secs(86_400));
        assert_eq!(secs(2.0), Duration::from_secs(2));
        let mut k = K9s {
            refresh_rate: f32::NAN,
            ..Default::default()
        };
        assert_eq!(k.refresh_duration(), Duration::from_secs(2));
        k.refresh_rate = 1e38;
        assert_eq!(k.refresh_duration(), Duration::from_secs(86_400));
    }

    use super::*;
    use crate::config::paths::MapEnv;

    fn td(p: &str) -> PathBuf {
        PathBuf::from(format!(
            "{}/src/config/testdata/{p}",
            env!("CARGO_MANIFEST_DIR")
        ))
    }

    #[test]
    fn defaults() {
        let k = K9s::default();
        assert_eq!(k.refresh_rate, 2.0);
        assert_eq!(k.max_conn_retry, 5);
        assert_eq!(k.api_server_timeout, "2m0s");
        assert_eq!(k.api_server_timeout(), Duration::from_secs(120));
        assert_eq!(k.port_forward_address, "localhost");
        assert_eq!(k.shell_pod.image, "busybox:1.37.0");
        assert_eq!(k.shell_pod.namespace, "default");
        assert!(!k.shell_pod.tty);
        assert_eq!(
            k.logger,
            Logger {
                tail: 100,
                buffer: 5000,
                since_seconds: -1,
                ..Logger::default()
            }
        );
        assert_eq!(
            k.thresholds.cpu,
            Severity {
                critical: 90,
                warn: 70
            }
        );
        assert_eq!(k.known_gpu_vendors().len(), 4);
    }

    fn mock_config() -> Config {
        let mut c = Config::default();
        c.k9s.screen_dump_dir = "/tmp/k9s-test/screen-dumps".into();
        c
    }

    #[test]
    fn config_save_default() {
        // TestConfigSave: a fresh config with a screen dump dir override.
        let mut c = mock_config();
        c.k9s.override_flags(&Flags {
            screen_dump_dir: Some("/tmp/k9s-test/screen-dumps".into()),
            ..Flags::default()
        });
        let _ = c.k9s.app_screen_dump_dir_or(Path::new("/nope"));
        c.k9s.validate_with(&MapEnv::default());
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("sub/k9s.yaml");
        c.save_file(&p).unwrap();
        let ee = std::fs::read_to_string(td("config/configs/default.yaml")).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), ee);
    }

    #[test]
    fn config_load() {
        let mut c = mock_config();
        c.load_file(&td("config/configs/k9s.yaml")).unwrap();
        assert_eq!(c.k9s.refresh_rate, 2.0);
        assert_eq!(c.k9s.logger.tail, 200);
        assert_eq!(c.k9s.logger.buffer, 2000);
        assert!(c.k9s.live_view_auto_refresh);
        assert_eq!(c.k9s.api_server_timeout, "10s");
        assert_eq!(c.k9s.api_server_timeout(), Duration::from_secs(10));
        // TestAppScreenDumpDir / TestContextScreenDumpDir.
        assert_eq!(
            c.k9s.app_screen_dump_dir_or(Path::new("/x")),
            PathBuf::from("/tmp/k9s-test/screen-dumps")
        );
        assert_eq!(
            c.k9s.context_screen_dump_dir(Some(("cl-1", "ct-1-1"))),
            PathBuf::from("/tmp/k9s-test/screen-dumps/cl-1/ct-1-1")
        );
    }

    #[test]
    fn config_load_toast() {
        let mut c = Config::default();
        let p = td("config/configs/k9s_toast.yaml");
        let err = c.load_file(&p).unwrap_err().to_string();
        assert_eq!(
            err,
            format!(
                "k9rs config file {:?} load failed:\nAdditional property disablePodCounts is not allowed\nAdditional property shellPods is not allowed\nInvalid type. Expected: boolean, given: string",
                p.to_string_lossy()
            )
        );
        // Settings still apply (yaml.v3 YAML 1.1 bool `yes`).
        assert!(c.k9s.skip_latest_rev_check);
        assert_eq!(c.k9s.logger.tail, 200);
        assert!(Config::load_from(&p).is_err());
    }

    #[test]
    fn config_load_crap() {
        let mut c = Config::default();
        assert!(c
            .load_file(&td("config/configs/k9s_not_there.yaml"))
            .is_err());
    }

    #[test]
    fn config_save_file_expected() {
        // TestConfigSaveFile.
        let mut c = mock_config();
        c.load_file(&td("config/configs/k9s.yaml")).unwrap();
        c.k9s.refresh_rate = 100.0;
        c.k9s.gpu_vendors = [("bozo".to_string(), "bozo/gpu.com".to_string())]
            .into_iter()
            .collect();
        c.k9s.api_server_timeout = "30s".into();
        c.k9s.read_only = true;
        c.k9s.logger.tail = 500;
        c.k9s.logger.buffer = 800;
        c.k9s.ui.use_full_gvr_name = true;
        c.k9s.validate_with(&MapEnv::default());
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("k9s.yaml");
        c.save_file(&p).unwrap();
        let ee = std::fs::read_to_string(td("config/configs/expected.yaml")).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), ee);
    }

    #[test]
    fn config_reset_roundtrip() {
        // TestConfigReset: loading and saving k9s.yaml gives it back.
        let mut c = mock_config();
        c.load_file(&td("config/configs/k9s.yaml")).unwrap();
        c.k9s.screen_dump_dir = "/tmp/k9s-test/screen-dumps".into();
        c.k9s.validate_with(&MapEnv::default());
        let s = yaml::to_string(&c).unwrap();
        assert_eq!(
            s,
            std::fs::read_to_string(td("config/configs/k9s.yaml")).unwrap()
        );
    }

    #[test]
    fn screen_dump_override() {
        for (dir, e) in [
            ("", "/tmp/k9s-test/screen-dumps"),
            ("/tmp/k9s-test/sd", "/tmp/k9s-test/sd"),
        ] {
            let mut c = mock_config();
            c.k9s.screen_dump_dir.clear();
            c.load_file(&td("config/configs/k9s.yaml")).unwrap();
            c.k9s.manual.screen_dump_dir = Some(dir.into());
            assert_eq!(
                c.k9s
                    .app_screen_dump_dir_or(Path::new("/tmp/k9s-test/screen-dumps")),
                PathBuf::from(e)
            );
        }
    }

    #[test]
    fn k9s_merge() {
        let mut k1 = K9s {
            refresh_rate: 0.0,
            max_conn_retry: 0,
            shell_pod: ShellPod::zero(),
            ..K9s::default()
        };
        let k2 = K9s {
            live_view_auto_refresh: true,
            max_conn_retry: 100,
            gpu_vendors: [("bozo".to_string(), "b.com/gpu".to_string())]
                .into_iter()
                .collect(),
            ..K9s::default()
        };
        k1.merge(k2);
        assert!(k1.live_view_auto_refresh);
        assert_eq!(k1.max_conn_retry, 100);
        assert_eq!(k1.shell_pod, ShellPod::default());
        assert!(k1.gpu_vendors.is_empty());
        assert_eq!(k1.known_gpu_vendors()["bozo"], "b.com/gpu");
    }

    #[test]
    fn overrides() {
        // Test_k9sOverrides.
        let plain = K9s {
            refresh_rate: 10.0,
            ..K9s::default()
        };
        assert_eq!(plain.get_refresh_rate(), 10.0);
        assert!(!plain.is_read_only(None));
        assert!(
            !plain.is_headless()
                && !plain.is_logoless()
                && !plain.is_crumbsless()
                && !plain.is_splashless()
        );

        let sub = K9s {
            refresh_rate: 0.5,
            ..K9s::default()
        };
        assert_eq!(sub.get_refresh_rate(), 2.0);

        let set = K9s {
            refresh_rate: 10.0,
            read_only: true,
            ui: Ui {
                headless: true,
                logoless: true,
                crumbsless: true,
                splashless: true,
                ..Ui::default()
            },
            ..K9s::default()
        };
        assert!(set.is_read_only(None));
        assert!(
            set.is_headless() && set.is_logoless() && set.is_crumbsless() && set.is_splashless()
        );

        let mut ov = K9s {
            refresh_rate: 10.0,
            ..K9s::default()
        };
        ov.override_flags(&Flags {
            refresh_rate: Some(100.0),
            headless: Some(true),
            logoless: Some(true),
            crumbsless: Some(true),
            splashless: Some(true),
            invert: Some(true),
            read_only: Some(true),
            command: Some("po".into()),
            screen_dump_dir: Some("/tmp/blee".into()),
            ..Flags::default()
        });
        assert_eq!(ov.get_refresh_rate(), 100.0);
        assert!(ov.is_read_only(None));
        assert!(ov.is_headless() && ov.is_logoless() && ov.is_crumbsless() && ov.is_splashless());
        assert!(ov.is_invert());
        assert_eq!(
            ov.app_screen_dump_dir_or(Path::new("/x")),
            PathBuf::from("/tmp/blee")
        );
        assert_eq!(ov.screen_dump_dir, "/tmp/blee");

        // Default refresh flag value does not override the config.
        let mut d = K9s {
            refresh_rate: 5.0,
            ..K9s::default()
        };
        d.override_flags(&Flags {
            refresh_rate: Some(2.0),
            ..Flags::default()
        });
        assert_eq!(d.get_refresh_rate(), 5.0);
        // Manual below minimum is capped.
        let mut m = K9s {
            refresh_rate: 3.0,
            ..K9s::default()
        };
        m.manual.refresh_rate = 0.5;
        assert_eq!(m.get_refresh_rate(), 2.0);
        m.manual.refresh_rate = 4.0;
        assert_eq!(m.get_refresh_rate(), 4.0);
        assert_eq!(m.refresh_duration(), Duration::from_secs(4));
    }

    #[test]
    fn read_only_precedence() {
        let ctx_ro = ContextConfig {
            read_only: Some(true),
            ..ContextConfig::default()
        };
        let ctx_rw = ContextConfig {
            read_only: Some(false),
            ..ContextConfig::default()
        };
        let mut k = K9s::default();
        assert!(k.is_read_only(Some(&ctx_ro)));
        k.read_only = true;
        assert!(!k.is_read_only(Some(&ctx_rw)));
        k.override_flags(&Flags {
            write: Some(true),
            ..Flags::default()
        });
        assert!(!k.is_read_only(Some(&ctx_ro)));
        // --write wins over --readonly.
        k.override_flags(&Flags {
            read_only: Some(true),
            write: Some(true),
            ..Flags::default()
        });
        assert!(!k.is_read_only(None));
        let mut k = K9s::default();
        k.override_flags(&Flags {
            read_only: Some(true),
            ..Flags::default()
        });
        assert!(k.is_read_only(Some(&ctx_rw)));
        // --readonly=false does not force write mode.
        let mut k = K9s {
            read_only: true,
            ..K9s::default()
        };
        k.override_flags(&Flags {
            read_only: Some(false),
            write: Some(false),
            ..Flags::default()
        });
        assert!(k.is_read_only(None));
    }

    #[test]
    fn refresh_rate_yaml() {
        for (y, e) in [("refreshRate: 2", 2.0), ("refreshRate: 2.5", 2.5)] {
            let k: K9s = serde_yaml::from_str(y).unwrap();
            assert_eq!(k.refresh_rate, e);
        }
    }

    #[test]
    fn validate_fixes() {
        let mut k: K9s = serde_yaml::from_str(
            "refreshRate: 0\nmaxConnRetry: -1\nportForwardAddress: \"\"\nlogger:\n  tail: 10000\n  buffer: 0\n  sinceSeconds: 0\nthresholds:\n  cpu:\n    critical: 101\n  memory:\n    warn: 0\nshellPod:\n  image: \"\"\n",
        )
        .unwrap();
        k.validate_with(&MapEnv::default());
        assert_eq!(k.refresh_rate, 2.0);
        assert_eq!(k.max_conn_retry, 5);
        assert_eq!(k.port_forward_address, "localhost");
        assert_eq!(k.logger.tail, 5000);
        assert_eq!(k.logger.buffer, 5000);
        assert_eq!(k.logger.since_seconds, -1);
        assert_eq!(
            k.thresholds.cpu,
            Severity {
                critical: 90,
                warn: 70
            }
        );
        assert_eq!(
            k.thresholds.memory,
            Severity {
                critical: 90,
                warn: 70
            }
        );
        assert_eq!(k.shell_pod.image, DEFAULT_SHELL_IMAGE);
        assert_eq!(k.shell_pod.namespace, "");
        assert_eq!(k.shell_pod.limits, default_limits());
        let mut k = K9s::default();
        k.validate_with(&MapEnv::new(&[(ENV_PF_ADDRESS, "0.0.0.0")]));
        assert_eq!(k.port_forward_address, "0.0.0.0");
    }

    #[test]
    fn thresholds_levels() {
        let t = Thresholds::default();
        assert_eq!(t.level_for("cpu", 95), SeverityLevel::High);
        assert_eq!(t.level_for("memory", 75), SeverityLevel::Medium);
        assert_eq!(t.level_for("memory", 10), SeverityLevel::Low);
        assert_eq!(t.level_for("cpu", 101), SeverityLevel::Low);
        assert_eq!(t.level_for("blee", 99), SeverityLevel::Low);
        assert_eq!(t.severity_color("cpu", 95), "red");
        assert_eq!(t.severity_color("cpu", 75), "orangered");
        assert_eq!(t.severity_color("cpu", 5), "green");
    }

    #[test]
    fn scans_exclusions() {
        let mut s = ImageScans::default();
        s.exclusions.namespaces = vec!["ns-1".into()];
        s.exclusions
            .labels
            .insert("app".into(), vec!["fred".into()]);
        let ll: BTreeMap<String, String> = [("app".to_string(), "fred".to_string())]
            .into_iter()
            .collect();
        assert!(!s.should_exclude("ns-1", &ll));
        s.enable = true;
        assert!(s.should_exclude("ns-1", &BTreeMap::new()));
        assert!(s.should_exclude("ns-2", &ll));
        assert!(!s.should_exclude("ns-2", &BTreeMap::new()));
    }

    #[test]
    fn go_durations() {
        assert_eq!(parse_go_duration("2m0s"), Some(Duration::from_secs(120)));
        assert_eq!(parse_go_duration("1.5h"), Some(Duration::from_secs(5400)));
        assert_eq!(parse_go_duration("300ms"), Some(Duration::from_millis(300)));
        assert_eq!(parse_go_duration("0"), Some(Duration::ZERO));
        assert_eq!(parse_go_duration("10"), None);
        assert_eq!(parse_go_duration("blee"), None);
        assert_eq!(parse_go_duration(""), None);
        assert_eq!(format_go_duration(Duration::from_secs(120)), "2m0s");
        assert_eq!(format_go_duration(Duration::from_secs(15)), "15s");
        assert_eq!(format_go_duration(Duration::from_secs(3725)), "1h2m5s");
        assert_eq!(format_go_duration(Duration::from_millis(1500)), "1.5s");
        assert_eq!(format_go_duration(Duration::from_millis(300)), "300ms");
        assert_eq!(format_go_duration(Duration::ZERO), "0s");
    }

    #[test]
    fn active_view_and_command() {
        let ctx = ContextConfig {
            view: View {
                active: "svc".into(),
            },
            ..ContextConfig::default()
        };
        let mut k = K9s::default();
        assert_eq!(k.active_view(Some(&ctx)), "svc");
        assert_eq!(k.active_view(None), "po");
        k.override_flags(&Flags {
            command: Some("dp".into()),
            ..Flags::default()
        });
        assert_eq!(k.active_view(Some(&ctx)), "dp");
        // Only once.
        assert_eq!(k.active_view(Some(&ctx)), "svc");
    }

    #[test]
    fn reset_active_view() {
        let mut k = K9s::default();
        let mut ctx = ContextConfig {
            view: View {
                active: "po kube-system".into(),
            },
            ..ContextConfig::default()
        };
        k.reset_active_view(&mut ctx);
        assert_eq!(ctx.view.active, "po");
        let mut ctx = ContextConfig {
            view: View {
                active: "po app=blee /fred @ctx".into(),
            },
            ..ContextConfig::default()
        };
        k.reset_active_view(&mut ctx);
        assert_eq!(ctx.view.active, "po app=blee /fred @ctx");
        let mut ctx = ContextConfig {
            view: View {
                active: "xray po ns-1".into(),
            },
            ..ContextConfig::default()
        };
        k.reset_active_view(&mut ctx);
        assert_eq!(ctx.view.active, "xray");
        let mut ctx = ContextConfig {
            view: View {
                active: "xray po".into(),
            },
            ..ContextConfig::default()
        };
        k.reset_active_view(&mut ctx);
        assert_eq!(ctx.view.active, "xray po");
        let mut ctx = ContextConfig {
            view: View {
                active: "ctx fred".into(),
            },
            ..ContextConfig::default()
        };
        k.reset_active_view(&mut ctx);
        assert_eq!(ctx.view.active, "ctx fred");
        // A -c command skips the reset.
        k.override_flags(&Flags {
            command: Some("dp".into()),
            ..Flags::default()
        });
        let mut ctx = ContextConfig {
            view: View {
                active: "po ns-1".into(),
            },
            ..ContextConfig::default()
        };
        k.reset_active_view(&mut ctx);
        assert_eq!(ctx.view.active, "po ns-1");
    }

    #[test]
    fn refine_namespace() {
        let mut c = Config::default();
        let mut ctx = ContextConfig::from_kube("cl-1", "");
        assert_eq!(c.refine_namespace(&mut ctx, false, None), "default");
        assert_eq!(c.refine_namespace(&mut ctx, false, Some("ns-2")), "ns-2");
        assert_eq!(ctx.namespace.active, "ns-2");
        assert_eq!(ctx.namespace.favorites, vec!["ns-2", "default"]);
        assert_eq!(c.refine_namespace(&mut ctx, true, Some("ns-2")), "all");
        assert_eq!(ctx.namespace.favorites, vec!["all", "ns-2", "default"]);
        let mut ctx = ContextConfig::from_kube("cl-1", "");
        ctx.namespace.active.clear();
        assert_eq!(c.refine_namespace(&mut ctx, false, None), "default");
    }

    // ---- data package ------------------------------------------------------

    #[test]
    fn ns_set_active() {
        let mut ns = Namespace::default();
        for (n, fav) in [
            ("all", vec!["all", "default"]),
            ("ns1", vec!["ns1", "all", "default"]),
            ("ns2", vec!["ns2", "ns1", "all", "default"]),
            ("ns3", vec!["ns3", "ns2", "ns1", "all", "default"]),
            ("ns4", vec!["ns4", "ns3", "ns2", "ns1", "all", "default"]),
        ] {
            ns.set_active(n);
            assert_eq!(ns.active, n);
            assert_eq!(ns.favorites, fav);
        }
        // Existing favorite: order unchanged.
        ns.set_active("ns1");
        assert_eq!(ns.favorites[0], "ns4");
        // Blank is all namespaces.
        ns.set_active("");
        assert_eq!(ns.active, "all");
        // Cap.
        for i in 5..20 {
            ns.set_active(&format!("ns{i}"));
        }
        assert_eq!(ns.favorites.len(), MAX_FAVORITES_NS);
        assert_eq!(ns.favorites[0], "ns19");
        // Locked.
        ns.lock_favorites = true;
        ns.set_active("zorg");
        assert_eq!(ns.active, "zorg");
        assert!(!ns.favorites.contains(&"zorg".to_string()));
    }

    #[test]
    fn ns_validate() {
        let mut ns = Namespace::default();
        ns.validate_with(&|_| true);
        assert_eq!(ns.active, "default");
        assert_eq!(ns.favorites, vec!["default"]);

        let mut ns = Namespace {
            favorites: [
                "ns9", "ns8", "ns7", "ns6", "ns5", "ns4", "ns3", "ns2", "ns1", "all", "default",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            ..Namespace::default()
        };
        ns.validate_with(&|_| true);
        assert_eq!(ns.favorites.len(), MAX_FAVORITES_NS);

        let mut ns = Namespace {
            favorites: vec!["default".into(), "fred".into()],
            ..Namespace::default()
        };
        ns.validate_with(&|_| true);
        assert_eq!(ns.favorites, vec!["default", "fred"]);
        ns.validate_with(&|n| n != "fred");
        assert_eq!(ns.favorites, vec!["default"]);
        // Unknown active namespace skips validation.
        ns.favorites = vec!["default".into(), "fred".into()];
        ns.active = "fred".into();
        ns.validate_with(&|n| n != "fred");
        assert_eq!(ns.favorites, vec!["default", "fred"]);
    }

    #[test]
    fn context_merge() {
        let mk = |active: &str, favs: &[&str], lock: bool| ContextConfig {
            namespace: Namespace {
                active: active.into(),
                lock_favorites: lock,
                favorites: favs.iter().map(|s| s.to_string()).collect(),
            },
            ..ContextConfig::default()
        };
        let mut c1 = mk("ns1", &["ns1", "ns2", "ns3"], false);
        c1.merge(&mk("ns10", &["ns10", "ns11", "ns12"], false));
        assert_eq!(
            c1,
            mk("ns1", &["ns1", "ns2", "ns3", "ns10", "ns11", "ns12"], false)
        );

        let mut c1 = mk("ns1", &["ns1", "ns2", "ns3"], true);
        c1.merge(&mk("ns10", &["ns10", "ns11", "ns12"], false));
        assert_eq!(c1, mk("ns1", &["ns1", "ns2", "ns3"], true));

        let all9 = [
            "ns1", "ns2", "ns3", "ns4", "ns5", "ns6", "ns7", "ns8", "ns9",
        ];
        let mut c1 = mk("ns1", &all9, false);
        c1.merge(&mk("ns10", &["ns10", "ns11", "ns12"], false));
        assert_eq!(c1, mk("ns1", &all9, false));

        let mut c1 = ContextConfig::default();
        c1.merge(&ContextConfig {
            namespace: Namespace::zero(),
            ..ContextConfig::default()
        });
        assert_eq!(c1, ContextConfig::default());
    }

    #[test]
    fn context_validate() {
        let mut c = ContextConfig::default();
        c.validate_with(&MapEnv::default(), "cl-1");
        assert_eq!(c.cluster, "cl-1");
        assert_eq!(c.view.active, "po");
        assert_eq!(c.namespace.active, "default");
        assert_eq!(c.namespace.favorites, vec!["default"]);
        c.validate_with(&MapEnv::new(&[(ENV_FG_NODE_SHELL, "true")]), "cl-1");
        assert!(c.feature_gates.node_shell);
        c.validate_with(&MapEnv::new(&[(ENV_FG_NODE_SHELL, "blee")]), "cl-1");
        assert!(!c.feature_gates.node_shell);
        let mut v = View::zero();
        v.validate();
        assert_eq!(v.active, "po");
        v.active = "fred".into();
        v.validate();
        assert_eq!(v.active, "fred");
    }

    fn must_load(p: &str) -> ContextConfig {
        ContextConfig::parse(&std::fs::read_to_string(td(p)).unwrap()).unwrap()
    }

    #[test]
    fn dir_load() {
        // TestDirLoad.
        let root = td("data/data/k9s");
        for (cl, ct, f) in [
            ("cl-1", "ct-1-1", "data/configs/ct-1-1.yaml"),
            ("cl-1", "ct-1-2", "data/configs/ct-1-2.yaml"),
            ("cl-2", "ct-2-1", "data/configs/ct-2-1.yaml"),
        ] {
            assert_eq!(
                ContextConfig::load_dir(&root, cl, ct, "").unwrap(),
                must_load(f),
                "{ct}"
            );
        }
        let tmp = tempfile::tempdir().unwrap();
        let c = ContextConfig::load_dir(tmp.path(), "cl-test", "ct-test-1", "").unwrap();
        assert_eq!(c, must_load("data/configs/def_ct.yaml"));
        let p = tmp.path().join("cl-test/ct-test-1/config.yaml");
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "k9s:\n  cluster: cl-test\n  namespace:\n    active: default\n    lockFavorites: false\n    favorites:\n      - default\n  view:\n    active: po\n  featureGates:\n    nodeShell: false\n  proxy: null\n"
        );
        let aws = "arn:aws:eks:eu-central-1:xxx:cluster/fred-blee";
        let c = ContextConfig::load_dir(tmp.path(), aws, "fred-blee", "").unwrap();
        assert_eq!(c, must_load("data/configs/aws_ct.yaml"));
        assert!(tmp
            .path()
            .join("arn-aws-eks-eu-central-1-xxx-cluster-fred-blee/fred-blee/config.yaml")
            .exists());
        // Empty file regenerates with the kube namespace.
        let p = tmp.path().join("cl-x/ct-x/config.yaml");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "").unwrap();
        let c = ContextConfig::load_dir(tmp.path(), "cl-x", "ct-x", "ns-1").unwrap();
        assert_eq!(c.namespace.active, "ns-1");
        assert_eq!(c.namespace.favorites, vec!["default"]);
    }

    #[test]
    fn activate_and_save() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ContextConfig::activate(tmp.path(), "cl-1", "ct-1", "kube-ns").unwrap();
        assert_eq!(c.cluster, "cl-1");
        assert_eq!(c.namespace.active, "kube-ns");
        let mut c2 = c.clone();
        c2.set_active_namespace("ns-2");
        c2.set_active_namespace("*");
        assert_eq!(c2.namespace.active, "ns-2");
        let p = tmp.path().join("cl-1/ct-1/config.yaml");
        // Another k9s saved extra favorites meanwhile: they are merged in.
        let mut other = ContextConfig::read(&p).unwrap();
        other.namespace.favorites = vec!["other".into()];
        std::fs::write(
            &p,
            yaml::to_string(&ContextFile { k9s: Some(other) }).unwrap(),
        )
        .unwrap();
        c2.save_to(&p).unwrap();
        let back = ContextConfig::read(&p).unwrap();
        assert_eq!(back.namespace.active, "ns-2");
        assert_eq!(back.namespace.favorites, vec!["ns-2", "default", "other"]);
        // A context with a skin, readOnly and proxy round trips.
        let mut s = back.clone();
        s.skin = Some("dracula".into());
        s.read_only = Some(true);
        s.proxy = Some(Proxy {
            address: "http://p:3128".into(),
        });
        let q = tmp.path().join("q.yaml");
        s.save_to(&q).unwrap();
        let txt = std::fs::read_to_string(&q).unwrap();
        assert!(
            txt.starts_with("k9s:\n  cluster: cl-1\n  readOnly: true\n  skin: dracula\n"),
            "{txt}"
        );
        assert!(
            txt.ends_with("  proxy:\n    address: http://p:3128\n"),
            "{txt}"
        );
        assert_eq!(ContextConfig::read(&q).unwrap(), s);
        assert!(json::validate(json::CONTEXT_SCHEMA, &txt).is_ok());
    }

    #[test]
    fn context_parse_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("bad.yaml");
        std::fs::write(&p, "k9s:\n  namespace: [\n").unwrap();
        let e = ContextConfig::read(&p).unwrap_err().to_string();
        assert!(e.starts_with("context-config yaml load failed: "), "{e}");
        assert!(e.ends_with("k9s:\n  namespace: [\n"));
        // No root: defaults.
        assert_eq!(
            ContextConfig::parse("fred: 1\n").unwrap(),
            ContextConfig::default()
        );
        // Partial namespace keeps Go zero values.
        let c = ContextConfig::parse("k9s:\n  namespace:\n    active: blee\n").unwrap();
        assert!(c.namespace.favorites.is_empty());
        let c = ContextConfig::parse("k9s:\n  namespace:\n  view:\n").unwrap();
        assert_eq!(c.namespace, Namespace::default());
        assert_eq!(c.view.active, "po");
    }

    #[test]
    fn view_cmd_ns() {
        assert_eq!(view_cmd_has_ns("po"), ("po".into(), false));
        assert_eq!(view_cmd_has_ns("PO fred"), ("po".into(), true));
        assert_eq!(view_cmd_has_ns("po 'app in (a,b)'"), ("po".into(), false));
        assert_eq!(view_cmd_has_ns("po -f blee"), ("po".into(), false));
        assert_eq!(view_cmd_has_ns("dir /tmp"), ("dir".into(), false));
        assert_eq!(view_cmd_has_ns(""), ("".into(), false));
    }
}
