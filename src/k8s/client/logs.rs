//! Pod log tailing (k9s dao.Pod.TailLogs / tailLogs / readLogs).

use std::time::Duration;

use anyhow::{anyhow, Result};
use futures::AsyncBufReadExt;
use k8s_openapi::api::core::v1::Pod;
use kube::api::LogParams;
use tokio::sync::mpsc;

use crate::k8s::{LogLine, LogOptions};

/// k9s logRetryCount.
pub(crate) const LOG_RETRY_COUNT: usize = 20;
const LOG_BACKOFF_INITIAL: Duration = Duration::from_millis(500);
/// k9s caps the backoff interval at logBackoffMax / 2.
const LOG_BACKOFF_MAX_INTERVAL: Duration = Duration::from_secs(15);
const LOG_BACKOFF_MULTIPLIER: f64 = 1.5;
/// k9s dao.DefaultContainerAnnotation.
pub(crate) const DEFAULT_CONTAINER_ANNOTATION: &str = "kubectl.kubernetes.io/default-container";
/// Head mode byte limit (LogOptions.ToPodLogOptions).
const HEAD_LIMIT_BYTES: i64 = 5000;

/// LogOptions.ToPodLogOptions for one container. `since_seconds: None` means
/// head mode; negative or zero means tail only.
pub(crate) fn pod_log_params(opts: &LogOptions, container: &str) -> LogParams {
    let mut lp = LogParams {
        container: (!container.is_empty()).then(|| container.to_string()),
        follow: opts.follow,
        previous: opts.previous,
        timestamps: true,
        tail_lines: Some(opts.lines),
        ..Default::default()
    };
    match opts.since_seconds {
        None => {
            lp.follow = false;
            lp.tail_lines = None;
            lp.since_seconds = None;
            lp.limit_bytes = Some(HEAD_LIMIT_BYTES);
        }
        Some(s) if s > 0 => lp.since_seconds = Some(s),
        Some(_) => {}
    }
    lp
}

/// Split a raw log line (always fetched with timestamps) into timestamp + text.
pub(crate) fn parse_line(raw: &[u8], pod: &str, container: &str, show_ts: bool) -> LogLine {
    let mut s = String::from_utf8_lossy(raw).into_owned();
    if s.ends_with('\n') {
        s.pop();
    }
    let (ts, text) = match s.split_once(' ') {
        Some((ts, rest)) if !ts.is_empty() && chrono::DateTime::parse_from_rfc3339(ts).is_ok() => {
            (Some(ts.to_string()), rest.to_string())
        }
        _ => (None, s),
    };
    LogLine {
        pod: pod.to_string(),
        container: container.to_string(),
        timestamp: if show_ts { ts } else { None },
        text,
    }
}

/// The containers to tail for a pod (TailLogs container selection).
pub(crate) fn containers_to_tail(pod: &Pod, opts: &LogOptions) -> Vec<String> {
    let spec = pod.spec.clone().unwrap_or_default();
    let explicit = opts.container.clone().filter(|c| !c.is_empty());
    if !opts.all_containers {
        if let Some(c) = explicit {
            return vec![c];
        }
        if let Some(def) = pod
            .metadata
            .annotations
            .as_ref()
            .and_then(|a| a.get(DEFAULT_CONTAINER_ANNOTATION))
        {
            if spec.containers.iter().any(|c| &c.name == def) {
                return vec![def.clone()];
            }
            tracing::warn!("Container {def} not found. Annotation ignored");
        }
    }
    let mut out: Vec<String> = Vec::new();
    out.extend(
        spec.init_containers
            .unwrap_or_default()
            .into_iter()
            .map(|c| c.name),
    );
    out.extend(spec.containers.into_iter().map(|c| c.name));
    out.extend(
        spec.ephemeral_containers
            .unwrap_or_default()
            .into_iter()
            .map(|c| c.name),
    );
    out
}

/// Exponential backoff matching k9s' parameters (no jitter).
#[derive(Debug)]
pub(crate) struct Backoff {
    current: Duration,
}

