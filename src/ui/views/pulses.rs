//! Pulses: k9s view/pulse.go, model/pulse(_health).go and internal/tchart
//! (dial gauges + sparklines).

use std::any::Any;
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use crossterm::event::KeyEvent;
use kube::api::DynamicObject;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::Frame;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::util::{self, Seg};
use super::xray::pod_phase;
use crate::config::Styles;
use crate::k8s::{is_all_namespaces, Accessor, Gvr, SharedAccessor};
use crate::ui::{AppEvent, Ctx, FlashLevel, Hint, View, ViewCmd, ViewId};

/// model.pulseRate.
const PULSE_RATE: Duration = Duration::from_secs(10);
/// dao seriesRecordRate.
const SERIES_RATE: Duration = Duration::from_secs(60);
const CPU: &str = "cpu";
const MEM: &str = "memory";
const AXIS: Color = Color::Rgb(0xff, 0x00, 0x66);
const SPARKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
const DIR_LEFT: isize = 1;
const DIR_RIGHT: isize = -1;
const DIR_DOWN: isize = 4;
const DIR_UP: isize = -4;

/// model.PulseGVRs.
pub(crate) const PULSE_GVRS: [&str; 16] = [
    "v1/nodes",
    "v1/namespaces",
    "v1/services",
    "events.k8s.io/v1/events",
    "v1/pods",
    "apps/v1/deployments",
    "apps/v1/statefulsets",
    "apps/v1/daemonsets",
    "batch/v1/jobs",
    "batch/v1/cronjobs",
    "v1/persistentvolumes",
    "v1/persistentvolumeclaims",
    "autoscaling/v1/horizontalpodautoscalers",
    "networking.k8s.io/v1/ingresses",
    "networking.k8s.io/v1/networkpolicies",
    "v1/serviceaccounts",
];

/// model.HealthPoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HealthPoint {
    pub gvr: String,
    pub total: usize,
    pub faults: usize,
}

/// dao.Point value (client.NodeMetrics subset).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct SeriesPoint {
    time: i64,
    cpu: i64,
    mem: i64,
    alloc_cpu: i64,
    alloc_mem: i64,
}

enum PulseMsg {
    Health(Vec<HealthPoint>),
    Series(SeriesPoint),
}

struct Payload {
    gen: u64,
    msg: PulseMsg,
}

fn b(v: &Value, p: &str) -> bool {
    v.pointer(p).and_then(Value::as_bool).unwrap_or(false)
}

fn st<'a>(v: &'a Value, p: &str) -> &'a str {
    v.pointer(p).and_then(Value::as_str).unwrap_or("")
}

fn arr<'a>(v: &'a Value, p: &str) -> &'a [Value] {
    v.pointer(p)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// render.Pod.Healthy.
pub(crate) fn pod_healthy(po: &Value) -> bool {
    let phase = pod_phase(po);
    if phase == "Completed" {
        return true;
    }
    let cs = arr(po, "/status/containerStatuses");
    let mut cr = cs.iter().filter(|c| b(c, "/ready")).count();
    let mut ct = cs.len();
    let inits = arr(po, "/spec/initContainers");
    for (i, s) in arr(po, "/status/initContainerStatuses").iter().enumerate() {
        if inits
            .get(i)
            .map(|c| st(c, "/restartPolicy") == "Always")
            .unwrap_or(false)
        {
            ct += 1;
            if b(s, "/ready") {
                cr += 1;
            }
        }
    }
    if cr != ct || ct == 0 {
        return false;
    }
    let conds = arr(po, "/status/conditions");
    let gates = arr(po, "/spec/readinessGates");
    let rgr = gates
        .iter()
        .filter(|g| {
            conds
                .iter()
                .any(|c| st(c, "/type") == st(g, "/conditionType") && st(c, "/status") == "True")
        })
        .count();
    if !gates.is_empty() && rgr != gates.len() {
        return false;
    }
    let ready = conds
        .iter()
        .any(|c| st(c, "/type") == "Ready" && st(c, "/status") == "True");
    if !ready {
        return false;
    }
    phase != "Terminating"
}

/// render.Node.Healthy.
fn node_healthy(no: &Value) -> bool {
    if b(no, "/spec/unschedulable") {
        return false;
    }
    arr(no, "/status/conditions")
        .iter()
        .any(|c| st(c, "/type") == "Ready" && st(c, "/status") == "True")
}

/// Per-resource health (k9s Renderer.Healthy; most renderers are always healthy).
fn healthy(gvr: &str, o: &DynamicObject) -> bool {
    let v = serde_json::to_value(o).unwrap_or(Value::Null);
    match gvr {
        "v1/pods" => pod_healthy(&v),
        "v1/nodes" => node_healthy(&v),
        "v1/namespaces" => matches!(st(&v, "/status/phase"), "Active" | "Terminating"),
        // render.Event.Healthy reads server table cell 2, which for
        // events.k8s.io is REASON (not TYPE).
        "events.k8s.io/v1/events" => st(&v, "/reason") == "Normal",
        _ => true,
    }
}

/// model.PulseHealth.checkPulse.
/// The first failing list aborts the sweep: later charts get no point.
pub(crate) async fn check_pulse(acc: &dyn Accessor, ns: &str) -> Vec<HealthPoint> {
    let mut out = Vec::with_capacity(PULSE_GVRS.len());
    for g in PULSE_GVRS {
        let oo = match acc.list(&Gvr::parse(g), ns, None).await {
            Ok(oo) => oo,
            Err(e) => {
                tracing::error!(error = %e, "Pulse check failed");
                break;
            }
        };
        out.push(HealthPoint {
            gvr: g.to_string(),
            total: oo.len(),
            faults: oo.iter().filter(|o| !healthy(g, o)).count(),
        });
    }
    out
}

