//! describe / yaml views and the `?` help view.

use crate::common::*;
use crossterm::event::KeyCode;

async fn on_api_pod() -> Harness {
    let mut h = Harness::new().await;
    h.goto("po").await.unwrap();
    h
}

/// view/details.go title (` Describe(path) `), esc pops back.
#[tokio::test]
async fn describe_and_back() {
    let mut h = on_api_pod().await;
    h.press('d').await;
    assert_eq!(h.top().title(), " Describe(default/api-6f5d8c7b9-xyz12) ");
    assert_eq!(h.top().name(), "Describe");
    let s = h.render();
    assert!(
        s.contains("Name:         api-6f5d8c7b9-xyz12"),
        "{}",
        s.text()
    );
    assert!(s.contains("Namespace:    default"));
    // The details frame draws in the plain border color (focus sits on
    // its inner text view).
    assert_eq!(s.fg_at(0, 0), color("dodgerblue"));
    h.code(KeyCode::Esc).await;
    assert_eq!(h.top().title(), " pods(default)[7] ");
    assert_eq!(h.stack.len(), 1);
}

/// `y` YAML view with the stock yaml colors (views.yaml key/colon/value).
#[tokio::test]
async fn yaml_and_back() {
    let mut h = on_api_pod().await;
    h.press('y').await;
    assert_eq!(h.top().title(), " YAML(default/api-6f5d8c7b9-xyz12) ");
    let s = h.render();
    dump(&s);
    assert!(s.contains("apiVersion: v1") && s.contains("kind: Pod"));
    let (x, y) = s.find("apiVersion: v1").unwrap();
    assert_eq!(s.fg_at(x, y), color("steelblue"), "key");
    assert_eq!(s.fg_at(x + 10, y), color("white"), "colon");
    assert_eq!(s.fg_at(x + 12, y), color("papayawhip"), "value");
    h.code(KeyCode::Esc).await;
    assert_eq!(h.top().title(), " pods(default)[7] ");
}

/// view/help.go: RESOURCE / GENERAL / NAVIGATION sections, numbers first
/// then sorted by description; `?` pushes it, esc pops it.
#[tokio::test]
async fn help_for_pods() {
    let mut h = on_api_pod().await;
    h.press('?').await;
    assert_eq!(h.top().name(), "help");
    let s = h.render();
    dump(&s);
    let l0 = s.line(0);
    for sec in ["RESOURCE", "GENERAL", "NAVIGATION"] {
        assert!(l0.contains(sec), "{sec} missing: {l0}");
    }
    let resource: Vec<String> = s.lines()[1..6]
        .iter()
        .map(|l| l.split_whitespace().take(2).collect::<Vec<_>>().join(" "))
        .collect();
    assert_eq!(
        resource,
        [
            "<0> all",
            "<1> default",
            "<a> Attach",
            "<c> Copy",
            "<n> Copy"
        ]
    );
    for hint in [
        "<ctrl-d>",
        "<shift-j>",
        "<ctrl-a>",
        "<:cmd>",
        "</term>",
        "<ctrl-e>",
    ] {
        assert!(s.contains(hint), "{hint} missing");
    }
    h.code(KeyCode::Esc).await;
    assert_eq!(h.top().title(), " pods(default)[7] ");
}
