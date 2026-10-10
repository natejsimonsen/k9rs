//! Crash audit: hostile cluster data, terminal sizes and config files must
//! never panic the TUI (k9s shows n/a, skips or flashes instead).

use crossterm::event::KeyCode;
use k9rs::k8s::fake::obj;
use k9rs::k8s::Gvr;
use serde_json::json;

use crate::common::*;

const SIZES: &[(u16, u16)] = &[
    (1, 1),
    (2, 1),
    (1, 2),
    (3, 3),
    (10, 4),
    (40, 8),
    (80, 5),
    (80, 24),
    (400, 3),
    (1000, 60),
];

fn sweep(s: &mut Shell) {
    for &(w, h) in SIZES {
        s.render_size(w, h);
    }
}

/// Pods with missing fields, wrong types, unicode and huge values.
fn hostile_pods() -> Vec<kube::api::DynamicObject> {
    let long = "x".repeat(5000);
    vec![
        obj(json!({"apiVersion": "v1", "kind": "Pod",
                   "metadata": {"name": "🐶👩‍👩‍👧‍👦漢字-\u{202e}evil", "namespace": "default"}})),
        obj(json!({"apiVersion": "v1", "kind": "Pod",
                   "metadata": {"name": long, "namespace": "default",
                                "labels": {"a": "⚡️".repeat(300)}},
                   "spec": "garbage", "status": [1, 2, 3]})),
        obj(json!({"apiVersion": "v1", "kind": "Pod",
                   "metadata": {"name": "neg", "namespace": "default",
                                "creationTimestamp": "1970-01-01T00:00:00Z"},
                   "spec": {"containers": [{"name": 7, "resources":
                       {"requests": {"cpu": "-9999999999999Ei", "memory": "1e400"},
                        "limits": {"cpu": 12, "memory": null}}}], "nodeName": {}},
                   "status": {"phase": 42, "containerStatuses": [{"name": "c",
                       "restartCount": -9223372036854775808i64, "ready": "yes",
                       "state": {"waiting": {"reason": ""}}}], "podIP": ["x"]}})),
        obj(json!({"apiVersion": "v1", "kind": "Pod",
                   "metadata": {"name": "", "namespace": "default"}})),
    ]
}

#[tokio::test]
async fn hostile_pods_all_sizes_and_views() {
    let h = Harness::new().await;
    h.acc
        .objects
        .lock()
        .unwrap()
        .entry(Gvr::parse("v1/pods"))
        .or_default()
        .extend(hostile_pods());
    let mut s = h.into_shell("pods");
    s.boot().await;
    sweep(&mut s);
    // Walk every row through describe and YAML, at every size.
    for row in 0..11 {
        for k in ['d', 'y'] {
            s.press(k).await;
            sweep(&mut s);
            // Search with multibyte and fuzzy queries, then jump matches
            // (on the first row and the hostile ones sorted last).
            let qs: &[&str] = if row == 0 || row >= 8 {
                &["🐶", "-f 🐶漢", "İ", "ü[", "-f x"]
            } else {
                &[]
            };
            for q in qs {
                s.press('/').await;
                s.typ(q);
                s.code(KeyCode::Enter).await;
                s.press('n').await;
                s.key(shift('n')).await;
                s.render_size(20, 6);
                s.render();
            }
            s.code(KeyCode::Esc).await;
            s.code(KeyCode::Esc).await;
        }
        s.press('l').await;
        sweep(&mut s);
        s.code(KeyCode::Esc).await;
        // Containers, then the delete and port-forward dialogs.
        s.code(KeyCode::Enter).await;
        sweep(&mut s);
        s.code(KeyCode::Esc).await;
        s.ctrl('d').await;
        sweep(&mut s);
        s.code(KeyCode::Esc).await;
        s.key(shift('f')).await;
        sweep(&mut s);
        s.code(KeyCode::Esc).await;
        s.code(KeyCode::Down).await;
    }
    for c in [
        "xray pods",
        "pulses",
        "ns",
        "nodes",
        "workloads",
        "ctx",
        "aliases",
        "foo",
    ] {
        s.command(c).await;
        sweep(&mut s);
        s.code(KeyCode::Esc).await;
    }
    s.press('?').await;
    sweep(&mut s);
    s.code(KeyCode::Esc).await;
    s.press(':').await;
    s.typ("🐶漢字ü");
    sweep(&mut s);
    s.code(KeyCode::Esc).await;
    s.press('/').await;
    s.typ("ü[(*");
    s.code(KeyCode::Enter).await;
    sweep(&mut s);
    s.ctrl('e').await;
    sweep(&mut s);
}

