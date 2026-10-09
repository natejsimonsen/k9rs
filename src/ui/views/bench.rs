//! Benchmarks: k9s internal/perf (a hey run against a port forward) with a
//! report formatted exactly like hey's default text output.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context as _, Result};
use bytes::Bytes;
use http::header::{HeaderName, HeaderValue, AUTHORIZATION, CONTENT_LENGTH, HOST, USER_AGENT};
use http::{Method, Request, Uri};
use http_body_util::{BodyExt, Full};
use hyper::client::conn::{http1, http2};
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;

use super::util::sanitize_file_name;
use super::BenchSpec;

/// perf.benchTimeout.
const BENCH_TIMEOUT: Duration = Duration::from_secs(120);
const BAR: &str = "■";

/// Byte stream wrapper recording when the request was written and when the
/// first response byte arrived (hey's httptrace WroteRequest /
/// GotFirstResponseByte).
struct Timed<T> {
    inner: T,
    marks: Arc<Mutex<Marks>>,
}

#[derive(Default, Clone, Copy)]
struct Marks {
    last_write: Option<Instant>,
    first_read: Option<Instant>,
}

impl<T: AsyncRead + Unpin> AsyncRead for Timed<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let me = self.get_mut();
        let before = buf.filled().len();
        let r = Pin::new(&mut me.inner).poll_read(cx, buf);
        if matches!(r, Poll::Ready(Ok(()))) && buf.filled().len() > before {
            let mut m = me.marks.lock().expect("marks");
            if m.first_read.is_none() {
                m.first_read = Some(Instant::now());
            }
        }
        r
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for Timed<T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let me = self.get_mut();
        let r = Pin::new(&mut me.inner).poll_write(cx, data);
        if matches!(r, Poll::Ready(Ok(n)) if n > 0) {
            me.marks.lock().expect("marks").last_write = Some(Instant::now());
        }
        r
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

pub(crate) fn tls_connector(h2: bool) -> Result<tokio_rustls::TlsConnector> {
    // Unlike k9s (hey with InsecureSkipVerify), certificates are verified
    // against the OS trust store. Self-signed endpoints fail with a TLS error.
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut roots = rustls::RootCertStore::empty();
    let native = rustls_native_certs::load_native_certs();
    for cert in native.certs {
        // Skip individual unparsable system certs, as rustls-native-certs advises.
        let _ = roots.add(cert);
    }
    let mut cfg = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    cfg.alpn_protocols = if h2 {
        vec![b"h2".to_vec(), b"http/1.1".to_vec()]
    } else {
        vec![b"http/1.1".to_vec()]
    };
    Ok(tokio_rustls::TlsConnector::from(Arc::new(cfg)))
}

enum Sender {
    H1(http1::SendRequest<Full<Bytes>>),
    H2(http2::SendRequest<Full<Bytes>>),
}

impl Sender {
    fn is_closed(&self) -> bool {
        match self {
            Sender::H1(s) => s.is_closed(),
            Sender::H2(s) => s.is_closed(),
        }
    }
}

/// Fully resolved request template.
struct Target {
    method: Method,
    uri: Uri,
    host: String,
    port: u16,
    tls: bool,
    h2: bool,
    headers: Vec<(HeaderName, HeaderValue)>,
    body: Bytes,
}

