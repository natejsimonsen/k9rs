//! The real shell (`App`) driven through its test API: header, prompt,
//! dialogs, crumbs, hotkeys and context switching (k9s view/app.go).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::common::*;

async fn shell(command: &str) -> Shell {
    shell_with(Setup::default(), command).await
}

async fn shell_with(setup: Setup, command: &str) -> Shell {
    let mut s = Harness::with(setup).await.into_shell(command);
    s.boot().await;
    s
}

/// view/app.go layout: cluster info, menu, logo, the view and crumbs.
#[tokio::test]
async fn header_logo_menu_and_crumbs() {
    let mut s = shell("pods").await;
    let sc = s.render();
    dump(&sc);
    assert!(
        sc.line(0).starts_with(" Context:  fake [RW]"),
        "{}",
        sc.line(0)
    );
    assert!(sc.line(1).starts_with(" Cluster:  "));
    assert!(
        sc.line(3)
            .starts_with(&format!(" K9rs Rev: {} ", k9rs::version::VERSION)),
        "{}",
        sc.line(3)
    );
    assert!(sc.line(5).starts_with(" CPU:      10%"), "{}", sc.line(5));
    // Namespace favorites then the pod actions (ui/menu.go).
    assert!(sc.line(0).contains("<0> all"), "{}", sc.line(0));
    assert!(sc.line(1).contains("<1> default"), "{}", sc.line(1));
    assert!(sc.contains("<ctrl-d>  Delete"), "{}", sc.text());
    assert!(sc.contains("<shift-f> Port-Forward"));
    // Logo (ui/logo.go).
    assert!(sc.line(0).contains("____  __ ________"));
    assert_eq!(sc.fg_of("____  __ ________"), color("orange"));
    // Title, crumbs and the first-load flash.
    assert!(sc.contains(" pods(default)[7] "));
    assert!(sc.line(H - 2).contains("<pod>"), "{}", sc.line(H - 2));
    assert!(
        sc.line(H - 1)
            .contains("Viewing v1/pods in namespace default"),
        "{}",
        sc.line(H - 1)
    );
}

/// ui/prompt.go: `:` opens the command prompt; the suggestion completes
/// the typed text and Tab accepts it.
#[tokio::test]
async fn command_prompt_suggestions() {
    let mut s = shell("pods").await;
    s.press(':').await;
    s.typ("dep");
    s.settle().await;
    let sc = s.render();
    let row = sc.row_of("> dep").expect("prompt row");
    assert!(sc.line(row).contains("deploy"), "{}", sc.line(row));
    s.code(KeyCode::Tab).await;
    s.code(KeyCode::Enter).await;
    let sc = s.render();
    assert!(sc.contains(" deployments(default)["), "{}", sc.text());
    assert!(
        sc.line(H - 2).contains("<deployment>"),
        "{}",
        sc.line(H - 2)
    );
}

/// view/app.go cowCmd: unknown commands raise the error dialog.
#[tokio::test]
async fn unknown_command_error_dialog() {
    let mut s = shell("pods").await;
    s.command("bozo").await;
    let sc = s.render();
    assert!(sc.contains("<error>"), "{}", sc.text());
    assert!(sc.contains("< Ruroh? `bozo` command not found >"));
    assert!(sc.contains("Dismiss"));
    s.code(KeyCode::Enter).await;
    let sc = s.render();
    assert!(!sc.contains("<error>"));
    assert!(sc.contains(" pods(default)[7] "));
}

/// `:q` quits (view/command.go exitCmd).
#[tokio::test]
async fn quit_command() {
    let mut s = shell("pods").await;
    assert!(!s.app.is_quitting());
    s.command("q").await;
    assert!(s.app.is_quitting());
}

/// ctrl-a opens the aliases view (view/app.go aliasCmd).
#[tokio::test]
async fn ctrl_a_aliases() {
    let mut s = shell("pods").await;
    s.ctrl('a').await;
    let sc = s.render();
    assert!(sc.contains(" aliases(all)["), "{}", sc.text());
    assert!(sc.line(H - 2).contains("<aliases>"), "{}", sc.line(H - 2));
    assert!(sc.contains("<enter> Goto"));
}

