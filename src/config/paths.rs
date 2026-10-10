//! Config file locations (config/files.go + config/data/helpers.go).
//!
//! - `$K9S_CONFIG_DIR` set: everything lives under it (`config.yaml`,
//!   `skins/`, `clusters/`, `screen-dumps/`, `benchmarks/`).
//! - otherwise XDG dirs (adrg/xdg semantics): config in
//!   `$XDG_CONFIG_HOME/k9s` (`~/.config` on Linux,
//!   `~/Library/Application Support` on macOS), context configs in
//!   `$XDG_DATA_HOME/k9s/clusters`, screen dumps and benchmarks in
//!   `$XDG_STATE_HOME/k9s`.
//! - logs: `$K9S_LOGS_DIR`, else `<tmp>/<user>/k9s` when `$K9S_CONFIG_DIR` is
//!   set, else `$XDG_STATE_HOME/k9s`; the file is `k9s.log`.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Result;

/// k9s configuration dir env var.
pub const K9S_ENV_CONFIG_DIR: &str = "K9S_CONFIG_DIR";
/// k9s logs dir env var.
pub const K9S_ENV_LOGS_DIR: &str = "K9S_LOGS_DIR";
/// App name used for dir names.
pub const APP_NAME: &str = "k9s";
/// Log file name.
pub const K9S_LOGS_FILE: &str = "k9rs.log";
/// Main config file name (data.MainConfigFile).
pub const MAIN_CONFIG_FILE: &str = "config.yaml";
/// Default dir permissions (data.DefaultDirMod).
pub const DEFAULT_DIR_MOD: u32 = 0o744;
/// Default file permissions (data.DefaultFileMod).
pub const DEFAULT_FILE_MOD: u32 = 0o600;

const BENCHMARK_TPL: &str = include_str!("templates/benchmarks.yaml");
const ALIASES_TPL: &str = include_str!("templates/aliases.yaml");
const HOTKEYS_TPL: &str = include_str!("templates/hotkeys.yaml");
/// The stock skin template (templates/stock-skin.yaml).
pub const STOCK_SKIN_TPL: &str = include_str!("templates/stock-skin.yaml");

/// Environment lookup, so path resolution is testable without touching the
/// process environment.
pub trait Env {
    fn get(&self, key: &str) -> Option<String>;
    /// Non-empty value (k9s isEnvSet).
    fn get_set(&self, key: &str) -> Option<String> {
        self.get(key).filter(|v| !v.is_empty())
    }
}

/// The real process environment.
pub struct ProcessEnv;

