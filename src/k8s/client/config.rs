//! Kubeconfig loading and CLI overrides (k9s client.Config over
//! genericclioptions.ConfigFlags).

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Context as _, Result};
use kube::config::{KubeConfigOptions, Kubeconfig};

use super::ConnectOptions;
use crate::k8s::ContextInfo;

/// k9s client.DefaultCallTimeoutDuration.
const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(120);

/// Name used for the synthetic context when running in a pod without a kubeconfig.
const IN_CLUSTER_CONTEXT: &str = "in-cluster";

/// A loaded kubeconfig plus the flags that override it.
#[derive(Clone, Debug)]
pub(crate) struct LoadedConfig {
    pub(crate) kubeconfig: Kubeconfig,
    pub(crate) opts: ConnectOptions,
    /// True when no kubeconfig was found and the in-cluster config is used.
    pub(crate) in_cluster: bool,
    /// Why the kubeconfig could not be read; k9s still starts (client-go
    /// errors on use).
    pub(crate) load_err: Option<String>,
}

impl LoadedConfig {
    /// Load the kubeconfig named by `opts` (or KUBECONFIG / ~/.kube/config).
    /// Like client-go, missing KUBECONFIG/default files are skipped (an empty
    /// config) while a missing `--kubeconfig` file or a bad file errors.
    pub(crate) fn load(opts: ConnectOptions) -> Result<LoadedConfig> {
        let explicit = opts.kubeconfig.as_deref().is_some_and(|p| !p.is_empty());
        let kc = read_paths(&config_paths(opts.kubeconfig.as_deref())?, explicit)?;
        if kc.is_none() && !explicit && std::env::var_os("KUBERNETES_SERVICE_HOST").is_some() {
            return Ok(LoadedConfig {
                kubeconfig: Kubeconfig::default(),
                opts,
                in_cluster: true,
                load_err: None,
            });
        }
        Ok(LoadedConfig {
            kubeconfig: kc.unwrap_or_default(),
            opts,
            in_cluster: false,
            load_err: None,
        })
    }

    /// `load`, keeping a read error for later (k9s starts without a
    /// kubeconfig and shows the error on use).
    pub(crate) fn load_or_empty(opts: ConnectOptions) -> LoadedConfig {
        match LoadedConfig::load(opts.clone()) {
            Ok(c) => c,
            Err(e) => LoadedConfig {
                kubeconfig: Kubeconfig::default(),
                opts,
                in_cluster: false,
                load_err: Some(e.to_string()),
            },
        }
    }

    /// Build from an in-memory kubeconfig.
    #[cfg(test)]
    pub(crate) fn from_kubeconfig(kubeconfig: Kubeconfig, opts: ConnectOptions) -> LoadedConfig {
        LoadedConfig {
            kubeconfig,
            opts,
            in_cluster: false,
            load_err: None,
        }
    }

    /// Re-read the kubeconfig from disk (k9s reloads on every RawConfig call).
    pub(crate) fn reload(&self) -> Result<LoadedConfig> {
        if self.in_cluster {
            return Ok(self.clone());
        }
        LoadedConfig::load(self.opts.clone())
    }

    /// Call timeout (client.Config.CallTimeout).
    pub(crate) fn call_timeout(&self) -> Duration {
        self.opts.request_timeout.unwrap_or(DEFAULT_CALL_TIMEOUT)
    }

    /// CurrentContextName: `--context` flag, else the kubeconfig current-context.
    pub(crate) fn current_context_name(&self) -> Result<String> {
        if let Some(c) = self.opts.context.as_deref().filter(|c| !c.is_empty()) {
            return Ok(c.to_string());
        }
        if self.in_cluster {
            return Ok(IN_CLUSTER_CONTEXT.into());
        }
        Ok(self.kubeconfig.current_context.clone().unwrap_or_default())
    }

    /// The kubeconfig's own current-context.
    pub(crate) fn file_current_context(&self) -> String {
        self.kubeconfig.current_context.clone().unwrap_or_default()
    }

    fn context(&self, name: &str) -> Option<kube::config::Context> {
        self.kubeconfig
            .contexts
            .iter()
            .find(|c| c.name == name)
            .and_then(|c| c.context.clone())
    }

