//! Header: cluster info (view/cluster_info.go), menu hints (ui/menu.go),
//! logo + status (ui/logo.go) and the collapsed status indicator
//! (ui/indicator.go).

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use super::text::{draw_segs, fill, seg, Align, Seg};
use crate::config::Styles;
use crate::k8s::ClusterInfo;
use crate::logo::LOGO_SMALL;
use crate::ui::{FlashLevel, Hint};

/// Header height (view/app.go buildHeader flex item size).
pub(crate) const HEADER_HEIGHT: u16 = 7;
/// view/app.go clusterInfoWidth / clusterInfoPad.
const CLUSTER_INFO_WIDTH: usize = 50;
const CLUSTER_INFO_PAD: usize = 15;
/// Logo column width.
pub(crate) const LOGO_WIDTH: u16 = 35;
/// ui/menu.go maxRows.
const MAX_ROWS: usize = 6;

const NA: &str = "n/a";
/// Markup names (orangered, ..): tcell fits them to the 256 palette.
const ORANGE_RED: Color = Color::Indexed(202);
const MEDIUM_VIOLET_RED: Color = Color::Indexed(162);
const LAWN_GREEN: Color = Color::Indexed(118);
const CADET_BLUE: Color = Color::Indexed(73);

/// What the header needs to render.
pub(crate) struct HeaderModel<'a> {
    pub info: &'a ClusterInfo,
    pub prev: &'a ClusterInfo,
    pub has_metrics: bool,
    pub readonly: bool,
    pub no_icons: bool,
    pub hints: &'a [Hint],
    pub show_logo: bool,
    pub status: Option<&'a (FlashLevel, String)>,
    /// Cluster info border padding (0, 0, 1, 0).
    pub padded: bool,
}

/// Width of the cluster info column.
pub(crate) fn cluster_info_width(info: &ClusterInfo) -> u16 {
    let n = info.cluster.len() + CLUSTER_INFO_PAD;
    n.max(CLUSTER_INFO_WIDTH) as u16
}

pub(crate) fn draw_header(buf: &mut Buffer, area: Rect, m: &HeaderModel<'_>, styles: &Styles) {
    fill(buf, area, Style::default().bg(styles.bg));
    let cl_w = cluster_info_width(m.info).min(area.width);
    let ci = Rect::new(area.x, area.y, cl_w, area.height);
    draw_cluster_info(buf, ci, m, styles);
    let logo_w = if m.show_logo {
        LOGO_WIDTH.min(area.width.saturating_sub(cl_w))
    } else {
        0
    };
    let menu_w = area.width.saturating_sub(cl_w + logo_w);
    let menu = Rect::new(area.x + cl_w, area.y, menu_w, area.height);
    draw_menu(buf, menu, m.hints, styles);
    if logo_w > 0 {
        let logo = Rect::new(area.x + cl_w + menu_w, area.y, logo_w, area.height);
        draw_logo(buf, logo, m.status, styles);
    }
}

// ---------------------------------------------------------------------------
// Cluster info

fn perc_delta(prev: Option<i64>, cur: Option<i64>) -> Vec<(String, Option<Color>)> {
    let c = cur.unwrap_or(0);
    let mut out = vec![(format!("{c}%"), None)];
    if let Some(p) = prev {
        if p < c {
            out.push(("↑".into(), Some(Color::LightRed)));
        } else if p > c {
            out.push(("↓".into(), Some(Color::Green)));
        }
    }
    out
}

/// ROIndicator (ui/table.go).
fn ro_indicator(ro: bool, no_icons: bool) -> &'static str {
    if no_icons {
        ""
    } else if ro {
        "[R]"
    } else {
        "[RW]"
    }
}

fn or_na(s: &str) -> String {
    if s.is_empty() {
        NA.to_string()
    } else {
        s.to_string()
    }
}