impl Target {
    fn new(spec: &BenchSpec) -> Result<Self> {
        let uri: Uri = spec
            .url
            .parse()
            .with_context(|| format!("invalid url {}", spec.url))?;
        let tls = match uri.scheme_str() {
            Some("https") => true,
            Some("http") | None => false,
            Some(s) => return Err(anyhow!("unsupported scheme {s}")),
        };
        let host = uri
            .host()
            .ok_or_else(|| anyhow!("no host in url {}", spec.url))?
            .to_string();
        let port = uri.port_u16().unwrap_or(if tls { 443 } else { 80 });
        let method = if spec.method.is_empty() {
            Method::GET
        } else {
            Method::from_bytes(spec.method.as_bytes())?
        };
        let mut headers: Vec<(HeaderName, HeaderValue)> = Vec::new();
        let mut ua: Option<String> = None;
        for (k, v) in &spec.headers {
            let name = HeaderName::from_bytes(k.as_bytes())?;
            if name == USER_AGENT {
                ua = Some(v.clone());
                continue;
            }
            headers.push((name, HeaderValue::from_str(v)?));
        }
        if let Some((u, p)) = &spec.user {
            if !u.is_empty() || !p.is_empty() {
                use base64::Engine;
                let token = base64::engine::general_purpose::STANDARD.encode(format!("{u}:{p}"));
                headers.push((
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Basic {token}"))?,
                ));
            }
        }
        let ua = match ua {
            Some(u) if !u.is_empty() => format!("{u} k9s/{}", crate::version::VERSION),
            _ => format!("k9s/{}", crate::version::VERSION),
        };
        headers.push((USER_AGENT, HeaderValue::from_str(&ua)?));
        Ok(Target {
            method,
            host,
            port,
            tls,
            h2: spec.http2,
            uri,
            headers,
            body: Bytes::from(spec.body.clone()),
        })
    }

    fn request(&self, h2: bool) -> Result<Request<Full<Bytes>>> {
        let uri: Uri = if h2 {
            self.uri.clone()
        } else {
            self.uri
                .path_and_query()
                .map(|p| p.as_str())
                .unwrap_or("/")
                .parse()?
        };
        let mut b = Request::builder().method(self.method.clone()).uri(uri);
        if !h2 {
            let host = match self.uri.port_u16() {
                Some(p) => format!("{}:{p}", self.host),
                None => self.host.clone(),
            };
            b = b.header(HOST, host);
        }
        for (k, v) in &self.headers {
            b = b.header(k, v);
        }
        if !self.body.is_empty() {
            b = b.header(CONTENT_LENGTH, self.body.len());
        }
        Ok(b.body(Full::new(self.body.clone()))?)
    }
}

/// One request outcome (hey requester.result).
#[derive(Clone, Debug, Default)]
pub(crate) struct Sample {
    pub err: Option<String>,
    pub status: u16,
    pub duration: f64,
    pub conn: f64,
    pub dns: f64,
    pub req: f64,
    pub delay: f64,
    pub res: f64,
    pub content_length: i64,
}

struct Worker {
    target: Arc<Target>,
    sender: Option<Sender>,
    marks: Arc<Mutex<Marks>>,
}

impl Worker {
    async fn connect(&mut self) -> Result<(f64, f64)> {
        let t0 = Instant::now();
        let addrs: Vec<_> = tokio::net::lookup_host((self.target.host.as_str(), self.target.port))
            .await?
            .collect();
        let dns = t0.elapsed().as_secs_f64();
        let addr = addrs
            .first()
            .ok_or_else(|| anyhow!("no address for {}", self.target.host))?;
        let tcp = TcpStream::connect(addr).await?;
        tcp.set_nodelay(true)?;
        self.marks = Arc::new(Mutex::new(Marks::default()));
        if self.target.tls {
            let name = rustls::pki_types::ServerName::try_from(self.target.host.clone())?;
            let tls = tls_connector(self.target.h2)?.connect(name, tcp).await?;
            let alpn_h2 = tls.get_ref().1.alpn_protocol() == Some(b"h2");
            let io = TokioIo::new(Timed {
                inner: tls,
                marks: self.marks.clone(),
            });
            if alpn_h2 {
                let (s, conn) = http2::handshake(TokioExecutor::new(), io).await?;
                tokio::spawn(async move {
                    let _ = conn.await;
                });
                self.sender = Some(Sender::H2(s));
            } else {
                let (s, conn) = http1::handshake(io).await?;
                tokio::spawn(async move {
                    let _ = conn.await;
                });
                self.sender = Some(Sender::H1(s));
            }
        } else {
            let io = TokioIo::new(Timed {
                inner: tcp,
                marks: self.marks.clone(),
            });
            let (s, conn) = http1::handshake(io).await?;
            tokio::spawn(async move {
                let _ = conn.await;
            });
            self.sender = Some(Sender::H1(s));
        }
        Ok((dns, t0.elapsed().as_secs_f64()))
    }