    /// CurrentClusterName.
    pub(crate) fn current_cluster_name(&self) -> Result<String> {
        if let Some(c) = self.opts.cluster.as_deref().filter(|c| !c.is_empty()) {
            return Ok(c.to_string());
        }
        if self.in_cluster {
            return Ok(IN_CLUSTER_CONTEXT.into());
        }
        let current = self.file_current_context();
        let mut ct = self
            .context(&current)
            .ok_or_else(|| anyhow!("invalid current context specified: {current:?}"))?;
        if let Some(name) = self.opts.context.as_deref().filter(|c| !c.is_empty()) {
            ct = self
                .context(name)
                .ok_or_else(|| anyhow!("current-cluster - invalid context specified: {name:?}"))?;
        }
        Ok(ct.cluster)
    }

    /// CurrentUserName.
    pub(crate) fn current_user_name(&self) -> Result<String> {
        if let Some(u) = self.opts.impersonate.as_deref().filter(|u| !u.is_empty()) {
            return Ok(u.to_string());
        }
        if let Some(u) = self.opts.user.as_deref().filter(|u| !u.is_empty()) {
            return Ok(u.to_string());
        }
        if self.in_cluster {
            return Ok(IN_CLUSTER_CONTEXT.into());
        }
        let current = self.current_context_name()?;
        match self.context(&current) {
            Some(ct) => Ok(ct.user.unwrap_or_default()),
            None => Err(anyhow!("unable to locate current user")),
        }
    }

    /// Credentials of the active user entry, if any.
    pub(crate) fn current_auth_info(&self) -> Option<kube::config::AuthInfo> {
        let user = match self.opts.user.as_deref().filter(|u| !u.is_empty()) {
            Some(u) => u.to_string(),
            None => self
                .current_context_name()
                .ok()
                .and_then(|n| self.context(&n))
                .and_then(|c| c.user)?,
        };
        self.kubeconfig
            .auth_infos
            .iter()
            .find(|a| a.name == user)
            .and_then(|a| a.auth_info.clone())
    }

    /// The kubeconfig files in use, in merge order.
    pub(crate) fn kubeconfig_paths(&self) -> Vec<String> {
        if self.in_cluster {
            return vec![];
        }
        config_paths(self.opts.kubeconfig.as_deref())
            .map(|pp| pp.iter().map(|p| p.display().to_string()).collect())
            .unwrap_or_default()
    }

    /// Namespace from `--namespace`, else the active context's namespace
    /// (CurrentNamespaceName). May be empty.
    pub(crate) fn current_namespace(&self) -> String {
        if let Some(ns) = self.opts.namespace.as_deref().filter(|n| !n.is_empty()) {
            return ns.to_string();
        }
        self.current_context_name()
            .ok()
            .and_then(|n| self.context(&n))
            .and_then(|c| c.namespace)
            .unwrap_or_default()
    }

    /// Default namespace: flag, context namespace, else "default".
    pub(crate) fn default_namespace(&self) -> String {
        let ns = self.current_namespace();
        if ns.is_empty() {
            crate::k8s::DEFAULT_NAMESPACE.into()
        } else {
            ns
        }
    }