/// dao.Recorder: cluster (all namespaces) or namespace metrics point.
async fn record(acc: &dyn Accessor, ns: &str) -> Option<SeriesPoint> {
    let mut p = SeriesPoint {
        time: chrono::Utc::now().timestamp(),
        ..Default::default()
    };
    if is_all_namespaces(ns) {
        let nn = acc.node_metrics().await.ok()?;
        if nn.is_empty() {
            return None;
        }
        for m in nn.values() {
            p.cpu += m.usage.cpu_millis;
            p.mem += m.usage.mem_bytes / (1024 * 1024);
            p.alloc_cpu += m.allocatable.cpu_millis;
            p.alloc_mem += m.allocatable.mem_bytes / (1024 * 1024);
        }
        return Some(p);
    }
    let pm = acc.pod_metrics(ns).await.ok()?;
    if pm.is_empty() {
        return None;
    }
    for m in pm.values() {
        let t = m.total();
        p.cpu += t.cpu_millis;
        p.mem += t.mem_bytes / (1024 * 1024);
    }
    p.alloc_cpu = p.cpu;
    p.alloc_mem = p.mem;
    Some(p)
}

/// tchart.Gauge state.
#[derive(Clone, Debug, Default)]
struct Gauge {
    ok: usize,
    fault: usize,
    delta_ok: i8,
    delta_fault: i8,
    legend: String,
    border: Option<Color>,
}

fn compute_delta(prev: usize, next: usize) -> i8 {
    if next == 0 {
        return 0;
    }
    match next.cmp(&prev) {
        std::cmp::Ordering::Greater => 1,
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
    }
}

impl Gauge {
    fn add(&mut self, ok: usize, fault: usize) {
        self.delta_ok = compute_delta(self.ok, ok);
        self.delta_fault = compute_delta(self.fault, fault);
        self.ok = ok;
        self.fault = fault;
    }
}

/// tchart.SparkLine state.
#[derive(Clone, Debug, Default)]
struct Spark {
    series: BTreeMap<i64, f64>,
    max: f64,
    color_index: usize,
    legend: String,
}

/// tchart dot matrix digits.
fn digit(n: u8) -> [[char; 3]; 3] {
    const H: char = '━';
    const V: char = '┃';
    const TL: char = '┏';
    const TR: char = '┓';
    const BL: char = '┗';
    const BR: char = '┛';
    const TEEL: char = '┫';
    const TEER: char = '┣';
    const LH: char = '\u{2578}';
    const RH: char = '\u{257a}';
    const HV: char = '\u{2579}';
    const LV: char = '\u{257b}';
    const B: char = ' ';
    match n {
        1 => [[B, LV, B], [B, V, B], [B, HV, B]],
        2 => [[RH, H, TR], [TL, H, BR], [BL, H, LH]],
        3 => [[H, H, TR], [RH, H, TEEL], [H, H, BR]],
        4 => [[LV, B, LV], [BL, H, TEEL], [B, B, HV]],
        5 => [[TL, H, LH], [BL, H, TR], [RH, H, BR]],
        6 => [[TL, H, LH], [TEER, H, TR], [BL, H, BR]],
        7 => [[H, H, TR], [B, B, V], [B, B, HV]],
        8 => [[TL, H, TR], [TEER, H, TEEL], [BL, H, BR]],
        9 => [[TL, H, TR], [BL, H, TEEL], [RH, H, BR]],
        _ => [[TL, H, TR], [V, B, V], [BL, H, BR]],
    }
}

fn set(buf: &mut Buffer, clip: Rect, x: i32, y: i32, ch: char, style: Style) {
    if x < clip.x as i32
        || y < clip.y as i32
        || x >= clip.right() as i32
        || y >= clip.bottom() as i32
    {
        return;
    }
    if let Some(c) = buf.cell_mut((x as u16, y as u16)) {
        c.set_char(ch);
        c.set_style(style);
    }
}

fn title_case(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f
            .to_uppercase()
            .chain(c.map(|x| x.to_ascii_lowercase()))
            .collect(),
        None => String::new(),
    }
}

fn res_of(gvr: &str) -> &str {
    gvr.rsplit('/').next().unwrap_or(gvr)
}

pub(crate) struct PulseView {
    id: ViewId,
    ns: String,
    title_ns: String,
    chart_gvrs: Vec<String>,
    has_metrics: bool,
    gauges: HashMap<String, Gauge>,
    sparks: HashMap<String, Spark>,
    focus: usize,
    prev_focus: isize,
    /// Chart rects of the last draw, by focus index (mouse focus).
    rects: Vec<(usize, Rect)>,
    /// A click focuses the chart's bare tview Box, which is not a Graphable:
    /// Pulse's actions then find no focused chart and do nothing.
    box_focused: bool,
    /// The pulses grid holds the app focus (a focused chart is highlighted).
    has_focus: bool,
    /// Grid items flagged Focus (Init flags 0, nextFocus adds, enter resets):
    /// regaining focus, tview Grid.Focus hands it to the first flagged one.
    flags: std::collections::BTreeSet<usize>,
    gen: u64,
    token: Option<CancellationToken>,
    styles: Styles,
    thresholds: crate::config::Thresholds,
}

