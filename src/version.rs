//! Build/version information, shown by `k9rs version` and the header "K9s Rev" line.

/// k9rs version as the UI shows it (model.NormalizeVersion).
pub const VERSION: &str = concat!("v", env!("CARGO_PKG_VERSION"));
/// The build version as `k9rs version` / `k9rs info` print it (goreleaser
/// sets cmd.version without the `v`).
pub const RAW_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Upstream k9s release whose behavior this port matches.
pub const K9S_VERSION: &str = "v0.50.18";
/// Git commit (build.rs: `K9RS_COMMIT` or the git HEAD, else "dev").
pub const COMMIT: &str = match option_env!("K9RS_COMMIT") {
    Some(c) => c,
    None => "dev",
};
/// Build date, RFC3339 UTC (build.rs: `K9RS_DATE`, `SOURCE_DATE_EPOCH` or
/// the build time of a git checkout, else "n/a").
pub const DATE: &str = match option_env!("K9RS_DATE") {
    Some(d) => d,
    None => "n/a",
};

// ---------------------------------------------------------------------------
// Latest release check (model/cluster_info.go, model/semver.go).

use std::sync::Mutex;
use std::time::{Duration, Instant};

// k9rs releases, not k9s: the badge compares against VERSION.
const K9S_GIT_HOST: &str = "api.github.com";
const K9S_GIT_PATH: &str = "/repos/natejsimonsen/k9rs/releases/latest";
const FETCH_TIMEOUT: Duration = Duration::from_secs(3);
/// cluster_info.go cacheExpiry.
const CACHE_EXPIRY: Duration = Duration::from_secs(3600);

static LATEST: Mutex<Option<(String, Instant)>> = Mutex::new(None);

/// model.SemVer: `vMAJOR.MINOR.PATCH`, zero when unparsable.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SemVer(u64, u64, u64);

impl SemVer {
    pub(crate) fn new(v: &str) -> SemVer {
        let v = normalize_version(v);
        let parts: Vec<&str> = v.strip_prefix('v').unwrap_or("").split('.').collect();
        let num = |s: &str| {
            (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                .then(|| s.parse::<u64>().ok())
                .flatten()
        };
        match parts.as_slice() {
            [a, b, c] => match (num(a), num(b), num(c)) {
                (Some(a), Some(b), Some(c)) => SemVer(a, b, c),
                _ => SemVer::default(),
            },
            _ => SemVer::default(),
        }
    }

    /// SemVer.IsCurrent (field-wise, as k9s does it).
    pub(crate) fn is_current(&self, latest: &SemVer) -> bool {
        self.0 >= latest.0 && self.1 >= latest.1 && self.2 >= latest.2
    }
}

impl std::fmt::Display for SemVer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "v{}.{}.{}", self.0, self.1, self.2)
    }
}

/// model.NormalizeVersion.
pub(crate) fn normalize_version(v: &str) -> String {
    if v.is_empty() || v.starts_with('v') {
        v.to_string()
    } else {
        format!("v{v}")
    }
}

/// ClusterInfo.Refresh: (shown k9s version, newer release or "").
pub(crate) fn k9s_revs(current: &str, latest: &str) -> (String, String) {
    let (v1, v2) = (SemVer::new(current), SemVer::new(latest));
    let newer = if v1.is_current(&v2) {
        String::new()
    } else {
        v2.to_string()
    };
    (v1.to_string(), newer)
}

/// The release `name` from the GitHub latest-release payload.
fn release_name(body: &[u8]) -> anyhow::Result<String> {
    let v: serde_json::Value = serde_json::from_slice(body)?;
    v.get("name")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("no version found"))
}

async fn fetch_latest_rev() -> anyhow::Result<String> {
    use http_body_util::{BodyExt, Empty};
    use hyper_util::rt::TokioIo;

    let tcp = tokio::net::TcpStream::connect((K9S_GIT_HOST, 443)).await?;
    let name = rustls::pki_types::ServerName::try_from(K9S_GIT_HOST)?;
    let tls = crate::ui::views::bench::tls_connector(false)?
        .connect(name, tcp)
        .await?;
    let (mut s, conn) = hyper::client::conn::http1::handshake(TokioIo::new(tls)).await?;
    tokio::spawn(async move {
        let _ = conn.await;
    });
    let req = http::Request::get(K9S_GIT_PATH)
        .header(http::header::HOST, K9S_GIT_HOST)
        .header(http::header::USER_AGENT, format!("k9rs/{VERSION}"))
        .header(http::header::ACCEPT, "application/json")
        .body(Empty::<bytes::Bytes>::new())?;
    let resp = s.send_request(req).await?;
    let body = resp.into_body().collect().await?.to_bytes();
    release_name(&body)
}