    async fn once(&mut self) -> Sample {
        let start = Instant::now();
        match self.try_once(start).await {
            Ok(s) => s,
            Err(e) => {
                self.sender = None;
                Sample {
                    err: Some(e.to_string()),
                    duration: start.elapsed().as_secs_f64(),
                    ..Default::default()
                }
            }
        }
    }

    async fn try_once(&mut self, start: Instant) -> Result<Sample> {
        let mut s = Sample::default();
        if self.sender.as_ref().is_none_or(Sender::is_closed) {
            let (dns, conn) = self.connect().await?;
            s.dns = dns;
            s.conn = conn;
        }
        let got_conn = Instant::now();
        *self.marks.lock().expect("marks") = Marks::default();
        let resp = match self.sender.as_mut().expect("sender") {
            Sender::H1(snd) => {
                snd.ready().await?;
                snd.send_request(self.target.request(false)?).await?
            }
            Sender::H2(snd) => {
                snd.ready().await?;
                snd.send_request(self.target.request(true)?).await?
            }
        };
        let status = resp.status().as_u16();
        let cl = resp
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(-1);
        let _ = resp.into_body().collect().await?;
        let end = Instant::now();
        let m = *self.marks.lock().expect("marks");
        let wrote = m.last_write.unwrap_or(got_conn);
        let first = m.first_read.unwrap_or(end).max(wrote);
        s.req = wrote.saturating_duration_since(got_conn).as_secs_f64();
        s.delay = first.saturating_duration_since(wrote).as_secs_f64();
        s.res = end.saturating_duration_since(first).as_secs_f64();
        s.duration = end.saturating_duration_since(start).as_secs_f64();
        s.status = status;
        s.content_length = cl;
        Ok(s)
    }
}

/// Run the load: `C` workers each issuing `N/C` requests (hey semantics).
pub(crate) async fn load(spec: &BenchSpec) -> Result<(Vec<Sample>, f64)> {
    let target = Arc::new(Target::new(spec)?);
    let c = spec.concurrency.max(1);
    let per = spec.requests / c;
    let deadline = Instant::now() + BENCH_TIMEOUT;
    let start = Instant::now();
    let mut tasks = Vec::with_capacity(c);
    for _ in 0..c {
        let target = target.clone();
        tasks.push(tokio::spawn(async move {
            let mut w = Worker {
                target,
                sender: None,
                marks: Arc::new(Mutex::new(Marks::default())),
            };
            let mut out = Vec::with_capacity(per);
            for _ in 0..per {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                match tokio::time::timeout(left, w.once()).await {
                    Ok(s) => out.push(s),
                    Err(_) => break,
                }
            }
            out
        }));
    }
    let mut all = Vec::new();
    for t in tasks {
        all.extend(t.await.unwrap_or_default());
    }
    Ok((all, start.elapsed().as_secs_f64()))
}

fn fmt_num(v: f64) -> String {
    format!("{v:4.4}")
}

