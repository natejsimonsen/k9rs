//! Exec credential plugins run like client-go's exec.Authenticator: once,
//! then cached until the credential expires, shared by every client of a
//! connection (kube-rs runs the plugin twice per client it builds).

use std::sync::Arc;
use std::time::SystemTime;

use kube::config::{ExecConfig, ExecInteractiveMode};
use serde::Deserialize;

/// A failed credential fetch, worded like client-go (`getting credentials:
/// exec: ...`).
#[derive(Debug, Clone)]
pub(crate) struct ExecError(pub String);

impl std::fmt::Display for ExecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "getting credentials: {}", self.0)
    }
}

impl std::error::Error for ExecError {}

/// What a plugin run returned.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Cred {
    Token(String, Option<SystemTime>),
    /// Client certificates: left to kube-rs (it wires them into TLS).
    Cert,
}

#[derive(Deserialize)]
struct Output {
    status: Option<Status>,
}

#[derive(Deserialize)]
struct Status {
    #[serde(rename = "expirationTimestamp")]
    expiration_timestamp: Option<String>,
    token: Option<String>,
    #[serde(rename = "clientCertificateData")]
    client_certificate_data: Option<String>,
}

/// exec.go installHintVerboseHelp.
const INSTALL_HINT: &str = "\n\nIt looks like you are trying to use a client-go credential plugin that is not installed.\n\nTo learn more about this feature, consult the documentation available at:\n      https://kubernetes.io/docs/reference/access-authn-authz/authentication/#client-go-credential-plugins";

/// Run the plugin (blocking).
pub(crate) fn run(cfg: &ExecConfig) -> Result<Cred, ExecError> {
    let Some(prog) = cfg.command.as_deref().filter(|c| !c.is_empty()) else {
        return Err(ExecError("exec: missing command".into()));
    };
    let mut cmd = std::process::Command::new(prog);
    cmd.args(cfg.args.iter().flatten());
    for e in cfg.env.iter().flatten() {
        if let (Some(n), Some(v)) = (e.get("name"), e.get("value")) {
            cmd.env(n, v);
        }
    }
    let interactive = cfg.interactive_mode != Some(ExecInteractiveMode::Never);
    if interactive {
        cmd.stdin(std::process::Stdio::inherit());
        cmd.stderr(std::process::Stdio::inherit());
    } else {
        cmd.stdin(std::process::Stdio::piped());
    }
    let info = serde_json::json!({
        "kind": "ExecCredential",
        "apiVersion": cfg.api_version,
        "spec": {"interactive": interactive},
    });
    cmd.env("KUBERNETES_EXEC_INFO", info.to_string());
    for e in cfg.drop_env.iter().flatten() {
        cmd.env_remove(e);
    }
    let out = match cmd.output() {
        Ok(o) => o,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut m = format!("exec: executable {prog} not found{INSTALL_HINT}");
            if let Some(h) = cfg.install_hint.as_deref().filter(|h| !h.is_empty()) {
                m.push_str(&format!("\n\n{h}"));
            }
            return Err(ExecError(m));
        }
        Err(e) => return Err(ExecError(format!("exec: {e}"))),
    };
    if !out.status.success() {
        return Err(ExecError(format!(
            "exec: executable {prog} failed with exit code {}",
            out.status.code().unwrap_or(-1)
        )));
    }
    parse(&out.stdout)
}

fn parse(stdout: &[u8]) -> Result<Cred, ExecError> {
    let o: Output = serde_json::from_slice(stdout)
        .or_else(|_| serde_yaml::from_slice(stdout))
        .map_err(|e| ExecError(format!("decoding stdout: {e}")))?;
    let Some(s) = o.status else {
        return Err(ExecError("exec plugin didn't return a status field".into()));
    };
    if s.client_certificate_data.is_some() {
        return Ok(Cred::Cert);
    }
    let Some(token) = s.token.filter(|t| !t.is_empty()) else {
        return Err(ExecError(
            "exec plugin didn't return a token or cert/key pair".into(),
        ));
    };
    let exp = match s.expiration_timestamp {
        Some(ts) => Some(
            chrono::DateTime::parse_from_rfc3339(&ts)
                .map_err(|e| ExecError(format!("parsing expiration timestamp: {e}")))?
                .into(),
        ),
        None => None,
    };
    Ok(Cred::Token(token, exp))
}

/// A connection's plugin with its cached token (client-go cachedCreds).
pub(crate) struct ExecAuth {
    cfg: ExecConfig,
    cached: tokio::sync::Mutex<Option<(String, Option<SystemTime>)>>,
}

impl ExecAuth {
    pub(crate) fn new(cfg: ExecConfig, seed: Option<(String, Option<SystemTime>)>) -> Arc<Self> {
        Arc::new(ExecAuth {
            cfg,
            cached: tokio::sync::Mutex::new(seed),
        })
    }