impl PulseView {
    pub(crate) fn new(ctx: &mut Ctx, ns: String) -> Self {
        let all = is_all_namespaces(&ns);
        let start = if all { 0 } else { 4 };
        let mut chart_gvrs: Vec<String> =
            PULSE_GVRS[start..].iter().map(|s| s.to_string()).collect();
        chart_gvrs.push(CPU.into());
        chart_gvrs.push(MEM.into());
        let has_metrics = ctx.has_metrics;
        let mut gauges = HashMap::new();
        for g in &chart_gvrs[..chart_gvrs.len() - 2] {
            gauges.insert(
                g.clone(),
                Gauge {
                    legend: format!(" {} ", title_case(res_of(g))),
                    ..Default::default()
                },
            );
        }
        let mut sparks = HashMap::new();
        if has_metrics {
            for g in [CPU, MEM] {
                sparks.insert(
                    g.to_string(),
                    Spark {
                        legend: format!(" {} ", title_case(g)),
                        ..Default::default()
                    },
                );
            }
        }
        let thresholds = ctx
            .config
            .read()
            .map(|c| c.k9s.thresholds.clone())
            .unwrap_or_default();
        PulseView {
            id: ctx.new_view_id(),
            title_ns: if all && ns.is_empty() {
                "all".into()
            } else {
                ns.clone()
            },
            ns: if all { String::new() } else { ns },
            chart_gvrs,
            has_metrics,
            gauges,
            sparks,
            focus: 0,
            prev_focus: -1,
            rects: Vec::new(),
            box_focused: false,
            has_focus: true,
            flags: [0].into_iter().collect(),
            gen: 0,
            token: None,
            styles: ctx.styles.clone(),
            thresholds,
        }
    }

    fn total(&self) -> usize {
        self.gauges.len() + self.sparks.len()
    }

    fn start_watch(&mut self, acc: SharedAccessor, ctx: &Ctx) {
        self.stop_watch();
        self.gen += 1;
        let token = CancellationToken::new();
        self.token = Some(token.clone());
        let (gen, view, ns, metrics) = (self.gen, self.id, self.ns.clone(), self.has_metrics);
        let tx = ctx.sender();
        let (acc2, tx2, tok2, ns2) = (acc.clone(), tx.clone(), token.clone(), ns.clone());
        ctx.spawn(async move {
            loop {
                let hh = check_pulse(acc.as_ref(), &ns).await;
                if token.is_cancelled() {
                    return;
                }
                let p = Payload {
                    gen,
                    msg: PulseMsg::Health(hh),
                };
                if tx
                    .send(AppEvent::Custom {
                        view,
                        payload: Box::new(p),
                    })
                    .is_err()
                {
                    return;
                }
                tokio::select! {
                    _ = token.cancelled() => return,
                    _ = tokio::time::sleep(PULSE_RATE) => {}
                }
            }
        });
        if !metrics {
            return;
        }
        ctx.spawn(async move {
            loop {
                if let Some(pt) = record(acc2.as_ref(), &ns2).await {
                    if tok2.is_cancelled() {
                        return;
                    }
                    let p = Payload {
                        gen,
                        msg: PulseMsg::Series(pt),
                    };
                    if tx2
                        .send(AppEvent::Custom {
                            view,
                            payload: Box::new(p),
                        })
                        .is_err()
                    {
                        return;
                    }
                }
                tokio::select! {
                    _ = tok2.cancelled() => return,
                    _ = tokio::time::sleep(SERIES_RATE) => {}
                }
            }
        });
    }

    fn stop_watch(&mut self) {
        if let Some(t) = self.token.take() {
            t.cancel();
        }
    }

    /// Pulse.PulseChanged.
    fn pulse_changed(&mut self, pt: &HealthPoint) {
        let Some(g) = self.gauges.get_mut(&pt.gvr) else {
            return;
        };
        g.legend = title_case(res_of(&pt.gvr));
        g.border = Some(if pt.faults > 0 {
            util::color_by_name("darkred").unwrap_or(Color::Red)
        } else {
            util::color_by_name("darkolivegreen").unwrap_or(Color::Green)
        });
        g.add(pt.total, pt.faults);
    }

    fn series_colors(&self, id: &str) -> Vec<Color> {
        let s = &self.styles;
        if let Some(cc) = s.chart_resource_colors.get(id) {
            return cc.clone();
        }
        if id == CPU || id == MEM {
            s.chart_default_chart.clone()
        } else {
            s.chart_default_dial.clone()
        }
    }

    /// SparkLine.GetSeriesColorNames: series colors are true colors and never
    /// equal a tcell.ColorNames entry, so only the fallback names remain.
    fn spark_color_names(&self, _id: &str) -> Vec<String> {
        vec!["green".into(), "orange".into(), "orangered".into()]
    }

    fn level(&self, kind: &str, v: i64) -> usize {
        let sev = if kind == CPU {
            &self.thresholds.cpu
        } else {
            &self.thresholds.memory
        };
        if !(0..=100).contains(&v) {
            return 0;
        }
        if v >= sev.critical as i64 {
            2
        } else if v >= sev.warn as i64 {
            1
        } else {
            0
        }
    }