/// hey's default text report.
pub(crate) fn report(samples: &[Sample], total: f64) -> String {
    let ok: Vec<&Sample> = samples.iter().filter(|s| s.err.is_none()).collect();
    let n = ok.len() as f64;
    let num_res = samples.len() as f64;
    let sum = |f: fn(&Sample) -> f64| ok.iter().map(|s| f(s)).sum::<f64>();
    let avg_total = sum(|s| s.duration) / n;
    let avg_conn = sum(|s| s.conn) / n;
    let avg_dns = sum(|s| s.dns) / n;
    let avg_req = sum(|s| s.req) / n;
    let avg_delay = sum(|s| s.delay) / n;
    let avg_res = sum(|s| s.res) / n;
    let rps = num_res / total;
    let size_total: i64 = ok
        .iter()
        .filter(|s| s.content_length > 0)
        .map(|s| s.content_length)
        .sum();
    let sorted = |f: fn(&Sample) -> f64| {
        let mut v: Vec<f64> = ok.iter().map(|s| f(s)).collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        v
    };
    let lats = sorted(|s| s.duration);
    let minmax = |v: &[f64]| {
        (
            v.first().copied().unwrap_or(0.0),
            v.last().copied().unwrap_or(0.0),
        )
    };
    let (fastest, slowest) = minmax(&lats);

    let mut out = String::new();
    out += "\nSummary:\n";
    out += &format!("  Total:\t{} secs\n", fmt_num(total));
    out += &format!("  Slowest:\t{} secs\n", fmt_num(slowest));
    out += &format!("  Fastest:\t{} secs\n", fmt_num(fastest));
    out += &format!("  Average:\t{} secs\n", fmt_num(avg_total));
    out += &format!("  Requests/sec:\t{}\n", fmt_num(rps));
    out += "  ";
    if size_total > 0 {
        let size_req = if ok.is_empty() {
            0
        } else {
            size_total / ok.len() as i64
        };
        out += &format!("\n  Total data:\t{size_total} bytes\n  Size/request:\t{size_req} bytes");
    }
    out += "\n\nResponse time histogram:\n";
    if !lats.is_empty() {
        out += &histogram(&lats, fastest, slowest);
    }
    out += "\n\nLatency distribution:";
    if !lats.is_empty() {
        for (p, l) in latencies(&lats) {
            out += &format!("\n  {p}% in {} secs", fmt_num(l));
        }
    }
    out += "\n\nDetails (average, fastest, slowest):\n";
    let row = |name: &str, avg: f64, v: Vec<f64>| {
        let (lo, hi) = minmax(&v);
        format!(
            "  {name}:\t{} secs, {} secs, {} secs\n",
            fmt_num(avg),
            fmt_num(lo),
            fmt_num(hi)
        )
    };
    out += &row("DNS+dialup", avg_conn, sorted(|s| s.conn));
    out += &row("DNS-lookup", avg_dns, sorted(|s| s.dns));
    out += &row("req write", avg_req, sorted(|s| s.req));
    out += &row("resp wait", avg_delay, sorted(|s| s.delay));
    out += &row("resp read", avg_res, sorted(|s| s.res));
    out += "\nStatus code distribution:";
    let mut codes: BTreeMap<u16, usize> = BTreeMap::new();
    for s in &ok {
        *codes.entry(s.status).or_default() += 1;
    }
    for (c, k) in codes {
        out += &format!("\n  [{c}]\t{k} responses");
    }
    out += "\n\n";
    let mut errs: BTreeMap<String, usize> = BTreeMap::new();
    for s in samples {
        if let Some(e) = &s.err {
            *errs.entry(e.clone()).or_default() += 1;
        }
    }
    if !errs.is_empty() {
        out += "Error distribution:";
        for (e, k) in errs {
            out += &format!("\n  [{k}]\t{e}");
        }
    }
    out += "\n";
    out
}

/// hey report.histogram + print histogram.
fn histogram(lats: &[f64], fastest: f64, slowest: f64) -> String {
    let bc = 10;
    let mut buckets = vec![0f64; bc + 1];
    let mut counts = vec![0usize; bc + 1];
    let bs = (slowest - fastest) / bc as f64;
    for (i, b) in buckets.iter_mut().enumerate().take(bc) {
        *b = fastest + bs * i as f64;
    }
    buckets[bc] = slowest;
    let (mut bi, mut i) = (0, 0);
    while i < lats.len() {
        if lats[i] <= buckets[bi] {
            i += 1;
            counts[bi] += 1;
        } else if bi < buckets.len() - 1 {
            bi += 1;
        } else {
            break;
        }
    }
    let max = counts.iter().copied().max().unwrap_or(0);
    let mut out = String::new();
    for (mark, count) in buckets.iter().zip(counts.iter()) {
        let bar = (count * 40 + max / 2).checked_div(max).unwrap_or(0);
        out += &format!("  {mark:4.3} [{count}]\t|{}\n", BAR.repeat(bar));
    }
    out
}

