//! Pod specific actions that leave the TUI or open other views.

use crate::common::*;
use crossterm::event::KeyCode;
use k9rs::k8s::{Accessor, Gvr};
use k9rs::ui::{Dialog, FlashLevel};

async fn on_nginx() -> Harness {
    let mut h = Harness::new().await;
    h.goto("po").await.unwrap();
    h.code(KeyCode::Down).await;
    h.code(KeyCode::Down).await;
    h
}

/// `e` edit, `s` shell (view/exec.go computeShellArgs shellCheck), `a`
/// attach: each suspends the TUI with the argv built from the accessor.
#[tokio::test]
async fn edit_shell_attach_suspend() {
    let mut h = on_nginx().await;
    let pod = "nginx-7d9c5b8f4-abcde";
    h.press('e').await;
    h.press('s').await;
    h.press('a').await;
    let argv: Vec<Vec<String>> = h.suspends.iter().map(|s| s.argv.clone()).collect();
    assert_eq!(argv.len(), 3, "{argv:?}");
    assert_eq!(
        argv[0],
        h.acc
            .edit_argv(&Gvr::parse("v1/pods"), &format!("default/{pod}"))
    );
    assert_eq!(
        argv[1],
        [
            "kubectl",
            "exec",
            "-it",
            "-n",
            "default",
            pod,
            "-c",
            "nginx",
            "--",
            "sh",
            "-c",
            "command -v bash >/dev/null && exec bash || exec sh"
        ]
    );
    assert_eq!(
        argv[2],
        ["kubectl", "attach", "-it", "-n", "default", pod, "-c", "nginx"]
    );
    assert!(h.acc.calls().is_empty());
}

/// `p` previous logs (view/log.go updateTitle " Previous Logs").
#[tokio::test]
async fn previous_logs_title() {
    let mut h = on_nginx().await;
    h.press('p').await;
    assert_eq!(
        h.top().title(),
        " Previous Logs(default/nginx-7d9c5b8f4-abcde:nginx)[tail] "
    );
}

/// shift-j jumps to the owner (view/owner_extender.go gotoResource with
/// the owner as instance), `o` shows the pod's node (pod.go showNode).
#[tokio::test]
async fn jump_owner_and_show_node() {
    let mut h = on_nginx().await;
    h.key(shift('J')).await;
    assert_eq!(h.top().title(), " replicasets(default)[1] ");
    assert!(h.render().contains("nginx-7d9c5b8f4"));
    h.code(KeyCode::Esc).await;
    assert_eq!(h.top().title(), " pods(default)[7] ");
    h.press('o').await;
    assert_eq!(h.top().title(), " nodes(all)[1] ");
    let s = h.render();
    assert!(s.contains("node-1") && !s.contains("node-2"));
}

/// shift-f port-forward dialog, then the forward starts, the PF column
/// shows `[orange::b]Ⓕ` (view/pod.go pfIndicator) and `f` lists it.
#[tokio::test]
async fn port_forward_flow() {
    let mut h = on_nginx().await;
    h.key(shift('F')).await;
    let Dialog::PortForward { spec, ports, on_ok } = h.take_dialog() else {
        panic!("expected the port-forward dialog")
    };
    assert_eq!(spec.namespace, "default");
    assert_eq!(spec.pod, "nginx-7d9c5b8f4-abcde");
    assert_eq!(spec.container, "nginx");
    assert_eq!(spec.container_port, 80);
    assert!(
        ports.iter().any(|p| p.starts_with("nginx::80")),
        "{ports:?}"
    );
    let cmds = on_ok(&mut h.ctx, spec);
    h.apply(cmds);
    h.settle().await;
    assert_eq!(
        h.acc.calls(),
        ["port_forward default/nginx-7d9c5b8f4-abcde|nginx:80:80"]
    );
    assert!(h
        .flashes
        .contains(&(FlashLevel::Info, "PortForward activated 80".to_string())));
    h.tick().await;
    h.code(KeyCode::Up).await;
    let s = h.render();
    let y = s.row_of("nginx-7d9c5b8f4-abcde").unwrap();
    let (x, _) = s.find("Ⓕ").expect("pf indicator");
    assert_eq!(s.fg_at(x, y), color("orange"));
    h.code(KeyCode::Down).await;
    h.press('f').await;
    assert_eq!(
        h.top().title(),
        " portforwards(default/nginx-7d9c5b8f4-abcde)[1] "
    );
    assert!(h.render().contains("http://localhost:80/"));
}
