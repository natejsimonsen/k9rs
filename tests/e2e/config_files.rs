//! hotkeys.yaml, plugins.yaml, views.yaml and skins loaded from the
//! K9S_CONFIG_DIR layout.

use crate::common::*;
use crossterm::event::KeyCode;
use k9rs::ui::Dialog;

const PLUGINS: &str = r#"plugins:
  hello:
    shortCut: Shift-H
    description: Say Hello
    scopes: [pods]
    command: echo
    background: false
    args: ["-n", "$NAMESPACE", "$NAME", "$CONTEXT", "$COL-STATUS", "$RESOURCE_NAME", "$RESOURCE_VERSION"]
  careful:
    shortCut: Ctrl-T
    description: Careful
    confirm: true
    scopes: [pods]
    command: kubectl
    args: [get, pod, "$NAME", -n, "$NAMESPACE"]
  dp-only:
    shortCut: Shift-Q
    description: Deploy Only
    scopes: [deployments]
    command: echo
    args: ["$NAME"]
"#;

/// view/actions.go pluginActions: scoped hints, args substitution
/// (helpers.go defaultEnv + table.go envFn), run in the foreground.
#[tokio::test]
async fn plugin_hint_and_argv() {
    let mut h = Harness::with(Setup {
        plugins_yaml: Some(PLUGINS),
        ..Default::default()
    })
    .await;
    h.goto("po").await.unwrap();
    let hints = h.top().hints();
    assert!(hints
        .iter()
        .any(|x| x.mnemonic == "shift-h" && x.description == "Say Hello" && x.visible));
    assert!(
        !hints.iter().any(|x| x.description == "Deploy Only"),
        "out of scope plugin"
    );
    h.key(shift('H')).await;
    assert_eq!(h.suspends.len(), 1);
    assert_eq!(
        h.suspends[0].argv,
        [
            "echo",
            "-n",
            "default",
            "api-6f5d8c7b9-xyz12",
            "fake",
            "CrashLoopBackOff",
            "pods",
            "v1"
        ]
    );
    assert!(h.acc.calls().is_empty(), "plugins never touch the cluster");

    h.goto("deploy").await.unwrap();
    assert!(h
        .top()
        .hints()
        .iter()
        .any(|x| x.mnemonic == "shift-q" && x.description == "Deploy Only"));
}

/// `confirm: true` asks first (view/actions.go: "Run?\n<cmd> <args>").
#[tokio::test]
async fn plugin_confirm() {
    let mut h = Harness::with(Setup {
        plugins_yaml: Some(PLUGINS),
        ..Default::default()
    })
    .await;
    h.goto("po").await.unwrap();
    h.ctrl('t').await;
    assert!(h.suspends.is_empty());
    let Dialog::Confirm { title, msg, on_ok } = h.take_dialog() else {
        panic!("expected confirm")
    };
    assert_eq!(title, "Confirm Careful");
    assert_eq!(msg, "Run?\nkubectl get pod api-6f5d8c7b9-xyz12 -n default");
    let cmds = on_ok(&mut h.ctx);
    h.apply(cmds);
    assert_eq!(
        h.suspends[0].argv,
        [
            "kubectl",
            "get",
            "pod",
            "api-6f5d8c7b9-xyz12",
            "-n",
            "default"
        ]
    );
}

/// hotkeys.yaml entries are listed in the help HOTKEYS section
/// (view/help.go showHotKeys).
#[tokio::test]
async fn hotkeys_in_help() {
    let mut h = Harness::with(Setup {
        hotkeys_yaml: Some(
            "hotKeys:\n  shift-0:\n    shortCut: Shift-0\n    description: Viewing pods\n    command: pods\n  f2:\n    shortCut: F2\n    description: Deployments\n    command: dp\n",
        ),
        ..Default::default()
    })
    .await;
    let hk = &h.ctx.hotkeys.hot_keys;
    assert_eq!(hk["shift-0"].command, "pods");
    assert_eq!(hk["f2"].short_cut, "F2");
    h.goto("po").await.unwrap();
    h.press('?').await;
    let s = h.render();
    dump(&s);
    assert!(s.line(0).contains("HOTKEYS"), "{}", s.line(0));
    let row = s.line(s.row_of("<shift-0>").expect("hotkey row"));
    assert!(row.contains("Viewing pods"), "{row}");
    assert!(s.contains("<f2>"), "{}", s.text());
}

/// views.yaml (render/cust_cols.go Header): listed columns first, the
/// remaining ones after; sortColumn applies.
#[tokio::test]
async fn custom_columns_for_deployments() {
    let mut h = Harness::with(Setup {
        views_yaml: Some(
            "views:\n  apps/v1/deployments:\n    sortColumn: NAME:desc\n    columns:\n      - AGE\n      - NAME\n      - READY\n",
        ),
        ..Default::default()
    })
    .await;
    h.goto("deploy").await.unwrap();
    let s = h.render();
    dump(&s);
    let hdr: Vec<String> = s
        .line(1)
        .split_whitespace()
        .filter(|w| *w != "│")
        .map(String::from)
        .collect();
    assert_eq!(hdr, ["AGE", "NAME↓", "READY", "UP-TO-DATE", "AVAILABLE"]);
    let rows = s.table_names("AGE");
    assert!(
        rows[0].starts_with("10h") && rows[0].contains("nginx"),
        "{rows:?}"
    );
    assert!(rows[1].contains("api"), "{rows:?}");
    // Pods keep their stock columns.
    h.goto("po").await.unwrap();
    assert!(h.render().line(1).contains("NAME↑"));
}