/// ui/dialog/delete.go: focus starts on Cancel; OK deletes.
#[tokio::test]
async fn delete_dialog() {
    let mut s = shell("pods").await;
    s.ctrl('d').await;
    let sc = s.render();
    assert!(sc.contains("<Delete>"), "{}", sc.text());
    assert!(sc.contains("Delete pods default/api-6f5d8c7b9-xyz12?"));
    assert!(sc.contains("Propagation: Background"));
    assert!(sc.contains("Cancel"));
    // Esc closes without deleting.
    s.code(KeyCode::Esc).await;
    assert!(!s.render().contains("<Delete>"));
    // Enter on the focused Cancel button deletes nothing either.
    s.ctrl('d').await;
    s.code(KeyCode::Enter).await;
    assert!(!s.render().contains("<Delete>"));
    assert!(!s.acc.calls().iter().any(|c| c.starts_with("delete")));
    // Right moves to OK.
    s.ctrl('d').await;
    s.code(KeyCode::Right).await;
    s.code(KeyCode::Enter).await;
    assert!(
        s.acc
            .calls()
            .contains(&"delete v1/pods default/api-6f5d8c7b9-xyz12 force=false".to_string()),
        "{:?}",
        s.acc.calls()
    );
}

/// view/app.go hotKeyActions: a hotkey runs its command.
#[tokio::test]
async fn hotkey_dispatch() {
    let mut s = shell_with(
        Setup {
            hotkeys_yaml: Some(
                "hotKeys:\n  f2:\n    shortCut: F2\n    description: Deployments\n    command: deploy\n",
            ),
            ..Default::default()
        },
        "pods",
    )
    .await;
    s.key(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE))
        .await;
    let sc = s.render();
    assert!(sc.contains(" deployments(default)["), "{}", sc.text());
}

/// view/app.go switchContext: flash, accessor switch, then the
/// context's view (pods by default) in its namespace.
#[tokio::test]
async fn ctx_command_switches_context() {
    let mut s = shell("pods").await;
    s.command("ctx staging").await;
    assert!(
        s.acc
            .calls()
            .contains(&"switch_context staging".to_string()),
        "{:?}",
        s.acc.calls()
    );
    let sc = s.render();
    assert!(sc.contains(" pods(kube-system)["), "{}", sc.text());
    // The "Switching context" flash is replaced by the new view's first
    // load flash (browser.go TableDataChanged), as in k9s.
    assert!(
        sc.line(H - 1)
            .contains("Viewing v1/pods in namespace kube-system"),
        "{}",
        sc.line(H - 1)
    );
}

/// Enter on the contexts view goes through the same switch.
#[tokio::test]
async fn ctx_view_enter_switches_context() {
    let mut s = shell("ctx").await;
    s.code(KeyCode::Down).await;
    s.code(KeyCode::Enter).await;
    assert!(s
        .acc
        .calls()
        .contains(&"switch_context staging".to_string()));
    let sc = s.render();
    assert!(sc.contains(" pods(kube-system)["), "{}", sc.text());
}

/// ui/table_helper.go NSTitleFmt: `[fg:bg:b]pods(` bold, `[fg:bg:-])[`
/// plain, count bold, `]` plain. While the prompt has focus the table
/// border takes the plain border color (tview Box focus).
#[tokio::test]
async fn title_attributes_and_blur() {
    use ratatui::style::Modifier;
    let mut s = shell("pods").await;
    let sc = s.render();
    let (x, y) = sc.find("pods(default)[7]").unwrap();
    let bold = |dx: u16| sc.0[(x + dx, y)].modifier.contains(Modifier::BOLD);
    assert!(bold(0), "pods");
    assert!(bold(5), "namespace");
    assert!(!bold(12), ")");
    assert!(!bold(13), "[");
    assert!(bold(14), "count");
    assert!(!bold(15), "]");
    let border = sc.fg_at(0, y);
    assert_eq!(border, color("lightskyblue"));
    s.press(':').await;
    let sc = s.render();
    let (_, y) = sc.find("pods(default)[7]").unwrap();
    assert_eq!(sc.fg_at(0, y), color("dodgerblue"));
}

/// browser.go Init: CanForResource in the active namespace. A denial
/// raises the error dialog and keeps the current view.
#[tokio::test]
async fn list_denied_keeps_current_view() {
    let h = Harness::new().await;
    h.acc
        .denied
        .lock()
        .unwrap()
        .push(("v1/secrets".into(), "default".into()));
    let mut s = h.into_shell("pods");
    s.boot().await;
    s.command("secrets").await;
    let sc = s.render();
    // The cow wraps the message like tview's WordWrap: after punctuation.
    let row = sc
        .row_of("< Ruroh? [list] access denied on resource \"default\":")
        .expect("error dialog");
    assert!(
        sc.line(row + 1).contains(" \"v1/secrets\" >"),
        "{}",
        sc.text()
    );
    assert!(sc.line(H - 2).contains("<pod>"), "{}", sc.line(H - 2));
}