/// hey report.latencies.
fn latencies(lats: &[f64]) -> Vec<(usize, f64)> {
    let pctls = [10usize, 25, 50, 75, 90, 95, 99];
    let mut data = [0f64; 7];
    let mut j = 0;
    for (i, l) in lats.iter().enumerate() {
        if j >= pctls.len() {
            break;
        }
        let current = i * 100 / lats.len();
        if current >= pctls[j] {
            data[j] = *l;
            j += 1;
        }
    }
    pctls
        .iter()
        .zip(data.iter())
        .map(|(p, d)| if *d > 0.0 { (*p, *d) } else { (0, 0.0) })
        .collect()
}

/// k9s config.AppBenchmarksDir (`$XDG_STATE_HOME/k9s/benchmarks`).
pub(crate) fn benchmarks_dir() -> PathBuf {
    if let Ok(d) = std::env::var("XDG_STATE_HOME") {
        if !d.is_empty() {
            return PathBuf::from(d).join("k9s").join("benchmarks");
        }
    }
    dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .unwrap_or_else(std::env::temp_dir)
        .join("k9s")
        .join("benchmarks")
}

/// Benchmark.save file name: `<ns>_<name>_<unixnano>.txt`.
fn bench_file(name: &str) -> String {
    let (ns, n) = crate::k8s::namespaced(name);
    let n = n.replace(['|', ':'], "_");
    let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
    format!("{ns}_{n}_{nanos}.txt")
}

