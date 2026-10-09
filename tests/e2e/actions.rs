//! Mutating actions: delete, kill, scale, restart, cordon/drain, readonly.

use crate::common::*;
use crossterm::event::KeyCode;
use k9rs::k8s::DeleteOptions;
use k9rs::ui::{Dialog, FlashLevel};

fn info(h: &Harness, msg: &str) -> bool {
    h.flashes.contains(&(FlashLevel::Info, msg.to_string()))
}

/// view/browser.go deleteCmd/resourceDelete: dialog, then the delete call,
/// the flash, and the row goes away.
#[tokio::test]
async fn delete_flow() {
    let mut h = Harness::new().await;
    h.goto("po").await.unwrap();
    h.ctrl('d').await;
    assert!(h.acc.calls().is_empty(), "nothing deleted before confirm");
    let Dialog::Delete { msg, on_ok } = h.take_dialog() else {
        panic!("expected the delete dialog")
    };
    assert_eq!(msg, "Delete pods default/api-6f5d8c7b9-xyz12?");
    let cmds = on_ok(&mut h.ctx, DeleteOptions::default());
    h.apply(cmds);
    h.settle().await;
    assert_eq!(
        h.acc.calls(),
        ["delete v1/pods default/api-6f5d8c7b9-xyz12 force=false"]
    );
    assert!(info(
        &h,
        "Delete resource v1/pods default/api-6f5d8c7b9-xyz12"
    ));
    h.tick().await;
    assert_eq!(h.top().title(), " pods(default)[6] ");
    assert!(!h.render().contains("api-6f5d8c7b9-xyz12"));
}

/// Force delete passes through the dialog options.
#[tokio::test]
async fn delete_force() {
    let mut h = Harness::new().await;
    h.goto("cm").await.unwrap();
    h.ctrl('d').await;
    let Dialog::Delete { msg, on_ok } = h.take_dialog() else {
        panic!("expected the delete dialog")
    };
    assert_eq!(msg, "Delete configmaps default/app-config?");
    let opts = DeleteOptions {
        force: true,
        grace: Some(0),
        ..Default::default()
    };
    let cmds = on_ok(&mut h.ctx, opts);
    h.apply(cmds);
    h.settle().await;
    assert_eq!(
        h.acc.calls(),
        ["delete v1/configmaps default/app-config force=true"]
    );
}

/// view/pod.go killCmd: no dialog, deletes right away.
#[tokio::test]
async fn kill_pod() {
    let mut h = Harness::new().await;
    h.goto("po").await.unwrap();
    h.ctrl('k').await;
    assert!(h.dialogs.is_empty());
    let calls = h.acc.calls();
    assert_eq!(calls.len(), 1);
    assert!(
        calls[0].starts_with("delete v1/pods default/api-6f5d8c7b9-xyz12"),
        "{calls:?}"
    );
    assert!(info(
        &h,
        "Delete resource v1/pods default/api-6f5d8c7b9-xyz12"
    ));
}

/// Readonly (config readOnly / --readonly) drops the mutating actions.
#[tokio::test]
async fn readonly_blocks_delete() {
    let mut h = Harness::with(Setup {
        readonly: true,
        ..Default::default()
    })
    .await;
    h.goto("po").await.unwrap();
    let hints = h.top().hints();
    for m in ["ctrl-d", "ctrl-k", "e", "s", "a"] {
        assert!(
            !hints.iter().any(|x| x.mnemonic == m),
            "<{m}> must be hidden in readonly"
        );
    }
    assert!(hints.iter().any(|x| x.mnemonic == "d"), "describe stays");
    h.ctrl('d').await;
    h.ctrl('k').await;
    assert!(h.dialogs.is_empty());
    assert!(h.acc.calls().is_empty());
    h.goto("deploy").await.unwrap();
    h.press('s').await;
    h.press('r').await;
    assert!(h.dialogs.is_empty());
    assert!(h.acc.calls().is_empty());
}

/// view/scale_extender.go: dialog with the current replicas, then scale.
#[tokio::test]
async fn scale_deployment() {
    let mut h = Harness::new().await;
    h.goto("deploy").await.unwrap();
    h.code(KeyCode::Down).await; // nginx
    h.press('s').await;
    let Dialog::Scale {
        msg,
        current,
        on_ok,
    } = h.take_dialog()
    else {
        panic!("expected the scale dialog")
    };
    assert_eq!(msg, "Scale deployment default/nginx?");
    assert_eq!(current, 2);
    let cmds = on_ok(&mut h.ctx, 5);
    h.apply(cmds);
    h.settle().await;
    assert_eq!(h.acc.calls(), ["scale apps/v1/deployments default/nginx 5"]);
    assert!(info(&h, "deployments default/nginx scaled successfully"));
}

/// view/restart_extender.go: confirm, then the rollout restart.
#[tokio::test]
async fn restart_deployment() {
    let mut h = Harness::new().await;
    h.goto("deploy").await.unwrap();
    h.press('r').await;
    let Dialog::Confirm { title, msg, on_ok } = h.take_dialog() else {
        panic!("expected the confirm dialog")
    };
    assert_eq!(title, "Confirm Restart");
    assert_eq!(msg, "Restart deployment default/api?");
    let cmds = on_ok(&mut h.ctx);
    h.apply(cmds);
    h.settle().await;
    assert_eq!(h.acc.calls(), ["restart apps/v1/deployments default/api"]);
    assert!(info(&h, "Restart in progress for `default/api..."));
}

/// view/node.go toggleCordonCmd and drainCmd.
#[tokio::test]
async fn cordon_uncordon_drain() {
    let mut h = Harness::new().await;
    h.goto("no").await.unwrap();
    for (k, title, msg, call) in [
        (
            'c',
            "Confirm Cordon",
            "Cordon node-1?",
            "cordon node-1 true",
        ),
        (
            'u',
            "Confirm Uncordon",
            "Uncordon node-1?",
            "cordon node-1 false",
        ),
    ] {
        h.press(k).await;
        let Dialog::Confirm {
            title: t,
            msg: m,
            on_ok,
        } = h.take_dialog()
        else {
            panic!("expected confirm for {k}")
        };
        assert_eq!((t.as_str(), m.as_str()), (title, msg));
        let cmds = on_ok(&mut h.ctx);
        h.apply(cmds);
        h.settle().await;
        assert_eq!(h.acc.calls().last().unwrap(), call);
    }
    h.press('r').await;
    let Dialog::Drain { node, on_ok } = h.take_dialog() else {
        panic!("expected the drain dialog")
    };
    assert_eq!(node, "node-1");
    let cmds = on_ok(&mut h.ctx, Default::default());
    h.apply(cmds);
    h.settle().await;
    assert_eq!(h.acc.calls().last().unwrap(), "drain node-1");
    assert_eq!(h.top().title(), " Drain Progress(nodes) ");
    assert!(h.render().contains("node/node-1 drained"));
}
