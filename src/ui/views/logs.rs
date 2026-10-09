//! Log viewer: k9s view/log.go, view/log_indicator.go, view/logger.go,
//! model/log.go and dao/log_item(s).go.

use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossterm::event::KeyEvent;
use futures::StreamExt;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::Frame;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::util::{self, SLine, Seg, TextPane};
use crate::config::Styles;
use crate::k8s::{fqn, Gvr, LogLine, LogOptions, SharedAccessor};
use crate::ui::{AppEvent, Ctx, FlashLevel, Hint, View, ViewCmd, ViewId};

/// k9s defaultFlushTimeout.
const FLUSH_TIMEOUT: Duration = Duration::from_millis(50);
const LOG_MESSAGE: &str = "Waiting for logs...";
/// dao.podPalette (`fushia` is a typo in k9s and resolves to the default color).
const POD_PALETTE: [&str; 8] = [
    "teal", "green", "purple", "lime", "blue", "yellow", "fushia", "aqua",
];
/// color.Highlight uses xterm color 209.
const HIGHLIGHT: Color = Color::Indexed(209);
const SPACER: &str = "     ";

/// First event of a (re)started stream, used to discard stale events.
struct StreamStart {
    gen: u64,
    single_container: bool,
}

/// dao.LogItem.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LogItem {
    pod: String,
    container: String,
    single: bool,
    ts: Option<String>,
    text: String,
    is_error: bool,
}

/// dao.LogItems + model.Log state.
#[derive(Default)]
struct LogModel {
    items: Vec<LogItem>,
    colors: HashMap<String, Color>,
    last_sent: usize,
    filter: String,
    cap: usize,
}

impl LogModel {
    fn clear(&mut self) {
        self.items.clear();
        self.colors.clear();
        self.last_sent = 0;
    }

    /// model.Log.Append.
    fn append(&mut self, item: LogItem) {
        if item.text.is_empty() && !item.is_error {
            return;
        }
        if self.items.len() < self.cap {
            self.items.push(item);
            return;
        }
        if !self.items.is_empty() {
            self.items.remove(0);
        }
        self.items.push(item);
        self.last_sent = self.last_sent.saturating_sub(1);
    }

    /// LogItems.podColorFor.
    fn color_for(&mut self, id: &str, base: Color) -> Color {
        if let Some(c) = self.colors.get(id) {
            return *c;
        }
        let idx: usize = id
            .char_indices()
            .map(|(i, r)| i.wrapping_mul(r as usize))
            .fold(0usize, |a, b| a.wrapping_add(b));
        let name = POD_PALETTE[idx % POD_PALETTE.len()];
        let c = util::color_by_name(name).unwrap_or(base);
        self.colors.insert(id.to_string(), c);
        c
    }

    fn render(&mut self, index: usize, show_time: bool, s: &Styles) -> Vec<SLine> {
        let base = log_base(s);
        let items: Vec<LogItem> = self.items.get(index..).unwrap_or_default().to_vec();
        items
            .iter()
            .map(|it| {
                let id = if it.pod.is_empty() {
                    it.container.clone()
                } else {
                    it.pod.clone()
                };
                let paint = self.color_for(&id, base.fg.unwrap_or(Color::Reset));
                render_item(it, paint, show_time, base)
            })
            .collect()
    }

    /// model.Log.applyFilter.
    fn filtered(
        &mut self,
        index: usize,
        show_time: bool,
        s: &Styles,
    ) -> anyhow::Result<Vec<SLine>> {
        let q = self.filter.clone();
        let lines = self.render(index, show_time, s);
        let texts: Vec<String> = lines.iter().map(|l| util::plain(l)).collect();
        let mut hits: Vec<(usize, Vec<usize>)> = Vec::new();
        if let Some(f) = util::fuzzy_selector(&q) {
            let f = f.trim().to_string();
            let mut mm: Vec<(i64, usize, Vec<usize>)> = texts
                .iter()
                .enumerate()
                .filter_map(|(i, t)| util::fuzzy_indices(&f, t).map(|(sc, ix)| (sc, i, ix)))
                .collect();
            mm.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            hits = mm.into_iter().map(|(_, i, ix)| (i, ix)).collect();
        } else {
            let (invert, q) = match q.strip_prefix('!') {
                Some(r) => (true, r.to_string()),
                None => (false, q.clone()),
            };
            let rx = regex::Regex::new(&format!("(?i){q}"))?;
            for (i, t) in texts.iter().enumerate() {
                let locs: Vec<(usize, usize)> =
                    rx.find_iter(t).map(|m| (m.start(), m.end())).collect();
                let found = !locs.is_empty();
                if found == invert {
                    continue;
                }
                let ii: Vec<usize> = locs
                    .iter()
                    .flat_map(|&(a, b)| t[a..b].char_indices().map(move |(o, _)| a + o))
                    .collect();
                hits.push((i, ii));
            }
        }
        let base = log_base(s);
        Ok(hits
            .into_iter()
            .map(|(i, ii)| highlight(&lines[i], &ii, base))
            .collect())
    }
}

fn log_base(s: &Styles) -> Style {
    Style::default().fg(s.logs_fg).bg(s.logs_bg)
}