/// Run and save under `<base>/<cluster>/<context>/`.
pub(crate) async fn run_into(
    spec: BenchSpec,
    base: &Path,
    cluster: &str,
    context: &str,
    name: &str,
) -> Result<PathBuf> {
    let (samples, total) = load(&spec).await?;
    let body = report(&samples, total);
    let dir = base
        .join(sanitize_file_name(cluster))
        .join(sanitize_file_name(context));
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(bench_file(name));
    std::fs::write(&path, body)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::Full;
    use hyper::server::conn::http1 as server;
    use hyper::service::service_fn;
    use std::sync::atomic::{AtomicUsize, Ordering};

    async fn serve() -> (String, Arc<AtomicUsize>, Arc<Mutex<Vec<String>>>) {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let (h, s) = (hits.clone(), seen.clone());
        tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = l.accept().await else {
                    return;
                };
                let (h, s) = (h.clone(), s.clone());
                tokio::spawn(async move {
                    let svc = service_fn(move |req: Request<hyper::body::Incoming>| {
                        let (h, s) = (h.clone(), s.clone());
                        async move {
                            h.fetch_add(1, Ordering::SeqCst);
                            let hdr = |k: &str| {
                                req.headers()
                                    .get(k)
                                    .and_then(|v| v.to_str().ok())
                                    .unwrap_or("")
                                    .to_string()
                            };
                            s.lock().unwrap().push(format!(
                                "{} {} ua={} auth={} x={}",
                                req.method(),
                                req.uri(),
                                hdr("user-agent"),
                                hdr("authorization"),
                                hdr("x-k9s")
                            ));
                            let body = req.into_body().collect().await.unwrap().to_bytes();
                            let status = if body.as_ref() == b"fail" { 500 } else { 200 };
                            Ok::<_, std::convert::Infallible>(
                                hyper::Response::builder()
                                    .status(status)
                                    .body(Full::new(Bytes::from_static(b"hello")))
                                    .unwrap(),
                            )
                        }
                    });
                    let _ = server::Builder::new()
                        .serve_connection(TokioIo::new(tcp), svc)
                        .await;
                });
            }
        });
        (format!("http://{addr}/bench?x=1"), hits, seen)
    }

    fn spec(url: String) -> BenchSpec {
        BenchSpec {
            url,
            method: "POST".into(),
            concurrency: 2,
            requests: 5,
            headers: vec![("X-K9s".into(), "yes".into())],
            body: "ping".into(),
            user: Some(("fred".into(), "secret".into())),
            http2: false,
        }
    }

    #[tokio::test]
    async fn runs_load_and_writes_report() {
        let (url, hits, seen) = serve().await;
        let dir = tempfile::tempdir().unwrap();
        let path = run_into(
            spec(url),
            dir.path(),
            "c:1",
            "ctx/1",
            "default/nginx|nginx:8080:80",
        )
        .await
        .unwrap();
        // N/C requests per worker: 2 * (5/2) = 4.
        assert_eq!(hits.load(Ordering::SeqCst), 4);
        let s = seen.lock().unwrap()[0].clone();
        assert_eq!(
            s,
            format!(
                "POST /bench?x=1 ua=k9s/{} auth=Basic ZnJlZDpzZWNyZXQ= x=yes",
                crate::version::VERSION
            )
        );
        let rel = path
            .strip_prefix(dir.path())
            .unwrap()
            .to_string_lossy()
            .to_string();
        assert!(
            rel.starts_with("c-1/ctx-1/default_nginx_nginx_8080_80_"),
            "{rel}"
        );
        assert!(rel.ends_with(".txt"));
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.starts_with("\nSummary:\n  Total:\t"));
        assert!(body.contains("\n  Total data:\t20 bytes\n  Size/request:\t5 bytes\n"));
        assert!(body.contains("\n\nResponse time histogram:\n  "));
        assert!(body.contains("\n\nLatency distribution:\n  10% in "));
        assert!(body.contains("\nDetails (average, fastest, slowest):\n  DNS+dialup:\t"));
        assert!(body.contains("  resp read:\t"));
        assert!(body.contains("\nStatus code distribution:\n  [200]\t4 responses\n\n"));
        assert!(!body.contains("Error distribution"));
    }

    #[tokio::test]
    async fn reports_status_and_errors() {
        let (url, _hits, _seen) = serve().await;
        let mut sp = spec(url);
        sp.body = "fail".into();
        sp.concurrency = 1;
        sp.requests = 2;
        let (samples, total) = load(&sp).await.unwrap();
        let r = report(&samples, total);
        assert!(r.contains("[500]\t2 responses"));
        let mut sp = spec("http://127.0.0.1:1/".into());
        sp.concurrency = 1;
        sp.requests = 1;
        let (samples, total) = load(&sp).await.unwrap();
        let r = report(&samples, total);
        assert!(r.contains("Error distribution:\n  [1]\t"), "{r}");
    }

    #[test]
    fn report_format_is_hey_like() {
        let mk = |d: f64| Sample {
            status: 200,
            duration: d,
            conn: 0.001,
            dns: 0.0005,
            req: 0.0001,
            delay: d / 2.0,
            res: d / 4.0,
            content_length: -1,
            err: None,
        };
        let samples: Vec<Sample> = (1..=10).map(|i| mk(i as f64 / 100.0)).collect();
        let r = report(&samples, 1.0);
        let expect_head = "\nSummary:\n  Total:\t1.0000 secs\n  Slowest:\t0.1000 secs\n  Fastest:\t0.0100 secs\n  Average:\t0.0550 secs\n  Requests/sec:\t10.0000\n  \n\nResponse time histogram:\n  0.010 [1]\t|";
        assert!(r.starts_with(expect_head), "{r}");
        assert!(r.contains("  0.100 [1]\t|"));
        assert!(r.contains("\n  10% in 0.0200 secs\n  25% in 0.0400 secs\n  50% in 0.0600 secs"));
        assert!(r.contains("  DNS+dialup:\t0.0010 secs, 0.0010 secs, 0.0010 secs\n"));
        assert!(r.ends_with("\nStatus code distribution:\n  [200]\t10 responses\n\n\n"));
        assert_eq!(histogram(&[1.0, 1.0], 1.0, 1.0).lines().count(), 11);
    }
}