const GARBAGE: &[&str] = &[
    "",
    "\u{0}\u{1}garbage: [",
    "- 1\n- 2\n",
    "k9s: 42",
    "k9s:\n  body: [1, 2]\n  frame: {border: 7}\n  ui: {skin: 3, enableMouse: maybe}",
    "k9s:\n  refreshRate: -99999999999999999999\n  logger: {tail: -1, buffer: 1e400}",
    "k9s:\n  refreshRate: .nan\n  apiServerTimeout: 1e400h",
    "k9s:\n  refreshRate: 1e38\n  apiServerTimeout: -5s",
    "hotKeys: {a: 1, b: {shortCut: 9, command: []}, 'ü': {shortCut: 'ctrlü-x', command: pods}}",
    "plugins: {x: {shortCut: [], command: 3, args: 'no', scopes: 7}}",
    "views: {v1/pods: {columns: 3, sortColumn: [1]}}",
    "aliases: {pp: [], '': ''}",
    "apiVersion: v1\nkind: Config\nclusters: 7\ncontexts: [{name: x}]\nusers: null\ncurrent-context: [x]",
    "apiVersion: v1\nkind: Config\ncurrent-context: c\ncontexts: [{name: c, context: {cluster: nope, user: nope}}]",
    "\u{feff}🐶: 漢字",
];

/// Config files and kubeconfigs with wrong types or garbage load (or fail)
/// without panicking, through the same loaders startup uses.
#[tokio::test]
async fn garbage_config_files_do_not_panic() {
    let dir = tempfile::tempdir().unwrap();
    for (i, body) in GARBAGE.iter().enumerate() {
        let p = dir.path().join(format!("f{i}.yaml"));
        std::fs::write(&p, body).unwrap();
        if let Ok(c) = k9rs::config::Config::load_from(&p) {
            let _ = c.k9s.refresh_duration();
        }
        let _ = k9rs::config::HotKeys::load_from(&p);
        let _ = k9rs::config::Aliases::load_from(&p);
        let _ = k9rs::config::Plugins::load_from(&p);
        let _ = k9rs::config::CustomView::load_from(&p);
        let _ = k9rs::config::Skin::load_from(&p);
        let opts = k9rs::k8s::client::ConnectOptions {
            kubeconfig: Some(p.to_string_lossy().into()),
            ..Default::default()
        };
        let _ = k9rs::k8s::client::kubeconfig_names(opts.clone());
        if let Ok(acc) = k9rs::k8s::client::connect(opts).await {
            let _ = acc.contexts().await;
            let _ = acc.active_context();
            let _ = acc.default_namespace();
        }
    }
}

/// A refresh that panics flashes an error; the app keeps running.
#[tokio::test]
async fn panicking_refresh_flashes_and_survives() {
    let h = Harness::new().await;
    h.acc
        .list_panics
        .lock()
        .unwrap()
        .push("v1/configmaps".into());
    let mut s = h.into_shell("pods");
    s.boot().await;
    s.command("cm").await;
    let sc = s.render();
    assert!(
        sc.line(H - 1)
            .contains("Boom!! fake list of v1/configmaps blew up"),
        "{}",
        sc.text()
    );
    s.command("pods").await;
    assert!(s.render().contains(" pods(default)[7] "));
}