impl Env for ProcessEnv {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

/// A fixed environment (tests).
#[derive(Clone, Debug, Default)]
pub struct MapEnv(pub BTreeMap<String, String>);

impl MapEnv {
    pub fn new(pairs: &[(&str, &str)]) -> MapEnv {
        MapEnv(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }
}

impl Env for MapEnv {
    fn get(&self, key: &str) -> Option<String> {
        self.0.get(key).cloned()
    }
}

/// XDG base dirs (adrg/xdg v0.5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Xdg {
    pub config_home: PathBuf,
    pub data_home: PathBuf,
    pub state_home: PathBuf,
    pub cache_home: PathBuf,
    pub data_dirs: Vec<PathBuf>,
    pub config_dirs: Vec<PathBuf>,
}

fn expand_home(p: &str, home: &Path) -> PathBuf {
    if p.is_empty() || home.as_os_str().is_empty() {
        return PathBuf::from(p);
    }
    if let Some(rest) = p.strip_prefix('~') {
        return home.join(rest.trim_start_matches('/'));
    }
    if let Some(rest) = p.strip_prefix("$HOME") {
        return home.join(rest.trim_start_matches('/'));
    }
    PathBuf::from(p)
}

fn env_path(env: &dyn Env, name: &str, home: &Path, default: PathBuf) -> PathBuf {
    if let Some(v) = env.get_set(name) {
        let p = expand_home(&v, home);
        if p.is_absolute() {
            return p;
        }
    }
    default
}

fn env_path_list(env: &dyn Env, name: &str, home: &Path, defaults: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(v) = env.get_set(name) {
        for p in v.split(':') {
            let p = expand_home(p, home);
            if p.is_absolute() && !out.contains(&p) {
                out.push(p);
            }
        }
    }
    if out.is_empty() {
        defaults
    } else {
        out
    }
}

impl Xdg {
    /// Resolve the XDG dirs for `home` from `env`. `macos` selects the
    /// darwin defaults.
    pub fn resolve(env: &dyn Env, home: &Path, macos: bool) -> Xdg {
        if macos {
            let app_support = home.join("Library").join("Application Support");
            let root_app_support = PathBuf::from("/Library/Application Support");
            Xdg {
                data_home: env_path(env, "XDG_DATA_HOME", home, app_support.clone()),
                config_home: env_path(env, "XDG_CONFIG_HOME", home, app_support.clone()),
                state_home: env_path(env, "XDG_STATE_HOME", home, app_support),
                cache_home: env_path(
                    env,
                    "XDG_CACHE_HOME",
                    home,
                    home.join("Library").join("Caches"),
                ),
                data_dirs: env_path_list(
                    env,
                    "XDG_DATA_DIRS",
                    home,
                    vec![root_app_support.clone()],
                ),
                config_dirs: env_path_list(
                    env,
                    "XDG_CONFIG_DIRS",
                    home,
                    vec![
                        home.join("Library").join("Preferences"),
                        root_app_support,
                        PathBuf::from("/Library/Preferences"),
                    ],
                ),
            }
        } else {
            Xdg {
                data_home: env_path(
                    env,
                    "XDG_DATA_HOME",
                    home,
                    home.join(".local").join("share"),
                ),
                config_home: env_path(env, "XDG_CONFIG_HOME", home, home.join(".config")),
                state_home: env_path(
                    env,
                    "XDG_STATE_HOME",
                    home,
                    home.join(".local").join("state"),
                ),
                cache_home: env_path(env, "XDG_CACHE_HOME", home, home.join(".cache")),
                data_dirs: env_path_list(
                    env,
                    "XDG_DATA_DIRS",
                    home,
                    vec![
                        PathBuf::from("/usr/local/share"),
                        PathBuf::from("/usr/share"),
                    ],
                ),
                config_dirs: env_path_list(
                    env,
                    "XDG_CONFIG_DIRS",
                    home,
                    vec![PathBuf::from("/etc/xdg")],
                ),
            }
        }
    }

    /// XDG dirs for the running process.
    pub fn current() -> Xdg {
        Xdg::resolve(
            &ProcessEnv,
            &home_dir(&ProcessEnv),
            cfg!(target_os = "macos"),
        )
    }
}

/// User home dir ($HOME, else the passwd entry).
pub fn home_dir(env: &dyn Env) -> PathBuf {
    if let Some(h) = env.get_set("HOME") {
        return PathBuf::from(h);
    }
    dirs::home_dir().unwrap_or_default()
}

/// All k9s artifact locations (files.go App* vars after InitLocs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Locations {
    pub config_dir: PathBuf,
    pub config_file: PathBuf,
    pub hotkeys_file: PathBuf,
    pub aliases_file: PathBuf,
    pub plugins_file: PathBuf,
    pub views_file: PathBuf,
    pub skins_dir: PathBuf,
    pub dumps_dir: PathBuf,
    pub benchmarks_dir: PathBuf,
    pub contexts_dir: PathBuf,
    /// The XDG dirs these locations were derived from (plugins dirs).
    pub xdg: Xdg,
}

