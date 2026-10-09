//! Command line (k9s cmd/root.go, info.go, version.go): flags, `info`,
//! `version` and `help` subcommands, and bootstrapping (logging, config
//! load, connect, App::run).

pub mod flags;
pub mod info;
pub mod logging;

use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{anyhow, Result};

use crate::config::paths::{self, Locations, ProcessEnv};
use crate::config::{Aliases, Config, ContextConfig, CustomView, Flags, HotKeys, Plugins, Styles};

/// K9s is a CLI to view and manage your Kubernetes clusters.
///
/// Parsed with k9s (cobra/pflag) semantics by `flags::parse`; `Cli::parse()`
/// prints help or errors exactly like k9s and exits.
#[derive(Debug, Clone, PartialEq)]
pub struct Cli {
    pub command: Option<Sub>,
    /// `-h, --help`.
    pub help: bool,
    /// Specify the default refresh rate as a float (sec)
    pub refresh: f32,
    /// Specify a log level (error, warn, info, debug)
    pub log_level: String,
    /// Specify the log file
    pub log_file: Option<String>,
    /// Turn K9s header off
    pub headless: bool,
    /// Turn K9s logo off
    pub logoless: bool,
    /// Turn K9s crumbs off
    pub crumbsless: bool,
    /// Turn K9s splash screen off
    pub splashless: bool,
    /// Invert skin (dark to light, light to dark), preserving colors
    pub invert: bool,
    /// Launch K9s in all namespaces
    pub all_namespaces: bool,
    /// Overrides the default resource to load when the application launches
    pub command_override: Option<String>,
    /// Sets readOnly mode by overriding readOnly configuration setting
    pub readonly: bool,
    /// Sets write mode by overriding the readOnly configuration setting
    pub write: bool,
    /// Sets a path to a dir for a screen dumps
    pub screen_dump_dir: Option<String>,
    /// Path to the kubeconfig file to use for CLI requests
    pub kubeconfig: Option<String>,
    /// The length of time to wait before giving up on a single server request
    pub request_timeout: Option<String>,
    /// The name of the kubeconfig context to use
    pub context: Option<String>,
    /// The name of the kubeconfig cluster to use
    pub cluster: Option<String>,
    /// The name of the kubeconfig user to use
    pub user: Option<String>,
    /// If present, the namespace scope for this CLI request
    pub namespace: Option<String>,
    /// Username to impersonate for the operation
    pub impersonate: Option<String>,
    /// Group to impersonate for the operation
    pub impersonate_group: Vec<String>,
    /// If true, the server's caCertFile will not be checked for validity
    pub insecure_skip_tls_verify: bool,
    /// Path to a cert file for the certificate authority
    pub certificate_authority: Option<String>,
    /// Path to a client key file for TLS
    pub client_key: Option<String>,
    /// Path to a client certificate file for TLS
    pub client_certificate: Option<String>,
    /// Bearer token for authentication to the API server
    pub token: Option<String>,
}