    /// All contexts, sorted by name, with the active one flagged.
    pub(crate) fn contexts(&self) -> Vec<ContextInfo> {
        let active = self.current_context_name().unwrap_or_default();
        let mut out: Vec<ContextInfo> = self
            .kubeconfig
            .contexts
            .iter()
            .map(|nc| {
                let c = nc.context.clone().unwrap_or_default();
                ContextInfo {
                    name: nc.name.clone(),
                    cluster: c.cluster,
                    user: c.user.unwrap_or_default(),
                    namespace: c.namespace.unwrap_or_default(),
                    active: nc.name == active,
                }
            })
            .collect();
        if self.in_cluster && out.is_empty() {
            out.push(ContextInfo {
                name: IN_CLUSTER_CONTEXT.into(),
                cluster: IN_CLUSTER_CONTEXT.into(),
                user: IN_CLUSTER_CONTEXT.into(),
                namespace: self.default_namespace(),
                active: true,
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Flags after a context switch (client.Config.SwitchContext): context and
    /// cluster pinned to the new context; namespace, timeout, kubeconfig,
    /// impersonation, insecure and token carried over. User, CA and client
    /// cert/key overrides are dropped like k9s does.
    pub(crate) fn switched(&self, name: &str) -> Result<LoadedConfig> {
        let ct = self
            .context(name)
            .ok_or_else(|| anyhow!("context {name:?} does not exist"))?;
        let opts = ConnectOptions {
            kubeconfig: self.opts.kubeconfig.clone(),
            context: Some(name.to_string()),
            cluster: Some(ct.cluster),
            user: None,
            namespace: self.opts.namespace.clone(),
            request_timeout: self.opts.request_timeout,
            impersonate: self.opts.impersonate.clone(),
            impersonate_groups: self.opts.impersonate_groups.clone(),
            insecure_skip_tls_verify: self.opts.insecure_skip_tls_verify,
            certificate_authority: None,
            client_key: None,
            client_certificate: None,
            token: self.opts.token.clone(),
            // k9s only ever sets the proxy func, never clears it.
            proxy: self.opts.proxy.clone(),
        };
        Ok(LoadedConfig {
            kubeconfig: self.kubeconfig.clone(),
            opts,
            in_cluster: false,
            load_err: None,
        })
    }

    /// Build the kube-rs client configuration with all overrides applied.
    pub(crate) async fn kube_config(&self) -> Result<kube::Config> {
        if let Some(e) = &self.load_err {
            return Err(anyhow!("{e}"));
        }
        let mut cfg = if self.in_cluster {
            kube::Config::incluster().context("unable to load in-cluster configuration")?
        } else {
            let mut kc = self.kubeconfig.clone();
            let context = self.current_context_name()?;
            if context.is_empty() {
                return Err(anyhow!("no current context set in kubeconfig"));
            }
            // client-go DirectClientConfig.ConfirmUsable.
            if self.context(&context).is_none() {
                return Err(anyhow!("context {context:?} does not exist"));
            }
            let cluster = self.current_cluster_name()?;
            if let Some(named) = kc.clusters.iter_mut().find(|c| c.name == cluster) {
                let cl = named.cluster.get_or_insert_with(Default::default);
                if let Some(ca) = self
                    .opts
                    .certificate_authority
                    .as_deref()
                    .filter(|s| !s.is_empty())
                {
                    cl.certificate_authority = Some(ca.to_string());
                    cl.certificate_authority_data = None;
                    cl.insecure_skip_tls_verify = Some(false);
                }
                if self.opts.insecure_skip_tls_verify {
                    cl.insecure_skip_tls_verify = Some(true);
                    cl.certificate_authority = None;
                    cl.certificate_authority_data = None;
                }
            }
            let kopts = KubeConfigOptions {
                context: Some(context),
                cluster: self.opts.cluster.clone().filter(|c| !c.is_empty()),
                user: self.opts.user.clone().filter(|u| !u.is_empty()),
            };
            kube::Config::from_custom_kubeconfig(kc, &kopts)
                .await
                .map_err(|e| anyhow!("{e}"))?
        };
        let ns = self.current_namespace();
        if !ns.is_empty() {
            cfg.default_namespace = ns;
        }
        cfg.read_timeout = self.opts.request_timeout;
        if let Some(p) = self.opts.proxy.as_deref().filter(|p| !p.is_empty()) {
            cfg.proxy_url = Some(p.parse().map_err(|e| anyhow!("parse {p:?}: {e}"))?);
        }
        if self.in_cluster && self.opts.insecure_skip_tls_verify {
            cfg.accept_invalid_certs = true;
        }
        let auth = &mut cfg.auth_info;
        if let Some(cert) = self
            .opts
            .client_certificate
            .as_deref()
            .filter(|s| !s.is_empty())
        {
            auth.client_certificate = Some(cert.to_string());
            auth.client_certificate_data = None;
        }
        if let Some(key) = self.opts.client_key.as_deref().filter(|s| !s.is_empty()) {
            auth.client_key = Some(key.to_string());
            auth.client_key_data = None;
        }
        if let Some(token) = self.opts.token.as_deref().filter(|s| !s.is_empty()) {
            auth.token = Some(secrecy::SecretString::from(token.to_string()));
            auth.token_file = None;
            auth.exec = None;
            auth.auth_provider = None;
        }
        if let Some(u) = self.opts.impersonate.as_deref().filter(|s| !s.is_empty()) {
            auth.impersonate = Some(u.to_string());
        }
        if !self.opts.impersonate_groups.is_empty() {
            auth.impersonate_groups = Some(self.opts.impersonate_groups.clone());
        }
        Ok(cfg)
    }
}

/// Paths from a kubeconfig flag or env value (colon separated on unix).
fn split_paths(value: &str) -> Vec<PathBuf> {
    std::env::split_paths(value)
        .filter(|p| !p.as_os_str().is_empty())
        .collect()
}

/// Read and merge kubeconfig files: explicit path(s), else KUBECONFIG, else
/// ~/.kube/config.
fn config_paths(explicit: Option<&str>) -> Result<Vec<PathBuf>> {
    Ok(match explicit.filter(|p| !p.is_empty()) {
        Some(p) => split_paths(p),
        None => match std::env::var("KUBECONFIG") {
            Ok(v) if !split_paths(&v).is_empty() => split_paths(&v),
            _ => {
                let home = dirs::home_dir().ok_or_else(|| anyhow!("unable to locate home dir"))?;
                vec![home.join(".kube").join("config")]
            }
        },
    })
}

/// Read and merge kubeconfig files, first wins. `None` when none exist.
/// client-go: an explicit (`--kubeconfig`) file must exist.
fn read_paths(paths: &[PathBuf], explicit: bool) -> Result<Option<Kubeconfig>> {
    let mut merged: Option<Kubeconfig> = None;
    for p in paths {
        if let Err(e) = std::fs::metadata(p) {
            if explicit {
                return Err(anyhow!(crate::cli::info::go_path_error("stat", p, &e)));
            }
            continue;
        }
        let kc = Kubeconfig::read_from(p).map_err(|e| {
            anyhow!(
                "error loading config file {:?}: {e}",
                p.display().to_string()
            )
        })?;
        merged = Some(match merged {
            Some(m) => m.merge(kc).map_err(|e| anyhow!("{e}"))?,
            None => kc,
        });
    }
    Ok(merged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const KC1: &str = r#"
apiVersion: v1
kind: Config
current-context: ctx-a
clusters:
- name: cl-a
  cluster:
    server: https://a.example.com
- name: cl-b
  cluster:
    server: https://b.example.com
    certificate-authority-data: Zm9v
users:
- name: u-a
  user:
    token: tok-a
- name: u-b
  user:
    token: tok-b
contexts:
- name: ctx-a
  context:
    cluster: cl-a
    user: u-a
    namespace: team-a
- name: ctx-b
  context:
    cluster: cl-b
    user: u-b
"#;

    const KC2: &str = r#"
apiVersion: v1
kind: Config
current-context: ctx-c
clusters:
- name: cl-c
  cluster:
    server: https://c.example.com
users:
- name: u-c
  user:
    token: tok-c
contexts:
- name: ctx-c
  context:
    cluster: cl-c
    user: u-c
"#;

    fn write(dir: &tempfile::TempDir, name: &str, body: &str) -> String {
        let p = dir.path().join(name);
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        p.display().to_string()
    }

    fn load(opts: ConnectOptions) -> LoadedConfig {
        LoadedConfig::load(opts).unwrap()
    }

    #[test]
    fn loads_explicit_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(&dir, "config", KC1);
        let c = load(ConnectOptions {
            kubeconfig: Some(p),
            ..Default::default()
        });
        assert_eq!(c.current_context_name().unwrap(), "ctx-a");
        assert_eq!(c.current_cluster_name().unwrap(), "cl-a");
        assert_eq!(c.current_user_name().unwrap(), "u-a");
        assert_eq!(c.default_namespace(), "team-a");
        assert!(c.current_auth_info().unwrap().token.is_some());
        let names: Vec<_> = c
            .contexts()
            .into_iter()
            .map(|c| (c.name, c.active))
            .collect();
        assert_eq!(
            names,
            vec![("ctx-a".to_string(), true), ("ctx-b".to_string(), false)]
        );
    }

    #[test]
    fn merges_colon_separated_paths_first_wins() {
        let dir = tempfile::tempdir().unwrap();
        let p1 = write(&dir, "one", KC1);
        let p2 = write(&dir, "two", KC2);
        let c = load(ConnectOptions {
            kubeconfig: Some(format!("{p2}:{p1}")),
            ..Default::default()
        });
        assert_eq!(c.current_context_name().unwrap(), "ctx-c");
        assert_eq!(c.contexts().len(), 3);
        assert_eq!(c.kubeconfig_paths(), vec![p2, p1]);
    }

    // client-go errors (was "kubeconfig not found: ..."): an explicit file
    // must exist; missing KUBECONFIG entries are skipped.
    #[test]
    fn missing_files_error() {
        let err = LoadedConfig::load(ConnectOptions {
            kubeconfig: Some("/nonexistent/k9rs/kubeconfig".into()),
            ..Default::default()
        })
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "stat /nonexistent/k9rs/kubeconfig: no such file or directory"
        );
        let c = LoadedConfig::load_or_empty(ConnectOptions {
            kubeconfig: Some("/nonexistent/k9rs/kubeconfig".into()),
            ..Default::default()
        });
        assert_eq!(c.load_err.as_deref(), Some(err.to_string().as_str()));
        assert!(c.contexts().is_empty());
        let paths = [PathBuf::from("/nonexistent/k9rs/a")];
        assert!(read_paths(&paths, false).unwrap().is_none());
        let dir = tempfile::tempdir().unwrap();
        let p = write(&dir, "one", KC1);
        let kc = read_paths(&[paths[0].clone(), PathBuf::from(&p)], false)
            .unwrap()
            .unwrap();
        assert_eq!(kc.contexts.len(), 2);
        let bad = write(&dir, "bad", "contexts: 12");
        let e = read_paths(&[PathBuf::from(&bad)], false).unwrap_err();
        assert!(
            e.to_string()
                .starts_with(&format!("error loading config file {bad:?}: ")),
            "{e}"
        );
    }

    /// Context config proxy.address -> kube proxy_url (SetProxy); carried
    /// over on a context switch like k9s.
    #[tokio::test]
    async fn context_proxy() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(&dir, "config", KC1);
        let c = load(ConnectOptions {
            kubeconfig: Some(p),
            proxy: Some("http://proxy.local:3128".into()),
            ..Default::default()
        });
        let k = c.kube_config().await.unwrap();
        assert_eq!(k.proxy_url.unwrap().to_string(), "http://proxy.local:3128/");
        let s = c.switched("ctx-b").unwrap();
        assert_eq!(s.opts.proxy.as_deref(), Some("http://proxy.local:3128"));
        let bad = LoadedConfig {
            opts: ConnectOptions {
                proxy: Some("::nope::".into()),
                ..c.opts.clone()
            },
            ..c.clone()
        };
        assert!(bad.kube_config().await.is_err());
    }

    #[test]
    fn flag_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(&dir, "config", KC1);
        let c = load(ConnectOptions {
            kubeconfig: Some(p.clone()),
            context: Some("ctx-b".into()),
            namespace: Some("ns-flag".into()),
            ..Default::default()
        });
        assert_eq!(c.current_context_name().unwrap(), "ctx-b");
        assert_eq!(c.current_cluster_name().unwrap(), "cl-b");
        assert_eq!(c.current_user_name().unwrap(), "u-b");
        assert_eq!(c.default_namespace(), "ns-flag");
        assert!(c.contexts().iter().any(|x| x.name == "ctx-b" && x.active));

        // ctx-b has no namespace: falls back to default.
        let c = load(ConnectOptions {
            kubeconfig: Some(p.clone()),
            context: Some("ctx-b".into()),
            ..Default::default()
        });
        assert_eq!(c.current_namespace(), "");
        assert_eq!(c.default_namespace(), "default");

        let c = load(ConnectOptions {
            kubeconfig: Some(p.clone()),
            cluster: Some("cl-b".into()),
            user: Some("u-b".into()),
            ..Default::default()
        });
        assert_eq!(c.current_cluster_name().unwrap(), "cl-b");
        assert_eq!(c.current_user_name().unwrap(), "u-b");

        let c = load(ConnectOptions {
            kubeconfig: Some(p),
            impersonate: Some("jane".into()),
            user: Some("u-b".into()),
            ..Default::default()
        });
        assert_eq!(c.current_user_name().unwrap(), "jane");
    }

    #[test]
    fn invalid_context_flag() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(&dir, "config", KC1);
        let c = load(ConnectOptions {
            kubeconfig: Some(p),
            context: Some("nope".into()),
            ..Default::default()
        });
        assert_eq!(
            c.current_cluster_name().unwrap_err().to_string(),
            "current-cluster - invalid context specified: \"nope\""
        );
        assert!(c.current_user_name().is_err());
        assert_eq!(c.default_namespace(), "default");
    }

