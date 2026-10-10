//! client-go / apimachinery wording for kube-rs request errors, so flashes,
//! dialogs and logs read like k9s's.

use std::error::Error as StdError;

/// A request as client-go reports it: method and full URL.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Req<'a> {
    pub method: &'a str,
    pub url: &'a str,
    /// The kubeconfig exec plugin command, if any.
    pub exec: Option<&'a str>,
}

/// The error text client-go would return for `e` on `req`.
pub(crate) fn message(e: &kube::Error, req: Option<Req<'_>>) -> String {
    match e {
        kube::Error::Api(s) => status_message(s, req),
        kube::Error::Auth(a) => with_url(
            req,
            &format!("getting credentials: {}", auth(a, req.and_then(|r| r.exec))),
        ),
        other => with_url(req, &transport(other, req)),
    }
}

/// `context deadline exceeded` on `req` (client-go request timeout).
pub(crate) fn deadline(req: Option<Req<'_>>) -> String {
    with_url(req, "context deadline exceeded")
}

/// net/http url.Error: `Get "https://...": cause`.
fn with_url(req: Option<Req<'_>>, cause: &str) -> String {
    match req.filter(|r| !r.url.is_empty()) {
        Some(r) => format!("{} {:?}: {cause}", url_op(r.method), r.url),
        None => cause.to_string(),
    }
}

/// net/http urlErrorOp: `GET` -> `Get`.
pub(crate) fn url_op(method: &str) -> String {
    let mut cs = method.chars();
    match cs.next() {
        Some(c) => c
            .to_uppercase()
            .chain(cs.as_str().to_lowercase().chars())
            .collect(),
        None => String::new(),
    }
}

/// kube-rs reconstructs non-Status error bodies with this reason.
const UNPARSED: &str = "Failed to parse error data";

/// StatusError text: the server message, or errors.NewGenericServerResponse
/// for bodies that are not a Status.
fn status_message(s: &kube::core::Status, req: Option<Req<'_>>) -> String {
    if s.reason != UNPARSED && !s.message.is_empty() {
        return s.message.clone();
    }
    let body = if s.reason == UNPARSED {
        s.message.trim().to_string()
    } else {
        String::new()
    };
    let server_msg = if body.is_empty() {
        "unknown".to_string()
    } else {
        body
    };
    let method = req.map(|r| r.method).unwrap_or("GET");
    let (resource, name) = req.map(|r| resource_of(r.url)).unwrap_or_default();
    generic(s.code, method, &resource, &name, &server_msg)
}

/// errors.NewGenericServerResponse message.
pub(crate) fn generic(
    code: u16,
    verb: &str,
    resource: &str,
    name: &str,
    server_msg: &str,
) -> String {
    let mut msg = match code {
        409 => "the server reported a conflict".to_string(),
        404 => "the server could not find the requested resource".to_string(),
        400 => "the server rejected our request for an unknown reason".to_string(),
        401 => "the server has asked for the client to provide credentials".to_string(),
        403 => server_msg.to_string(),
        406 if server_msg.is_empty() || server_msg == "unknown" => {
            "the server was unable to respond with a content type that the client supports".into()
        }
        406 | 415 => server_msg.to_string(),
        405 => "the server does not allow this method on the requested resource".into(),
        422 => "the server rejected our request due to an error in our request".into(),
        503 => "the server is currently unable to handle the request".into(),
        504 => "the server was unable to return a response in the time allotted, but may still be processing the request".into(),
        429 => "the server has received too many requests and has asked us to try again later".into(),
        c if c >= 500 => format!(
            "an error on the server ({server_msg:?}) has prevented the request from succeeding"
        ),
        c => format!("the server responded with the status code {c} but did not return more information"),
    };
    if !resource.is_empty() {
        let verb = verb.to_lowercase();
        msg = if name.is_empty() {
            format!("{msg} ({verb} {resource})")
        } else {
            format!("{msg} ({verb} {resource} {name})")
        };
    }
    msg
}