impl Backoff {
    pub(crate) fn new() -> Self {
        Backoff {
            current: LOG_BACKOFF_INITIAL,
        }
    }
    /// Delay to wait now; advances the backoff.
    pub(crate) fn next_delay(&mut self) -> Duration {
        let d = self.current;
        let next = self.current.mul_f64(LOG_BACKOFF_MULTIPLIER);
        self.current = next.min(LOG_BACKOFF_MAX_INTERVAL);
        d
    }
}

enum StreamResult {
    Eof,
    Error,
    Canceled,
}

/// One tailed container.
pub(crate) struct Tail {
    pub(crate) client: kube::Client,
    pub(crate) namespace: String,
    pub(crate) pod: String,
    pub(crate) container: String,
    pub(crate) opts: LogOptions,
}

impl Tail {
    fn info(&self) -> String {
        let path = crate::k8s::fqn(&self.namespace, &self.pod);
        if self.container.is_empty() {
            path
        } else {
            format!("{path} ({})", self.container)
        }
    }

    fn api(&self) -> kube::Api<Pod> {
        kube::Api::namespaced(self.client.clone(), &self.namespace)
    }

    /// Pod.shouldStopRetrying.
    async fn should_stop_retrying(&self) -> bool {
        match self.api().get(&self.pod).await {
            Err(_) => true,
            Ok(p) => {
                if p.metadata.deletion_timestamp.is_some() {
                    return true;
                }
                matches!(
                    p.status.and_then(|s| s.phase).as_deref(),
                    Some("Succeeded") | Some("Failed")
                )
            }
        }
    }

    /// Run the tail loop, sending lines to `tx` until EOF, cancellation or
    /// retry exhaustion.
    pub(crate) async fn run(self, tx: mpsc::Sender<Result<LogLine>>) {
        let lp = pod_log_params(&self.opts, &self.container);
        let mut backoff = Backoff::new();
        for _ in 0..LOG_RETRY_COUNT {
            let res = match self.api().log_stream(&self.pod, &lp).await {
                Ok(stream) => self.read(stream, &tx).await,
                Err(e) => {
                    tracing::error!("Stream logs failed for {}: {e}", self.info());
                    StreamResult::Error
                }
            };
            match res {
                StreamResult::Eof | StreamResult::Canceled => return,
                StreamResult::Error => {
                    if self.should_stop_retrying().await {
                        return;
                    }
                    tokio::select! {
                        _ = tx.closed() => return,
                        _ = tokio::time::sleep(backoff.next_delay()) => {}
                    }
                }
            }
        }
        let _ = tx
            .send(Err(anyhow!(
                "failed to maintain log stream after {LOG_RETRY_COUNT} retries"
            )))
            .await;
    }