/// dao.LogItem.Render.
fn render_item(it: &LogItem, paint: Color, show_time: bool, base: Style) -> SLine {
    let mut out: SLine = Vec::new();
    if show_time {
        if let Some(ts) = it.ts.as_deref().filter(|t| !t.is_empty()) {
            let pad = 30usize.saturating_sub(ts.len());
            out.push(Seg::new(
                format!("{ts} {}", " ".repeat(pad)),
                base.fg(Color::DarkGray).add_modifier(Modifier::BOLD),
            ));
        }
    }
    if !it.pod.is_empty() {
        out.push(Seg::new(it.pod.clone(), base.fg(paint)));
    }
    if !it.single && !it.container.is_empty() {
        if !it.pod.is_empty() {
            out.push(Seg::new(" ", base.fg(paint)));
        }
        out.push(Seg::new(
            it.container.clone(),
            base.fg(paint).add_modifier(Modifier::BOLD),
        ));
        out.push(Seg::new(" ", base));
    } else if !it.pod.is_empty() {
        out.push(Seg::new(" ", base));
    }
    if it.is_error {
        out.push(Seg::new(
            it.text.clone(),
            base.fg(util::color_by_name("orange").unwrap_or(Color::Yellow))
                .add_modifier(Modifier::BOLD),
        ));
    } else {
        out.extend(parse_ansi(&it.text, base));
    }
    out.retain(|s| !s.text.is_empty());
    out
}

/// color.Highlight: paint matched chars with xterm 209, then reset.
fn highlight(line: &[Seg], idx: &[usize], base: Style) -> SLine {
    if idx.is_empty() {
        return line.to_vec();
    }
    let set: std::collections::HashSet<usize> = idx.iter().copied().collect();
    let mut out: SLine = Vec::new();
    let mut off = 0;
    for seg in line {
        let mut style = seg.style;
        let mut cur = String::new();
        let mut cur_style = style;
        for (o, ch) in seg.text.char_indices() {
            let lit = set.contains(&(off + o));
            let st = if lit { base.fg(HIGHLIGHT) } else { style };
            if st != cur_style && !cur.is_empty() {
                out.push(Seg::new(std::mem::take(&mut cur), cur_style));
            }
            cur_style = st;
            cur.push(ch);
            if lit {
                style = base;
            }
        }
        if !cur.is_empty() {
            out.push(Seg::new(cur, cur_style));
        }
        off += seg.text.len();
    }
    out
}

fn ansi_basic(n: u16, bright: bool) -> Color {
    let names = if bright {
        [
            "gray", "red", "lime", "yellow", "blue", "fuchsia", "aqua", "white",
        ]
    } else {
        [
            "black", "maroon", "green", "olive", "navy", "purple", "teal", "silver",
        ]
    };
    util::color_by_name(names[(n % 8) as usize]).unwrap_or(Color::Reset)
}