/// ClusterInfo.fetchK9sLatestRev: the latest k9s release, cached for an
/// hour on success. Failures are logged and yield "".
pub(crate) async fn latest_rev() -> String {
    latest_rev_with(&LATEST, fetch_latest_rev()).await
}

async fn latest_rev_with(
    cache: &Mutex<Option<(String, Instant)>>,
    fetch: impl std::future::Future<Output = anyhow::Result<String>>,
) -> String {
    if let Some((v, at)) = cache.lock().expect("latest rev").as_ref() {
        if at.elapsed() < CACHE_EXPIRY {
            return v.clone();
        }
    }
    tracing::debug!("Fetching latest k9rs rev...");
    match tokio::time::timeout(FETCH_TIMEOUT, fetch).await {
        Ok(Ok(v)) => {
            tracing::debug!(revision = %v, "K9rs latest rev");
            *cache.lock().expect("latest rev") = Some((v.clone(), Instant::now()));
            v
        }
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "k9rs latest rev fetch failed");
            String::new()
        }
        Err(_) => {
            tracing::warn!(
                error = "context deadline exceeded",
                "k9rs latest rev fetch failed"
            );
            String::new()
        }
    }
}

#[cfg(test)]
mod tests {
    /// build.rs embeds a sha and goreleaser-style date, or k9s's defaults.
    #[test]
    fn build_info() {
        assert!(super::COMMIT == "dev" || super::COMMIT.bytes().all(|b| b.is_ascii_alphanumeric()));
        let d = super::DATE;
        let ok = d == "n/a"
            || (d.len() == 20
                && d.ends_with('Z')
                && d.as_bytes()[4] == b'-'
                && d.as_bytes()[10] == b'T');
        assert!(ok, "{d}");
    }

    use super::*;

    #[test]
    fn semver_like_k9s() {
        assert_eq!(SemVer::new("0.50.18").to_string(), "v0.50.18");
        assert_eq!(SemVer::new("dev").to_string(), "v0.0.0");
        assert_eq!(SemVer::new("v1.2.3-rc1").to_string(), "v0.0.0");
        assert_eq!(SemVer::new("").to_string(), "v0.0.0");
        // Field-wise compare, quirks included.
        assert!(!SemVer::new("v0.50.18").is_current(&SemVer::new("v0.51.0")));
        assert!(SemVer::new("v0.50.18").is_current(&SemVer::new("v0.50.18")));
        assert!(!SemVer::new("v1.0.0").is_current(&SemVer::new("v0.51.0")));
    }

    #[test]
    fn revs() {
        assert_eq!(
            k9s_revs("v0.50.18", "v0.51.0"),
            ("v0.50.18".into(), "v0.51.0".into())
        );
        assert_eq!(k9s_revs("v0.50.18", ""), ("v0.50.18".into(), "".into()));
        assert_eq!(
            k9s_revs("v0.50.18", "v0.50.18"),
            ("v0.50.18".into(), "".into())
        );
    }

    /// fetchK9sLatestRev: a success is cached for an hour, a failure (e.g.
    /// GitHub's rate limit: "no version found") is not, so the next
    /// refresh retries. No network: the fetch is injected.
    #[tokio::test]
    async fn latest_rev_caching() {
        let cache = Mutex::new(None);
        let fail = async { Err(anyhow::anyhow!("no version found")) };
        assert_eq!(latest_rev_with(&cache, fail).await, "");
        assert!(cache.lock().unwrap().is_none());
        let ok = async { Ok("v0.51.0".to_string()) };
        assert_eq!(latest_rev_with(&cache, ok).await, "v0.51.0");
        let unused = async { Err(anyhow::anyhow!("not called")) };
        assert_eq!(latest_rev_with(&cache, unused).await, "v0.51.0");
    }

    #[test]
    fn parses_release_name() {
        assert_eq!(
            release_name(br#"{"name":"v0.51.0","tag_name":"v0.51.0"}"#).unwrap(),
            "v0.51.0"
        );
        assert!(release_name(br#"{"message":"API rate limit exceeded"}"#).is_err());
        assert!(release_name(b"<html>").is_err());
    }
}