    #[test]
    fn switch_keeps_and_drops_flags() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(&dir, "config", KC1);
        let c = load(ConnectOptions {
            kubeconfig: Some(p.clone()),
            user: Some("u-a".into()),
            namespace: Some("fred".into()),
            impersonate: Some("jane".into()),
            impersonate_groups: vec!["g1".into()],
            insecure_skip_tls_verify: true,
            certificate_authority: Some("/ca".into()),
            client_key: Some("/key".into()),
            client_certificate: Some("/cert".into()),
            token: Some("t".into()),
            request_timeout: Some(Duration::from_secs(5)),
            ..Default::default()
        });
        let s = c.switched("ctx-b").unwrap();
        assert_eq!(s.opts.context.as_deref(), Some("ctx-b"));
        assert_eq!(s.opts.cluster.as_deref(), Some("cl-b"));
        assert_eq!(s.opts.user, None);
        assert_eq!(s.opts.namespace.as_deref(), Some("fred"));
        assert_eq!(s.opts.kubeconfig.as_deref(), Some(p.as_str()));
        assert_eq!(s.opts.impersonate.as_deref(), Some("jane"));
        assert_eq!(s.opts.impersonate_groups, vec!["g1".to_string()]);
        assert!(s.opts.insecure_skip_tls_verify);
        assert_eq!(s.opts.certificate_authority, None);
        assert_eq!(s.opts.client_key, None);
        assert_eq!(s.opts.client_certificate, None);
        assert_eq!(s.opts.token.as_deref(), Some("t"));
        assert_eq!(s.call_timeout(), Duration::from_secs(5));
        assert_eq!(
            c.switched("zorg").unwrap_err().to_string(),
            "context \"zorg\" does not exist"
        );
    }

    #[tokio::test]
    async fn kube_config_applies_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(&dir, "config", KC1);
        let c = load(ConnectOptions {
            kubeconfig: Some(p.clone()),
            ..Default::default()
        });
        let cfg = c.kube_config().await.unwrap();
        assert_eq!(cfg.cluster_url.to_string(), "https://a.example.com/");
        assert_eq!(cfg.default_namespace, "team-a");
        assert_eq!(cfg.read_timeout, None);
        assert!(!cfg.accept_invalid_certs);

        let c = load(ConnectOptions {
            kubeconfig: Some(p),
            context: Some("ctx-b".into()),
            request_timeout: Some(Duration::from_secs(7)),
            impersonate: Some("jane".into()),
            impersonate_groups: vec!["a".into(), "b".into()],
            insecure_skip_tls_verify: true,
            token: Some("override".into()),
            client_certificate: Some("/tmp/cert.pem".into()),
            client_key: Some("/tmp/key.pem".into()),
            ..Default::default()
        });
        let cfg = c.kube_config().await.unwrap();
        assert_eq!(cfg.cluster_url.to_string(), "https://b.example.com/");
        assert_eq!(cfg.default_namespace, "default");
        assert_eq!(cfg.read_timeout, Some(Duration::from_secs(7)));
        assert!(cfg.accept_invalid_certs);
        assert!(cfg.root_cert.is_none());
        use secrecy::ExposeSecret;
        assert_eq!(
            cfg.auth_info.token.as_ref().unwrap().expose_secret(),
            "override"
        );
        assert_eq!(cfg.auth_info.impersonate.as_deref(), Some("jane"));
        assert_eq!(
            cfg.auth_info.impersonate_groups,
            Some(vec!["a".to_string(), "b".to_string()])
        );
        assert_eq!(
            cfg.auth_info.client_certificate.as_deref(),
            Some("/tmp/cert.pem")
        );
        assert_eq!(cfg.auth_info.client_key.as_deref(), Some("/tmp/key.pem"));
    }

    #[tokio::test]
    async fn kube_config_ca_override_reads_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = write(&dir, "config", KC1);
        let c = load(ConnectOptions {
            kubeconfig: Some(p),
            certificate_authority: Some("/nonexistent/ca.crt".into()),
            ..Default::default()
        });
        // The override is honored: loading fails because the CA file is missing.
        assert!(c.kube_config().await.is_err());
    }
}