/// Translate ANSI SGR escapes into styles (tview.ANSIWriter).
pub(crate) fn parse_ansi(text: &str, base: Style) -> SLine {
    let mut out: SLine = Vec::new();
    let mut style = base;
    let mut cur = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            if c != '\r' && c != '\n' {
                cur.push(c);
            }
            continue;
        }
        if chars.peek() != Some(&'[') {
            chars.next();
            continue;
        }
        chars.next();
        let mut params = String::new();
        let mut fin = ' ';
        for ch in chars.by_ref() {
            if ('@'..='~').contains(&ch) {
                fin = ch;
                break;
            }
            params.push(ch);
        }
        if fin != 'm' {
            continue;
        }
        if !cur.is_empty() {
            out.push(Seg::new(std::mem::take(&mut cur), style));
        }
        let nums: Vec<u16> = if params.is_empty() {
            vec![0]
        } else {
            params
                .split(';')
                .map(|p| p.parse::<u16>().unwrap_or(0))
                .collect()
        };
        let mut i = 0;
        while i < nums.len() {
            let n = nums[i];
            match n {
                0 => style = base,
                1 => style = style.add_modifier(Modifier::BOLD),
                2 => style = style.add_modifier(Modifier::DIM),
                3 => style = style.add_modifier(Modifier::ITALIC),
                4 => style = style.add_modifier(Modifier::UNDERLINED),
                5 | 6 => style = style.add_modifier(Modifier::SLOW_BLINK),
                7 => style = style.add_modifier(Modifier::REVERSED),
                9 => style = style.add_modifier(Modifier::CROSSED_OUT),
                22 => style = style.remove_modifier(Modifier::BOLD | Modifier::DIM),
                23 => style = style.remove_modifier(Modifier::ITALIC),
                24 => style = style.remove_modifier(Modifier::UNDERLINED),
                25 => style = style.remove_modifier(Modifier::SLOW_BLINK),
                27 => style = style.remove_modifier(Modifier::REVERSED),
                29 => style = style.remove_modifier(Modifier::CROSSED_OUT),
                30..=37 => style.fg = Some(ansi_basic(n - 30, false)),
                39 => style.fg = base.fg,
                40..=47 => style.bg = Some(ansi_basic(n - 40, false)),
                49 => style.bg = base.bg,
                90..=97 => style.fg = Some(ansi_basic(n - 90, true)),
                100..=107 => style.bg = Some(ansi_basic(n - 100, true)),
                38 | 48 => {
                    let color = match nums.get(i + 1) {
                        Some(5) => {
                            let c = nums.get(i + 2).map(|v| Color::Indexed(*v as u8));
                            i += 2;
                            c
                        }
                        Some(2) => {
                            let c = match (nums.get(i + 2), nums.get(i + 3), nums.get(i + 4)) {
                                (Some(r), Some(g), Some(b)) => {
                                    Some(Color::Rgb(*r as u8, *g as u8, *b as u8))
                                }
                                _ => None,
                            };
                            i += 4;
                            c
                        }
                        _ => None,
                    };
                    if let Some(c) = color {
                        if n == 38 {
                            style.fg = Some(c);
                        } else {
                            style.bg = Some(c);
                        }
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }
    if !cur.is_empty() {
        out.push(Seg::new(cur, style));
    }
    out
}

/// Shared stream generation; sends happen under the lock so a restarted
/// view never receives events of the previous stream after its marker.
type Gen = Arc<Mutex<u64>>;

fn send_if_current(
    gen: &Gen,
    mine: u64,
    tx: &mpsc::UnboundedSender<AppEvent>,
    ev: AppEvent,
) -> bool {
    let g = gen.lock().expect("gen lock");
    if *g != mine {
        return false;
    }
    tx.send(ev).is_ok()
}

struct StreamReq {
    acc: SharedAccessor,
    opts: LogOptions,
    multi: bool,
    cap: usize,
    gen: Gen,
    mine: u64,
    tx: mpsc::UnboundedSender<AppEvent>,
    view: ViewId,
    token: CancellationToken,
}

async fn run_stream(r: StreamReq) {
    let single = if r.multi {
        false
    } else {
        match r
            .acc
            .get(
                &Gvr::parse("v1/pods"),
                &fqn(&r.opts.namespace, &r.opts.path),
            )
            .await
        {
            Ok(po) => container_count(&po) == 1,
            Err(_) => false,
        }
    };
    let start = AppEvent::Custom {
        view: r.view,
        payload: Box::new(StreamStart {
            gen: r.mine,
            single_container: single,
        }),
    };
    if !send_if_current(&r.gen, r.mine, &r.tx, start) {
        return;
    }
    let send = |ev: AppEvent| send_if_current(&r.gen, r.mine, &r.tx, ev);
    let mut stream = match r.acc.tail_logs(r.opts.clone()).await {
        Ok(s) => s,
        Err(e) => {
            send(AppEvent::LogEnd {
                view: r.view,
                error: Some(e.to_string()),
            });
            return;
        }
    };
    let mut batch: Vec<LogLine> = Vec::new();
    let flush = |batch: &mut Vec<LogLine>| -> bool {
        if batch.is_empty() {
            return true;
        }
        send(AppEvent::LogLines {
            view: r.view,
            lines: std::mem::take(batch),
        })
    };
    loop {
        let next = tokio::select! {
            _ = r.token.cancelled() => return,
            n = tokio::time::timeout(FLUSH_TIMEOUT, stream.next()) => n,
        };
        match next {
            Ok(Some(Ok(line))) => {
                batch.push(line);
                if batch.len() > r.cap && !flush(&mut batch) {
                    return;
                }
            }
            Ok(Some(Err(e))) => {
                flush(&mut batch);
                send(AppEvent::LogEnd {
                    view: r.view,
                    error: Some(e.to_string()),
                });
                return;
            }
            Ok(None) => {
                flush(&mut batch);
                send(AppEvent::LogEnd {
                    view: r.view,
                    error: None,
                });
                return;
            }
            Err(_) => {
                if !flush(&mut batch) {
                    return;
                }
            }
        }
    }
}

fn container_count(po: &kube::api::DynamicObject) -> usize {
    ["initContainers", "containers", "ephemeralContainers"]
        .iter()
        .map(|k| {
            po.data
                .pointer(&format!("/spec/{k}"))
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0)
        })
        .sum()
}

pub(crate) struct LogsView {
    id: ViewId,
    namespace: String,
    pod: String,
    container: Option<String>,
    default_container: Option<String>,
    has_default: bool,
    all_containers: bool,
    single: bool,
    multi: bool,
    previous: bool,
    since: i64,
    head: bool,
    show_time: bool,
    autoscroll: bool,
    follow: bool,
    column_lock: bool,
    fullscreen: bool,
    request_one_refresh: bool,
    waiting: bool,
    awaiting_start: bool,
    filter: String,
    model: LogModel,
    pane: TextPane,
    gen: Gen,
    token: Option<CancellationToken>,
    started: bool,
    width: u16,
    styles: Styles,
}

impl LogsView {
    pub(crate) fn new(ctx: &mut Ctx, opts: LogOptions) -> Self {
        let cfg = ctx.config.read().map(|c| c.k9s.clone()).unwrap_or_default();
        let logger = cfg.logger.clone();
        let multi = opts.path.contains('=');
        let container = opts.container.clone().filter(|c| !c.is_empty());
        // model.Log.HasDefaultContainer: only the pod annotation counts.
        let has_default = opts
            .default_container
            .as_deref()
            .is_some_and(|c| !c.is_empty());
        let mut pane = TextPane::new(logger.text_wrap);
        // Every log line is written with its newline (view/log.go Flush).
        pane.trailing_newline = true;
        pane.max_lines = if logger.buffer > 0 {
            logger.buffer as usize
        } else {
            0
        };
        let cap = if logger.tail > 0 {
            logger.tail as usize
        } else if logger.buffer > 0 {
            logger.buffer as usize
        } else {
            usize::MAX
        };
        let mut v = LogsView {
            id: ctx.new_view_id(),
            namespace: opts.namespace.clone(),
            pod: opts.path.clone(),
            default_container: opts.default_container.clone().or_else(|| container.clone()),
            container,
            has_default,
            all_containers: opts.all_containers || !has_default,
            single: false,
            multi,
            previous: opts.previous,
            since: logger.since_seconds,
            head: false,
            show_time: logger.show_time,
            autoscroll: !logger.disable_autoscroll,
            follow: !logger.disable_autoscroll,
            column_lock: false,
            fullscreen: cfg.ui.defaults_to_full_screen,
            request_one_refresh: false,
            waiting: true,
            awaiting_start: true,
            filter: String::new(),
            model: LogModel {
                cap,
                ..Default::default()
            },
            pane,
            gen: Arc::new(Mutex::new(0)),
            token: None,
            started: false,
            width: 0,
            styles: ctx.styles.clone(),
        };
        v.show_waiting();
        v
    }

    fn show_waiting(&mut self) {
        self.waiting = true;
        let st = log_base(&self.styles)
            .fg(util::color_by_name("orange").unwrap_or(Color::Yellow))
            .add_modifier(Modifier::DIM);
        self.pane.set_lines(vec![vec![Seg::new(LOG_MESSAGE, st)]]);
    }

    fn path(&self) -> String {
        fqn(&self.namespace, &self.pod)
    }

    /// dao.LogOptions.Info.
    fn info(&self) -> String {
        match &self.container {
            Some(c) => format!("{} ({c})", self.path()),
            None => self.path(),
        }
    }

    fn log_options(&self) -> LogOptions {
        let tail = self.model.cap.min(i64::MAX as usize) as i64;
        LogOptions {
            namespace: self.namespace.clone(),
            path: self.pod.clone(),
            container: self.container.clone(),
            default_container: self
                .has_default
                .then(|| self.default_container.clone())
                .flatten(),
            all_containers: self.all_containers,
            lines: if self.head { 0 } else { tail },
            // `None` is head mode; tail and since windows stream with the
            // configured value (LogOptions.ToPodLogOptions).
            since_seconds: (!self.head).then_some(self.since),
            previous: self.previous,
            show_timestamp: true,
            follow: !self.head,
        }
    }

    fn cancel(&mut self) {
        if let Some(t) = self.token.take() {
            t.cancel();
        }
    }

    /// model.Log.Start.
    fn start_stream(&mut self, ctx: &Ctx) {
        self.cancel();
        let mine = {
            let mut g = self.gen.lock().expect("gen lock");
            *g += 1;
            *g
        };
        self.awaiting_start = true;
        let token = CancellationToken::new();
        self.token = Some(token.clone());
        let req = StreamReq {
            acc: ctx.accessor.clone(),
            opts: self.log_options(),
            multi: self.multi,
            cap: self.model.cap,
            gen: self.gen.clone(),
            mine,
            tx: ctx.sender(),
            view: self.id,
            token,
        };
        ctx.spawn(run_stream(req));
    }

    /// model.Log.Restart.
    fn restart(&mut self, ctx: &Ctx) {
        self.cancel();
        self.model.clear();
        self.log_cleared();
        self.start_stream(ctx);
    }

    fn log_cleared(&mut self) {
        self.waiting = false;
        self.pane.clear();
    }

    /// Log.Flush.
    fn flush(&mut self, lines: Vec<SLine>) {
        if lines.is_empty() || (!self.request_one_refresh && !self.autoscroll) {
            return;
        }
        self.request_one_refresh = false;
        for l in lines {
            self.pane.append(l);
        }
        if self.follow {
            if self.column_lock {
                self.pane.track_end = true;
            } else {
                self.pane.scroll_to_end();
            }
        }
    }

    /// Log.LogChanged.
    fn log_changed(&mut self, lines: Vec<SLine>) {
        if self.waiting {
            self.waiting = false;
            self.pane.clear();
        }
        self.flush(lines);
    }

    /// Log.LogFailed.
    fn log_failed(&mut self, err: &str) -> Vec<ViewCmd> {
        if self.waiting {
            self.waiting = false;
            self.pane.clear();
        }
        let st = log_base(&self.styles).fg(Color::Red);
        self.pane.append(vec![Seg::new(err.to_string(), st)]);
        vec![util::flash(FlashLevel::Err, err.to_string())]
    }

    /// model.Log.fireLogBuffChanged.
    fn buff_changed(&mut self, index: usize) -> Vec<ViewCmd> {
        let lines = if self.model.filter.is_empty() {
            self.model.render(index, self.show_time, &self.styles)
        } else {
            match self.model.filtered(index, self.show_time, &self.styles) {
                Ok(l) => l,
                Err(e) => return self.log_failed(&e.to_string()),
            }
        };
        if !lines.is_empty() {
            self.log_changed(lines);
        }
        Vec::new()
    }

    /// model.Log.Notify.
    fn notify(&mut self) -> Vec<ViewCmd> {
        if self.model.last_sent < self.model.items.len() {
            let from = self.model.last_sent;
            let cmds = self.buff_changed(from);
            self.model.last_sent = self.model.items.len();
            return cmds;
        }
        Vec::new()
    }

    /// model.Log.Refresh.
    fn refresh(&mut self) {
        self.log_cleared();
        let lines = self.model.render(0, self.show_time, &self.styles);
        self.log_changed(lines);
    }

    fn set_filter(&mut self, q: &str) -> Vec<ViewCmd> {
        self.filter = q.to_string();
        self.model.filter = q.to_string();
        self.log_cleared();
        self.buff_changed(0)
    }

    fn to_item(&self, l: &LogLine) -> LogItem {
        LogItem {
            pod: if self.multi {
                l.pod.clone()
            } else {
                String::new()
            },
            container: l.container.clone(),
            single: self.single,
            ts: l.timestamp.clone(),
            text: l.text.trim_end_matches(['\n', '\r']).to_string(),
            is_error: false,
        }
    }

    fn since_label(&self) -> String {
        if self.head {
            return "head".into();
        }
        if self.since > 0 && self.since < 3600 {
            return format!("{}m", self.since / 60);
        }
        if self.since >= 3600 {
            return format!("{}h", self.since / 3600);
        }
        "tail".into()
    }

    fn title_markup(&self) -> String {
        let s = &self.styles;
        let mut t = if self.previous {
            " Previous Logs".to_string()
        } else {
            " Logs".to_string()
        };
        let since = self.since_label();
        let path = self.path();
        let body = match &self.container {
            None => format!("([hilite:bg:]{path}[-:bg:-])[[green:bg:b]{since}[-:bg:-]] "),
            Some(co) => format!(
                "([hilite:bg:]{path}:[hilite:bg:b]{co}[-:bg:-])[[green:bg:b]{since}[-:bg:-]] "
            ),
        };
        // SkinTitle only replaces the first [hilite; tview reads the
        // leftover `hilite` as an unknown color, i.e. the terminal default.
        let mut body = util::skin_title(&body, s);
        body = body.replace("[hilite", "[default");
        t += &body;
        if !self.filter.is_empty() {
            t += &util::search_title(&self.filter, s);
        }
        t
    }

    /// LogIndicator.Refresh.
    fn indicator(&self) -> SLine {
        let s = &self.styles;
        let on = util::color_tag(s.log_toggle_on);
        let off = util::color_tag(s.log_toggle_off);
        let toggle = |name: &str, state: bool, spacer: &str| {
            if state {
                format!("[::b]{name}:[{on}::b]On[-::] {spacer}")
            } else {
                format!("[::b]{name}:[{off}::d]Off[-::]{spacer}")
            }
        };
        let mut m = String::new();
        if self.has_default {
            m += &toggle("AllContainers", self.all_containers, SPACER);
        }
        m += &toggle("Autoscroll", self.autoscroll, SPACER);
        m += &toggle("ColumnLock", self.column_lock, SPACER);
        m += &toggle("FullScreen", self.fullscreen, SPACER);
        m += &toggle("Timestamps", self.show_time, SPACER);
        m += &toggle("Wrap", self.pane.wrap, "");
        util::markup_line(
            &m,
            Style::default()
                .fg(s.log_indicator_fg)
                .bg(s.log_indicator_bg),
        )
    }

    fn since_cmd(&mut self, n: i64, ctx: &Ctx) {
        self.log_cleared();
        if n == 0 {
            self.head = true;
        } else {
            self.since = n;
            self.head = false;
        }
        self.restart(ctx);
        self.request_one_refresh = true;
    }

    fn save(&self, ctx: &Ctx) -> ViewCmd {
        let dir = util::screen_dump_dir(ctx);
        match util::save_log(&dir, &self.path(), &self.pane.text()) {
            Ok(p) => util::flash(
                FlashLevel::Info,
                format!("Log {} saved successfully!", p.display()),
            ),
            Err(e) => util::flash(FlashLevel::Err, e.to_string()),
        }
    }
}

impl View for LogsView {
    fn owns_frame(&self) -> bool {
        true
    }
    fn id(&self) -> ViewId {
        self.id
    }

    fn name(&self) -> String {
        "logs".into()
    }

    fn title(&self) -> String {
        util::plain(&util::markup_line(
            &self.title_markup(),
            util::title_base(&self.styles),
        ))
    }

    fn hints(&self) -> Vec<Hint> {
        let mut hh = vec![
            Hint::new("0", "tail"),
            Hint::new("1", "head"),
            Hint::new("2", "1m"),
            Hint::new("3", "5m"),
            Hint::new("4", "15m"),
            Hint::new("5", "30m"),
            Hint::new("6", "1h"),
            Hint::new("esc", "Back").hidden(),
            Hint::new("q", "Back").hidden(),
            Hint::new("shift-c", "Clear"),
            Hint::new("m", "Mark"),
            Hint::new("s", "Toggle AutoScroll"),
            Hint::new("shift-l", "Toggle ColumnLock"),
            Hint::new("f", "Toggle FullScreen"),
            Hint::new("t", "Toggle Timestamp"),
            Hint::new("w", "Toggle Wrap"),
            Hint::new("ctrl-s", "Save"),
            Hint::new("c", "Copy"),
        ];
        if self.has_default {
            hh.push(Hint::new("a", "Toggle AllContainers"));
        }
        util::sort_hints(&mut hh);
        hh
    }

    fn start(&mut self, ctx: &mut Ctx) -> Vec<ViewCmd> {
        self.styles = ctx.styles.clone();
        if self.started {
            self.restart(ctx);
        } else {
            self.started = true;
            self.start_stream(ctx);
        }
        Vec::new()
    }

    fn stop(&mut self, _ctx: &mut Ctx) {
        self.cancel();
        *self.gen.lock().expect("gen lock") += 1;
    }

    fn handle_key(&mut self, key: KeyEvent, ctx: &mut Ctx) -> Vec<ViewCmd> {
        let k = util::key_label(&key);
        match k.as_str() {
            "0" => self.since_cmd(-1, ctx),
            "1" => self.since_cmd(0, ctx),
            "2" => self.since_cmd(60, ctx),
            "3" => self.since_cmd(5 * 60, ctx),
            "4" => self.since_cmd(15 * 60, ctx),
            "5" => self.since_cmd(30 * 60, ctx),
            "6" => self.since_cmd(60 * 60, ctx),
            "enter" => self.pane.append(Vec::new()),
            "esc" | "q" => {
                if self.filter.is_empty() {
                    return vec![ViewCmd::Pop];
                }
                return self.set_filter("");
            }
            "/" => return vec![ViewCmd::OpenFilterPrompt],
            "shift-c" => {
                self.model.clear();
                self.log_cleared();
            }
            "m" => {
                let w = (self.width as usize).saturating_sub(4);
                let st = log_base(&self.styles).add_modifier(Modifier::BOLD);
                self.pane.append(vec![Seg::new("-".repeat(w), st)]);
                self.follow = true;
                if self.column_lock {
                    self.pane.track_end = true;
                } else {
                    self.pane.scroll_to_end();
                }
            }
            "s" => {
                self.autoscroll = !self.autoscroll;
                self.follow = self.autoscroll;
            }
            "shift-l" => self.column_lock = !self.column_lock,
            "f" => self.fullscreen = !self.fullscreen,
            "t" => {
                self.show_time = !self.show_time;
                self.refresh();
            }
            "w" => self.pane.wrap = !self.pane.wrap,
            "ctrl-s" => return vec![self.save(ctx)],
            "c" => return vec![util::copy_to_clipboard(&self.pane.text())],
            "a" if self.has_default => {
                self.all_containers = !self.all_containers;
                if !self.single {
                    if self.all_containers {
                        self.default_container = self.container.take();
                    } else if let Some(d) = &self.default_container {
                        self.container = Some(d.clone());
                    }
                }
                self.restart(ctx);
            }
            _ => {
                if self.pane.scroll_key(&k) && !matches!(k.as_str(), "shift-g" | "end") {
                    self.follow = false;
                }
            }
        }
        Vec::new()
    }

    fn on_event(&mut self, ev: &AppEvent, _ctx: &mut Ctx) -> Vec<ViewCmd> {
        match ev {
            AppEvent::Custom { view, payload } if *view == self.id => {
                if let Some(st) = payload.downcast_ref::<StreamStart>() {
                    if st.gen == *self.gen.lock().expect("gen lock") {
                        self.awaiting_start = false;
                        self.single = st.single_container;
                    }
                }
                Vec::new()
            }
            AppEvent::LogLines { view, lines } if *view == self.id && !self.awaiting_start => {
                for l in lines {
                    let it = self.to_item(l);
                    self.model.append(it);
                }
                self.notify()
            }
            AppEvent::LogEnd { view, error } if *view == self.id && !self.awaiting_start => {
                match error {
                    None => {
                        let ts =
                            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true);
                        self.model.append(LogItem {
                            pod: String::new(),
                            container: String::new(),
                            single: self.single,
                            ts: Some(ts),
                            text: format!("stream closed: EOF for {}", self.info()),
                            is_error: true,
                        });
                        self.notify()
                    }
                    Some(e) => self.log_failed(e),
                }
            }
            _ => Vec::new(),
        }
    }

    fn on_filter(&mut self, text: &str, _ctx: &mut Ctx) -> Vec<ViewCmd> {
        self.set_filter(text)
    }

    fn draw(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &Ctx) {
        self.styles = ctx.styles.clone();
        self.width = area.width;
        let s = self.styles.clone();
        let buf = frame.buffer_mut();
        let base = log_base(&s);
        let inner = if self.fullscreen {
            util::fill(buf, area, base);
            area
        } else {
            // No color tag before " Logs": tview's TitleColor, which k9s
            // sets to the body fg (config/styles.go Update).
            let title = util::markup_line(
                &self.title_markup(),
                Style::default().fg(s.fg).bg(s.title_bg),
            );
            // Focus sits on the inner log text view (view/log.go Flex).
            util::draw_box(buf, area, &title, base.fg(s.border), base)
        };
        if inner.height == 0 {
            return;
        }
        let ind = Rect::new(inner.x, inner.y, inner.width, 1);
        util::fill(
            buf,
            ind,
            Style::default()
                .fg(s.log_indicator_fg)
                .bg(s.log_indicator_bg),
        );
        util::put_centered(buf, ind.x, ind.y, ind.width, &self.indicator());
        let logs = Rect::new(inner.x, inner.y + 1, inner.width, inner.height - 1);
        let logs = if self.fullscreen {
            logs
        } else {
            util::pad_x(logs, 1)
        };
        self.pane.draw(buf, logs);
    }

    fn is_fullscreen(&self) -> bool {
        self.fullscreen
    }

    fn filter_text(&self) -> String {
        self.filter.clone()
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
    use crossterm::event::{KeyCode, KeyModifiers};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::sync::RwLock;

    fn line(co: &str, ts: &str, text: &str) -> LogLine {
        LogLine {
            pod: "nginx".into(),
            container: co.into(),
            timestamp: Some(ts.into()),
            text: text.into(),
        }
    }

    fn setup(lines: Vec<LogLine>, pod: bool) -> (Ctx, mpsc::UnboundedReceiver<AppEvent>) {
        let f = FakeAccessor::new();
        f.logs.lock().unwrap().insert("default/nginx".into(), lines);
        if pod {
            f.add(
                "v1/pods",
                obj(serde_json::json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"nginx","namespace":"default"},"spec":{"containers":[{"name":"web"}]}})),
            );
        }
        let (tx, rx) = mpsc::unbounded_channel();
        let ctx = Ctx::new(
            Arc::new(f),
            Arc::new(RwLock::new(crate::config::Config::default())),
            Styles::default(),
            "default".into(),
            tx,
        );
        (ctx, rx)
    }

    fn opts(co: Option<&str>) -> LogOptions {
        LogOptions {
            namespace: "default".into(),
            path: "nginx".into(),
            container: co.map(String::from),
            ..Default::default()
        }
    }

    async fn pump(v: &mut LogsView, ctx: &mut Ctx, rx: &mut mpsc::UnboundedReceiver<AppEvent>) {
        loop {
            let ev = tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .expect("event")
                .expect("open");
            let end = matches!(ev, AppEvent::LogEnd { .. });
            v.on_event(&ev, ctx);
            if end {
                return;
            }
        }
    }

    fn render(v: &mut LogsView, ctx: &Ctx, w: u16, h: u16) -> ratatui::buffer::Buffer {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| v.draw(f, f.area(), ctx)).unwrap();
        t.backend().buffer().clone()
    }

    fn row(b: &ratatui::buffer::Buffer, y: u16) -> String {
        (0..b.area.width).map(|x| b[(x, y)].symbol()).collect()
    }

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    #[test]
    fn indicator_and_title() {
        let (mut ctx, _rx) = setup(vec![], false);
        let v = LogsView::new(&mut ctx, opts(None));
        assert_eq!(
            util::plain(&v.indicator()),
            "Autoscroll:On      ColumnLock:Off     FullScreen:Off     Timestamps:Off     Wrap:Off"
        );
        let ind = v.indicator();
        let on = ind.iter().find(|s| s.text == "On").unwrap();
        assert_eq!(on.style.fg, Some(ctx.styles.log_toggle_on));
        let off = ind.iter().find(|s| s.text == "Off").unwrap();
        assert_eq!(off.style.fg, Some(ctx.styles.log_toggle_off));
        assert!(off.style.add_modifier.contains(Modifier::DIM));
        assert_eq!(v.title(), " Logs(default/nginx)[tail] ");
        // A plain container choice has no AllContainers toggle; only the
        // pod's default-container annotation brings it (HasDefaultContainer).
        let v = LogsView::new(&mut ctx, opts(Some("web")));
        assert_eq!(v.title(), " Logs(default/nginx:web)[tail] ");
        assert!(util::plain(&v.indicator()).starts_with("Autoscroll:On "));
        assert!(!v.hints().iter().any(|h| h.mnemonic == "a"));
        let mut o = opts(Some("web"));
        o.default_container = Some("web".into());
        let v = LogsView::new(&mut ctx, o);
        assert!(util::plain(&v.indicator()).starts_with("AllContainers:Off     Autoscroll:On "));
        assert!(v.hints().iter().any(|h| h.mnemonic == "a"));
        let mut o = opts(None);
        o.previous = true;
        let v = LogsView::new(&mut ctx, o);
        assert_eq!(v.title(), " Previous Logs(default/nginx)[tail] ");
        // Title colors: body fg base, highlighted path, and the container in
        // the default color (SkinTitle replaces only the first [hilite).
        let v = LogsView::new(&mut ctx, opts(Some("web")));
        let base = Style::default().fg(ctx.styles.fg);
        let segs = util::markup_line(&v.title_markup(), base);
        let fg_of = |t: &str| segs.iter().find(|s| s.text.contains(t)).unwrap().style.fg;
        assert_eq!(fg_of("Logs("), Some(ctx.styles.fg));
        assert_eq!(fg_of("default/nginx"), Some(ctx.styles.title_highlight));
        assert_eq!(fg_of("web"), Some(Color::Reset));
        assert_eq!(fg_of("tail"), Some(Color::Green));
    }

    #[tokio::test]
    async fn streams_and_renders_with_container_prefix() {
        let lines = vec![
            line("web", "2024-01-01T00:00:00Z", "hello"),
            line(
                "sidecar",
                "2024-01-01T00:00:01Z",
                "\x1b[31mred\x1b[0m world",
            ),
        ];
        let (mut ctx, mut rx) = setup(lines, false);
        let mut v = LogsView::new(&mut ctx, opts(None));
        let b = render(&mut v, &ctx, 80, 8);
        assert!(row(&b, 2).contains("Waiting for logs..."));
        v.start(&mut ctx);
        pump(&mut v, &mut ctx, &mut rx).await;
        let b = render(&mut v, &ctx, 80, 8);
        assert!(row(&b, 0).contains(" Logs(default/nginx)[tail] "));
        assert!(row(&b, 1).contains("Autoscroll:On"));
        assert!(row(&b, 2).contains("web hello"), "{}", row(&b, 2));
        assert!(row(&b, 3).contains("sidecar red world"));
        assert!(row(&b, 4).contains("stream closed: EOF for default/nginx"));
        // container prefix is bold, colored from the pod palette.
        assert!(b[(2, 2)].modifier.contains(Modifier::BOLD));
        // ANSI red.
        let x = row(&b, 3).find("red").unwrap() as u16;
        assert_eq!(b[(x, 3)].fg, Color::Red);
        // timestamps toggle.
        v.handle_key(key(KeyCode::Char('t')), &mut ctx);
        let b = render(&mut v, &ctx, 100, 8);
        assert!(row(&b, 2).contains("2024-01-01T00:00:00Z"));
        assert!(util::plain(&v.indicator()).contains("Timestamps:On "));
    }

    #[tokio::test]
    async fn single_container_has_no_prefix_and_filter_highlights() {
        let lines = vec![
            line("web", "t1", "GET /index"),
            line("web", "t2", "POST /login"),
        ];
        let (mut ctx, mut rx) = setup(lines, true);
        let mut v = LogsView::new(&mut ctx, opts(Some("web")));
        v.start(&mut ctx);
        pump(&mut v, &mut ctx, &mut rx).await;
        let b = render(&mut v, &ctx, 60, 8);
        assert!(row(&b, 2).starts_with("│ GET /index"), "{}", row(&b, 2));
        v.on_filter("post", &mut ctx);
        assert_eq!(v.title(), " Logs(default/nginx:web)[tail] </post> ");
        let b = render(&mut v, &ctx, 60, 8);
        assert!(row(&b, 2).starts_with("│ POST /login"));
        assert_eq!(b[(2, 2)].fg, HIGHLIGHT);
        assert!(!row(&b, 3).contains("GET"));
        v.on_filter("!post", &mut ctx);
        let b = render(&mut v, &ctx, 60, 8);
        assert!(row(&b, 2).contains("GET /index"));
        // esc clears the filter first, then pops.
        v.handle_key(key(KeyCode::Esc), &mut ctx);
        assert_eq!(v.filter_text(), "");
        let cmds = v.handle_key(key(KeyCode::Esc), &mut ctx);
        assert!(matches!(cmds[0], ViewCmd::Pop));
    }

    #[tokio::test]
    async fn buffer_cap_and_since_restart() {
        let lines: Vec<LogLine> = (0..10)
            .map(|i| line("web", "t", &format!("l{i}")))
            .collect();
        let (mut ctx, mut rx) = setup(lines, true);
        {
            let mut c = ctx.config.write().unwrap();
            c.k9s.logger.tail = 3;
            c.k9s.logger.buffer = 4;
        }
        let mut v = LogsView::new(&mut ctx, opts(Some("web")));
        // Stock logger.sinceSeconds -1: tail, not head.
        assert_eq!(v.log_options().since_seconds, Some(-1));
        v.start(&mut ctx);
        pump(&mut v, &mut ctx, &mut rx).await;
        assert_eq!(v.model.items.len(), 3);
        assert_eq!(v.pane.lines.len(), 4);
        // since 5m restarts the stream with since_seconds.
        v.handle_key(key(KeyCode::Char('3')), &mut ctx);
        assert_eq!(v.title(), " Logs(default/nginx:web)[5m] ");
        assert_eq!(v.log_options().since_seconds, Some(300));
        assert!(v.pane.lines.is_empty());
        pump(&mut v, &mut ctx, &mut rx).await;
        assert!(util::plain(&v.pane.lines[0]).starts_with('l'));
        v.handle_key(key(KeyCode::Char('1')), &mut ctx);
        assert_eq!(v.title(), " Logs(default/nginx:web)[head] ");
        let o = v.log_options();
        assert!(!o.follow);
        assert_eq!(o.since_seconds, None);
        v.handle_key(key(KeyCode::Char('6')), &mut ctx);
        assert_eq!(v.title(), " Logs(default/nginx:web)[1h] ");
        v.handle_key(key(KeyCode::Char('0')), &mut ctx);
        assert_eq!(v.title(), " Logs(default/nginx:web)[tail] ");
        // Tail streams (follow, tail lines), never head mode.
        let o = v.log_options();
        assert!(o.follow);
        assert!(o.since_seconds.is_some());
    }

    #[tokio::test]
    async fn stale_events_are_dropped_after_restart() {
        let lines = vec![line("web", "t", "x")];
        let (mut ctx, mut rx) = setup(lines, true);
        let mut v = LogsView::new(&mut ctx, opts(Some("web")));
        v.start(&mut ctx);
        // restart before consuming: the first stream's events are stale.
        v.handle_key(key(KeyCode::Char('0')), &mut ctx);
        let mut lines_seen = 0;
        loop {
            let ev = tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .unwrap()
                .unwrap();
            let end = matches!(ev, AppEvent::LogEnd { .. }) && !v.awaiting_start;
            if matches!(ev, AppEvent::LogLines { .. }) && !v.awaiting_start {
                lines_seen += 1;
            }
            v.on_event(&ev, &mut ctx);
            if end {
                break;
            }
        }
        assert_eq!(lines_seen, 1);
        assert_eq!(v.model.items.len(), 2);
    }

    #[tokio::test]
    async fn toggles_mark_clear_and_autoscroll_pause() {
        let (mut ctx, mut rx) = setup(vec![line("web", "t", "a")], true);
        let mut v = LogsView::new(&mut ctx, opts(Some("web")));
        v.start(&mut ctx);
        pump(&mut v, &mut ctx, &mut rx).await;
        render(&mut v, &ctx, 30, 6);
        v.handle_key(key(KeyCode::Char('m')), &mut ctx);
        assert_eq!(util::plain(v.pane.lines.last().unwrap()), "-".repeat(26));
        v.handle_key(
            KeyEvent::new(KeyCode::Char('C'), KeyModifiers::SHIFT),
            &mut ctx,
        );
        assert!(v.pane.lines.is_empty());
        assert!(v.model.items.is_empty());
        v.handle_key(key(KeyCode::Char('s')), &mut ctx);
        assert!(util::plain(&v.indicator()).contains("Autoscroll:Off"));
        v.on_event(
            &AppEvent::LogLines {
                view: v.id(),
                lines: vec![line("web", "t", "hidden")],
            },
            &mut ctx,
        );
        assert!(v.pane.lines.is_empty());
        assert_eq!(v.model.items.len(), 1);
        v.handle_key(key(KeyCode::Char('w')), &mut ctx);
        assert!(util::plain(&v.indicator()).ends_with("Wrap:On "));
        v.handle_key(key(KeyCode::Char('f')), &mut ctx);
        assert!(v.is_fullscreen());
        v.handle_key(
            KeyEvent::new(KeyCode::Char('L'), KeyModifiers::SHIFT),
            &mut ctx,
        );
        assert!(util::plain(&v.indicator()).contains("ColumnLock:On "));
    }

    #[test]
    fn ansi_and_palette() {
        let base = Style::default().fg(Color::White);
        let l = parse_ansi("\x1b[1;38;5;100mX\x1b[0mY\x1b[38;2;1;2;3mZ", base);
        assert_eq!(l[0].style.fg, Some(Color::Indexed(100)));
        assert!(l[0].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(l[1].style, base);
        assert_eq!(l[2].style.fg, Some(Color::Rgb(1, 2, 3)));
        let mut m = LogModel::default();
        let c = m.color_for("ab", Color::White);
        // idx = 0*'a' + 1*'b' = 98 -> 98 % 8 = 2 -> purple
        assert_eq!(c, Color::Magenta);
    }
}