    fn severity_color(level: usize) -> &'static str {
        match level {
            2 => "red",
            1 => "orangered",
            _ => "green",
        }
    }

    /// Pulse.SeriesChanged.
    fn series_changed(&mut self, pt: SeriesPoint) {
        for (id, cur, alloc, unit) in [
            (CPU, pt.cpu, pt.alloc_cpu, "m"),
            (MEM, pt.mem, pt.alloc_mem, "Mi"),
        ] {
            let mut nn = self.spark_color_names(id);
            let Some(sp) = self.sparks.get_mut(id) else {
                return;
            };
            if alloc as f64 > sp.max {
                sp.max = alloc as f64;
            }
            sp.series.entry(pt.time).or_insert(cur as f64);
            let max = sp.max as i64;
            let perc = if max == 0 {
                0
            } else {
                ((cur as f64 / max as f64) * 100.0).floor() as i64
            };
            let level = self.level(id, perc);
            let sp = self.sparks.get_mut(id).expect("spark");
            sp.color_index = level;
            if cur == 0 {
                nn[0] = "gray".into();
            }
            if alloc == 0 {
                nn[1] = "gray".into();
            }
            sp.legend = format!(
                " {} [{}::b]{}%[white::-]([{}::]{}{unit}[white::]/[white::]{}{unit}[-::])",
                title_case(id),
                Self::severity_color(level),
                perc,
                nn[level],
                util::as_thousands(cur),
                util::as_thousands(max),
            );
        }
    }

    fn title_markup(&self) -> String {
        util::skin_title(
            &format!(
                "[fg:bg:b] Pulses([hilite:bg:b]{}[fg:bg:-])[fg:bg:-] ",
                self.title_ns
            ),
            &self.styles,
        )
    }

    fn next_focus_cmd(&mut self, direction: isize) {
        let current = self.focus as isize;
        let total = self.total() as isize;
        let mut next = current + direction;
        if next < 0 {
            return;
        }
        match direction {
            DIR_LEFT => {
                if next >= total {
                    return;
                }
                self.prev_focus = -1;
            }
            DIR_RIGHT => self.prev_focus = -1,
            DIR_UP if self.has_metrics => {
                if current >= total - 2 {
                    if self.prev_focus >= 0 && self.prev_focus != current {
                        next = self.prev_focus;
                    } else if current == self.chart_gvrs.len() as isize - 1 {
                        next += 1;
                    }
                } else {
                    self.prev_focus = current;
                }
            }
            DIR_DOWN if self.has_metrics => {
                if current >= total - 6 && current < total - 2 {
                    self.prev_focus = current;
                    next = if current % 4 <= 1 {
                        total - 2
                    } else {
                        total - 1
                    };
                } else if current >= total - 2 {
                    return;
                }
            }
            _ => {}
        }
        if next < 0 {
            next = 0;
        } else if next > total - 1 {
            next = current;
        }
        self.focus = if next as usize >= self.chart_gvrs.len() {
            0
        } else {
            next as usize
        };
        self.flags.insert(self.focus);
    }

    /// tview Grid.Focus: focus the first flagged chart.
    fn regain_focus(&mut self) {
        self.box_focused = false;
        if let Some(&i) = self.flags.first() {
            self.focus = i;
        }
    }

    fn draw_gauge(&self, buf: &mut Buffer, area: Rect, id: &str, focused: bool) {
        let s = &self.styles;
        let g = self.gauges.get(id).cloned().unwrap_or_default();
        let bg = Style::default().bg(s.chart_dial_bg);
        let border = bg.fg(g.border.unwrap_or(s.border));
        let rect = util::draw_box(buf, area, &[], border, bg);
        if rect.width == 0 || rect.height == 0 {
            return;
        }
        let colors = self.series_colors(id);
        let c0 = colors.first().copied().unwrap_or(Color::Green);
        let c1 = colors.get(1).copied().unwrap_or(Color::LightRed);
        let dimmed = bg.fg(Color::DarkGray).add_modifier(Modifier::DIM);
        let (minx, miny) = (rect.x as i32, rect.y as i32);
        let midx = minx + rect.width as i32 / 2;
        let midy = miny + rect.height as i32 / 2 - 1;
        let d1 = g.ok.to_string();
        let d2 = g.fault.to_string();
        let total = (d1.len() * 3 + d2.len() * 3 + 1) as i32;
        let mut ox = midx - total / 2;
        let oy = midy - 1;
        let num = |buf: &mut Buffer, mut ox: i32, s: &str, val: usize, style: Style| {
            let style = if val == 0 { dimmed } else { style };
            for ch in s.bytes() {
                let m = digit(ch - b'0');
                for (r, row) in m.iter().enumerate() {
                    for (c, dot) in row.iter().enumerate() {
                        if *dot != ' ' {
                            set(buf, rect, ox + c as i32, oy + r as i32, *dot, style);
                        }
                    }
                }
                ox += 3;
            }
            ox
        };
        let delta = |buf: &mut Buffer, x: i32, y: i32, d: i8, style: Style| match d {
            -1 => set(buf, rect, x - 1, y + 1, '↓', style),
            1 => set(buf, rect, x - 1, y + 1, '↑', style),
            _ => {}
        };
        let ok_style = bg.fg(c0);
        delta(buf, ox, oy, g.delta_ok, ok_style);
        num(buf, ox, &d1, g.ok, ok_style);
        ox += d1.len() as i32 * 3;
        set(buf, rect, ox, midy, '⠔', bg.fg(Color::Reset));
        ox += 1;
        let fault_style = bg.fg(c1);
        let end = num(buf, ox, &d2, g.fault, fault_style);
        let fs = if g.fault == 0 { dimmed } else { fault_style };
        delta(
            buf,
            end + 1,
            oy,
            g.delta_fault,
            fs.remove_modifier(Modifier::DIM),
        );
        let ly = oy + 3;
        if ly >= miny && ly < rect.bottom() as i32 && !g.legend.is_empty() {
            let legend = if focused {
                format!(
                    "[{}:{}:]{}[::]",
                    util::color_tag(s.chart_focus_fg),
                    util::color_tag(s.chart_focus_bg),
                    g.legend
                )
            } else {
                g.legend.clone()
            };
            let line = util::markup_line(&legend, bg.fg(Color::White));
            util::put_centered(buf, rect.x, ly as u16, rect.width, &line);
        }
    }

    fn draw_spark(&self, buf: &mut Buffer, rect: Rect, id: &str, focused: bool) {
        let s = &self.styles;
        let sp = self.sparks.get(id).cloned().unwrap_or_default();
        let bg = Style::default().bg(s.chart_bg);
        util::fill(buf, rect, bg);
        if rect.width < 2 || rect.height < 4 {
            return;
        }
        let (minx, miny) = (rect.x as i32, rect.y as i32);
        let (maxx, maxy) = (rect.right() as i32, rect.bottom() as i32);
        let (dx, dy) = (rect.width as i32, rect.height as i32);
        // x axis labels and line.
        let mut t = chrono::Local::now();
        let mut vals = Vec::new();
        let mut i = dx - 1;
        while i > 0 {
            vals.push(t.format("%H:%M").to_string());
            t -= chrono::Duration::minutes(10);
            i -= 10;
        }
        let mut lx = dx - 1;
        let orange = bg.fg(util::color_by_name("orange").unwrap_or(Color::Yellow));
        for v in vals {
            if lx <= 2 {
                break;
            }
            let x = minx + lx - 5;
            if x >= minx {
                let line = vec![Seg::new(v, orange)];
                util::put_centered(
                    buf,
                    x as u16,
                    (maxy - 2) as u16,
                    6.min((maxx - x) as u16),
                    &line,
                );
            }
            lx -= 10;
        }
        let axis = bg.fg(AXIS);
        for x in 1..dx - 1 {
            set(buf, rect, minx + x, maxy - 3, '─', axis);
        }
        // series.
        let mut series = sp.series.clone();
        let width = (dx - 1) as usize;
        while series.len() > width && width > 0 {
            let first = *series.keys().next().expect("key");
            series.remove(&first);
        }
        let mut cx = if (series.len() as i32) < dx {
            maxx - series.len() as i32 - 1
        } else {
            minx + 1
        };
        let pad = if sp.legend.is_empty() { 2 } else { 3 };
        let colors = self.series_colors(id);
        let color = colors
            .get(sp.color_index % colors.len().max(1))
            .copied()
            .unwrap_or(Color::Green);
        if sp.max > 0.0 {
            let scale = (SPARKS.len() as f64 * (dy - pad) as f64) / sp.max;
            let cy = maxy - pad - 1;
            let style = bg.fg(color);
            for v in series.values() {
                let scaled = (v * scale).round();
                let full = (scaled / SPARKS.len() as f64) as i64;
                let p = (scaled as i64).rem_euclid(SPARKS.len() as i64) as usize;
                let mut y = cy;
                if *v >= 0.0 {
                    for _ in 0..full {
                        set(buf, rect, cx, y, SPARKS[SPARKS.len() - 1], style);
                        y -= 1;
                        if y < miny {
                            break;
                        }
                    }
                    if p > 0 {
                        set(buf, rect, cx, y, SPARKS[p], style);
                    }
                }
                cx += 1;
            }
        }
        // y axis.
        for y in 0..dy - 3 {
            set(buf, rect, minx, miny + y, '│', axis);
        }
        set(buf, rect, minx, miny + dy - 3, '└', axis);
        if !sp.legend.is_empty() {
            let legend = if focused {
                format!(
                    "[{}:{}:]{}[::]",
                    util::color_tag(s.chart_focus_fg),
                    util::color_tag(s.chart_focus_bg),
                    sp.legend
                )
            } else {
                sp.legend.clone()
            };
            let line = util::markup_line(&legend, bg.fg(Color::White));
            util::put_centered(buf, rect.x, (maxy - 1) as u16, rect.width, &line);
        }
    }
}