impl Locations {
    /// Resolve locations (no filesystem access).
    pub fn resolve(env: &dyn Env, xdg: &Xdg) -> Locations {
        if let Some(dir) = env.get_set(K9S_ENV_CONFIG_DIR) {
            let cfg = PathBuf::from(dir);
            return Locations {
                config_file: cfg.join(MAIN_CONFIG_FILE),
                hotkeys_file: cfg.join("hotkeys.yaml"),
                aliases_file: cfg.join("aliases.yaml"),
                plugins_file: cfg.join("plugins.yaml"),
                views_file: cfg.join("views.yaml"),
                skins_dir: cfg.join("skins"),
                dumps_dir: cfg.join("screen-dumps"),
                benchmarks_dir: cfg.join("benchmarks"),
                contexts_dir: cfg.join("clusters"),
                config_dir: cfg,
                xdg: xdg.clone(),
            };
        }
        let cfg = xdg.config_home.join(APP_NAME);
        let state = xdg.state_home.join(APP_NAME);
        Locations {
            config_file: cfg.join(MAIN_CONFIG_FILE),
            hotkeys_file: cfg.join("hotkeys.yaml"),
            aliases_file: cfg.join("aliases.yaml"),
            plugins_file: cfg.join("plugins.yaml"),
            views_file: cfg.join("views.yaml"),
            skins_dir: cfg.join("skins"),
            dumps_dir: state.join("screen-dumps"),
            benchmarks_dir: state.join("benchmarks"),
            contexts_dir: xdg.data_home.join(APP_NAME).join("clusters"),
            config_dir: cfg,
            xdg: xdg.clone(),
        }
    }

    /// Locations for the running process.
    pub fn current() -> Locations {
        Locations::resolve(&ProcessEnv, &Xdg::current())
    }

    /// Create the directories k9s creates on startup (InitLocs).
    pub fn ensure(&self, env: &dyn Env) -> Result<()> {
        if env.get_set(K9S_ENV_CONFIG_DIR).is_some() {
            ensure_full_path(&self.config_dir)?;
            for d in [
                &self.dumps_dir,
                &self.benchmarks_dir,
                &self.skins_dir,
                &self.contexts_dir,
            ] {
                if let Err(e) = ensure_full_path(d) {
                    tracing::warn!(dir = %d.display(), error = %e, "Unable to create dir");
                }
            }
            return Ok(());
        }
        // xdg.ConfigFile/StateFile/DataFile create the parent dirs.
        ensure_full_path(&self.config_dir)?;
        if let Err(e) = ensure_full_path(&self.skins_dir) {
            tracing::warn!(error = %e, "No skins dir detected");
        }
        ensure_dir_path(&self.dumps_dir)?;
        if let Err(e) = ensure_dir_path(&self.benchmarks_dir) {
            tracing::warn!(dir = %self.benchmarks_dir.display(), error = %e, "No benchmarks dir detected");
        }
        if let Err(e) = ensure_full_path(&self.contexts_dir) {
            tracing::warn!(dir = %self.contexts_dir.display(), error = %e, "No context dir detected");
        }
        Ok(())
    }

    /// `clusters/<cluster>/<context>` (AppContextDir).
    pub fn context_dir(&self, cluster: &str, context: &str) -> PathBuf {
        self.contexts_dir
            .join(sanitize_context_subpath(cluster, context))
    }
    /// Context specific config file (AppContextConfig).
    pub fn context_config(&self, cluster: &str, context: &str) -> PathBuf {
        self.context_dir(cluster, context).join(MAIN_CONFIG_FILE)
    }
    /// Context specific aliases file.
    pub fn context_aliases_file(&self, cluster: &str, context: &str) -> PathBuf {
        self.context_dir(cluster, context).join("aliases.yaml")
    }
    /// Context specific plugins file.
    pub fn context_plugins_file(&self, cluster: &str, context: &str) -> PathBuf {
        self.context_dir(cluster, context).join("plugins.yaml")
    }
    /// Context specific hotkeys file.
    pub fn context_hotkeys_file(&self, cluster: &str, context: &str) -> PathBuf {
        self.context_dir(cluster, context).join("hotkeys.yaml")
    }
    /// Per context screen dumps dir, created if missing (DumpsDir).
    pub fn context_dumps_dir(&self, cluster: &str, context: &str) -> Result<PathBuf> {
        let dir = self
            .dumps_dir
            .join(sanitize_context_subpath(cluster, context));
        ensure_dir_path(&dir)?;
        Ok(dir)
    }
    /// Per context benchmarks dir (EnsureBenchmarksDir).
    pub fn ensure_benchmarks_dir(&self, cluster: &str, context: &str) -> Result<PathBuf> {
        let dir = self
            .benchmarks_dir
            .join(sanitize_context_subpath(cluster, context));
        ensure_dir_path(&dir)?;
        Ok(dir)
    }
    /// Context benchmarks.yaml, written from the template if missing.
    pub fn ensure_benchmarks_cfg_file(&self, cluster: &str, context: &str) -> Result<PathBuf> {
        let f = self.context_dir(cluster, context).join("benchmarks.yaml");
        ensure_template(&f, BENCHMARK_TPL)?;
        Ok(f)
    }
    /// Global aliases.yaml, written from the template if missing.
    pub fn ensure_aliases_cfg_file(&self) -> Result<PathBuf> {
        let f = self.config_dir.join("aliases.yaml");
        ensure_template(&f, ALIASES_TPL)?;
        Ok(f)
    }
    /// Global hotkeys.yaml, written from the template if missing.
    pub fn ensure_hotkeys_cfg_file(&self) -> Result<PathBuf> {
        let f = self.config_dir.join("hotkeys.yaml");
        ensure_template(&f, HOTKEYS_TPL)?;
        Ok(f)
    }
    /// Skin file for a skin name (`stock` when empty).
    pub fn skin_file_from_name(&self, name: &str) -> PathBuf {
        let n = if name.is_empty() { "stock" } else { name };
        self.skins_dir.join(format!("{n}.yaml"))
    }
    /// XDG plugin dirs scanned for extra plugins (`<dir>/k9s/plugins`).
    pub fn plugin_dirs(&self) -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = self.xdg.data_dirs.clone();
        dirs.push(self.xdg.data_home.clone());
        dirs.push(self.xdg.config_home.clone());
        dirs.into_iter()
            .map(|d| d.join("k9s").join("plugins"))
            .collect()
    }
}

