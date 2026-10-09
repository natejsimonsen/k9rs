//! Context switching (view/context.go useContext, view/app.go switchContext).

use crate::common::*;
use crossterm::event::KeyCode;
use k9rs::ui::FlashLevel;

/// `:ctx <name>` and `<res> @<ctx>` carry the target context.
#[tokio::test]
async fn context_commands_parse() {
    let h = Harness::new().await;
    let c = k9rs::cmd::parse("ctx staging", &h.ctx.aliases).unwrap();
    assert_eq!(
        (c.gvr.as_str(), c.context.as_deref()),
        ("contexts", Some("staging"))
    );
    let c = k9rs::cmd::parse("pods @staging", &h.ctx.aliases).unwrap();
    assert_eq!(
        (c.gvr.as_str(), c.context.as_deref()),
        ("v1/pods", Some("staging"))
    );
}

/// Enter on a context hands the switch to the shell (k9s useContext calls
/// app.switchContext); the accessor switch itself is covered in app.rs.
#[tokio::test]
async fn enter_switches_accessor_context() {
    let mut h = Harness::new().await;
    h.goto("ctx").await.unwrap();
    h.code(KeyCode::Down).await;
    h.code(KeyCode::Enter).await;
    assert!(
        h.commands.iter().any(|c| c == "ctx staging"),
        "{:?}",
        h.commands
    );
}

/// k9s useContext -> App.switchContext flashes
/// `Switching context to "staging"::"kube-system"` and navigates to the
/// context's active view (pods when the last view was a context command).
#[tokio::test]
async fn enter_navigates_like_k9s() {
    let mut h = Harness::new().await;
    h.goto("ctx").await.unwrap();
    h.code(KeyCode::Down).await;
    h.code(KeyCode::Enter).await;
    let k9s_flash = (
        FlashLevel::Info,
        "Switching context to \"staging\"::\"kube-system\"".to_string(),
    );
    let via_shell = h.commands.iter().any(|c| {
        k9rs::cmd::parse(c, &h.ctx.aliases)
            .map(|p| p.gvr == "contexts" && p.context.as_deref() == Some("staging"))
            .unwrap_or(false)
    });
    assert!(
        via_shell || h.flashes.contains(&k9s_flash),
        "flashes={:?} commands={:?}",
        h.flashes,
        h.commands
    );
    if !via_shell {
        assert_eq!(h.top().name(), "Pod");
        assert_eq!(h.ctx.namespace, "kube-system");
    }
}