/// browser.go Start: the watch of a cluster scoped resource the user may
/// not list fails with a flash and the view stays empty.
#[tokio::test]
async fn watch_denied_flashes() {
    let h = Harness::new().await;
    h.acc
        .denied
        .lock()
        .unwrap()
        .push(("v1/persistentvolumes".into(), "".into()));
    let mut s = h.into_shell("pods");
    s.boot().await;
    s.command("pv").await;
    let sc = s.render();
    assert!(
        sc.line(H - 2).contains("<persistentvolume>"),
        "{}",
        sc.line(H - 2)
    );
    // No data ever arrives, so the frame keeps no title.
    assert!(!sc.contains("persistentvolumes("), "{}", sc.text());
    assert!(
        sc.line(H - 1).contains(
            "Watcher failed for v1/persistentvolumes -- [list] access denied on resource \"\":\"v1/persistentvolumes\""
        ),
        "{}",
        sc.line(H - 1)
    );
}

/// browser.go TableNoData: the boot view drops its empty refresh while the
/// app is not running, then the first-view skip; the next one warns. Later
/// views skip their first empty result too (Start resets the counter).
#[tokio::test]
async fn boot_view_skips_first_empty_result() {
    let mut s = shell("ingresses").await;
    let sc = s.render();
    assert!(!sc.contains("No resources found"), "{}", sc.text());
    assert!(!sc.contains("ingresses(default)"), "{}", sc.text());
    // The first refresh after boot is the first-view skip.
    s.tick().await;
    let sc = s.render();
    assert!(!sc.contains("No resources found"), "{}", sc.text());
    s.tick().await;
    let sc = s.render();
    assert!(sc.contains(" ingresses(default)[0] "), "{}", sc.text());
    assert!(
        sc.line(H - 1).contains(
            "No resources found for networking.k8s.io/v1/ingresses in \"default\" namespace"
        ),
        "{}",
        sc.line(H - 1)
    );
    s.command("pods").await;
    // Browser.Start resets the first-view counter: skip once more, then warn.
    s.command("ingresses").await;
    assert!(!s.render().contains("No resources found"));
    s.tick().await;
    assert!(s
        .render()
        .line(H - 1)
        .contains("No resources found for networking.k8s.io/v1/ingresses"));
}

/// ui/app.go activateCmd: `:` shows no history suggestion, but Tab still
/// completes the first one.
#[tokio::test]
async fn command_prompt_opens_without_suggestion() {
    let mut s = shell("pods").await;
    s.press(':').await;
    let sc = s.render();
    let row = sc
        .lines()
        .iter()
        .position(|l| l.contains('🐶'))
        .expect("prompt row") as u16;
    assert!(!sc.line(row).contains("pods"), "{}", sc.line(row));
    s.code(KeyCode::Tab).await;
    let sc = s.render();
    assert!(sc.line(row).contains("> v1/pods"), "{}", sc.line(row));
}

/// The first frame does not wait on the API: with cluster info still
/// pending, the header shows the kubeconfig names and the k9s version and
/// the view loads.
#[tokio::test]
async fn header_before_cluster_info_resolves() {
    use k9rs::k8s::accessor::Accessor;
    let mut h = Harness::new().await;
    h.acc
        .cluster_info_hangs
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let c = h
        .acc
        .contexts()
        .await
        .unwrap()
        .into_iter()
        .find(|c| c.name == "fake")
        .unwrap();
    h.ctx.cluster_info = k9rs::k8s::ClusterInfo::from_context(&c);
    let mut s = h.into_shell("pods");
    s.boot().await;
    let sc = s.render();
    assert!(sc.line(0).starts_with(" Context:  fake "), "{}", sc.line(0));
    assert!(
        sc.line(1).contains(&format!("Cluster:  {}", c.cluster)),
        "{}",
        sc.line(1)
    );
    assert!(
        sc.line(3)
            .starts_with(&format!(" K9rs Rev: {} ", k9rs::version::VERSION)),
        "{}",
        sc.line(3)
    );
    assert!(sc.contains(" pods(default)[7] "), "{}", sc.text());
}

/// dao/registry.go: workloads is a namespaced pseudo resource, so
/// `:workloads kube-system` switches namespace even when started on all.
#[tokio::test]
async fn workloads_takes_namespace_arg() {
    let setup = Setup {
        namespace: Some("all".into()),
        ..Default::default()
    };
    let mut s = shell_with(setup, "pods").await;
    s.command("workloads kube-system").await;
    let sc = s.render();
    assert!(sc.contains(" workloads(kube-system)["), "{}", sc.text());
    assert!(!sc.contains("NAMESPACE"), "{}", sc.text());
}