    async fn read(
        &self,
        stream: impl futures::AsyncBufRead + Unpin,
        tx: &mpsc::Sender<Result<LogLine>>,
    ) -> StreamResult {
        let mut reader = stream;
        let mut buf = Vec::with_capacity(256);
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf).await {
                Ok(0) => {
                    let msg = format!("stream closed: EOF for {}", self.info());
                    let _ = tx.send(Err(anyhow!(msg))).await;
                    return StreamResult::Eof;
                }
                Ok(_) => {
                    let line =
                        parse_line(&buf, &self.pod, &self.container, self.opts.show_timestamp);
                    if tx.send(Ok(line)).await.is_err() {
                        return StreamResult::Canceled;
                    }
                }
                Err(e) => {
                    tracing::debug!("Log stream error for {}, will retry: {e}", self.info());
                    return StreamResult::Error;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn opts() -> LogOptions {
        LogOptions {
            namespace: "ns".into(),
            path: "p".into(),
            ..Default::default()
        }
    }

    #[test]
    fn log_params_tail() {
        let mut o = opts();
        o.since_seconds = Some(-1);
        let lp = pod_log_params(&o, "c1");
        assert_eq!(lp.container.as_deref(), Some("c1"));
        assert!(lp.follow);
        assert!(lp.timestamps);
        assert_eq!(lp.tail_lines, Some(100));
        assert_eq!(lp.since_seconds, None);
        assert_eq!(lp.limit_bytes, None);

        o.since_seconds = Some(0);
        assert_eq!(pod_log_params(&o, "c1").since_seconds, None);

        o.since_seconds = Some(300);
        o.previous = true;
        o.follow = false;
        let lp = pod_log_params(&o, "");
        assert_eq!(lp.container, None);
        assert_eq!(lp.since_seconds, Some(300));
        assert_eq!(lp.tail_lines, Some(100));
        assert!(lp.previous);
        assert!(!lp.follow);
    }

    #[test]
    fn log_params_head() {
        let o = opts();
        let lp = pod_log_params(&o, "c");
        assert!(!lp.follow);
        assert_eq!(lp.tail_lines, None);
        assert_eq!(lp.since_seconds, None);
        assert_eq!(lp.limit_bytes, Some(5000));
    }

    #[test]
    fn parses_lines() {
        let l = parse_line(
            b"2024-01-02T03:04:05.123456789Z hello world\n",
            "p",
            "c",
            true,
        );
        assert_eq!(
            l.timestamp.as_deref(),
            Some("2024-01-02T03:04:05.123456789Z")
        );
        assert_eq!(l.text, "hello world");
        assert_eq!(l.pod, "p");
        assert_eq!(l.container, "c");

        let l = parse_line(b"2024-01-02T03:04:05Z hello\n", "p", "c", false);
        assert_eq!(l.timestamp, None);
        assert_eq!(l.text, "hello");

        let l = parse_line(b"no timestamp here\n", "p", "c", true);
        assert_eq!(l.timestamp, None);
        assert_eq!(l.text, "no timestamp here");

        let l = parse_line(b"2024-01-02T03:04:05Z\n", "p", "c", true);
        assert_eq!(l.text, "2024-01-02T03:04:05Z");

        let l = parse_line(b"2024-01-02T03:04:05Z \xff\n", "p", "c", true);
        assert_eq!(l.text, "\u{fffd}");
        let l = parse_line(b"partial", "p", "c", true);
        assert_eq!(l.text, "partial");
    }

    fn pod(v: serde_json::Value) -> Pod {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn container_selection() {
        let p = pod(json!({
            "metadata": {"name": "p", "annotations": {DEFAULT_CONTAINER_ANNOTATION: "b"}},
            "spec": {
                "initContainers": [{"name": "i"}],
                "containers": [{"name": "a"}, {"name": "b"}],
                "ephemeralContainers": [{"name": "e"}]
            }
        }));
        let mut o = opts();
        assert_eq!(containers_to_tail(&p, &o), vec!["b"]);
        o.container = Some("a".into());
        assert_eq!(containers_to_tail(&p, &o), vec!["a"]);
        o.all_containers = true;
        assert_eq!(containers_to_tail(&p, &o), vec!["i", "a", "b", "e"]);

        let p = pod(json!({
            "metadata": {"name": "p", "annotations": {DEFAULT_CONTAINER_ANNOTATION: "zorg"}},
            "spec": {"containers": [{"name": "a"}, {"name": "b"}]}
        }));
        assert_eq!(containers_to_tail(&p, &opts()), vec!["a", "b"]);
        let p = pod(json!({"metadata": {"name": "p"}, "spec": {"containers": [{"name": "a"}]}}));
        assert_eq!(containers_to_tail(&p, &opts()), vec!["a"]);
    }

    #[test]
    fn backoff_grows_and_caps() {
        let mut b = Backoff::new();
        assert_eq!(b.next_delay(), Duration::from_millis(500));
        assert_eq!(b.next_delay(), Duration::from_millis(750));
        assert_eq!(b.next_delay(), Duration::from_millis(1125));
        for _ in 0..20 {
            b.next_delay();
        }
        assert_eq!(b.next_delay(), Duration::from_secs(15));
    }
}