/// `(group.resource, name)` from an API path (schema.GroupResource.String).
fn resource_of(url: &str) -> (String, String) {
    let path = url
        .split_once("://")
        .map(|(_, r)| r.split_once('/').map(|(_, p)| p).unwrap_or(""))
        .unwrap_or(url.trim_start_matches('/'));
    let path = path.split('?').next().unwrap_or("");
    let mut seg: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let group = match seg.first() {
        Some(&"api") if seg.len() >= 2 => {
            seg.drain(..2);
            ""
        }
        Some(&"apis") if seg.len() >= 3 => {
            let g = seg[1];
            seg.drain(..3);
            g
        }
        _ => return Default::default(),
    };
    if seg.first() == Some(&"namespaces") && seg.len() >= 3 {
        seg.drain(..2);
    }
    let Some(res) = seg.first() else {
        return Default::default();
    };
    let gr = if group.is_empty() {
        res.to_string()
    } else {
        format!("{res}.{group}")
    };
    (gr, seg.get(1).map(|s| s.to_string()).unwrap_or_default())
}

/// exec.Authenticator errors.
fn auth(a: &kube::client::AuthError, exec: Option<&str>) -> String {
    use kube::client::AuthError as A;
    match a {
        A::AuthExecRun { cmd, status, .. } => format!(
            "exec: executable {} failed with exit code {}",
            exec_program(cmd),
            status.code().unwrap_or(-1)
        ),
        A::AuthExecStart(e) if e.kind() == std::io::ErrorKind::NotFound => {
            format!(
                "exec: executable {} not found{INSTALL_HINT}",
                exec.unwrap_or_default()
            )
        }
        other => format!("exec: {other}"),
    }
}

/// exec.go installHintVerboseHelp.
const INSTALL_HINT: &str = "\n\nIt looks like you are trying to use a client-go credential plugin that is not installed.\n\nTo learn more about this feature, consult the documentation available at:\n      https://kubernetes.io/docs/reference/access-authn-authz/authentication/#client-go-credential-plugins";

/// The program of a `std::process::Command` Debug string
/// (`ENV="..." "/path" "arg"`).
fn exec_program(cmd: &str) -> String {
    let mut rest = cmd.trim();
    loop {
        let Some(tok) = next_token(rest) else {
            return cmd.to_string();
        };
        let (t, r) = tok;
        rest = r;
        // KEY="value" environment prefixes.
        if !t.starts_with('"') && t.contains('=') {
            continue;
        }
        return t.trim_matches('"').replace("\\\"", "\"");
    }
}

/// Next whitespace separated token, keeping quoted runs (with `\"`) whole.
fn next_token(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start();
    if s.is_empty() {
        return None;
    }
    let b = s.as_bytes();
    let (mut i, mut quoted) = (0, false);
    while i < b.len() {
        match b[i] {
            b'\\' if quoted => i += 1,
            b'"' => quoted = !quoted,
            b' ' if !quoted => break,
            _ => {}
        }
        i += 1;
    }
    let i = i.min(s.len());
    Some((&s[..i], &s[i..]))
}