impl Default for Cli {
    fn default() -> Self {
        Cli {
            command: None,
            help: false,
            refresh: 2.0,
            log_level: crate::config::DEFAULT_LOG_LEVEL.into(),
            log_file: None,
            headless: false,
            logoless: false,
            crumbsless: false,
            splashless: false,
            invert: false,
            all_namespaces: false,
            command_override: None,
            readonly: false,
            write: false,
            screen_dump_dir: None,
            kubeconfig: None,
            request_timeout: None,
            context: None,
            cluster: None,
            user: None,
            namespace: None,
            impersonate: None,
            impersonate_group: vec![],
            insecure_skip_tls_verify: false,
            certificate_authority: None,
            client_key: None,
            client_certificate: None,
            token: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Sub {
    /// List K9s configurations info
    Info,
    /// Print version/build info
    Version {
        /// Prints K9s version info in short format
        short: bool,
    },
}

fn default_log_file() -> String {
    paths::log_file().to_string_lossy().into_owned()
}

impl Cli {
    /// Parse arguments (without the program name) like k9s.
    pub fn parse_args(args: &[String]) -> flags::Parsed {
        flags::parse(args, &default_log_file())
    }

    /// Parse the process arguments; prints help/errors and exits like k9s.
    pub fn from_env() -> Cli {
        let args: Vec<String> = std::env::args().skip(1).collect();
        match Cli::parse_args(&args) {
            flags::Parsed::Run(c) => *c,
            flags::Parsed::Help(h) => {
                print!("{h}");
                std::process::exit(0)
            }
            flags::Parsed::Fail(e) => {
                eprint!("{e}");
                std::process::exit(1)
            }
        }
    }

    /// k9s flags (config.Flags) from the command line.
    pub fn k9s_flags(&self) -> Flags {
        Flags {
            refresh_rate: Some(self.refresh),
            log_level: Some(self.log_level.clone()),
            log_file: self.log_file.clone(),
            headless: Some(self.headless),
            logoless: Some(self.logoless),
            command: Some(self.command_override.clone().unwrap_or_default()),
            all_namespaces: Some(self.all_namespaces),
            read_only: Some(self.readonly),
            write: Some(self.write),
            crumbsless: Some(self.crumbsless),
            splashless: Some(self.splashless),
            invert: Some(self.invert),
            screen_dump_dir: self.screen_dump_dir.clone(),
        }
    }

    /// Connection options; `default_timeout` applies when `--request-timeout`
    /// is not given.
    pub fn connect_options(
        &self,
        default_timeout: Duration,
    ) -> Result<crate::k8s::client::ConnectOptions> {
        let request_timeout = match self.request_timeout.as_deref().filter(|s| !s.is_empty()) {
            Some(t) => parse_timeout(t)?,
            None => Some(default_timeout),
        };
        Ok(crate::k8s::client::ConnectOptions {
            kubeconfig: self.kubeconfig.clone(),
            context: self.context.clone(),
            cluster: self.cluster.clone(),
            user: self.user.clone(),
            namespace: self.namespace.clone(),
            request_timeout: request_timeout.filter(|d| !d.is_zero()),
            impersonate: self.impersonate.clone(),
            impersonate_groups: self.impersonate_group.clone(),
            insecure_skip_tls_verify: self.insecure_skip_tls_verify,
            certificate_authority: self.certificate_authority.clone(),
            client_key: self.client_key.clone(),
            client_certificate: self.client_certificate.clone(),
            token: self.token.clone(),
        })
    }
}

impl clap::FromArgMatches for Cli {
    fn from_arg_matches(_m: &clap::ArgMatches) -> std::result::Result<Self, clap::Error> {
        Ok(Cli::from_env())
    }
    fn update_from_arg_matches(
        &mut self,
        m: &clap::ArgMatches,
    ) -> std::result::Result<(), clap::Error> {
        *self = Cli::from_arg_matches(m)?;
        Ok(())
    }
}

impl clap::CommandFactory for Cli {
    /// A pass-through command: argument handling is done by `flags::parse`.
    fn command() -> clap::Command {
        clap::Command::new("k9s")
            .disable_help_flag(true)
            .disable_version_flag(true)
            .disable_help_subcommand(true)
            .arg(
                clap::Arg::new("args")
                    .num_args(0..)
                    .action(clap::ArgAction::Append)
                    .allow_hyphen_values(true)
                    .trailing_var_arg(true)
                    .value_parser(clap::value_parser!(std::ffi::OsString)),
            )
    }
    fn command_for_update() -> clap::Command {
        Self::command()
    }
}

impl clap::Parser for Cli {}

/// client-go request timeout: integer seconds or a Go duration; 0 = none.
pub fn parse_timeout(s: &str) -> Result<Option<Duration>> {
    if let Ok(i) = s.parse::<i64>() {
        if i >= 0 {
            return Ok(Some(Duration::from_secs(i as u64)));
        }
    }
    crate::config::parse_go_duration(s).map(Some).ok_or_else(|| {
        anyhow!("Invalid timeout value. Timeout must be a single integer in seconds, or an integer followed by a corresponding time unit (e.g. 1s | 2m | 3h)")
    })
}

/// Initial view command: `-c`, else k9s defaultView (stored as the context
/// active view, like k9s), else the context view.active, else `po`.
pub fn initial_command(cfg: &mut Config, ctx: &mut ContextConfig) -> String {
    if !cfg.k9s.default_view.is_empty() {
        ctx.view.active = cfg.k9s.default_view.clone();
    }
    let v = cfg.k9s.active_view(Some(ctx));
    if v.is_empty() {
        crate::config::DEFAULT_VIEW.into()
    } else {
        v
    }
}

/// Entry point: run the command line.
pub async fn run(cli: Cli) -> Result<()> {
    if cli.help {
        print!("{}", flags::root_help(&default_log_file()));
        return Ok(());
    }
    match cli.command {
        Some(Sub::Info) => {
            let locs = Locations::current();
            locs.ensure(&ProcessEnv)?;
            let log = paths::init_log_loc().unwrap_or_else(|e| {
                println!("Fail to init k9s logs location {e}");
                paths::log_file()
            });
            let dumps = info::screen_dump_dir_for_info(Some(&locs.config_file), &locs.dumps_dir);
            info::print_info(
                &mut std::io::stdout(),
                crate::version::VERSION,
                &locs,
                &log,
                &dumps,
            )?;
            Ok(())
        }
        Some(Sub::Version { short }) => {
            info::print_version(
                &mut std::io::stdout(),
                short,
                crate::version::VERSION,
                crate::version::COMMIT,
                crate::version::DATE,
            )?;
            Ok(())
        }
        None => {
            if let Err(e) = run_app(&cli).await {
                // cobra: error then usage, exit 1.
                eprint!("Error: {e:#}\n{}\n", flags::root_usage(&default_log_file()));
                std::process::exit(1);
            }
            Ok(())
        }
    }
}

async fn run_app(cli: &Cli) -> Result<()> {
    let locs = Locations::current();
    locs.ensure(&ProcessEnv)?;
    let log_file = match cli.log_file.as_deref().filter(|f| !f.is_empty()) {
        Some(f) => PathBuf::from(f),
        None => paths::init_log_loc().unwrap_or_else(|e| {
            println!("Fail to init k9s logs location {e}");
            paths::log_file()
        }),
    };
    logging::init(&log_file, &cli.log_level)?;
    tracing::info!("🐶 K9s starting up...");

    let (mut cfg, err) = Config::load_with(&locs)?;
    if let Some(e) = &err {
        tracing::warn!(error = %e, "Fail to load global/context configuration");
    }
    cfg.k9s.override_flags(&cli.k9s_flags());

    let opts = cli.connect_options(cfg.k9s.api_server_timeout())?;
    let wanted_ctx = cli.context.clone().unwrap_or_default();
    let accessor = crate::k8s::client::connect(opts).await?;
    // k9s starts without a connection and falls back to the context view.
    let connected = match accessor.check_connectivity().await {
        Ok(()) => {
            tracing::info!("✅ Kubernetes connectivity OK");
            true
        }
        Err(e) => {
            tracing::error!(error = %e, "Unable to fetch server version");
            tracing::warn!("💣 Kubernetes connectivity toast!");
            false
        }
    };

    // Config.Refine: activate the context and pick the namespace.
    let ctx_name = if wanted_ctx.is_empty() {
        accessor.active_context()
    } else {
        wanted_ctx.clone()
    };
    let contexts = accessor.contexts().await.unwrap_or_default();
    let Some(kctx) = contexts.iter().find(|c| c.name == ctx_name) else {
        let e =
            format!("unable to activate context {ctx_name:?}: no context found for: {ctx_name:?}");
        return Err(if wanted_ctx.is_empty() {
            anyhow!(e)
        } else {
            anyhow!("k8sflags. {e}")
        });
    };
    let mut ctx_cfg = ContextConfig::activate(
        &locs.contexts_dir,
        &kctx.cluster,
        &ctx_name,
        &kctx.namespace,
    )?;
    let ns = cfg.refine_namespace(&mut ctx_cfg, cli.all_namespaces, cli.namespace.as_deref());
    let dump_dir = cfg.k9s.app_screen_dump_dir_or(&locs.dumps_dir);
    if let Err(e) = paths::ensure_dir_path(&dump_dir) {
        tracing::error!(error = %e, "Fail to refine k9s config");
    }

    let mut command = initial_command(&mut cfg, &mut ctx_cfg);
    if !connected {
        command = "context".into();
    }

    let mut aliases = Aliases::load_with(
        &locs,
        Some(&locs.context_aliases_file(&kctx.cluster, &ctx_name)),
    )?;
    let resources = if connected {
        accessor.resources().await.unwrap_or_else(|e| {
            tracing::error!(error = %e, "Load cluster resources failed");
            vec![]
        })
    } else {
        vec![]
    };
    aliases.add_resources(&resources);
    let hotkeys = HotKeys::load_with(
        &locs,
        Some(&locs.context_hotkeys_file(&kctx.cluster, &ctx_name)),
    )
    .unwrap_or_else(|e| {
        tracing::warn!(error = %e, "HotKeys load failed");
        HotKeys::default()
    });
    let mut plugins = Plugins::default();
    if let Err(e) = plugins.load_all(
        &locs,
        Some(&locs.context_plugins_file(&kctx.cluster, &ctx_name)),
        true,
    ) {
        tracing::warn!(error = %e, "Plugins load failed");
    }
    let views = CustomView::load_from(&locs.views_file).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "Custom view load failed");
        CustomView::default()
    });
    let styles = Styles::load_active(
        &locs,
        &ProcessEnv,
        &cfg.k9s.ui.skin,
        ctx_cfg.skin_name(),
        cfg.k9s.is_invert(),
    );