const SKIN: &str = r#"k9s:
  body:
    fgColor: white
    bgColor: navy
  frame:
    title:
      fgColor: yellow
      bgColor: navy
      highlightColor: lime
      counterColor: red
    border:
      fgColor: green
      focusColor: fuchsia
    status:
      newColor: pink
      addColor: gold
      errorColor: maroon
  views:
    table:
      bgColor: navy
      cursorFgColor: white
      header:
        fgColor: orange
        bgColor: purple
        sorterColor: lime
"#;

/// skins/<name>.yaml selected by k9s.ui.skin (config/styles.go).
#[tokio::test]
async fn skin_applied() {
    let mut h = Harness::with(Setup {
        skin_yaml: Some(SKIN),
        ..Default::default()
    })
    .await;
    h.goto("po").await.unwrap();
    let s = h.render();
    let (x, _) = s.find("pods(").unwrap();
    assert_eq!(s.fg_at(x, 0), color("yellow"), "title");
    assert_eq!(s.fg_at(x + 5, 0), color("lime"), "highlight");
    let (cx, _) = s.find("[7]").unwrap();
    assert_eq!(s.fg_at(cx + 1, 0), color("red"), "counter");
    assert_eq!(s.fg_at(0, 0), color("fuchsia"), "focused border");
    let (nx, ny) = s.find("NAME↑").unwrap();
    assert_eq!(s.fg_at(nx, ny), color("orange"), "header fg");
    assert_eq!(s.bg_at(nx, ny), color("purple"), "header bg");
    assert_eq!(s.fg_at(nx + 4, ny), color("lime"), "sorter");
    assert_eq!(s.fg_of("nginx-7d9c5b8f4-abcde"), color("pink"), "newColor");
    assert_eq!(
        s.bg_of("api-6f5d8c7b9-xyz12"),
        color("maroon"),
        "cursor on err row"
    );
    assert_eq!(
        s.fg_of("api-6f5d8c7b9-xyz12"),
        color("white"),
        "cursorFgColor"
    );
    h.code(KeyCode::Down).await;
    let s = h.render();
    assert_eq!(
        s.fg_of("api-6f5d8c7b9-xyz12"),
        color("maroon"),
        "errorColor"
    );
    h.goto("deploy").await.unwrap();
    assert_eq!(h.render().fg_of("nginx"), color("gold"), "addColor");
}

/// model/table.go: with views.yaml columns, server tables are fetched with
/// full objects so `.spec` JSONPath and JQ columns resolve. (`{range}` specs
/// never reach JSONPath: kubectl's RelaxedJSONPathExpression rejects them.)
async fn ingress_harness(cols: &str) -> Harness {
    let views: &'static str = Box::leak(
        format!("views:\n  networking.k8s.io/v1/ingresses:\n    columns:\n{cols}").into_boxed_str(),
    );
    let h = Harness::with(Setup {
        views_yaml: Some(views),
        ..Default::default()
    })
    .await;
    h.acc
        .objects
        .lock()
        .unwrap()
        .entry(k9rs::k8s::Gvr::parse("networking.k8s.io/v1/ingresses"))
        .or_default()
        .push(k9rs::k8s::fake::obj(serde_json::json!({
            "apiVersion": "networking.k8s.io/v1", "kind": "Ingress",
            "metadata": {"name": "web", "namespace": "default"},
            "spec": {"ingressClassName": "nginx",
                     "rules": [{"host": "a.io"}, {"host": "b.io"}]}
        })));
    h
}

#[tokio::test]
async fn custom_columns_on_server_table_resources() {
    let mut h = ingress_harness(
        "      - NAME\n      - CLASS:.spec.ingressClassName\n      - JQ:{.spec.rules | map(.host) | join(\"+\")}\n",
    )
    .await;
    h.goto("ing").await.unwrap();
    let s = h.render();
    dump(&s);
    let hdr: Vec<String> = s
        .line(1)
        .split_whitespace()
        .filter(|w| *w != "│")
        .map(String::from)
        .collect();
    assert_eq!(hdr[1..3], ["CLASS", "JQ"]);
    let row = s.line(2);
    for want in ["web", "nginx", "a.io+b.io"] {
        assert!(row.contains(want), "{want}: {row}");
    }
}

/// A custom column JSONPath that does not parse fails the table load the
/// way k9s does (seen live: "column2 is an incomplete jsonpath template").
#[tokio::test]
async fn bad_custom_column_fails_the_load() {
    let mut h = ingress_harness(
        "      - NAME\n      - CLASS:.spec.ingressClassName\n      - BAD:{.spec[}\n",
    )
    .await;
    h.goto("ing").await.unwrap();
    let f = h.last_flash().cloned().unwrap();
    assert_eq!(
        f.1,
        "Watcher failed for networking.k8s.io/v1/ingresses -- column2 is an incomplete jsonpath template"
    );
}