fn ensure_template(f: &Path, tpl: &str) -> Result<()> {
    ensure_dir_path(f)?;
    if !f.exists() {
        write_file(f, tpl.as_bytes())?;
    }
    Ok(())
}

/// `<tmp>/<user>/k9s` (UserTmpDir).
pub fn user_tmp_dir(env: &dyn Env) -> PathBuf {
    let tmp = env.get_set("TMPDIR").unwrap_or_else(|| "/tmp".into());
    PathBuf::from(tmp).join(must_k9s_user(env)).join(APP_NAME)
}

/// Current user name (MustK9sUser): passwd entry, else `$USER`, `$LOGNAME`.
pub fn must_k9s_user(env: &dyn Env) -> String {
    if let Some(u) = passwd_user() {
        return u;
    }
    env.get_set("USER")
        .or_else(|| env.get_set("LOGNAME"))
        .unwrap_or_default()
}

#[cfg(unix)]
fn passwd_user() -> Option<String> {
    // SAFETY: getpwuid_r writes into the provided buffers only; the returned
    // pw_name points into `buf`, which outlives the CStr read below.
    unsafe {
        let uid = libc::getuid();
        let mut pwd: libc::passwd = std::mem::zeroed();
        let mut buf = vec![0 as libc::c_char; 16 * 1024];
        let mut res: *mut libc::passwd = std::ptr::null_mut();
        let rc = libc::getpwuid_r(uid, &mut pwd, buf.as_mut_ptr(), buf.len(), &mut res);
        if rc != 0 || res.is_null() || pwd.pw_name.is_null() {
            return None;
        }
        let name = std::ffi::CStr::from_ptr(pwd.pw_name)
            .to_string_lossy()
            .into_owned();
        (!name.is_empty()).then_some(name)
    }
}

#[cfg(not(unix))]
fn passwd_user() -> Option<String> {
    None
}

/// Log dir (InitLogLoc), without creating it.
pub fn resolve_log_dir(env: &dyn Env, xdg: &Xdg) -> PathBuf {
    if let Some(d) = env.get_set(K9S_ENV_LOGS_DIR) {
        return PathBuf::from(d);
    }
    if env.get_set(K9S_ENV_CONFIG_DIR).is_some() {
        return user_tmp_dir(env);
    }
    xdg.state_home.join(APP_NAME)
}

/// Log file path for `env`/`xdg` (no filesystem access).
pub fn resolve_log_file(env: &dyn Env, xdg: &Xdg) -> PathBuf {
    resolve_log_dir(env, xdg).join(K9S_LOGS_FILE)
}

