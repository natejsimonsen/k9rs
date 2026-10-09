//! Table behavior: row colors, sorting, wide, filters, marks, favorites.

use crate::common::*;
use crossterm::event::KeyCode;
use k9rs::ui::{Dialog, FlashLevel};

fn names(s: &Screen) -> Vec<String> {
    let ls = s.lines();
    let start = 2;
    ls[start..]
        .iter()
        .map(|l| {
            l.trim_matches(|c: char| c == '│' || c.is_whitespace())
                .to_string()
        })
        .take_while(|l| !l.is_empty() && !l.starts_with('└'))
        .map(|l| {
            let w: Vec<&str> = l.split_whitespace().collect();
            // Skip the NAMESPACE column in all-namespaces mode.
            if w.len() > 1 && ["default", "kube-system"].contains(&w[0]) {
                w[1].to_string()
            } else {
                w[0].to_string()
            }
        })
        .collect()
}

async fn pods() -> Harness {
    let mut h = Harness::new().await;
    h.goto("po").await.unwrap();
    h
}

/// render/pod.go ColorerFunc with the stock skin frame.status colors.
#[tokio::test]
async fn row_colors_per_pod_status() {
    let mut h = pods().await;
    // Park the cursor on the last row so the others show their own colors.
    for _ in 0..6 {
        h.code(KeyCode::Down).await;
    }
    let s = h.render();
    dump(&s);
    assert_eq!(
        s.fg_of("api-6f5d8c7b9-xyz12"),
        color("orangered"),
        "crashloop"
    );
    assert_eq!(
        s.fg_of("migrate-28xkq"),
        color("lightslategray"),
        "completed"
    );
    assert_eq!(
        s.fg_of("nginx-7d9c5b8f4-abcde"),
        color("lightskyblue"),
        "running"
    );
    assert_eq!(
        s.fg_of("old-worker-0"),
        color("mediumpurple"),
        "terminating"
    );
    assert_eq!(s.fg_of("pending-pod"), color("darkorange"), "pending");
    // Cursor row (ui/table.go selectionChanged): bg = row color, fg = cursor fg.
    assert_eq!(s.bg_of("web-0"), color("lightskyblue"));
    assert_eq!(s.fg_of("web-0"), color("black"));
    // Whole row is painted, not just the name cell.
    let y = s.row_of("pending-pod").unwrap();
    let (x, _) = s.find("Pending").unwrap();
    assert_eq!(s.fg_at(x, y), color("darkorange"));
}

/// ui/table.go SortColCmd: shift-<letter> sorts, again flips direction.
#[tokio::test]
async fn sort_keys() {
    let mut h = pods().await;
    h.key(shift('S')).await;
    let s = h.render();
    assert!(s.line(1).contains("STATUS↑"), "{}", s.line(1));
    assert_eq!(
        names(&s)[..3],
        ["migrate-28xkq", "api-6f5d8c7b9-xyz12", "pending-pod"]
    );
    assert_eq!(names(&s).last().unwrap(), "old-worker-0");
    h.key(shift('S')).await;
    let s = h.render();
    assert!(s.line(1).contains("STATUS↓"), "{}", s.line(1));
    assert_eq!(names(&s)[0], "old-worker-0");

    // Time columns sort by duration (model1 row Less): youngest first.
    h.key(shift('A')).await;
    let s = h.render();
    assert!(s.line(1).contains("AGE↑"), "{}", s.line(1));
    assert_eq!(names(&s)[0], "pending-pod");
    assert_eq!(names(&s).last().unwrap(), "web-0");
    h.key(shift('A')).await;
    let s = h.render();
    assert!(s.line(1).contains("AGE↓"), "{}", s.line(1));
    assert_eq!(names(&s)[0], "web-0");

    h.key(shift('N')).await;
    let s = h.render();
    assert!(s.line(1).contains("NAME↑"), "{}", s.line(1));
    assert_eq!(names(&s)[0], "api-6f5d8c7b9-xyz12");
    h.key(shift('N')).await;
    let s = h.render();
    assert!(s.line(1).contains("NAME↓"), "{}", s.line(1));
    assert_eq!(names(&s)[0], "web-0");
}