/// Dial/TLS causes as Go's net and crypto/tls packages word them.
fn transport(e: &kube::Error, req: Option<Req<'_>>) -> String {
    let (host, port) = req.map(|r| host_port(r.url)).unwrap_or_default();
    let addr = if host.is_empty() {
        String::new()
    } else {
        format!(" {host}:{port}")
    };
    let mut src: Option<&(dyn StdError + 'static)> = Some(e);
    let mut dns = false;
    while let Some(err) = src {
        if let Some(x) = err.downcast_ref::<super::exec::ExecError>() {
            return x.to_string();
        }
        if err.to_string().starts_with("dns error") {
            dns = true;
        }
        if let Some(t) = err.downcast_ref::<rustls::Error>() {
            return tls(t, &host);
        }
        if let Some(io) = err.downcast_ref::<std::io::Error>() {
            // A wrapped error (TLS inside the io error): look inside first.
            if let Some(inner) = io.get_ref() {
                if inner.downcast_ref::<std::io::Error>().is_some()
                    || inner.downcast_ref::<rustls::Error>().is_some()
                {
                    src = Some(inner);
                    continue;
                }
            }
            if dns {
                return format!("dial tcp: lookup {host}: no such host");
            }
            return match io.kind() {
                std::io::ErrorKind::ConnectionRefused => {
                    format!("dial tcp{addr}: connect: connection refused")
                }
                std::io::ErrorKind::TimedOut => format!("dial tcp{addr}: i/o timeout"),
                std::io::ErrorKind::ConnectionReset => {
                    format!("read tcp{addr}: read: connection reset by peer")
                }
                _ => format!("dial tcp{addr}: {}", errno_text(io)),
            };
        }
        src = err.source();
    }
    if dns {
        return format!("dial tcp: lookup {host}: no such host");
    }
    e.to_string()
}

/// Go syscall error text: lower case, no `(os error N)`.
fn errno_text(e: &std::io::Error) -> String {
    let mut m = e.to_string();
    if let Some(i) = m.find(" (os error ") {
        m.truncate(i);
    }
    let mut cs = m.chars();
    match cs.next() {
        Some(c) => c.to_lowercase().chain(cs).collect(),
        None => m,
    }
}

/// crypto/x509 errors behind `tls: failed to verify certificate:`.
fn tls(e: &rustls::Error, host: &str) -> String {
    use rustls::CertificateError as C;
    let x509 = match e {
        rustls::Error::InvalidCertificate(c) => match c {
            C::UnknownIssuer => "x509: certificate signed by unknown authority".to_string(),
            C::NotValidForName => format!("x509: certificate is not valid for {host}"),
            C::NotValidForNameContext { presented, .. } if presented.is_empty() => {
                format!("x509: certificate is not valid for any names, but wanted to match {host}")
            }
            C::NotValidForNameContext { presented, .. } => format!(
                "x509: certificate is valid for {}, not {host}",
                presented.join(", ")
            ),
            C::Expired | C::NotValidYet => {
                "x509: certificate has expired or is not yet valid".to_string()
            }
            // Platform (macOS Security framework) verdicts; Go appends the
            // system's text the same way.
            C::Other(o) => {
                let m = o.to_string();
                let detail = match m.rsplit_once(": -") {
                    Some((d, code)) if code.bytes().all(|b| b.is_ascii_digit()) => d.to_string(),
                    _ => m.clone(),
                };
                if m.contains("expired") || m.contains("not yet valid") {
                    format!("x509: certificate has expired or is not yet valid: {detail}")
                } else if m.contains("not trusted") {
                    "x509: certificate signed by unknown authority".into()
                } else {
                    format!("x509: {detail}")
                }
            }
            C::ExpiredContext { time, not_after } => format!(
                "x509: certificate has expired or is not yet valid: current time {} is after {}",
                rfc3339(time.as_secs()),
                rfc3339(not_after.as_secs())
            ),
            C::NotValidYetContext { time, not_before } => format!(
                "x509: certificate has expired or is not yet valid: current time {} is before {}",
                rfc3339(time.as_secs()),
                rfc3339(not_before.as_secs())
            ),
            other => format!("x509: {other:?}"),
        },
        other => return format!("remote error: tls: {other}"),
    };
    format!("tls: failed to verify certificate: {x509}")
}

fn rfc3339(secs: u64) -> String {
    chrono::DateTime::from_timestamp(secs as i64, 0)
        .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default()
}

/// `(host, port)` of a URL, with the scheme's default port.
fn host_port(url: &str) -> (String, String) {
    let (scheme, rest) = url.split_once("://").unwrap_or(("https", url));
    let auth = rest.split(['/', '?']).next().unwrap_or("");
    let default = if scheme == "http" { "80" } else { "443" };
    if let Some(r) = auth.strip_prefix('[') {
        // [ipv6]:port
        let (h, p) = r.split_once(']').unwrap_or((r, ""));
        let p = p.strip_prefix(':').unwrap_or(default);
        return (format!("[{h}]"), p.to_string());
    }
    match auth.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.to_string()),
        None => (auth.to_string(), default.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const R: Req<'static> = Req {
        method: "GET",
        url: "https://127.0.0.1:1/version?timeout=2m0s",
        exec: Some("aws"),
    };

    fn status(code: u16, msg: &str, reason: &str) -> kube::Error {
        let mut s = kube::core::Status::failure(msg, reason).with_code(code);
        s.reason = reason.into();
        kube::Error::Api(s.boxed())
    }

    #[test]
    fn statuses() {
        // Status bodies keep the server message (401 Unauthorized, 403, 404).
        assert_eq!(
            message(&status(401, "Unauthorized", "Unauthorized"), Some(R)),
            "Unauthorized"
        );
        let forbidden = "pods is forbidden: User \"u\" cannot list resource \"pods\"";
        assert_eq!(
            message(&status(403, forbidden, "Forbidden"), None),
            forbidden
        );
        assert_eq!(
            message(&status(404, "pods \"x\" not found", "NotFound"), None),
            "pods \"x\" not found"
        );
        // Non-Status bodies: NewGenericServerResponse with the resource.
        let r = Req {
            method: "GET",
            url: "https://h/apis/apps/v1/namespaces/ns/deployments/web",
            exec: None,
        };
        assert_eq!(
            message(&status(404, "404 page not found\n", UNPARSED), Some(r)),
            "the server could not find the requested resource (get deployments.apps web)"
        );
        let r = Req {
            method: "DELETE",
            url: "https://h/api/v1/namespaces/ns/pods",
            exec: None,
        };
        assert_eq!(
            message(&status(401, "", UNPARSED), Some(r)),
            "the server has asked for the client to provide credentials (delete pods)"
        );
        assert_eq!(message(&status(403, "nope", UNPARSED), None), "nope");
        assert_eq!(
            message(&status(500, "boom", UNPARSED), None),
            "an error on the server (\"boom\") has prevented the request from succeeding"
        );
        assert_eq!(
            generic(418, "GET", "", "", "x"),
            "the server responded with the status code 418 but did not return more information"
        );
    }

    #[derive(Debug)]
    struct Wrap(&'static str, Box<dyn StdError + Send + Sync>);
    impl std::fmt::Display for Wrap {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(self.0)
        }
    }
    impl StdError for Wrap {
        fn source(&self) -> Option<&(dyn StdError + 'static)> {
            Some(self.1.as_ref())
        }
    }

    fn svc(e: Box<dyn StdError + Send + Sync>) -> kube::Error {
        kube::Error::Service(e)
    }

    #[test]
    fn transports() {
        let refused = std::io::Error::from(std::io::ErrorKind::ConnectionRefused);
        let e = svc(Box::new(Wrap("client error (Connect)", Box::new(refused))));
        assert_eq!(
            message(&e, Some(R)),
            "Get \"https://127.0.0.1:1/version?timeout=2m0s\": dial tcp 127.0.0.1:1: connect: connection refused"
        );
        let lookup = std::io::Error::other("failed to lookup address information");
        let e = svc(Box::new(Wrap(
            "client error (Connect)",
            Box::new(Wrap("dns error", Box::new(lookup))),
        )));
        let r = Req {
            method: "GET",
            url: "https://api.nope.example/api",
            exec: None,
        };
        assert_eq!(
            message(&e, Some(r)),
            "Get \"https://api.nope.example/api\": dial tcp: lookup api.nope.example: no such host"
        );
        let timeout = std::io::Error::from(std::io::ErrorKind::TimedOut);
        let e = svc(Box::new(Wrap("connect", Box::new(timeout))));
        assert_eq!(
            message(
                &e,
                Some(Req {
                    method: "POST",
                    url: "https://10.0.0.1/apis",
                    exec: None,
                })
            ),
            "Post \"https://10.0.0.1/apis\": dial tcp 10.0.0.1:443: i/o timeout"
        );
        assert_eq!(
            deadline(Some(R)),
            "Get \"https://127.0.0.1:1/version?timeout=2m0s\": context deadline exceeded"
        );
    }

    #[test]
    fn tls_errors() {
        let x = |c: rustls::CertificateError| {
            let io = std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                rustls::Error::InvalidCertificate(c),
            );
            message(&svc(Box::new(Wrap("connect", Box::new(io)))), Some(R))
        };
        assert_eq!(
            x(rustls::CertificateError::UnknownIssuer),
            "Get \"https://127.0.0.1:1/version?timeout=2m0s\": tls: failed to verify certificate: x509: certificate signed by unknown authority"
        );
        assert!(x(rustls::CertificateError::Expired)
            .ends_with("x509: certificate has expired or is not yet valid"));
        let e = rustls::CertificateError::ExpiredContext {
            time: rustls::pki_types::UnixTime::since_unix_epoch(std::time::Duration::from_secs(
                1_700_000_000,
            )),
            not_after: rustls::pki_types::UnixTime::since_unix_epoch(
                std::time::Duration::from_secs(1_600_000_000),
            ),
        };
        assert!(x(e).ends_with("current time 2023-11-14T22:13:20Z is after 2020-09-13T12:26:40Z"));
        let e = rustls::CertificateError::NotValidForNameContext {
            expected: rustls::pki_types::ServerName::try_from("127.0.0.1").unwrap(),
            presented: vec!["a.example".into(), "b.example".into()],
        };
        assert!(
            x(e).ends_with("x509: certificate is valid for a.example, b.example, not 127.0.0.1")
        );
    }

    /// macOS platform verifier verdicts (live: badssl.com expired/self-signed).
    #[test]
    fn platform_tls_errors() {
        let other = |m: &'static str| {
            let e = rustls::Error::InvalidCertificate(rustls::CertificateError::Other(
                rustls::OtherError(std::sync::Arc::new(std::io::Error::other(m))),
            ));
            let io = std::io::Error::new(std::io::ErrorKind::InvalidData, e);
            let io = std::io::Error::other(io);
            message(&svc(Box::new(Wrap("connect", Box::new(io)))), None)
        };
        assert_eq!(
            other("“*.badssl.com” certificate is expired: -67818"),
            "tls: failed to verify certificate: x509: certificate has expired or is not yet valid: “*.badssl.com” certificate is expired"
        );
        assert_eq!(
            other("“*.badssl.com” certificate is not trusted: -67843"),
            "tls: failed to verify certificate: x509: certificate signed by unknown authority"
        );
    }

    #[test]
    fn exec_plugin() {
        let cmd = r#"KUBERNETES_EXEC_INFO="{\"kind\":\"ExecCredential\"}" "/usr/bin/aws-iam-authenticator" "token" "-i" "c""#;
        assert_eq!(exec_program(cmd), "/usr/bin/aws-iam-authenticator");
        assert_eq!(exec_program(r#""aws" "eks""#), "aws");
        let out = std::process::Command::new("sh")
            .args(["-c", "exit 1"])
            .output()
            .unwrap();
        let e = kube::Error::Auth(kube::client::AuthError::AuthExecRun {
            cmd: cmd.into(),
            status: out.status,
            out,
        });
        assert_eq!(
            message(&e, Some(R)),
            "Get \"https://127.0.0.1:1/version?timeout=2m0s\": getting credentials: exec: executable /usr/bin/aws-iam-authenticator failed with exit code 1"
        );
        let e = kube::Error::Auth(kube::client::AuthError::AuthExecStart(
            std::io::Error::from(std::io::ErrorKind::NotFound),
        ));
        assert!(message(&e, Some(R)).ends_with(
            "getting credentials: exec: executable aws not found\n\nIt looks like you are trying to use a client-go credential plugin that is not installed.\n\nTo learn more about this feature, consult the documentation available at:\n      https://kubernetes.io/docs/reference/access-authn-authz/authentication/#client-go-credential-plugins"
        ));
        assert_eq!(url_op("PATCH"), "Patch");
        assert_eq!(
            host_port("https://[::1]:6443/x"),
            ("[::1]".into(), "6443".into())
        );
        assert_eq!(host_port("http://h/x"), ("h".into(), "80".into()));
    }
}
