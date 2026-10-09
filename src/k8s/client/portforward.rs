//! Port forwarding (k9s dao.PortForwarder) over kube's websocket portforward.

use anyhow::{anyhow, Context as _, Result};
use k8s_openapi::api::core::v1::Pod;
use kube::api::Portforwarder;
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

use crate::k8s::PortForwardSpec;

/// Resolve the bind address (`localhost` and blanks bind loopback).
pub(crate) fn bind_addr(spec: &PortForwardSpec) -> String {
    let host = match spec.address.trim() {
        "" => "localhost",
        a => a,
    };
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{}", spec.local_port)
    } else {
        format!("{host}:{}", spec.local_port)
    }
}

/// Bind the local listener.
pub(crate) async fn bind(spec: &PortForwardSpec) -> Result<TcpListener> {
    let addr = bind_addr(spec);
    TcpListener::bind(&addr)
        .await
        .with_context(|| format!("unable to listen on {addr}"))
}

async fn connect(api: &kube::Api<Pod>, pod: &str, port: u16) -> Result<Portforwarder> {
    api.portforward(pod, &[port])
        .await
        .map_err(|e| anyhow!("{}", super::kerr_msg(&e)))
}

/// Pipe one local connection through an established forwarder.
async fn pipe(mut conn: TcpStream, mut pf: Portforwarder, port: u16) -> Result<()> {
    let mut upstream = pf
        .take_stream(port)
        .ok_or_else(|| anyhow!("port {port} not forwarded"))?;
    let err = pf.take_error(port);
    tokio::io::copy_bidirectional(&mut conn, &mut upstream).await?;
    drop(upstream);
    if let Some(err) = err {
        if let Some(msg) = err.await {
            return Err(anyhow!(msg));
        }
    }
    pf.join().await.map_err(|e| anyhow!("{e}"))
}

/// Start forwarding. A first websocket session is opened up front so that
/// connection errors are reported to the caller (like client-go which dials
/// before returning); it serves the first local connection.
pub(crate) async fn start(
    api: kube::Api<Pod>,
    spec: PortForwardSpec,
    cancel: CancellationToken,
) -> Result<()> {
    let listener = bind(&spec).await?;
    let pod = spec.pod.clone();
    let port = spec.container_port;
    let first = connect(&api, &pod, port).await?;
    tokio::spawn(async move {
        let mut warm = Some(first);
        loop {
            let accepted = tokio::select! {
                _ = cancel.cancelled() => break,
                a = listener.accept() => a,
            };
            let conn = match accepted {
                Ok((c, _)) => c,
                Err(e) => {
                    tracing::warn!("port-forward {} accept failed: {e}", spec.id());
                    continue;
                }
            };
            let pf = match warm.take() {
                Some(pf) => pf,
                None => match connect(&api, &pod, port).await {
                    Ok(pf) => pf,
                    Err(e) => {
                        tracing::error!("port-forward {} failed: {e}", spec.id());
                        continue;
                    }
                },
            };
            let id = spec.id();
            let token = cancel.clone();
            tokio::spawn(async move {
                tokio::select! {
                    _ = token.cancelled() => {}
                    r = pipe(conn, pf, port) => {
                        if let Err(e) = r {
                            tracing::warn!("port-forward {id} connection error: {e}");
                        }
                    }
                }
            });
        }
        if let Some(pf) = warm {
            pf.abort();
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(addr: &str, port: u16) -> PortForwardSpec {
        PortForwardSpec {
            address: addr.into(),
            local_port: port,
            ..Default::default()
        }
    }

    #[test]
    fn addresses() {
        assert_eq!(bind_addr(&spec("localhost", 8080)), "localhost:8080");
        assert_eq!(bind_addr(&spec("", 80)), "localhost:80");
        assert_eq!(bind_addr(&spec("0.0.0.0", 1)), "0.0.0.0:1");
        assert_eq!(bind_addr(&spec("::1", 2)), "[::1]:2");
        assert_eq!(bind_addr(&spec("[::1]", 2)), "[::1]:2");
    }

    #[tokio::test]
    async fn bind_reports_errors() {
        let l = bind(&spec("127.0.0.1", 0)).await.unwrap();
        let port = l.local_addr().unwrap().port();
        let err = bind(&spec("127.0.0.1", port)).await.unwrap_err();
        assert!(
            err.to_string().contains("unable to listen on 127.0.0.1:"),
            "{err}"
        );
    }
}