/// Create the log dir and return the log file path (InitLogLoc).
pub fn init_log_loc() -> Result<PathBuf> {
    let dir = resolve_log_dir(&ProcessEnv, &Xdg::current());
    ensure_full_path(&dir)?;
    Ok(dir.join(K9S_LOGS_FILE))
}

/// Root config dir.
pub fn config_dir() -> PathBuf {
    Locations::current().config_dir
}
/// `<config>/config.yaml`.
pub fn config_file() -> PathBuf {
    Locations::current().config_file
}
/// `<config>/skins`.
pub fn skins_dir() -> PathBuf {
    Locations::current().skins_dir
}
/// Context configs dir (`$XDG_DATA_HOME/k9s/clusters` or `<config>/clusters`).
pub fn contexts_dir() -> PathBuf {
    Locations::current().contexts_dir
}
/// Log file path (`k9s.log` in the logs dir).
pub fn log_file() -> PathBuf {
    resolve_log_file(&ProcessEnv, &Xdg::current())
}
/// Screen dumps dir (`$XDG_STATE_HOME/k9s/screen-dumps` or `<config>/screen-dumps`).
pub fn dumps_dir() -> PathBuf {
    Locations::current().dumps_dir
}
/// Benchmarks dir.
pub fn benchmarks_dir() -> PathBuf {
    Locations::current().benchmarks_dir
}
/// `<config>/views.yaml`.
pub fn views_file() -> PathBuf {
    Locations::current().views_file
}
/// `<config>/plugins.yaml`.
pub fn plugins_file() -> PathBuf {
    Locations::current().plugins_file
}
/// `<config>/hotkeys.yaml`.
pub fn hotkeys_file() -> PathBuf {
    Locations::current().hotkeys_file
}
/// `<config>/aliases.yaml`.
pub fn aliases_file() -> PathBuf {
    Locations::current().aliases_file
}

/// Replace `:` and `/` runs with `-` (data.SanitizeFileName).
pub fn sanitize_file_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut in_run = false;
    for c in name.chars() {
        if c == ':' || c == '/' {
            if !in_run {
                out.push('-');
            }
            in_run = true;
        } else {
            in_run = false;
            out.push(c);
        }
    }
    out
}

/// `<cluster>/<context>` with both sanitized (data.SanitizeContextSubpath).
pub fn sanitize_context_subpath(cluster: &str, context: &str) -> PathBuf {
    let c = sanitize_file_name(cluster);
    let x = sanitize_file_name(context);
    // filepath.Join drops empty elements.
    match (c.is_empty(), x.is_empty()) {
        (true, true) => PathBuf::new(),
        (true, false) => PathBuf::from(x),
        (false, true) => PathBuf::from(c),
        (false, false) => PathBuf::from(c).join(x),
    }
}

/// Create `path` (and parents) if missing (data.EnsureFullPath).
pub fn ensure_full_path(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty() || path.exists() {
        return Ok(());
    }
    mkdir_all(path)
}

/// os.MkdirAll, with Go's `mkdir <path>: <errno>` errors.
fn mkdir_all(path: &Path) -> Result<()> {
    let go_err =
        |e: &std::io::Error| anyhow::anyhow!(crate::cli::info::go_path_error("mkdir", path, e));
    if let Ok(m) = std::fs::metadata(path) {
        if m.is_dir() {
            return Ok(());
        }
        return Err(anyhow::anyhow!("mkdir {}: not a directory", path.display()));
    }
    if let Some(p) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        mkdir_all(p)?;
    }
    let mut b = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(DEFAULT_DIR_MOD);
    }
    match b.create(path) {
        Ok(()) => Ok(()),
        Err(_) if path.is_dir() => Ok(()),
        Err(e) => Err(go_err(&e)),
    }
}

/// Create the parent dir of `path` if missing (data.EnsureDirPath).
pub fn ensure_dir_path(path: &Path) -> Result<()> {
    match path.parent() {
        Some(p) => ensure_full_path(p),
        None => Ok(()),
    }
}