/// tview Grid proportional distribution: each track takes
/// remaining/tracks-left (floored), so the remainder lands on the last ones.
fn split(start: u16, len: u16, n: u16) -> Vec<(u16, u16)> {
    let (mut rem, mut pos) = (len, start);
    (0..n)
        .map(|i| {
            let w = rem / (n - i);
            rem -= w;
            pos += w;
            (pos - w, w)
        })
        .collect()
}

impl View for PulseView {
    fn owns_frame(&self) -> bool {
        true
    }
    fn id(&self) -> ViewId {
        self.id
    }

    fn name(&self) -> String {
        "Pulses".into()
    }

    fn title(&self) -> String {
        util::plain(&util::markup_line(
            &self.title_markup(),
            util::title_base(&self.styles),
        ))
    }

    fn hints(&self) -> Vec<Hint> {
        let mut hh = vec![
            // KeyActions.Hints order (tcell key codes), kept by the stable sort.
            Hint::new("tab", "Next"),
            Hint::new("enter", "Goto"),
            Hint::new("h", "Prev").hidden(),
            Hint::new("j", "Down").hidden(),
            Hint::new("k", "Up").hidden(),
            Hint::new("l", "Next").hidden(),
            Hint::new("up", "Up").hidden(),
            Hint::new("down", "Down").hidden(),
            Hint::new("right", "Next").hidden(),
            Hint::new("left", "Prev").hidden(),
            Hint::new("backtab", "Prev"),
        ];
        util::sort_hints(&mut hh);
        hh
    }