/// cluster_info.go: `%s ⚡️[cadetblue::b]%s` when a newer release exists.
fn k9s_rev_segs(k9s: &str, latest: &str, val: Style) -> Vec<Seg> {
    if latest.is_empty() {
        return vec![seg(or_na(k9s), val)];
    }
    vec![
        seg(format!("{k9s} \u{26a1}\u{fe0f}"), val),
        seg(latest.to_string(), val.fg(CADET_BLUE)),
    ]
}

pub(crate) fn cluster_info_rows(m: &HeaderModel<'_>, styles: &Styles) -> Vec<(String, Vec<Seg>)> {
    let val = Style::default()
        .fg(styles.info_section)
        .bg(styles.bg)
        .add_modifier(Modifier::BOLD);
    // model.NewClusterMeta: n/a until the cluster is known.
    let mut context = or_na(&m.info.context);
    let ic = ro_indicator(m.readonly, m.no_icons);
    if !ic.is_empty() {
        context = format!("{context} {ic}");
    }
    let k9s = if m.info.k9s_version.is_empty() {
        crate::version::VERSION.to_string()
    } else {
        m.info.k9s_version.clone()
    };
    let mut rows = vec![
        ("Context".to_string(), vec![seg(or_na(&context), val)]),
        ("Cluster".into(), vec![seg(or_na(&m.info.cluster), val)]),
        ("User".into(), vec![seg(or_na(&m.info.user), val)]),
        (
            "K9rs Rev".into(),
            k9s_rev_segs(&k9s, &m.info.k9s_latest, val),
        ),
        ("K8s Rev".into(), vec![seg(or_na(&m.info.k8s_version), val)]),
    ];
    let metrics = m.has_metrics && m.info.cpu_pct.is_some() && m.info.mem_pct.is_some();
    for (label, prev, cur) in [
        ("CPU", m.prev.cpu_pct, m.info.cpu_pct),
        ("MEM", m.prev.mem_pct, m.info.mem_pct),
    ] {
        if metrics {
            let segs = perc_delta(prev, cur)
                .into_iter()
                .map(|(t, c)| seg(t, c.map(|c| val.fg(c)).unwrap_or(val)))
                .collect();
            rows.push((label.into(), segs));
        } else {
            rows.push((label.into(), vec![seg(NA, val.fg(ORANGE_RED))]));
        }
    }
    rows
}

fn draw_cluster_info(buf: &mut Buffer, area: Rect, m: &HeaderModel<'_>, styles: &Styles) {
    let rows = cluster_info_rows(m, styles);
    let label_style = Style::default().fg(styles.info_fg).bg(styles.bg);
    let labels: Vec<String> = rows.iter().map(|(l, _)| format!("{l}:")).collect();
    let lw = labels.iter().map(|l| l.len()).max().unwrap_or(0) as u16;
    // Border padding (0, 0, 1, 0).
    let pad = u16::from(m.padded);
    let x0 = area.x + pad;
    let avail = area.width.saturating_sub(pad);
    for (i, ((_, val), label)) in rows.iter().zip(labels.iter()).enumerate() {
        let y = area.y + i as u16;
        if y >= area.bottom() {
            break;
        }
        draw_segs(
            buf,
            x0,
            y,
            lw.min(avail),
            &[seg(label.clone(), label_style)],
            Align::Left,
            false,
        );
        let vx = x0 + lw + 1;
        if vx < area.x + area.width {
            // tview tables never draw their overflow ellipsis (derailed/tview
            // table.go passes it a bogus position).
            let w = area.x + area.width - vx;
            draw_segs(buf, vx, y, w, val, Align::Left, false);
            table_ellipsis(buf, vx, y, w, val);
        }
    }
}

/// derailed/tview table.go draws a truncated cell's ellipsis with
/// printWithStyle(x+w, 0, skipWidth=y): it only lands on screen row 0, in the
/// cell's last column.
fn table_ellipsis(buf: &mut Buffer, x: u16, y: u16, w: u16, segs: &[Seg]) {
    if y != 0 || w == 0 || super::text::segs_width(segs) <= w as usize {
        return;
    }
    if let Some(cell) = buf.cell_mut((x + w - 1, y)) {
        cell.set_symbol(super::text::ELLIPSIS);
    }
}

