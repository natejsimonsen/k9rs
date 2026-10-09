//! Logs view: title, indicator line, since keys, toggles.

use crate::common::*;
use crossterm::event::KeyCode;

async fn nginx_logs() -> Harness {
    let mut h = Harness::new().await;
    h.goto("po").await.unwrap();
    h.code(KeyCode::Down).await;
    h.code(KeyCode::Down).await;
    h.press('l').await;
    h
}

/// view/log.go updateTitle + log_indicator.go Refresh (stock colors).
#[tokio::test]
async fn indicator_and_lines() {
    let mut h = nginx_logs().await;
    assert_eq!(h.top().name(), "logs");
    assert_eq!(
        h.top().title(),
        " Logs(default/nginx-7d9c5b8f4-abcde:nginx)[tail] "
    );
    let s = h.render();
    dump(&s);
    let ind: Vec<String> = s.line(1).split_whitespace().map(String::from).collect();
    // No AllContainers toggle: the pod has no default-container annotation
    // (view/log.go only adds it when model.HasDefaultContainer).
    assert_eq!(
        ind,
        [
            "│",
            "Autoscroll:On",
            "ColumnLock:Off",
            "FullScreen:Off",
            "Timestamps:Off",
            "Wrap:Off",
            "│"
        ]
    );
    assert_eq!(s.fg_at(0, 0), color("dodgerblue"), "frame border");
    let (x, y) = s.find("Autoscroll:On").unwrap();
    assert_eq!(s.fg_at(x + 11, y), color("limegreen"), "toggle on");
    let (x, y) = s.find("ColumnLock:Off").unwrap();
    assert_eq!(s.fg_at(x + 11, y), color("gray"), "toggle off");
    for i in 1..=5 {
        assert!(s.contains(&format!("GET /healthz 200 request-{i}")));
    }
    let (_, y1) = s.find("request-1").unwrap();
    assert_eq!(y1, 2, "lines start under the indicator");
}

/// Since keys 0-6 (view/log.go bindKeys) update the title.
#[tokio::test]
async fn since_keys() {
    let mut h = nginx_logs().await;
    for (k, since) in [
        ('2', "1m"),
        ('3', "5m"),
        ('4', "15m"),
        ('5', "30m"),
        ('6', "1h"),
        ('1', "head"),
        ('0', "tail"),
    ] {
        h.press(k).await;
        assert_eq!(
            h.top().title(),
            format!(" Logs(default/nginx-7d9c5b8f4-abcde:nginx)[{since}] "),
            "key {k}"
        );
    }
    let hints: Vec<(String, String)> = h
        .top()
        .hints()
        .into_iter()
        .map(|x| (x.mnemonic, x.description))
        .collect();
    for (m, d) in [
        ("0", "tail"),
        ("1", "head"),
        ("6", "1h"),
        ("s", "Toggle AutoScroll"),
        ("w", "Toggle Wrap"),
        ("t", "Toggle Timestamp"),
        ("f", "Toggle FullScreen"),
        ("shift-c", "Clear"),
        ("ctrl-s", "Save"),
    ] {
        assert!(
            hints.contains(&(m.into(), d.into())),
            "<{m}> {d}: {hints:?}"
        );
    }
}

/// Toggles flip the indicator; `t` shows padded timestamps
/// (dao/log_item.go Render), `f` drops the border. With autoscroll off k9s
/// drops buffer refreshes (view/log.go Flush), so `t` goes first.
#[tokio::test]
async fn toggles() {
    let mut h = nginx_logs().await;
    h.press('t').await;
    h.press('s').await;
    h.press('w').await;
    let s = h.render();
    let l1 = s.line(1);
    for t in ["Autoscroll:Off", "Wrap:On", "Timestamps:On"] {
        assert!(l1.contains(t), "{t}: {l1}");
    }
    let row = s.line(s.row_of("request-1").unwrap());
    let body = row.trim_matches(|c: char| c == '│' || c == ' ');
    assert!(
        body.starts_with("2026-10-09T10:00:01Z           GET /healthz 200 request-1"),
        "{body:?}"
    );
    let (x, y) = s.find("2026-10-09T10:00:01Z").unwrap();
    assert_eq!(s.fg_at(x, y), color("gray"), "timestamp color");
    h.press('f').await;
    let s = h.render();
    assert!(s.line(0).contains("FullScreen:On"), "{}", s.line(0));
    assert!(!s.line(0).contains('┌'));
}