/// ctrl-w toggles the wide columns (render/pod.go Wide attrs).
#[tokio::test]
async fn wide_toggle() {
    let mut h = pods().await;
    let narrow = h.render().line(1);
    assert!(!narrow.contains("LAST RESTART") && !narrow.contains("CPU/RL"));
    h.ctrl('w').await;
    let wide = h.render().line(1);
    for c in ["LAST RESTART", "CPU/RL", "MEM/RL", "GPU/RL"] {
        assert!(wide.contains(c), "{c} missing: {wide}");
    }
    let row = h.render();
    let api = row.line(row.row_of("api-6f5d8c7b9-xyz12").unwrap());
    assert!(api.contains("100:500") && api.contains("64:128"), "{api}");
    h.ctrl('w').await;
    assert!(!h.render().line(1).contains("CPU/RL"));
}

/// `/` filter: regex (case insensitive), title `</filter>` in filter color.
#[tokio::test]
async fn filter_regex() {
    let mut h = pods().await;
    h.filter("nginx").await;
    assert_eq!(h.top().title(), " pods(default)[2] </nginx> ");
    let s = h.render();
    assert_eq!(
        names(&s),
        ["nginx-7d9c5b8f4-abcde", "nginx-7d9c5b8f4-fghij"]
    );
    let (x, _) = s.find("</nginx>").unwrap();
    assert_eq!(s.fg_at(x + 1, 0), color("seagreen"), "filter color");

    h.filter("FGHIJ|web-.").await;
    assert_eq!(names(&h.render()), ["nginx-7d9c5b8f4-fghij", "web-0"]);
    // Rows can match on any column, e.g. the status.
    h.filter("crashloop").await;
    assert_eq!(names(&h.render()), ["api-6f5d8c7b9-xyz12"]);
}

/// `!` inverts the match (internal/model1 rxFilter inverse).
#[tokio::test]
async fn filter_inverse() {
    let mut h = pods().await;
    h.filter("!nginx").await;
    assert_eq!(h.top().title(), " pods(default)[5] </!nginx> ");
    let n = names(&h.render());
    assert_eq!(n.len(), 5);
    assert!(n.iter().all(|r| !r.contains("nginx")), "{n:?}");
}

/// `-f` fuzzy filter keeps the raw buffer in the title.
#[tokio::test]
async fn filter_fuzzy() {
    let mut h = pods().await;
    h.filter("-f ngnxab").await;
    assert_eq!(h.top().title(), " pods(default)[1] </-f ngnxab> ");
    assert_eq!(names(&h.render()), ["nginx-7d9c5b8f4-abcde"]);
}

/// `-l` label filter goes server side; rows match the selector.
#[tokio::test]
async fn filter_label_rows() {
    let mut h = pods().await;
    h.filter("-l app=api").await;
    assert_eq!(names(&h.render()), ["api-6f5d8c7b9-xyz12"]);
    h.filter("-l tier=web").await;
    assert_eq!(
        names(&h.render()),
        ["nginx-7d9c5b8f4-abcde", "nginx-7d9c5b8f4-fghij"]
    );
}

/// ui/table.go styleTitle: a `-l` buffer is shown as the parsed selector
/// (`ExtractLabelSelector(buff).String()`), not the raw `-l ...` text.
#[tokio::test]
async fn filter_label_title() {
    let mut h = pods().await;
    h.filter("-l app=api").await;
    assert_eq!(h.top().title(), " pods(default)[1] </app=api> ");
    let s = h.render();
    assert!(
        s.line(0).contains(" pods(default)[1] </app=api> "),
        "{}",
        s.line(0)
    );
}