    let app_opts = crate::ui::app::AppOptions {
        command,
        headless: cfg.k9s.is_headless(),
        logoless: cfg.k9s.is_logoless(),
        crumbsless: cfg.k9s.is_crumbsless(),
        splashless: cfg.k9s.is_splashless(),
        readonly: cfg.k9s.is_read_only(Some(&ctx_cfg)),
        refresh_rate: cfg.k9s.get_refresh_rate(),
        disconnected: !connected,
    };
    if let Err(e) = ctx_cfg.save_to(&locs.context_config(&kctx.cluster, &ctx_name)) {
        tracing::error!(error = %e, "K9s config save failed");
    }

    let (has_metrics, cluster_info, namespaces) = if connected {
        (
            accessor.has_metrics().await,
            accessor.cluster_info().await.unwrap_or_default(),
            accessor.namespaces().await.unwrap_or_default(),
        )
    } else {
        Default::default()
    };
    // Views send AppEvents through ctx.sender(); the app reads them from rx.
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let mut ctx = crate::ui::Ctx::new(accessor, Arc::new(RwLock::new(cfg)), styles, ns, tx);
    ctx.readonly = app_opts.readonly;
    ctx.aliases = aliases;
    ctx.hotkeys = hotkeys;
    ctx.plugins = plugins;
    ctx.views = views;
    ctx.resources = resources;
    ctx.namespaces = namespaces;
    ctx.cluster_info = cluster_info;
    ctx.has_metrics = has_metrics;
    crate::ui::App::with_receiver(ctx, app_opts, rx).run().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeouts() {
        assert_eq!(parse_timeout("0").unwrap(), Some(Duration::ZERO));
        assert_eq!(parse_timeout("30").unwrap(), Some(Duration::from_secs(30)));
        assert_eq!(parse_timeout("1m").unwrap(), Some(Duration::from_secs(60)));
        assert!(parse_timeout("blee").is_err());
        let c = Cli::default();
        let o = c.connect_options(Duration::from_secs(120)).unwrap();
        assert_eq!(o.request_timeout, Some(Duration::from_secs(120)));
        let c = Cli {
            request_timeout: Some("0".into()),
            ..Cli::default()
        };
        assert_eq!(
            c.connect_options(Duration::from_secs(120))
                .unwrap()
                .request_timeout,
            None
        );
        let c = Cli {
            request_timeout: Some("5s".into()),
            context: Some("ctx".into()),
            impersonate_group: vec!["a".into()],
            ..Cli::default()
        };
        let o = c.connect_options(Duration::from_secs(120)).unwrap();
        assert_eq!(o.request_timeout, Some(Duration::from_secs(5)));
        assert_eq!(o.context.as_deref(), Some("ctx"));
        assert_eq!(o.impersonate_groups, vec!["a"]);
    }

