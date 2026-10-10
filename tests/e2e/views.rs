//! Containers, xray, pulses, nodes and events views.

use crate::common::*;
use crossterm::event::KeyCode;
use ratatui::backend::TestBackend;
use ratatui::Terminal;

fn render_w(h: &mut Harness, w: u16) -> Screen {
    let mut t = Terminal::new(TestBackend::new(w, H)).unwrap();
    let ctx = &h.ctx;
    let top = h.stack.last_mut().expect("a view");
    t.draw(|f| top.draw(f, f.area(), ctx)).unwrap();
    Screen(t.backend().buffer().clone())
}

/// tview Table.Draw only skips leading columns while the table overflows
/// and clamps columnOffset to what it skipped: Right on a table that fits
/// is a no-op, and on a narrow one scrolls, then Left scrolls back.
#[tokio::test]
async fn right_scrolls_only_when_table_overflows() {
    let mut h = Harness::new().await;
    h.goto("po").await.unwrap();
    let fits = h.render();
    let hdr = fits.row_of("NAME").expect("header");
    for _ in 0..3 {
        h.code(KeyCode::Right).await;
    }
    let s = h.render();
    assert_eq!(
        s.line(hdr),
        fits.line(hdr),
        "a fitting table must not scroll"
    );
    // One Left after the clamp scrolls nowhere either.
    h.code(KeyCode::Left).await;
    assert_eq!(h.render().line(hdr), fits.line(hdr));

    let narrow = render_w(&mut h, 60);
    assert!(narrow.line(hdr).contains("NAME"));
    h.code(KeyCode::Right).await;
    let scrolled = render_w(&mut h, 60);
    assert!(
        !scrolled.line(hdr).contains("NAME"),
        "{}",
        scrolled.line(hdr)
    );
    h.code(KeyCode::Left).await;
    assert!(render_w(&mut h, 60).line(hdr).contains("NAME"));
}

/// node.go yamlCmd: `y` on a node opens a one-shot YAML Details view (no
/// auto-refresh/managed-fields/decode toggles, unlike the live YAML view).
#[tokio::test]
async fn node_yaml_is_static_details() {
    let mut h = Harness::new().await;
    h.goto("no").await.unwrap();
    h.press('y').await;
    let top = h.top();
    assert!(top.title().starts_with(" YAML("), "{}", top.title());
    let hints: Vec<String> = top.hints().into_iter().map(|h| h.description).collect();
    for absent in [
        "Toggle Auto-Refresh",
        "Toggle ManagedFields",
        "Toggle Decode",
    ] {
        assert!(!hints.iter().any(|d| d == absent), "{hints:?}");
    }
    assert!(hints.iter().any(|d| d == "Copy"), "{hints:?}");
}

fn cursor_y(s: &Screen) -> Option<u16> {
    (2..H).find(|&y| s.bg_at(2, y) != s.bg_at(0, y) && s.fg_at(2, y) == color("black"))
}

/// tview Table.MouseHandler: a click selects the row under the pointer;
/// header, borders and rows past the end select the first row; the wheel
/// scrolls the viewport but never pushes the selection out of it.
#[tokio::test]
async fn mouse_selects_table_rows() {
    use k9rs::ui::Mouse;
    let mut h = Harness::new().await;
    h.goto("po").await.unwrap();
    let area = ratatui::layout::Rect::new(0, 0, W, H);
    let first = cursor_y(&h.render()).expect("cursor");
    assert_eq!(first, 2);
    let click = |h: &mut Harness, y: u16| {
        let ctx = &mut h.ctx;
        h.stack
            .last_mut()
            .unwrap()
            .on_mouse(Mouse::Click { x: 10, y }, area, ctx);
    };
    click(&mut h, 4);
    assert_eq!(cursor_y(&h.render()), Some(4));
    click(&mut h, 1); // header
    assert_eq!(cursor_y(&h.render()), Some(2));
    click(&mut h, 5);
    click(&mut h, 40); // below the last row
    assert_eq!(cursor_y(&h.render()), Some(2));
    // Few rows: nothing to scroll.
    let ctx = &mut h.ctx;
    h.stack
        .last_mut()
        .unwrap()
        .on_mouse(Mouse::ScrollDown { x: 10, y: 5 }, area, ctx);
    assert_eq!(cursor_y(&h.render()), Some(2));
}

/// browser.go Start: Model.Watch for a cluster-scoped resource checks list
/// access in namespace "" and flashes "Watcher failed" when denied, even if
/// the active namespace allows it (live: pv on t1).
#[tokio::test]
async fn cluster_scoped_watch_denied_flashes() {
    let mut h = Harness::new().await;
    h.acc
        .denied
        .lock()
        .unwrap()
        .push(("v1/persistentvolumes".into(), "".into()));
    h.goto("pv").await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(700)).await;
    h.settle().await;
    let f = h.last_flash().cloned();
    assert!(
        matches!(&f, Some((k9rs::ui::FlashLevel::Err, m)) if m.starts_with("Watcher failed for v1/persistentvolumes -- ")),
        "{f:?}"
    );
}