/// Write a file with k9s's default permissions (0600), truncating it.
pub fn write_file(path: &Path, bb: &[u8]) -> Result<()> {
    let mut o = std::fs::OpenOptions::new();
    o.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(DEFAULT_FILE_MOD);
    }
    let mut f = o.open(path)?;
    f.write_all(bb)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mkdir_errors_like_go() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("file");
        std::fs::write(&f, "x").unwrap();
        let e = ensure_full_path(&f.join("a/b")).unwrap_err().to_string();
        assert_eq!(e, format!("mkdir {}: not a directory", f.display()));
        let d = tmp.path().join("x/y/z");
        ensure_full_path(&d).unwrap();
        assert!(d.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let ro = tmp.path().join("ro");
            std::fs::create_dir(&ro).unwrap();
            std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o500)).unwrap();
            let e = ensure_dir_path(&ro.join("dumps/x"))
                .unwrap_err()
                .to_string();
            assert_eq!(
                e,
                format!("mkdir {}: permission denied", ro.join("dumps").display())
            );
        }
    }

    fn home() -> PathBuf {
        PathBuf::from("/home/fred")
    }

    #[test]
    fn sanitize() {
        for (f, e) in [
            ("", ""),
            ("bumble-bee-tuna", "bumble-bee-tuna"),
            ("bumble/bee/tuna", "bumble-bee-tuna"),
            ("bumble::bee:tuna", "bumble-bee-tuna"),
            (
                "arn:aws:eks:us-east-1:123456789:cluster/us-east-1-app-dev-common-eks",
                "arn-aws-eks-us-east-1-123456789-cluster-us-east-1-app-dev-common-eks",
            ),
            ("a:/b", "a-b"),
        ] {
            assert_eq!(sanitize_file_name(f), e);
        }
        assert_eq!(
            sanitize_context_subpath(
                "arn:aws:eks:eu-central-1:xxx:cluster/fred-blee",
                "fred-blee"
            ),
            PathBuf::from("arn-aws-eks-eu-central-1-xxx-cluster-fred-blee/fred-blee")
        );
    }

    #[test]
    fn xdg_linux_defaults() {
        let x = Xdg::resolve(&MapEnv::default(), &home(), false);
        assert_eq!(x.config_home, PathBuf::from("/home/fred/.config"));
        assert_eq!(x.data_home, PathBuf::from("/home/fred/.local/share"));
        assert_eq!(x.state_home, PathBuf::from("/home/fred/.local/state"));
        assert_eq!(
            x.data_dirs,
            vec![
                PathBuf::from("/usr/local/share"),
                PathBuf::from("/usr/share")
            ]
        );
        let l = Locations::resolve(&MapEnv::default(), &x);
        assert_eq!(l.config_dir, PathBuf::from("/home/fred/.config/k9s"));
        assert_eq!(
            l.config_file,
            PathBuf::from("/home/fred/.config/k9s/config.yaml")
        );
        assert_eq!(l.skins_dir, PathBuf::from("/home/fred/.config/k9s/skins"));
        assert_eq!(
            l.dumps_dir,
            PathBuf::from("/home/fred/.local/state/k9s/screen-dumps")
        );
        assert_eq!(
            l.benchmarks_dir,
            PathBuf::from("/home/fred/.local/state/k9s/benchmarks")
        );
        assert_eq!(
            l.contexts_dir,
            PathBuf::from("/home/fred/.local/share/k9s/clusters")
        );
        assert_eq!(
            resolve_log_file(&MapEnv::default(), &x),
            PathBuf::from("/home/fred/.local/state/k9s/k9rs.log")
        );
    }

    #[test]
    fn xdg_macos_defaults() {
        let x = Xdg::resolve(&MapEnv::default(), &home(), true);
        let sup = PathBuf::from("/home/fred/Library/Application Support");
        assert_eq!(x.config_home, sup);
        assert_eq!(x.data_home, sup);
        assert_eq!(x.state_home, sup);
        let l = Locations::resolve(&MapEnv::default(), &x);
        assert_eq!(l.config_dir, sup.join("k9s"));
        assert_eq!(l.contexts_dir, sup.join("k9s/clusters"));
        assert_eq!(l.dumps_dir, sup.join("k9s/screen-dumps"));
        assert_eq!(
            resolve_log_file(&MapEnv::default(), &x),
            sup.join("k9s/k9rs.log")
        );
        assert_eq!(
            l.plugin_dirs(),
            vec![
                PathBuf::from("/Library/Application Support/k9s/plugins"),
                sup.join("k9s/plugins"),
                sup.join("k9s/plugins"),
            ]
        );
    }

    #[test]
    fn xdg_env() {
        // Test_initXDGLocs.
        let tmp = "/tmp/fred/k9s";
        let env = MapEnv::new(&[
            ("XDG_CONFIG_HOME", "/tmp/fred/k9s/k9s-xdg/config"),
            ("XDG_CACHE_HOME", "/tmp/fred/k9s/k9s-xdg/cache"),
            ("XDG_STATE_HOME", "/tmp/fred/k9s/k9s-xdg/state"),
            ("XDG_DATA_HOME", "/tmp/fred/k9s/k9s-xdg/data"),
            ("XDG_DATA_DIRS", "relative:/opt/share:~/share"),
        ]);
        let x = Xdg::resolve(&env, &home(), true);
        assert_eq!(
            x.data_dirs,
            vec![
                PathBuf::from("/opt/share"),
                PathBuf::from("/home/fred/share")
            ]
        );
        let l = Locations::resolve(&env, &x);
        let p = |s: &str| PathBuf::from(format!("{tmp}/k9s-xdg/{s}"));
        assert_eq!(l.config_dir, p("config/k9s"));
        assert_eq!(l.config_file, p("config/k9s/config.yaml"));
        assert_eq!(l.benchmarks_dir, p("state/k9s/benchmarks"));
        assert_eq!(l.contexts_dir, p("data/k9s/clusters"));
        assert_eq!(
            l.context_hotkeys_file("cl-1", "ct-1-1"),
            p("data/k9s/clusters/cl-1/ct-1-1/hotkeys.yaml")
        );
        assert_eq!(
            l.context_config("cl-1", "ct-1-1"),
            p("data/k9s/clusters/cl-1/ct-1-1/config.yaml")
        );
        assert_eq!(l.hotkeys_file, p("config/k9s/hotkeys.yaml"));
        // relative XDG values are ignored.
        let x = Xdg::resolve(
            &MapEnv::new(&[("XDG_CONFIG_HOME", "rel/dir")]),
            &home(),
            false,
        );
        assert_eq!(x.config_home, PathBuf::from("/home/fred/.config"));
        let x = Xdg::resolve(
            &MapEnv::new(&[("XDG_CONFIG_HOME", "~/cfg")]),
            &home(),
            false,
        );
        assert_eq!(x.config_home, PathBuf::from("/home/fred/cfg"));
    }

    #[test]
    fn k9s_config_dir_env() {
        let env = MapEnv::new(&[
            (K9S_ENV_CONFIG_DIR, "/tmp/test-config"),
            ("TMPDIR", "/var/tmp/"),
        ]);
        let x = Xdg::resolve(&env, &home(), false);
        let l = Locations::resolve(&env, &x);
        assert_eq!(l.config_dir, PathBuf::from("/tmp/test-config"));
        assert_eq!(l.dumps_dir, PathBuf::from("/tmp/test-config/screen-dumps"));
        assert_eq!(
            l.benchmarks_dir,
            PathBuf::from("/tmp/test-config/benchmarks")
        );
        assert_eq!(l.skins_dir, PathBuf::from("/tmp/test-config/skins"));
        assert_eq!(l.contexts_dir, PathBuf::from("/tmp/test-config/clusters"));
        assert_eq!(l.views_file, PathBuf::from("/tmp/test-config/views.yaml"));
        assert_eq!(
            l.plugins_file,
            PathBuf::from("/tmp/test-config/plugins.yaml")
        );
        assert_eq!(
            l.aliases_file,
            PathBuf::from("/tmp/test-config/aliases.yaml")
        );
        // TestInitLogLoc: cfg-env logs land in the user tmp dir.
        let user = must_k9s_user(&env);
        assert_eq!(
            resolve_log_file(&env, &x),
            PathBuf::from(format!("/var/tmp/{user}/k9s/k9rs.log"))
        );
    }

    #[test]
    fn log_locations() {
        let env = MapEnv::new(&[(K9S_ENV_LOGS_DIR, "/tmp/test/k9s/logs")]);
        let x = Xdg::resolve(&env, &home(), false);
        assert_eq!(
            resolve_log_file(&env, &x),
            PathBuf::from("/tmp/test/k9s/logs/k9rs.log")
        );
        let env = MapEnv::new(&[("XDG_STATE_HOME", "/tmp/test/xdg-state")]);
        let x = Xdg::resolve(&env, &home(), false);
        assert_eq!(
            resolve_log_file(&env, &x),
            PathBuf::from("/tmp/test/xdg-state/k9s/k9rs.log")
        );
        // Empty values count as unset.
        let env = MapEnv::new(&[(K9S_ENV_LOGS_DIR, ""), (K9S_ENV_CONFIG_DIR, "")]);
        let x = Xdg::resolve(&env, &home(), false);
        assert_eq!(
            resolve_log_file(&env, &x),
            PathBuf::from("/home/fred/.local/state/k9s/k9rs.log")
        );
    }

    #[test]
    fn ensure_and_templates() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("cfg");
        let env = MapEnv::new(&[(K9S_ENV_CONFIG_DIR, cfg.to_str().unwrap())]);
        let l = Locations::resolve(&env, &Xdg::resolve(&env, &home(), false));
        l.ensure(&env).unwrap();
        for d in [
            &l.config_dir,
            &l.dumps_dir,
            &l.benchmarks_dir,
            &l.skins_dir,
            &l.contexts_dir,
        ] {
            assert!(d.is_dir(), "{d:?}");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let m = std::fs::metadata(&l.config_dir)
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(m & 0o700, 0o700);
        }

        // TestEnsureBenchmarkCfg.
        std::fs::create_dir_all(l.contexts_dir.join("cl-1/ct-2")).unwrap();
        std::fs::write(l.contexts_dir.join("cl-1/ct-2/benchmarks.yaml"), "").unwrap();
        let f = l.ensure_benchmarks_cfg_file("cl-1", "ct-1").unwrap();
        assert_eq!(f, l.contexts_dir.join("cl-1/ct-1/benchmarks.yaml"));
        assert_eq!(
            std::fs::read_to_string(&f).unwrap(),
            "benchmarks:\n  defaults:\n    concurrency: 2\n    requests: 200"
        );
        let f = l.ensure_benchmarks_cfg_file("cl-1", "ct-2").unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let f = l.ensure_benchmarks_cfg_file("cl-1", "ct-1").unwrap();
            let m = std::fs::metadata(f).unwrap().permissions().mode();
            assert_eq!(m & 0o777, 0o600);
        }

        let hk = l.ensure_hotkeys_cfg_file().unwrap();
        assert_eq!(hk, cfg.join("hotkeys.yaml"));
        assert!(std::fs::read_to_string(hk).unwrap().starts_with("hotKeys:"));
        let al = l.ensure_aliases_cfg_file().unwrap();
        assert!(std::fs::read_to_string(al)
            .unwrap()
            .contains("dp: deployments"));

        let d = l.context_dumps_dir("cl-1", "ct-1-1").unwrap();
        assert_eq!(d, l.dumps_dir.join("cl-1/ct-1-1"));
        let b = l.ensure_benchmarks_dir("cl-1", "ct-1-1").unwrap();
        assert_eq!(b, l.benchmarks_dir.join("cl-1/ct-1-1"));
    }

    #[test]
    fn skin_file_from_name() {
        let env = MapEnv::new(&[(K9S_ENV_CONFIG_DIR, "/tmp/k9s-test")]);
        let l = Locations::resolve(&env, &Xdg::resolve(&env, &home(), false));
        assert_eq!(
            l.skin_file_from_name(""),
            PathBuf::from("/tmp/k9s-test/skins/stock.yaml")
        );
        assert_eq!(
            l.skin_file_from_name("fred-blee"),
            PathBuf::from("/tmp/k9s-test/skins/fred-blee.yaml")
        );
    }

    #[test]
    fn user_name() {
        assert!(!must_k9s_user(&MapEnv::new(&[("USER", "fred")])).is_empty());
    }
}
