//! Enter drill-downs and esc back (view/dp.go, ns.go, pod.go, container.go).

use crate::common::*;
use crossterm::event::KeyCode;

fn lower(h: &Harness) -> Vec<String> {
    h.crumbs().iter().map(|c| c.to_lowercase()).collect()
}

/// dp.go showPods: pods for the deployment selector, path + selector in
/// the title (ui/table.go styleTitle), esc pops back.
#[tokio::test]
async fn deploy_to_pods_and_back() {
    let mut h = Harness::new().await;
    h.goto("deploy").await.unwrap();
    h.code(KeyCode::Down).await; // nginx
    h.code(KeyCode::Enter).await;
    assert_eq!(h.top().title(), " pods(default/nginx)[2] </app=nginx> ");
    assert_eq!(lower(&h), ["deployment", "pod"]);
    let s = h.render();
    assert!(s.contains("nginx-7d9c5b8f4-abcde") && s.contains("nginx-7d9c5b8f4-fghij"));
    assert!(!s.contains("api-6f5d8c7b9-xyz12"));
    h.code(KeyCode::Esc).await;
    assert_eq!(h.top().title(), " deployments(default)[2] ");
    assert_eq!(lower(&h), ["deployment"]);
}

/// ns.go: enter on a namespace shows its pods and makes it active.
#[tokio::test]
async fn namespace_to_pods() {
    let mut h = Harness::new().await;
    h.goto("ns").await.unwrap();
    h.code(KeyCode::Down).await;
    h.code(KeyCode::Down).await; // kube-system
    h.code(KeyCode::Enter).await;
    assert_eq!(h.top().title(), " pods(kube-system)[2] ");
    assert_eq!(h.ctx.namespace, "kube-system");
    assert_eq!(lower(&h), ["namespace", "pod"]);
    let s = h.render();
    assert!(s.contains("coredns-5d78c9869d-q2w3e") && s.contains("fluentd-x7k2p"));
}

/// pod.go enter -> containers (render/container.go), enter -> logs.
#[tokio::test]
async fn pod_to_containers_to_logs() {
    let mut h = Harness::new().await;
    h.goto("po").await.unwrap();
    h.code(KeyCode::Down).await;
    h.code(KeyCode::Down).await; // nginx-...-abcde
    h.code(KeyCode::Enter).await;
    assert_eq!(
        h.top().title(),
        " containers(default/nginx-7d9c5b8f4-abcde)[1] "
    );
    assert_eq!(lower(&h), ["pod", "containers"]);
    let s = h.render();
    let hdr: Vec<&str> = s.lines()[1]
        .split_whitespace()
        .filter(|w| *w != "│")
        .map(|w| Box::leak(w.to_string().into_boxed_str()) as &str)
        .collect();
    assert_eq!(
        &hdr[..8],
        [
            "IDX↑",
            "NAME",
            "PF",
            "IMAGE",
            "READY",
            "STATE",
            "RESTARTS",
            "PROBES(L:R:S)"
        ]
    );
    let row = s.line(s.row_of("nginx:1.27").unwrap());
    for c in ["M1", "nginx", "true", "Running", "off:off:off", "http:80"] {
        assert!(row.contains(c), "{c} missing: {row}");
    }
    h.code(KeyCode::Enter).await;
    assert_eq!(
        h.top().title(),
        " Logs(default/nginx-7d9c5b8f4-abcde:nginx)[tail] "
    );
    assert_eq!(lower(&h), ["pod", "containers", "logs"]);
    assert!(h.render().contains("GET /healthz 200 request-1"));
    h.code(KeyCode::Esc).await;
    assert_eq!(lower(&h), ["pod", "containers"]);
    h.code(KeyCode::Esc).await;
    assert_eq!(h.top().title(), " pods(default)[7] ");
}

/// svc.go: enter shows the pods behind the service selector.
#[tokio::test]
async fn service_to_pods() {
    let mut h = Harness::new().await;
    h.goto("svc").await.unwrap();
    h.code(KeyCode::Down).await; // nginx
    h.code(KeyCode::Enter).await;
    assert!(
        h.top().title().starts_with(" pods(default/nginx)[2]"),
        "{}",
        h.top().title()
    );
}