    #[test]
    fn flags_mapping() {
        let c = Cli {
            readonly: true,
            write: true,
            command_override: Some("dp".into()),
            refresh: 3.0,
            ..Cli::default()
        };
        let mut k = crate::config::K9s::default();
        k.override_flags(&c.k9s_flags());
        assert!(!k.is_read_only(None));
        assert_eq!(k.get_refresh_rate(), 3.0);
        assert_eq!(k.manual.command.as_deref(), Some("dp"));
    }

    #[test]
    fn initial_command_precedence() {
        let mk = |active: &str| ContextConfig {
            view: crate::config::View {
                active: active.into(),
            },
            ..ContextConfig::default()
        };
        // -c wins.
        let mut cfg = Config::default();
        cfg.k9s.default_view = "svc".into();
        cfg.k9s.override_flags(
            &Cli {
                command_override: Some("dp".into()),
                ..Cli::default()
            }
            .k9s_flags(),
        );
        assert_eq!(initial_command(&mut cfg, &mut mk("ns")), "dp");
        // k9s defaultView replaces the context view (k9s run()).
        let mut cfg = Config::default();
        cfg.k9s.default_view = "svc".into();
        let mut ctx = mk("ns");
        assert_eq!(initial_command(&mut cfg, &mut ctx), "svc");
        assert_eq!(ctx.view.active, "svc");
        // Context view.
        let mut cfg = Config::default();
        assert_eq!(initial_command(&mut cfg, &mut mk("ns")), "ns");
        // Fallback.
        assert_eq!(initial_command(&mut cfg, &mut mk("")), "po");
    }

    #[test]
    fn cli_parse_args() {
        match Cli::parse_args(&["-c".into(), "dp".into()]) {
            flags::Parsed::Run(c) => assert_eq!(c.command_override.as_deref(), Some("dp")),
            other => panic!("{other:?}"),
        }
        use clap::CommandFactory;
        Cli::command().debug_assert();
        let m = Cli::command()
            .try_get_matches_from(["k9s", "-A", "--context", "x", "info"])
            .unwrap();
        assert_eq!(m.get_many::<std::ffi::OsString>("args").unwrap().count(), 4);
    }
}