    fn start(&mut self, ctx: &mut Ctx) -> Vec<ViewCmd> {
        self.styles = ctx.styles.clone();
        // Re-focusing the view hands focus to the flagged chart again.
        self.regain_focus();
        let acc = ctx.accessor.clone();
        self.start_watch(acc, ctx);
        Vec::new()
    }

    fn stop(&mut self, _ctx: &mut Ctx) {
        self.stop_watch();
    }

    fn handle_key(&mut self, key: KeyEvent, _ctx: &mut Ctx) -> Vec<ViewCmd> {
        let k = util::key_label(&key);
        let own = [
            "enter", "tab", "backtab", "up", "down", "left", "right", "h", "j", "k", "l",
        ];
        if self.box_focused && own.contains(&k.as_str()) {
            return Vec::new();
        }
        match k.as_str() {
            "enter" => {
                self.prev_focus = self.focus as isize;
                self.flags = [self.focus].into_iter().collect();
                self.stop_watch();
                let id = &self.chart_gvrs[self.focus];
                let res = if id == CPU || id == MEM {
                    "v1/pods".to_string()
                } else {
                    res_of(id).to_string()
                };
                // pulse.go: gotoResource(res+" "+ns, "", false, true) pushes.
                let cmd = format!("{res} {}", self.ns);
                vec![ViewCmd::Goto {
                    cmd: cmd.trim_end().to_string(),
                    path: String::new(),
                }]
            }
            "tab" | "right" | "l" => {
                self.next_focus_cmd(DIR_LEFT);
                Vec::new()
            }
            "backtab" | "left" | "h" => {
                self.next_focus_cmd(DIR_RIGHT);
                Vec::new()
            }
            "down" | "j" => {
                self.next_focus_cmd(DIR_DOWN);
                Vec::new()
            }
            "up" | "k" => {
                self.next_focus_cmd(DIR_UP);
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn on_event(&mut self, ev: &AppEvent, _ctx: &mut Ctx) -> Vec<ViewCmd> {
        if let AppEvent::Custom { view, payload } = ev {
            if *view != self.id {
                return Vec::new();
            }
            if let Some(p) = payload.downcast_ref::<Payload>() {
                if p.gen != self.gen {
                    return Vec::new();
                }
                match &p.msg {
                    PulseMsg::Health(hh) => {
                        for h in hh {
                            self.pulse_changed(h);
                        }
                    }
                    PulseMsg::Series(pt) => self.series_changed(*pt),
                }
            }
        }
        if let AppEvent::Error { view, msg } = ev {
            if *view == self.id {
                return vec![util::flash(FlashLevel::Err, msg.clone())];
            }
        }
        Vec::new()
    }

    fn draw(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &Ctx) {
        self.styles = ctx.styles.clone();
        let s = self.styles.clone();
        let buf = frame.buffer_mut();
        let bg = Style::default().bg(s.chart_bg).fg(s.fg);
        let title = util::markup_line(&self.title_markup(), util::title_base(&s));
        let inner = util::pad_x(
            // The grid box never sets a focus color: always the plain border.
            util::draw_box(buf, area, &title, bg.fg(s.border), bg),
            1,
        );
        if inner.width == 0 || inner.height == 0 {
            return;
        }
        let ngauges = self.gauges.len();
        let gauge_rows = ngauges.div_ceil(4) * 2;
        let rows_n = gauge_rows + if self.sparks.is_empty() { 0 } else { 2 };
        let rows = split(inner.y, inner.height, rows_n as u16);
        let cols = split(inner.x, inner.width, 8);
        let cell = |r: usize, c: usize, rs: usize, cs: usize| -> Rect {
            let (y, _) = rows[r];
            let (x, _) = cols[c];
            let h: u16 = rows[r..r + rs].iter().map(|v| v.1).sum();
            let w: u16 = cols[c..c + cs].iter().map(|v| v.1).sum();
            Rect::new(x, y, w, h)
        };
        let mut rects = Vec::new();
        let gids: Vec<String> = self.chart_gvrs[..self.chart_gvrs.len() - 2].to_vec();
        for (i, id) in gids.iter().enumerate() {
            let (r, c) = ((i / 4) * 2, (i % 4) * 2);
            let rect = cell(r, c, 2, 2);
            rects.push((i, rect));
            self.draw_gauge(buf, rect, id, self.has_focus && self.focus == i);
        }
        if !self.sparks.is_empty() {
            let base = self.chart_gvrs.len() - 2;
            rects.push((base, cell(gauge_rows, 0, 2, 4)));
            rects.push((base + 1, cell(gauge_rows, 4, 2, 4)));
            self.draw_spark(
                buf,
                cell(gauge_rows, 0, 2, 4),
                CPU,
                self.has_focus && self.focus == base,
            );
            self.draw_spark(
                buf,
                cell(gauge_rows, 4, 2, 4),
                MEM,
                self.has_focus && self.focus == base + 1,
            );
        }
        self.rects = rects;
    }

    fn set_focused(&mut self, focused: bool) {
        if focused && !self.has_focus {
            self.regain_focus();
        }
        self.has_focus = focused;
    }

    /// tview Box.MouseHandler: a click focuses the chart under the mouse;
    /// key navigation then starts from it (nextFocusCmd uses GetFocus).
    fn on_mouse(&mut self, m: crate::ui::Mouse, _area: Rect, _ctx: &mut Ctx) -> Vec<ViewCmd> {
        if let crate::ui::Mouse::Click { x, y } = m {
            let p = ratatui::layout::Position::new(x, y);
            if let Some((i, _)) = self.rects.iter().find(|(_, r)| r.contains(p)) {
                self.focus = *i;
                self.box_focused = true;
                // The click itself focused the box (no Grid.Focus pass).
                self.has_focus = true;
            }
        }
        Vec::new()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::k8s::fake::{obj, FakeAccessor};
    use crate::k8s::{Metrics, NodeMetrics, PodMetrics};
    use crossterm::event::{KeyCode, KeyModifiers};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use serde_json::json;
    use std::sync::{Arc, RwLock};
    use tokio::sync::mpsc;

    fn cluster() -> FakeAccessor {
        let f = FakeAccessor::new().with_metrics();
        let ok = json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"a","namespace":"default"},
            "spec":{"containers":[{"name":"c"}]},
            "status":{"phase":"Running","conditions":[{"type":"Ready","status":"True"}],
                "containerStatuses":[{"name":"c","ready":true,"state":{"running":{}}}]}});
        let bad = json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"b","namespace":"default"},
            "spec":{"containers":[{"name":"c"}]},
            "status":{"phase":"Running","containerStatuses":[{"name":"c","ready":false,"state":{"waiting":{"reason":"CrashLoopBackOff"}}}]}});
        let done = json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"d","namespace":"default"},
            "spec":{"containers":[{"name":"c"}]},
            "status":{"phase":"Succeeded","containerStatuses":[{"name":"c","ready":false,"state":{"terminated":{"reason":"Completed"}}}]}});
        for p in [ok, bad, done] {
            f.add("v1/pods", obj(p));
        }
        f.add("apps/v1/deployments", obj(json!({"apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":"x","namespace":"default"}})));
        f.add("v1/nodes", obj(json!({"apiVersion":"v1","kind":"Node","metadata":{"name":"n1"},"status":{"conditions":[{"type":"Ready","status":"True"}]}})));
        f.add("v1/nodes", obj(json!({"apiVersion":"v1","kind":"Node","metadata":{"name":"n2"},"spec":{"unschedulable":true},"status":{"conditions":[{"type":"Ready","status":"True"}]}})));
        f.add("events.k8s.io/v1/events", obj(json!({"apiVersion":"events.k8s.io/v1","kind":"Event","metadata":{"name":"e","namespace":"default"},"type":"Normal","reason":"Pulled"})));
        f.pod_metrics.lock().unwrap().insert(
            "default/a".into(),
            PodMetrics {
                containers: [(
                    "c".to_string(),
                    Metrics {
                        cpu_millis: 250,
                        mem_bytes: 64 * 1024 * 1024,
                    },
                )]
                .into_iter()
                .collect(),
            },
        );
        f.node_metrics.lock().unwrap().insert(
            "n1".into(),
            NodeMetrics {
                usage: Metrics {
                    cpu_millis: 950,
                    mem_bytes: 1024 * 1024 * 1024,
                },
                allocatable: Metrics {
                    cpu_millis: 1000,
                    mem_bytes: 4 * 1024 * 1024 * 1024,
                },
            },
        );
        f
    }

    fn ctx() -> (Ctx, mpsc::UnboundedReceiver<AppEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut c = Ctx::new(
            Arc::new(cluster()),
            Arc::new(RwLock::new(crate::config::Config::default())),
            Styles::default(),
            "default".into(),
            tx,
        );
        c.has_metrics = true;
        (c, rx)
    }

    #[tokio::test]
    async fn health_counts() {
        let f = cluster();
        let hh = check_pulse(&f, "").await;
        let get = |g: &str| hh.iter().find(|h| h.gvr == g).unwrap().clone();
        assert_eq!(
            get("v1/pods"),
            HealthPoint {
                gvr: "v1/pods".into(),
                total: 3,
                faults: 1
            }
        );
        assert_eq!(get("v1/nodes").faults, 1);
        assert_eq!(get("events.k8s.io/v1/events").faults, 1);
        assert_eq!(get("apps/v1/deployments").faults, 0);
        assert_eq!(get("batch/v1/jobs").total, 0);
        // A forbidden events list stops the sweep (live: events.k8s.io denied).
        f.list_errors
            .lock()
            .unwrap()
            .push("events.k8s.io/v1/events".into());
        let hh = check_pulse(&f, "").await;
        let gg: Vec<&str> = hh.iter().map(|h| h.gvr.as_str()).collect();
        assert_eq!(gg, ["v1/nodes", "v1/namespaces", "v1/services"]);
    }

    #[tokio::test]
    async fn view_layout_metrics_and_focus() {
        let (mut c, mut rx) = ctx();
        let mut v = PulseView::new(&mut c, "default".into());
        assert_eq!(v.title(), " Pulses(default) ");
        assert_eq!(v.total(), 14);
        v.start(&mut c);
        let mut got = 0;
        while got < 2 {
            let ev = rx.recv().await.unwrap();
            v.on_event(&ev, &mut c);
            got += 1;
        }
        let pods = &v.gauges["v1/pods"];
        assert_eq!((pods.ok, pods.fault), (3, 1));
        assert_eq!(pods.legend, "Pods");
        assert_eq!(pods.border, util::color_by_name("darkred"));
        let cpu = &v.sparks[CPU];
        assert_eq!(cpu.max, 250.0);
        // namespaced metrics: allocatable == current -> 100% -> critical.
        assert!(
            cpu.legend.starts_with(" Cpu [red::b]100%"),
            "{}",
            cpu.legend
        );
        assert!(v.sparks[MEM].legend.contains("64Mi"));
        // Value color: fallback names [green orange orangered][level].
        assert!(cpu.legend.contains("([orangered::]250m"), "{}", cpu.legend);

        let mut t = Terminal::new(TestBackend::new(120, 40)).unwrap();
        t.draw(|f| v.draw(f, f.area(), &c)).unwrap();
        let b = t.backend().buffer().clone();
        let text: String = (0..40)
            .map(|y| (0..120).map(|x| b[(x, y)].symbol()).collect::<String>() + "\n")
            .collect();
        assert!(text.contains(" Pulses(default) "));
        assert!(text.contains("Pods"));
        assert!(text.contains("Deployments"));
        assert!(text.contains("⠔"));
        assert!(text.contains("Cpu 100%"));
        assert!(text.contains('┃') || text.contains('╻'));
        // Outer grid border: plain border color, never the focus color.
        assert_eq!(b[(0, 0)].fg, c.styles.border);

        // focus navigation.
        v.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &mut c);
        assert_eq!(v.focus, 1);
        v.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut c);
        assert_eq!(v.focus, 5);
        v.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut c);
        assert_eq!(v.focus, 9);
        v.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut c);
        assert_eq!(v.focus, 12);
        v.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), &mut c);
        assert_eq!(v.focus, 9);
        let cmds = v.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &mut c);
        assert!(matches!(&cmds[0], ViewCmd::Goto { cmd, .. } if cmd == "ingresses default"));
        v.focus = 12;
        let cmds = v.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &mut c);
        assert!(matches!(&cmds[0], ViewCmd::Goto { cmd, .. } if cmd == "v1/pods default"));
    }

    #[tokio::test]
    async fn all_namespaces_uses_node_metrics() {
        let (mut c, _rx) = ctx();
        let mut v = PulseView::new(&mut c, "all".into());
        assert_eq!(v.title(), " Pulses(all) ");
        assert_eq!(v.total(), 18);
        let f = cluster();
        let pt = record(&f, "").await.unwrap();
        assert_eq!((pt.cpu, pt.alloc_cpu), (950, 1000));
        v.series_changed(pt);
        assert!(v.sparks[CPU].legend.starts_with(" Cpu [red::b]95%"));
        assert!(v.sparks[MEM].legend.contains("[green::b]25%"));
    }

    #[tokio::test]
    async fn mouse_click_focuses_chart_box() {
        let (mut c, _rx) = ctx();
        let mut v = PulseView::new(&mut c, "default".into());
        let mut t = Terminal::new(TestBackend::new(120, 40)).unwrap();
        t.draw(|f| v.draw(f, f.area(), &c)).unwrap();
        let (i, r) = v.rects[5];
        let area = Rect::new(0, 0, 120, 40);
        v.set_focused(false); // e.g. a header box was clicked first
        v.on_mouse(
            crate::ui::Mouse::Click {
                x: r.x + 1,
                y: r.y + 1,
            },
            area,
            &mut c,
        );
        v.set_focused(true);
        assert_eq!(v.focus, i);
        // pulse.go nextFocusCmd: the focused bare Box is not a Graphable.
        v.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &mut c);
        assert_eq!(v.focus, i);
        // Regaining focus (prompt closed, view re-entered): tview Grid.Focus
        // picks the first Focus-flagged item, not the clicked box.
        v.set_focused(false);
        v.set_focused(true);
        assert_eq!(v.focus, 0);
        v.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &mut c);
        assert_eq!(v.focus, 1);
        v.on_mouse(
            crate::ui::Mouse::Click {
                x: r.x + 1,
                y: r.y + 1,
            },
            area,
            &mut c,
        );
        v.start(&mut c);
        assert_eq!(v.focus, 0, "flags 0 and 1: first wins");
    }

    #[test]
    fn hints_keep_key_code_order_within_a_description() {
        let (mut c, _rx) = ctx();
        let v = PulseView::new(&mut c, "default".into());
        let keys: Vec<String> = v.hints().into_iter().map(|h| h.mnemonic).collect();
        assert_eq!(
            keys,
            ["j", "down", "enter", "tab", "l", "right", "h", "left", "backtab", "k", "up"]
        );
    }

    #[test]
    fn gauge_digits_and_delta() {
        let mut g = Gauge::default();
        g.add(3, 0);
        assert_eq!(g.delta_ok, 1);
        g.add(2, 1);
        assert_eq!((g.delta_ok, g.delta_fault), (-1, 1));
        assert_eq!(digit(1)[1][1], '┃');
        assert_eq!(split(0, 10, 3), vec![(0, 3), (3, 3), (6, 4)]);
        // 156 cols / 8: 19,19,19,19,20,20,20,20 (live k9s gauges 38,38,40,40).
        let w: Vec<u16> = split(0, 156, 8).iter().map(|c| c.1).collect();
        assert_eq!(w, [19, 19, 19, 19, 20, 20, 20, 20]);
    }
}