/// Esc on the table resets the filter (view/browser.go resetCmd).
#[tokio::test]
async fn esc_clears_filter() {
    let mut h = pods().await;
    h.filter("nginx").await;
    assert_eq!(names(&h.render()).len(), 2);
    h.code(KeyCode::Esc).await;
    assert_eq!(h.top().title(), " pods(default)[7] ");
    assert_eq!(names(&h.render()).len(), 7);
}

/// space marks rows (markColor), ctrl-d then targets every marked row,
/// ctrl-\ clears the marks.
#[tokio::test]
async fn marks() {
    let mut h = pods().await;
    h.press(' ').await;
    h.code(KeyCode::Down).await;
    h.code(KeyCode::Down).await;
    h.press(' ').await;
    h.code(KeyCode::Down).await;
    let s = h.render();
    assert_eq!(s.fg_of("api-6f5d8c7b9-xyz12"), color("palegreen"));
    assert_eq!(s.fg_of("nginx-7d9c5b8f4-abcde"), color("palegreen"));
    assert_eq!(s.fg_of("migrate-28xkq"), color("lightslategray"));
    h.ctrl('d').await;
    match h.take_dialog() {
        Dialog::Delete { msg, on_ok } => {
            assert_eq!(msg, "Delete 2 marked v1/pods?");
            let cmds = on_ok(&mut h.ctx, Default::default());
            h.apply(cmds);
            h.settle().await;
        }
        _ => panic!("expected delete dialog"),
    }
    let mut calls = h.acc.calls();
    calls.sort();
    assert_eq!(
        calls,
        [
            "delete v1/pods default/api-6f5d8c7b9-xyz12 force=false",
            "delete v1/pods default/nginx-7d9c5b8f4-abcde force=false",
        ]
    );
    assert!(h
        .flashes
        .contains(&(FlashLevel::Info, "Delete 2 marked v1/pods".to_string())));

    let mut h = pods().await;
    h.press(' ').await;
    h.code(KeyCode::Down).await;
    h.key(crossterm::event::KeyEvent::new(
        KeyCode::Char('\\'),
        crossterm::event::KeyModifiers::CONTROL,
    ))
    .await;
    let s = h.render();
    assert_eq!(
        s.fg_of("api-6f5d8c7b9-xyz12"),
        color("orangered"),
        "mark cleared"
    );
}

/// Number keys switch to favorite namespaces (view/browser.go
/// namespaceActions + switchNamespaceCmd flash).
#[tokio::test]
async fn namespace_favorites() {
    let mut h = Harness::with(Setup {
        context_yaml: Some(
            "k9s:\n  namespace:\n    active: default\n    favorites:\n    - default\n    - kube-system\n    - monitoring\n"
                .into(),
        ),
        ..Default::default()
    })
    .await;
    h.goto("po").await.unwrap();
    let favs: Vec<(String, String)> = h
        .top()
        .hints()
        .into_iter()
        .filter(|x| x.mnemonic.len() == 1 && x.mnemonic.chars().all(|c| c.is_ascii_digit()))
        .map(|x| (x.mnemonic, x.description))
        .collect();
    assert_eq!(
        favs,
        [
            ("0".to_string(), "all".to_string()),
            ("1".into(), "default".into()),
            ("2".into(), "kube-system".into()),
            ("3".into(), "monitoring".into()),
        ]
    );
    h.flashes.clear();
    h.press('2').await;
    assert_eq!(h.top().title(), " pods(kube-system)[2] ");
    assert_eq!(h.ctx.namespace, "kube-system");
    assert_eq!(
        h.flashes.first(),
        Some(&(
            FlashLevel::Info,
            "Viewing v1/pods in namespace `kube-system`...".to_string()
        ))
    );
    h.press('0').await;
    assert_eq!(h.top().title(), " pods(all)[9] ");
    assert!(h.render().line(1).contains("NAMESPACE"));
    h.press('1').await;
    assert_eq!(h.top().title(), " pods(default)[7] ");
}