// ---------------------------------------------------------------------------
// Menu

fn is_digit_hint(h: &Hint) -> bool {
    h.mnemonic.chars().any(|c| c.is_ascii_digit())
}

/// k9s MenuHints sort: numeric mnemonics first (ascending), then by description.
pub(crate) fn sort_hints(hh: &mut [Hint]) {
    hh.sort_by(|a, b| {
        let na = a.mnemonic.parse::<i64>();
        let nb = b.mnemonic.parse::<i64>();
        match (na, nb) {
            (Ok(x), Ok(y)) => x.cmp(&y),
            (Ok(_), Err(_)) => std::cmp::Ordering::Less,
            (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
            _ => a.description.cmp(&b.description),
        }
    });
}

fn key_conv(s: &str) -> String {
    if s.is_empty() || !s.contains("alt") || !cfg!(target_os = "macos") {
        return s.to_string();
    }
    s.replacen("alt", "opt", 1)
}

/// ui.ToMnemonic.
pub(crate) fn to_mnemonic(s: &str) -> String {
    if s.is_empty() {
        return String::new();
    }
    format!("<{}>", key_conv(&s.to_lowercase()))
}

#[derive(Clone, Debug, Default)]
struct Slot(Option<Hint>);

/// Lay hints out in columns of up to 6 rows (Menu.buildMenuTable). Returns
/// the formatted cells, `[row][col]`.
pub(crate) fn menu_table(hints: &[Hint], styles: &Styles) -> Vec<Vec<Vec<Seg>>> {
    let mut hh = hints.to_vec();
    sort_hints(&mut hh);
    let mut col_count = hh.len() / MAX_ROWS + 1;
    if hh.iter().any(|h| h.visible && is_digit_hint(h)) {
        col_count += 1;
    }
    let mut table: Vec<Vec<Slot>> = vec![vec![Slot::default(); col_count]; MAX_ROWS + 1];
    let mut max_keys = vec![0usize; col_count];
    let (mut row, mut col) = (0usize, 0usize);
    let mut first_cmd = true;
    for h in hh.iter().filter(|h| h.visible) {
        if !is_digit_hint(h) && first_cmd {
            row = 0;
            col += 1;
            first_cmd = false;
            if table[0][0].0.is_none() {
                col = 0;
            }
        }
        if col >= col_count {
            for r in table.iter_mut() {
                r.resize(col + 1, Slot::default());
            }
            max_keys.resize(col + 1, 0);
            col_count = col + 1;
        }
        max_keys[col] = max_keys[col].max(h.mnemonic.len());
        table[row][col] = Slot(Some(h.clone()));
        row += 1;
        if row >= MAX_ROWS {
            row = 0;
            col += 1;
        }
    }
    table
        .iter()
        .map(|r| {
            r.iter()
                .enumerate()
                .map(|(c, s)| match &s.0 {
                    Some(h) => format_menu(h, max_keys[c], styles),
                    None => Vec::new(),
                })
                .collect()
        })
        .collect()
}

fn format_menu(h: &Hint, size: usize, styles: &Styles) -> Vec<Seg> {
    if h.mnemonic.is_empty() || h.description.is_empty() {
        return Vec::new();
    }
    // ui/menu.go: a `default` skin color is `-`, the cell's color (body fg).
    let fg = |c: Color| if c == Color::Reset { styles.fg } else { c };
    let plain = Style::default().bg(styles.bg);
    let desc = Style::default()
        .fg(fg(styles.menu_fg))
        .bg(styles.bg)
        .add_modifier(styles.menu_fg_style);
    if let Ok(i) = h.mnemonic.parse::<i64>() {
        let key = Style::default()
            .fg(fg(styles.menu_num_key))
            .bg(styles.bg)
            .add_modifier(Modifier::BOLD);
        return vec![
            seg(" ", plain),
            seg(format!("<{i}> "), key),
            seg(format!("{} ", h.description), desc),
        ];
    }
    let key = Style::default()
        .fg(fg(styles.menu_key))
        .bg(styles.bg)
        .add_modifier(Modifier::BOLD);
    let m = to_mnemonic(&h.mnemonic);
    vec![
        seg(" ", plain),
        seg(format!("{m:<w$} ", w = size + 2), key),
        seg(format!("{} ", h.description), desc),
    ]
}

pub(crate) fn draw_menu(buf: &mut Buffer, area: Rect, hints: &[Hint], styles: &Styles) {
    let table = menu_table(hints, styles);
    let cols = table.first().map(|r| r.len()).unwrap_or(0);
    let widths: Vec<u16> = (0..cols)
        .map(|c| {
            table
                .iter()
                .map(|r| super::text::segs_width(&r[c]) as u16)
                .max()
                .unwrap_or(0)
        })
        .collect();
    let mut x = area.x;
    let right = area.x + area.width;
    for (c, w) in widths.iter().enumerate() {
        if x >= right {
            break;
        }
        let cw = (*w).min(right - x);
        for (r, row) in table.iter().enumerate() {
            let y = area.y + r as u16;
            if y >= area.bottom() {
                break;
            }
            draw_segs(buf, x, y, cw, &row[c], Align::Left, false);
            table_ellipsis(buf, x, y, cw, &row[c]);
        }
        x = x.saturating_add(w + 1);
    }
}

// ---------------------------------------------------------------------------
// Logo

pub(crate) fn level_color(styles: &Styles, l: FlashLevel) -> Color {
    match l {
        FlashLevel::Err => styles.logo_error,
        FlashLevel::Warn => styles.logo_warn,
        FlashLevel::Info => styles.logo_info,
    }
}

fn draw_logo(buf: &mut Buffer, area: Rect, status: Option<&(FlashLevel, String)>, styles: &Styles) {
    // logo.go "[%s::b]": a `default` color is `-`, the logo text views'
    // own color, white (they are built before the skin loads).
    let white = |c: Color| if c == Color::Reset { Color::White } else { c };
    let color = status
        .map(|(l, _)| level_color(styles, *l))
        .unwrap_or(styles.logo);
    let style = Style::default()
        .fg(white(color))
        .bg(styles.bg)
        .add_modifier(Modifier::BOLD);
    for (i, l) in LOGO_SMALL.iter().enumerate() {
        let y = area.y + i as u16;
        if y >= area.bottom() {
            return;
        }
        draw_segs(
            buf,
            area.x,
            y,
            area.width,
            &[seg(*l, style)],
            Align::Left,
            false,
        );
    }
    let y = area.y + LOGO_SMALL.len() as u16;
    if y >= area.bottom() {
        return;
    }
    if let Some((l, msg)) = status {
        let bg = level_color(styles, *l);
        let row = Rect::new(area.x, y, area.width, 1);
        fill(buf, row, Style::default().bg(bg));
        draw_segs(
            buf,
            area.x,
            y,
            area.width,
            &[seg(
                msg.clone(),
                Style::default()
                    .fg(white(styles.logo_msg))
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            )],
            Align::Center,
            false,
        );
    }
}

// ---------------------------------------------------------------------------
// Status indicator (headless)

pub(crate) fn draw_indicator(
    buf: &mut Buffer,
    area: Rect,
    m: &HeaderModel<'_>,
    temp: Option<&(FlashLevel, String)>,
    styles: &Styles,
) {
    fill(buf, area, Style::default().fg(styles.fg).bg(styles.bg));
    let b = Style::default().bg(styles.bg);
    let segs = match temp {
        Some((l, msg)) => {
            let c = match l {
                FlashLevel::Err => ORANGE_RED,
                FlashLevel::Warn => MEDIUM_VIOLET_RED,
                FlashLevel::Info => LAWN_GREEN,
            };
            vec![seg(
                format!(" <{msg}> "),
                b.fg(c).add_modifier(Modifier::BOLD),
            )]
        }
        None => {
            let k9s = if m.info.k9s_version.is_empty() {
                crate::version::VERSION.to_string()
            } else {
                m.info.k9s_version.clone()
            };
            // ClusterInfoChanged shows deltas (k9s compares MEM against the
            // previous CPU there); ClusterInfoUpdated plain percentages.
            let changed = m.prev != m.info;
            // `[color::]` tags: not bold; an unset skin color ("-") falls
            // back to the text (body) color.
            let col = |c: Color| b.fg(if c == Color::Reset { styles.fg } else { c });
            let cpu_style = col(styles.info_cpu);
            let mem_style = col(styles.info_mem);
            let perc = |prev: Option<i64>, cur: Option<i64>, st: Style| -> Vec<Seg> {
                if changed {
                    perc_delta(Some(prev.unwrap_or(0)), cur)
                        .into_iter()
                        .map(|(t, c)| seg(t, c.map(|c| st.fg(c)).unwrap_or(st)))
                        .collect()
                } else {
                    vec![seg(format!("{}%", cur.unwrap_or(0)), st)]
                }
            };
            let mut segs = vec![
                seg("K9rs ", b.fg(styles.logo).add_modifier(Modifier::BOLD)),
                seg(format!("{k9s} "), col(styles.info_k9s_rev)),
                seg(
                    format!(
                        "{}:{}:{} ",
                        m.info.context, m.info.cluster, m.info.k8s_version
                    ),
                    col(styles.info_fg),
                ),
            ];
            segs.extend(perc(m.prev.cpu_pct, m.info.cpu_pct, cpu_style));
            segs.push(seg("::", col(styles.fg)));
            segs.extend(perc(m.prev.cpu_pct, m.info.mem_pct, mem_style));
            segs
        }
    };
    draw_segs(buf, area.x, area.y, area.width, &segs, Align::Center, false);
}

#[cfg(test)]
mod tests {
    use super::super::text::testutil::*;
    use super::*;

    fn info() -> ClusterInfo {
        ClusterInfo {
            context: "kind-dev".into(),
            cluster: "kind".into(),
            user: "admin".into(),
            k9s_version: "v0.50.18".into(),
            k9s_latest: String::new(),
            k8s_version: "v1.34.0".into(),
            cpu_pct: Some(12),
            mem_pct: Some(40),
        }
    }

    fn model<'a>(i: &'a ClusterInfo, p: &'a ClusterInfo, hints: &'a [Hint]) -> HeaderModel<'a> {
        HeaderModel {
            info: i,
            prev: p,
            has_metrics: true,
            readonly: false,
            no_icons: false,
            hints,
            show_logo: true,
            status: None,
            padded: true,
        }
    }

    /// Started disconnected: view/cluster_info.go Init never ran (no
    /// padding) and the meta is model.NewClusterMeta (n/a).
    /// ui/indicator.go: `[color::]` tags are not bold and the stock skin's
    /// unset cpu/mem/rev colors fall back to the body color.
    #[test]
    fn indicator_stock_colors() {
        let styles = Styles::from_skin(crate::config::Skin::stock());
        let i = info();
        let m = model(&i, &i, &[]);
        let mut buf = Buffer::empty(Rect::new(0, 0, 100, 1));
        draw_indicator(&mut buf, Rect::new(0, 0, 100, 1), &m, None, &styles);
        let row = all_rows(&buf)[0].clone();
        let x = row.find("v0.50.18").unwrap() as u16;
        assert_eq!(fg(&buf, x, 0), styles.fg);
        assert!(!buf[(x, 0)].modifier.contains(Modifier::BOLD));
        let k = row.find("K9rs").unwrap() as u16;
        assert!(buf[(k, 0)].modifier.contains(Modifier::BOLD));
        let c = row.find("12%").unwrap() as u16;
        assert_eq!(fg(&buf, c, 0), styles.fg);
        let ctx = row.find("kind-dev").unwrap() as u16;
        assert_eq!(fg(&buf, ctx, 0), styles.info_fg);
        assert!(!buf[(ctx, 0)].modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn cluster_info_disconnected() {
        let styles = Styles::default();
        let i = ClusterInfo {
            k9s_version: "v0.50.18".into(),
            ..Default::default()
        };
        let mut m = model(&i, &i, &[]);
        m.readonly = true;
        m.padded = false;
        let mut buf = Buffer::empty(Rect::new(0, 0, 120, 7));
        {
            let a = buf.area;
            draw_header(&mut buf, a, &m, &styles)
        };
        let rows = all_rows(&buf);
        assert!(rows[0].starts_with("Context:  n/a [R] "), "{}", rows[0]);
        assert!(rows[1].starts_with("Cluster:  n/a "));
        assert!(rows[3].starts_with("K9rs Rev: v0.50.18 "));
        assert!(rows[6].starts_with("MEM:      n/a "));
    }

    #[test]
    fn cluster_info_layout_and_colors() {
        let styles = Styles::default();
        let i = info();
        let mut p = info();
        p.cpu_pct = Some(10);
        p.mem_pct = Some(50);
        let m = model(&i, &p, &[]);
        let mut buf = Buffer::empty(Rect::new(0, 0, 120, 7));
        {
            let a = buf.area;
            draw_header(&mut buf, a, &m, &styles)
        };
        let rows = all_rows(&buf);
        assert!(
            rows[0].starts_with(" Context:  kind-dev [RW]"),
            "{}",
            rows[0]
        );
        assert!(rows[1].starts_with(" Cluster:  kind "));
        assert!(rows[2].starts_with(" User:     admin "));
        assert!(rows[3].starts_with(" K9rs Rev: v0.50.18 "));
        assert!(rows[4].starts_with(" K8s Rev:  v1.34.0 "));
        assert!(rows[5].starts_with(" CPU:      12%↑ "), "{}", rows[5]);
        assert!(rows[6].starts_with(" MEM:      40%↓ "), "{}", rows[6]);
        assert_eq!(fg(&buf, 1, 0), styles.info_fg);
        assert_eq!(fg(&buf, 11, 0), styles.info_section);
        assert!(buf[(11, 0)].modifier.contains(Modifier::BOLD));
        let (x, y) = find(&buf, "↑").unwrap();
        assert_eq!(fg(&buf, x, y), Color::LightRed);
        let (x, y) = find(&buf, "↓").unwrap();
        assert_eq!(fg(&buf, x, y), Color::Green);
        // Logo on the right in logo color.
        let (x, y) = find(&buf, "|    |/  /").unwrap();
        assert_eq!(x, 120 - LOGO_WIDTH);
        assert_eq!(fg(&buf, x, y), styles.logo);
    }

    #[test]
    fn no_metrics_shows_na() {
        let styles = Styles::default();
        let i = ClusterInfo {
            cpu_pct: None,
            mem_pct: None,
            ..info()
        };
        let mut m = model(&i, &i, &[]);
        m.has_metrics = false;
        m.readonly = true;
        let mut buf = Buffer::empty(Rect::new(0, 0, 120, 7));
        {
            let a = buf.area;
            draw_header(&mut buf, a, &m, &styles)
        };
        assert!(row(&buf, 0).starts_with(" Context:  kind-dev [R] "));
        assert!(row(&buf, 5).starts_with(" CPU:      n/a "));
        assert_eq!(fg(&buf, 11, 5), ORANGE_RED);
    }

    #[test]
    fn menu_columns() {
        let styles = Styles::default();
        let mut hints = vec![
            Hint::new("0", "all"),
            Hint::new("1", "default"),
            Hint::new("ctrl-d", "Delete"),
            Hint::new("d", "Describe"),
            Hint::new("e", "Edit"),
            Hint::new("?", "Help"),
            Hint::new("l", "Logs"),
            Hint::new("shift-f", "Port-Forward"),
            Hint::new("y", "YAML"),
            Hint::new("s", "Shell"),
            Hint::new("ctrl-k", "Kill"),
            Hint::new("x", "Hidden").hidden(),
        ];
        hints.reverse();
        let t = menu_table(&hints, &styles);
        let text =
            |r: usize, c: usize| -> String { t[r][c].iter().map(|s| s.text.clone()).collect() };
        assert_eq!(text(0, 0), " <0> all ");
        assert_eq!(text(1, 0), " <1> default ");
        assert_eq!(text(0, 1), " <ctrl-d> Delete ");
        assert_eq!(text(1, 1), " <d>      Describe ");
        assert_eq!(text(4, 1), " <ctrl-k> Kill ");
        assert_eq!(text(5, 1), " <l>      Logs ");
        assert_eq!(text(0, 2), " <shift-f> Port-Forward ");
        assert_eq!(text(1, 2), " <s>       Shell ");
        assert_eq!(t.len(), 7, "6 rows + blank row");
        assert!(t
            .iter()
            .flatten()
            .all(|c| !c.iter().any(|s| s.text.contains("Hidden"))));

        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 7));
        {
            let a = buf.area;
            draw_menu(&mut buf, a, &hints, &styles)
        };
        let (x, y) = find(&buf, "<0>").unwrap();
        assert_eq!((x, y), (1, 0));
        assert_eq!(fg(&buf, x, y), styles.menu_num_key);
        let (x, y) = find(&buf, "<ctrl-d>").unwrap();
        // column 0 is " <1> default " (13 wide) + 1 separator.
        assert_eq!((x, y), (15, 0));
        assert_eq!(fg(&buf, x, y), styles.menu_key);
        assert!(buf[(x, y)].modifier.contains(Modifier::BOLD));
        let (x, y) = find(&buf, "Delete").unwrap();
        assert_eq!(fg(&buf, x, y), styles.menu_fg);
    }

    #[test]
    fn menu_without_digits_starts_at_col0() {
        let styles = Styles::default();
        let hints = vec![Hint::new("a", "Alpha"), Hint::new("b", "Beta")];
        let t = menu_table(&hints, &styles);
        let text: String = t[0][0].iter().map(|s| s.text.clone()).collect();
        assert_eq!(text, " <a> Alpha ");
    }

    #[test]
    fn menu_clips_without_ellipsis() {
        let styles = Styles::default();
        let hints = vec![Hint::new("a", "A very long description here")];
        let mut buf = Buffer::empty(Rect::new(0, 0, 12, 7));
        {
            let a = buf.area;
            draw_menu(&mut buf, a, &hints, &styles)
        };
        // derailed/tview only lands the overflow ellipsis on screen row 0.
        assert_eq!(row(&buf, 0), " <a> A very…");
        let mut buf = Buffer::empty(Rect::new(0, 0, 12, 7));
        draw_menu(&mut buf, Rect::new(0, 1, 12, 6), &hints, &styles);
        assert_eq!(row(&buf, 1), " <a> A very ");
    }

    #[test]
    fn default_skin_colors_fall_back_like_k9s() {
        // kiss skin: logo/menu colors `default` render as `-` in k9s markup.
        let styles = Styles {
            logo: Color::Reset,
            menu_key: Color::Reset,
            fg: Color::Rgb(1, 2, 3),
            ..Default::default()
        };
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 7));
        let a = buf.area;
        draw_logo(&mut buf, a, None, &styles);
        assert_eq!(buf[(1, 0)].fg, Color::White);
        let h = Hint::new("d", "Describe");
        let segs = format_menu(&h, 1, &styles);
        assert_eq!(segs[1].style.fg, Some(Color::Rgb(1, 2, 3)));
    }

    #[test]
    fn logo_status_and_indicator() {
        let styles = Styles::default();
        let i = info();
        let st = (FlashLevel::Err, "Critical CPU level!".to_string());
        let mut m = model(&i, &i, &[]);
        m.status = Some(&st);
        let mut buf = Buffer::empty(Rect::new(0, 0, 100, 7));
        {
            let a = buf.area;
            draw_header(&mut buf, a, &m, &styles)
        };
        let (x, y) = find(&buf, "Critical CPU level!").unwrap();
        assert_eq!(y, 6);
        assert_eq!(bg(&buf, x, y), styles.logo_error);
        assert_eq!(fg(&buf, x, y), styles.logo_msg);
        let (x, y) = find(&buf, "|____|").unwrap();
        assert_eq!(fg(&buf, x, y), styles.logo_error);

        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 1));
        {
            let a = buf.area;
            draw_indicator(&mut buf, a, &m, None, &styles)
        };
        let r = row(&buf, 0);
        assert!(
            r.contains("K9rs v0.50.18 kind-dev:kind:v1.34.0 12%::40%"),
            "{r}"
        );
        let (x, y) = find(&buf, "K9rs").unwrap();
        assert_eq!(fg(&buf, x, y), styles.logo);
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 1));
        {
            let a = buf.area;
            draw_indicator(&mut buf, a, &m, Some(&st), &styles)
        };
        let (x, y) = find(&buf, "<Critical CPU level!>").unwrap();
        assert_eq!(fg(&buf, x, y), ORANGE_RED);
    }

    #[test]
    fn mnemonic_formatting() {
        assert_eq!(to_mnemonic("Shift-F"), "<shift-f>");
        assert_eq!(to_mnemonic(""), "");
        let w = cluster_info_width(&ClusterInfo {
            cluster: "x".repeat(40),
            ..Default::default()
        });
        assert_eq!(w, 55);
        assert_eq!(cluster_info_width(&ClusterInfo::default()), 50);
    }

    #[test]
    fn latest_rev_badge() {
        let styles = Styles::default();
        let i = ClusterInfo {
            k9s_latest: "v0.51.0".into(),
            ..info()
        };
        let m = model(&i, &i, &[]);
        let mut buf = Buffer::empty(Rect::new(0, 0, 120, 7));
        {
            let a = buf.area;
            draw_header(&mut buf, a, &m, &styles)
        };
        let rows = all_rows(&buf);
        assert!(
            rows[3].starts_with(" K9rs Rev: v0.50.18 \u{26a1}\u{fe0f}v0.51.0 "),
            "{}",
            rows[3]
        );
        let (x, y) = find(&buf, "v0.51.0").unwrap();
        assert_eq!(fg(&buf, x, y), CADET_BLUE);
        assert!(buf[(x, y)].modifier.contains(Modifier::BOLD));
        assert_eq!(fg(&buf, 11, 3), styles.info_section);
    }

    #[test]
    fn menu_descriptions_are_dim() {
        let styles = Styles::default();
        let hints = vec![Hint {
            mnemonic: "d".into(),
            description: "Describe".into(),
            visible: true,
        }];
        let mut buf = Buffer::empty(Rect::new(0, 0, 60, 7));
        {
            let a = buf.area;
            draw_menu(&mut buf, a, &hints, &styles)
        };
        let (x, y) = find(&buf, "Describe").unwrap();
        assert!(buf[(x, y)].modifier.contains(Modifier::DIM));
        let (x, y) = find(&buf, "<d>").unwrap();
        assert!(!buf[(x, y)].modifier.contains(Modifier::DIM));
    }

    #[test]
    fn indicator_deltas() {
        let styles = Styles::default();
        let i = info();
        let mut p = info();
        p.cpu_pct = Some(10);
        let m = model(&i, &p, &[]);
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 1));
        {
            let a = buf.area;
            draw_indicator(&mut buf, a, &m, None, &styles)
        };
        // k9s diffs MEM against the previous CPU in the indicator.
        let r = row(&buf, 0);
        assert!(r.contains("12%↑::40%↑"), "{r}");
    }
}
