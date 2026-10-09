//! Build/version information, shown by `k9rs version` and the header "K9s Rev" line.

/// Upstream k9s version this port tracks.
pub const VERSION: &str = "v0.50.18";
/// Git commit (set via `K9RS_COMMIT` at build time, else "dev").
pub const COMMIT: &str = match option_env!("K9RS_COMMIT") {
    Some(c) => c,
    None => "dev",
};
/// Build date (set via `K9RS_DATE` at build time, else "n/a").
pub const DATE: &str = match option_env!("K9RS_DATE") {
    Some(d) => d,
    None => "n/a",
};

// ---------------------------------------------------------------------------
// Latest release check (model/cluster_info.go, model/semver.go).

use std::sync::Mutex;
use std::time::{Duration, Instant};

const K9S_GIT_HOST: &str = "api.github.com";
const K9S_GIT_PATH: &str = "/repos/derailed/k9s/releases/latest";
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
        .header(http::header::USER_AGENT, format!("k9s/{VERSION}"))
        .header(http::header::ACCEPT, "application/json")
        .body(Empty::<bytes::Bytes>::new())?;
    let resp = s.send_request(req).await?;
    let body = resp.into_body().collect().await?.to_bytes();
    release_name(&body)
}

/// ClusterInfo.fetchK9sLatestRev: the latest k9s release, cached for an
/// hour on success. Failures are logged and yield "".
pub(crate) async fn latest_rev() -> String {
    if let Some((v, at)) = LATEST.lock().expect("latest rev").as_ref() {
        if at.elapsed() < CACHE_EXPIRY {
            return v.clone();
        }
    }
    match tokio::time::timeout(FETCH_TIMEOUT, fetch_latest_rev()).await {
        Ok(Ok(v)) => {
            *LATEST.lock().expect("latest rev") = Some((v.clone(), Instant::now()));
            v
        }
        Ok(Err(e)) => {
            tracing::warn!("k9s latest rev fetch failed: {e}");
            String::new()
        }
        Err(_) => {
            tracing::warn!("k9s latest rev fetch failed: timeout");
            String::new()
        }
    }
}

#[cfg(test)]
mod tests {
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