    /// The cached token, or a fresh one once expired (credsExpired).
    pub(crate) async fn token(&self) -> Result<String, ExecError> {
        let mut c = self.cached.lock().await;
        if let Some((t, exp)) = c.as_ref() {
            if exp.is_none_or(|e| SystemTime::now() < e) {
                return Ok(t.clone());
            }
        }
        let cfg = self.cfg.clone();
        let res = tokio::task::spawn_blocking(move || run(&cfg))
            .await
            .map_err(|e| ExecError(format!("exec: {e}")))??;
        match res {
            Cred::Token(t, exp) => {
                *c = Some((t.clone(), exp));
                Ok(t)
            }
            Cred::Cert => Err(ExecError(
                "exec plugin started returning a cert/key pair".into(),
            )),
        }
    }
}

/// Tower filter adding `Authorization: Bearer` from the plugin.
#[derive(Clone)]
pub(crate) struct ExecFilter(pub Arc<ExecAuth>);

impl tower::filter::AsyncPredicate<http::Request<kube::client::Body>> for ExecFilter {
    type Future = std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<http::Request<kube::client::Body>, tower::BoxError>,
                > + Send,
        >,
    >;
    type Request = http::Request<kube::client::Body>;

    fn check(&mut self, mut req: http::Request<kube::client::Body>) -> Self::Future {
        let auth = self.0.clone();
        Box::pin(async move {
            let t = auth.token().await?;
            let v = http::HeaderValue::from_str(&format!("Bearer {t}"))
                .map_err(|e| ExecError(format!("invalid bearer token: {e}")))?;
            let mut v = v;
            v.set_sensitive(true);
            req.headers_mut().insert(http::header::AUTHORIZATION, v);
            Ok(req)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(script: &str) -> (tempfile::TempDir, ExecConfig) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("plugin.sh");
        std::fs::write(&p, format!("#!/bin/sh\n{script}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let c = ExecConfig {
            api_version: Some("client.authentication.k8s.io/v1beta1".into()),
            command: Some(p.to_string_lossy().into()),
            args: None,
            env: None,
            drop_env: None,
            install_hint: None,
            interactive_mode: Some(ExecInteractiveMode::Never),
            provide_cluster_info: false,
            cluster: None,
            other: Default::default(),
        };
        (d, c)
    }

    #[test]
    fn outputs() {
        let (_d, c) = cfg(
            r#"echo '{"kind":"ExecCredential","status":{"token":"t1","expirationTimestamp":"2099-01-01T00:00:00Z"}}'"#,
        );
        let Cred::Token(t, exp) = run(&c).unwrap() else {
            panic!()
        };
        assert_eq!(t, "t1");
        assert!(exp.unwrap() > SystemTime::now());
        let (_d, c) = cfg("exit 3");
        assert_eq!(
            run(&c).unwrap_err().to_string(),
            format!(
                "getting credentials: exec: executable {} failed with exit code 3",
                c.command.as_deref().unwrap()
            )
        );
        let (_d, mut c) = cfg("");
        c.command = Some("/definitely/not/here-k9rs".into());
        c.install_hint = Some("brew install it".into());
        let e = run(&c).unwrap_err().to_string();
        assert!(e.starts_with("getting credentials: exec: executable /definitely/not/here-k9rs not found\n\nIt looks like"));
        assert!(e.ends_with("credential-plugins\n\nbrew install it"), "{e}");
        assert_eq!(
            parse(b"status:\n  clientCertificateData: x\n  clientKeyData: y\n").unwrap(),
            Cred::Cert
        );
        assert_eq!(
            parse(br#"{"status":{"token":"t2"}}"#).unwrap(),
            Cred::Token("t2".into(), None)
        );
    }

    /// One plugin run per expiry, shared by all callers.
    #[tokio::test]
    async fn caches_until_expired() {
        let (d, c) = cfg("");
        let count = d.path().join("count");
        let script = format!(
            "echo x >> {}\necho '{{\"status\":{{\"token\":\"tok\",\"expirationTimestamp\":\"2099-01-01T00:00:00Z\"}}}}'",
            count.display()
        );
        std::fs::write(
            c.command.as_deref().unwrap(),
            format!("#!/bin/sh\n{script}\n"),
        )
        .unwrap();
        let a = ExecAuth::new(c.clone(), None);
        assert_eq!(a.token().await.unwrap(), "tok");
        assert_eq!(a.token().await.unwrap(), "tok");
        assert_eq!(std::fs::read_to_string(&count).unwrap().lines().count(), 1);
        // An expired seed is refreshed.
        let a = ExecAuth::new(c, Some(("old".into(), Some(SystemTime::UNIX_EPOCH))));
        assert_eq!(a.token().await.unwrap(), "tok");
        assert_eq!(std::fs::read_to_string(&count).unwrap().lines().count(), 2);
    }
}
