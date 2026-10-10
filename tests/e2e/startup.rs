//! Startup: the default `po` command, pod table title, rows, colors, hints.

use crate::common::*;
use k9rs::config::{Config, ContextConfig};
use k9rs::ui::FlashLevel;

const POD_NAMES: [&str; 7] = [
    "api-6f5d8c7b9-xyz12",
    "migrate-28xkq",
    "nginx-7d9c5b8f4-abcde",
    "nginx-7d9c5b8f4-fghij",
    "old-worker-0",
    "pending-pod",
    "web-0",
];

fn first_words(rows: &[String]) -> Vec<String> {
    rows.iter()
        .map(|r| r.split_whitespace().next().unwrap_or("").to_string())
        .collect()
}

/// k9s cmd/root.go + config: with no -c and no context view, start on `po`.
#[test]
fn default_command_is_pods() {
    let mut cfg = Config::default();
    let mut cc = ContextConfig::default();
    assert_eq!(k9rs::cli::initial_command(&mut cfg, &mut cc), "po");
}

/// ui/table.go styleTitle (NSTitleFmt) + view/browser.go first-load flash.
#[tokio::test]
async fn pod_view_title_rows_and_flash() {
    let mut h = Harness::new().await;
    h.goto("po").await.unwrap();
    assert_eq!(h.top().title(), " pods(default)[7] ");
    assert_eq!(
        h.flashes.first(),
        Some(&(
            FlashLevel::Info,
            "Viewing v1/pods in namespace default".to_string()
        ))
    );
    let s = h.render();
    dump(&s);
    assert!(s.line(0).contains(" pods(default)[7] "), "{}", s.line(0));
    let hdr: Vec<String> = s.line(1).split_whitespace().map(String::from).collect();
    assert_eq!(
        hdr,
        [
            "│", "NAME↑", "PF", "READY", "STATUS", "RESTARTS", "CPU", "%CPU/R", "%CPU/L", "MEM",
            "%MEM/R", "%MEM/L", "IP", "NODE", "AGE", "│"
        ]
    );
    assert_eq!(first_words(&s.table_names("NAME↑")), POD_NAMES);
    let row = s.line(s.row_of("api-6f5d8c7b9-xyz12").unwrap());
    for cell in [
        "0/1",
        "CrashLoopBackOff",
        "7",
        "10.244.0.12",
        "node-1",
        "120m",
    ] {
        assert!(row.contains(cell), "{cell} missing in {row}");
    }
    let pending = s.line(s.row_of("pending-pod").unwrap());
    assert!(pending.contains("n/a"), "{pending}");
}

/// Crumbs show the lowercased view names (ui/crumbs.go): `<pod>`.
#[tokio::test]
async fn crumb_name_is_pod() {
    let mut h = Harness::new().await;
    h.goto("pods").await.unwrap();
    let crumbs: Vec<String> = h.crumbs().iter().map(|c| c.to_lowercase()).collect();
    assert_eq!(crumbs, ["pod"]);
}

/// stock-skin.yaml frame.title / frame.border / views.table.header.
#[tokio::test]
async fn frame_title_and_header_colors() {
    let mut h = Harness::new().await;
    h.goto("po").await.unwrap();
    let s = h.render();
    let (x, _) = s.find("pods(").unwrap();
    assert_eq!(s.fg_at(x, 0), color("aqua"), "title fg");
    assert_eq!(s.fg_at(x + 5, 0), color("fuchsia"), "namespace highlight");
    let (cx, _) = s.find("[7]").unwrap();
    assert_eq!(s.fg_at(cx + 1, 0), color("papayawhip"), "counter");
    assert_eq!(s.fg_at(0, 0), color("lightskyblue"), "focused border");
    let (nx, ny) = s.find("NAME↑").unwrap();
    assert_eq!(s.fg_at(nx, ny), color("white"), "header fg");
    assert_eq!(s.bg_at(nx, ny), color("black"), "header bg");
    assert_eq!(s.fg_at(nx + 4, ny), color("aqua"), "sorter indicator");
}

/// view/pod.go + browser.go bindKeys: the pod menu hints.
#[tokio::test]
async fn pod_menu_hints() {
    let mut h = Harness::new().await;
    h.goto("po").await.unwrap();
    let hints = h.top().hints();
    let visible: Vec<(String, String)> = hints
        .iter()
        .filter(|h| h.visible)
        .map(|h| (h.mnemonic.clone(), h.description.clone()))
        .collect();
    for (m, d) in [
        ("0", "all"),
        ("1", "default"),
        ("ctrl-d", "Delete"),
        ("ctrl-k", "Kill"),
        ("d", "Describe"),
        ("e", "Edit"),
        ("l", "Logs"),
        ("p", "Logs Previous"),
        ("s", "Shell"),
        ("a", "Attach"),
        ("y", "YAML"),
        ("o", "Show Node"),
        ("t", "Transfer"),
        ("z", "Sanitize"),
        ("shift-f", "Port-Forward"),
        ("shift-j", "Jump Owner"),
        ("?", "Help"),
    ] {
        assert!(
            visible.contains(&(m.to_string(), d.to_string())),
            "missing visible hint <{m}> {d}: {visible:?}"
        );
    }
    for (m, d) in [
        ("enter", "View"),
        ("ctrl-w", "Toggle Wide"),
        ("shift-n", "Sort Name"),
        ("c", "Copy"),
        ("n", "Copy Namespace"),
    ] {
        assert!(
            hints
                .iter()
                .any(|h| !h.visible && h.mnemonic == m && h.description == d),
            "missing hidden hint <{m}> {d}"
        );
    }
}

/// The K9rs logo in k9s's LogoSmall style: same height, equal-width rows,
/// the header logo column fits it.
#[test]
fn logo_is_k9rs() {
    let l = k9rs::logo::LOGO_SMALL;
    assert_eq!(l.len(), 6);
    assert!(l.iter().all(|r| r.len() == l[0].len()));
    assert_eq!(l[1], r"|    |/  /   __   \_______  ______");
    assert_eq!(l[4], r"|____|\__ \/____/   |__|  /____  /");
}
