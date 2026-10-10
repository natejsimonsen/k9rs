//! Text viewers: k9s view/live_view.go (describe/yaml with optional live
//! refresh) and view/details.go (static text), plus the yaml colorizer of
//! view/yaml.go.

use std::any::Any;
use std::sync::OnceLock;
use std::time::Duration;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::Frame;
use regex::Regex;
use tokio_util::sync::CancellationToken;

use super::util::{self, SLine, Seg, TextMatch, TextPane};
use crate::config::Styles;
use crate::k8s::{Gvr, SharedAccessor};
use crate::ui::{AppEvent, Ctx, FlashLevel, Hint, View, ViewCmd, ViewId};

/// k9s defaultReaderRefreshRate.
const REFRESH_RATE: Duration = Duration::from_secs(5);
/// k9s maxReaderRetryInterval.
const MAX_RETRY: Duration = Duration::from_secs(120);

pub(crate) const YAML_ACTION: &str = "YAML";

/// What the viewer shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    /// Live `describe` (k9s model.Describe).
    Describe(Gvr),
    /// Live yaml (k9s model.YAML).
    Yaml(Gvr),
    /// Static text (k9s view.Details); `yaml` = content type yaml.
    Static { yaml: bool, searchable: bool },
    /// The cow error viewer (view/cow.go).
    Cow,
    /// Helm notes/manifest: a LiveView over the release (helm dao
    /// Describe/ToYAML); fetched once, so refresh toggles are inert.
    Release,
    /// Helm values (model.Values/RevValues in a LiveView): user supplied
    /// values, toggled with `v` to all values.
    Values {
        all: bool,
        user: String,
        computed: String,
    },
}

/// Refresh request sent to ourselves after an external edit.
struct Refetch;

/// editRes's patch access check result.
struct EditAllowed(bool);

pub(crate) struct DetailsView {
    id: ViewId,
    title: String,
    subject: String,
    source: Source,
    lines: Vec<String>,
    query: String,
    matches: Vec<TextMatch>,
    current: usize,
    /// Static details colorize once a filter ran (TextFiltered).
    filtered: bool,
    pane: TextPane,
    fullscreen: bool,
    auto_refresh: bool,
    managed: bool,
    decode: bool,
    readonly: bool,
    error: Option<String>,
    cancel: Option<CancellationToken>,
    styles: Styles,
}

impl DetailsView {
    pub(crate) fn live(ctx: &mut Ctx, title: &str, gvr: Gvr, path: String, yaml: bool) -> Self {
        let source = if yaml {
            Source::Yaml(gvr)
        } else {
            Source::Describe(gvr)
        };
        let mut v = Self::base(ctx, title, path, source);
        v.auto_refresh = ctx
            .config
            .read()
            .map(|c| c.k9s.live_view_auto_refresh)
            .unwrap_or(false);
        v
    }

    pub(crate) fn fixed(
        ctx: &mut Ctx,
        title: &str,
        subject: String,
        yaml: bool,
        searchable: bool,
        body: &str,
    ) -> Self {
        let mut v = Self::base(ctx, title, subject, Source::Static { yaml, searchable });
        v.set_text(body);
        v.pane.scroll_to_beginning();
        v
    }

    pub(crate) fn values(ctx: &mut Ctx, subject: String, user: String, computed: String) -> Self {
        let body = user.clone();
        let mut v = Self::base(
            ctx,
            "Values",
            subject,
            Source::Values {
                all: false,
                user,
                computed,
            },
        );
        v.set_text(&body);
        v.pane.scroll_to_beginning();
        v
    }

    pub(crate) fn release(ctx: &mut Ctx, title: &str, subject: String, body: &str) -> Self {
        let mut v = Self::base(ctx, title, subject, Source::Release);
        v.set_text(body);
        v.pane.scroll_to_beginning();
        v
    }

    pub(crate) fn cow(ctx: &mut Ctx, says: &str) -> Self {
        let mut v = Self::base(ctx, "cow", String::new(), Source::Cow);
        v.error = Some(if says.is_empty() {
            "Nothing to report here. Please move along...".into()
        } else {
            says.to_string()
        });
        v
    }

    fn base(ctx: &mut Ctx, title: &str, subject: String, source: Source) -> Self {
        let full = ctx
            .config
            .read()
            .map(|c| c.k9s.ui.defaults_to_full_screen)
            .unwrap_or(false);
        DetailsView {
            id: ctx.new_view_id(),
            title: title.to_string(),
            subject,
            source,
            lines: Vec::new(),
            query: String::new(),
            matches: Vec::new(),
            current: 0,
            filtered: false,
            pane: TextPane::new(true),
            fullscreen: full,
            auto_refresh: false,
            managed: false,
            decode: false,
            readonly: ctx.readonly,
            error: None,
            cancel: None,
            styles: ctx.styles.clone(),
        }
    }

    fn is_live(&self) -> bool {
        matches!(self.source, Source::Describe(_) | Source::Yaml(_))
    }

    fn searchable(&self) -> bool {
        match self.source {
            Source::Static { searchable, .. } => searchable,
            Source::Cow => false,
            _ => true,
        }
    }

    fn gvr(&self) -> Option<&Gvr> {
        match &self.source {
            Source::Describe(g) | Source::Yaml(g) => Some(g),
            _ => None,
        }
    }

    fn colorize(&self) -> bool {
        match self.source {
            Source::Static { yaml, .. } => yaml || self.filtered,
            Source::Cow => false,
            _ => true,
        }
    }

    fn set_text(&mut self, body: &str) {
        self.lines = body.split('\n').map(String::from).collect();
        self.matches = util::filter_lines(&self.query, &self.lines);
        self.current = 0;
        self.render_lines();
    }

    /// Per line match ranges (start, end, is current).
    fn line_ranges(&self) -> Vec<Vec<(usize, usize, bool)>> {
        let mut per_line: Vec<Vec<(usize, usize, bool)>> = vec![Vec::new(); self.lines.len()];
        for (i, m) in self.matches.iter().enumerate() {
            if let Some(v) = per_line.get_mut(m.line) {
                for &(s, e) in &m.ranges {
                    v.push((s, e, i == self.current));
                }
            }
        }
        per_line
    }

    /// Rebuild the styled lines (colors + current match highlight).
    fn render_lines(&mut self) {
        let base = self.styles.body();
        let colorize = self.colorize();
        let per_line = self.line_ranges();
        let out: Vec<SLine> = self
            .lines
            .iter()
            .zip(per_line.iter())
            .map(|(l, r)| {
                if colorize {
                    colorize_yaml_line(l, r, &self.styles, base)
                } else {
                    highlight(vec![Seg::new(l.clone(), base)], r)
                }
            })
            .collect();
        self.pane.set_lines(out);
    }

    fn reveal_current(&mut self) {
        if let Some(m) = self.matches.get(self.current) {
            self.pane.reveal = Some(m.line);
        }
    }

    /// LiveView.ResourceChanged / Details.TextFiltered.
    fn apply_filter(&mut self) {
        self.matches = util::filter_lines(&self.query, &self.lines);
        self.current = 0;
        self.filtered = true;
        self.render_lines();
        self.reveal_current();
    }

    fn fetch(&self, ctx: &Ctx) {
        let Some(gvr) = self.gvr().cloned() else {
            return;
        };
        let req = Fetch {
            acc: ctx.accessor.clone(),
            gvr,
            path: self.subject.clone(),
            yaml: matches!(self.source, Source::Yaml(_)),
            managed: self.managed,
            decode: self.decode,
        };
        let tx = ctx.sender();
        let view = self.id;
        ctx.spawn(async move {
            let ev = match req.run().await {
                Ok(text) => AppEvent::Text { view, text },
                Err(e) => AppEvent::Error {
                    view,
                    msg: e.to_string(),
                },
            };
            let _ = tx.send(ev);
        });
    }

    fn start_watch(&mut self, ctx: &Ctx) {
        self.stop_watch();
        let Some(gvr) = self.gvr().cloned() else {
            return;
        };
        let token = CancellationToken::new();
        self.cancel = Some(token.clone());
        let req = Fetch {
            acc: ctx.accessor.clone(),
            gvr,
            path: self.subject.clone(),
            yaml: matches!(self.source, Source::Yaml(_)),
            managed: self.managed,
            decode: self.decode,
        };
        let tx = ctx.sender();
        let view = self.id;
        ctx.spawn(async move {
            let mut delay = Duration::ZERO;
            loop {
                tokio::select! {
                    _ = token.cancelled() => return,
                    _ = tokio::time::sleep(delay) => {}
                }
                let ev = match req.run().await {
                    Ok(text) => {
                        delay = REFRESH_RATE;
                        AppEvent::Text { view, text }
                    }
                    Err(e) => {
                        delay = if delay < REFRESH_RATE {
                            REFRESH_RATE
                        } else {
                            (delay * 2).min(MAX_RETRY)
                        };
                        AppEvent::Error {
                            view,
                            msg: e.to_string(),
                        }
                    }
                };
                if token.is_cancelled() || tx.send(ev).is_err() {
                    return;
                }
            }
        });
    }

    fn stop_watch(&mut self) {
        if let Some(t) = self.cancel.take() {
            t.cancel();
        }
    }

    /// LiveView.Start.
    fn restart(&mut self, ctx: &Ctx) {
        if self.auto_refresh {
            self.start_watch(ctx);
        } else {
            self.fetch(ctx);
        }
    }

    fn title_markup(&self) -> String {
        let mut fmat = format!(
            "[fg:bg:b] {}([hilite:bg:b]{}[fg:bg:-])[fg:bg:-] ",
            self.title, self.subject
        );
        if self.query.is_empty() {
            return util::skin_title(&fmat, &self.styles);
        }
        let mut buff = self.query.clone();
        if !self.matches.is_empty() {
            buff += &format!("[{}:{}]", self.current + 1, self.matches.len());
        }
        fmat += &util::SEARCH_FMT.replace("{}", &buff);
        util::skin_title(&fmat, &self.styles)
    }

    /// tview TextView.GetText(true): colorized text went through tview.Escape
    /// (escapes are kept since dynamic colors are on); plain text loses its
    /// color tags. Copy and live saves then apply k9s sanitizeEsc.
    fn export_text(&self, sanitize: bool) -> String {
        let text = if self.colorize() {
            let ranges = self.line_ranges();
            self.lines
                .iter()
                .zip(ranges.iter())
                .map(|(l, r)| yaml_line_plain(&tview_escape(l), r))
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            strip_color_tags(&self.lines.join("\n"))
        };
        if sanitize {
            sanitize_esc(&text)
        } else {
            text
        }
    }

    fn save(&self, ctx: &Ctx) -> ViewCmd {
        let dir = util::screen_dump_dir(ctx);
        let text = self.export_text(self.is_live());
        if self.is_live() {
            let name = format!(
                "{}--{}",
                self.subject.replacen('/', "-", 1),
                self.title.to_lowercase()
            );
            match util::save_yaml(&dir, &name, &text) {
                Ok(_) => util::flash(
                    FlashLevel::Info,
                    format!("File {} saved successfully!", util::go_quote(&name)),
                ),
                Err(e) => util::flash(FlashLevel::Err, e.to_string()),
            }
        } else {
            match util::save_yaml(&dir, &self.title, &text) {
                Ok(p) => util::flash(
                    FlashLevel::Info,
                    format!("Log {} saved successfully!", p.display()),
                ),
                Err(e) => util::flash(FlashLevel::Err, e.to_string()),
            }
        }
    }

    /// LiveView.editCmd -> editRes: patch access is checked first.
    fn edit(&mut self, ctx: &mut Ctx) -> Vec<ViewCmd> {
        let Some(gvr) = self.gvr().cloned() else {
            return Vec::new();
        };
        if self.subject.is_empty() {
            return Vec::new();
        }
        let (ns, n) = crate::k8s::namespaced(&self.subject);
        let ns = if crate::k8s::is_cluster_scoped(ns) {
            ""
        } else {
            ns
        };
        let (acc, ns, n, view, tx) = (
            ctx.accessor.clone(),
            ns.to_string(),
            n.to_string(),
            self.id,
            ctx.sender(),
        );
        ctx.spawn(async move {
            let ok = acc.can_named(&gvr, &ns, &n, "patch").await.unwrap_or(false);
            let _ = tx.send(AppEvent::Custom {
                view,
                payload: Box::new(EditAllowed(ok)),
            });
        });
        Vec::new()
    }

    fn run_edit(&mut self, _ctx: &mut Ctx) -> Vec<ViewCmd> {
        let Some(gvr) = self.gvr().cloned() else {
            return Vec::new();
        };
        self.stop_watch();
        let view = self.id;
        vec![ViewCmd::Edit {
            gvr,
            path: self.subject.clone(),
            // live_view.go editCmd: Start again (re-fetch) once the edit ends.
            on_done: Some(Box::new(move |ctx: &mut Ctx, _st| {
                let _ = ctx.sender().send(AppEvent::Custom {
                    view,
                    payload: Box::new(Refetch),
                });
                Vec::new()
            })),
        }]
    }
}

/// One async fetch of describe/yaml text (with secret decoding).
struct Fetch {
    acc: SharedAccessor,
    gvr: Gvr,
    path: String,
    yaml: bool,
    managed: bool,
    decode: bool,
}

impl Fetch {
    async fn run(&self) -> anyhow::Result<String> {
        let secret = self.decode && self.gvr == Gvr::parse("v1/secrets");
        if self.yaml {
            if secret {
                let o = self.acc.get(&self.gvr, &self.path).await?;
                return decode_secret_yaml(o, self.managed);
            }
            return self.acc.to_yaml(&self.gvr, &self.path, self.managed).await;
        }
        let desc = self.acc.describe(&self.gvr, &self.path).await?;
        if !secret {
            return Ok(desc);
        }
        let o = self.acc.get(&self.gvr, &self.path).await?;
        decode_secret_description(&desc, &o)
    }
}

fn secret_data(o: &kube::api::DynamicObject) -> Vec<(String, String)> {
    use base64::Engine;
    let mut out: Vec<(String, String)> = o
        .data
        .get("data")
        .and_then(|d| d.as_object())
        .map(|m| {
            m.iter()
                .map(|(k, v)| {
                    let raw = v.as_str().unwrap_or_default();
                    let dec = base64::engine::general_purpose::STANDARD
                        .decode(raw)
                        .map(|b| String::from_utf8_lossy(&b).into_owned())
                        .unwrap_or_else(|_| raw.to_string());
                    (k.clone(), dec)
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// dao.Secret.decodeYAML.
fn decode_secret_yaml(mut o: kube::api::DynamicObject, managed: bool) -> anyhow::Result<String> {
    if !managed {
        o.metadata.managed_fields = None;
    }
    let data = secret_data(&o);
    let mut v = serde_json::to_value(&o)?;
    if let Some(m) = v.as_object_mut() {
        let dm: serde_json::Map<String, serde_json::Value> = data
            .into_iter()
            .map(|(k, s)| (k, serde_json::Value::String(s)))
            .collect();
        m.insert("data".into(), serde_json::Value::Object(dm));
    }
    Ok(crate::k8s::goyaml::to_yaml(&v))
}

/// dao.Secret.Decode.
fn decode_secret_description(desc: &str, o: &kube::api::DynamicObject) -> anyhow::Result<String> {
    let idx = desc
        .find("====")
        .ok_or_else(|| anyhow::anyhow!("unable to find data section in secret description"))?;
    let end = idx + 4;
    if end >= desc.len() {
        anyhow::bail!("data section in secret description is invalid");
    }
    let lines: Vec<String> = secret_data(o)
        .into_iter()
        .map(|(k, v)| format!("{k}: {v}").trim().to_string())
        .collect();
    Ok(format!("{}\n{}", &desc[..end], lines.join("\n")))
}

fn key_val_rx() -> &'static Regex {
    static RX: OnceLock<Regex> = OnceLock::new();
    // Go's \s and \w are ASCII-only.
    RX.get_or_init(|| {
        Regex::new(r"\A([\t\n\x0C\r ]*)([0-9A-Za-z_\-./\t\n\x0C\r ]+):[\t\n\x0C\r ](.+)\z")
            .expect("kv regex")
    })
}

fn key_rx() -> &'static Regex {
    static RX: OnceLock<Regex> = OnceLock::new();
    RX.get_or_init(|| {
        Regex::new(r"\A([\t\n\x0C\r ]*)([0-9A-Za-z_\-./\t\n\x0C\r ]+):[\t\n\x0C\r ]*\z")
            .expect("key regex")
    })
}

/// tview TextView region highlight: swap fore and background; a default
/// background becomes black or white by the text's lightness.
fn highlighted(st: Style) -> Style {
    let fg = st.fg.unwrap_or(Color::Reset);
    let bg = match st.bg {
        Some(c) if c != Color::Reset => c,
        _ => match crate::ui::shell::palette::lightness(fg) {
            Some(l) if l < 0.5 => Color::White,
            _ => Color::Black,
        },
    };
    st.fg(bg).bg(fg)
}

/// Overlay the current-match highlight on `line` (byte ranges of its text).
fn highlight(line: SLine, ranges: &[(usize, usize, bool)]) -> SLine {
    let hl: Vec<(usize, usize)> = ranges.iter().filter(|r| r.2).map(|r| (r.0, r.1)).collect();
    if hl.is_empty() {
        return line;
    }
    let mut out = Vec::new();
    let mut off = 0;
    for seg in line {
        let len = seg.text.len();
        let mut cuts = vec![0, len];
        for &(s, e) in &hl {
            for p in [s, e] {
                if p > off && p < off + len && seg.text.is_char_boundary(p - off) {
                    cuts.push(p - off);
                }
            }
        }
        cuts.sort_unstable();
        cuts.dedup();
        for w in cuts.windows(2) {
            let (a, b) = (w[0], w[1]);
            if a == b {
                continue;
            }
            let lit = hl.iter().any(|&(s, e)| off + a >= s && off + b <= e);
            let style = if lit {
                highlighted(seg.style)
            } else {
                seg.style
            };
            out.push(Seg::new(&seg.text[a..b], style));
        }
        off += len;
    }
    out
}

/// k9s colorizeYAML for one line. Search region tags inserted in the key
/// part break the key/value regexes in k9s; mirror that by falling back to
/// value coloring when any match starts before the value.
/// Text colorizeYAML leaves once its tags are stripped. Search markers in the
/// key (or anywhere on a key-only line) defeat the key regexes, as in k9s.
fn yaml_line_plain(l: &str, ranges: &[(usize, usize, bool)]) -> String {
    if let Some(c) = key_val_rx().captures(l) {
        let g3 = c.get(3).expect("value");
        if ranges.iter().all(|r| r.0 >= g3.start()) {
            return format!("{}{}: {}", &c[1], &c[2], g3.as_str());
        }
    } else if let Some(c) = key_rx().captures(l) {
        if ranges.is_empty() {
            return format!("{}{}:", &c[1], &c[2]);
        }
    }
    l.to_string()
}

/// tview.Escape.
pub(crate) fn tview_escape(s: &str) -> String {
    static RX: OnceLock<Regex> = OnceLock::new();
    RX.get_or_init(|| Regex::new(r##"(\[[a-zA-Z0-9_,;: \-\."#]+\[*)\]"##).expect("escape regex"))
        .replace_all(s, "${1}[]")
        .into_owned()
}

/// tview colorPattern removal done by GetText(true).
fn strip_color_tags(s: &str) -> String {
    static RX: OnceLock<Regex> = OnceLock::new();
    RX.get_or_init(|| {
        Regex::new(
            r"\[([a-zA-Z]+|#[0-9a-zA-Z]{6}|\-)?:([a-zA-Z]+|#[0-9a-zA-Z]{6}|\-)?:([lbdru]+|\-)?\]",
        )
        .expect("color regex")
    })
    .replace_all(s, "")
    .into_owned()
}

/// k9s sanitizeEsc (greedy per line, so only the outermost pair unescapes).
pub(crate) fn sanitize_esc(s: &str) -> String {
    static RX: OnceLock<Regex> = OnceLock::new();
    RX.get_or_init(|| Regex::new(r"\[(.+)\[\]").expect("bracket regex"))
        .replace_all(s, "[${1}]")
        .into_owned()
}

pub(crate) fn colorize_yaml_line(
    l: &str,
    ranges: &[(usize, usize, bool)],
    s: &Styles,
    base: Style,
) -> SLine {
    // A `default` color is `-` in the k9s markup: the text view color.
    let fg = |c: Color| match (c, base.fg) {
        (Color::Reset, Some(b)) => b,
        _ => c,
    };
    let key = base.fg(fg(s.yaml_key)).add_modifier(Modifier::BOLD);
    let colon = base.fg(fg(s.yaml_colon));
    let val = base.fg(fg(s.yaml_value));
    if let Some(c) = key_val_rx().captures(l) {
        let g3 = c.get(3).expect("value");
        if ranges.iter().all(|r| r.0 >= g3.start()) {
            let g1 = c.get(1).expect("indent");
            let g2 = c.get(2).expect("key");
            let mut segs = Vec::new();
            if !g1.as_str().is_empty() {
                segs.push(Seg::new(g1.as_str(), val));
            }
            segs.push(Seg::new(g2.as_str(), key));
            // yamlFullFmt rewrites the separator as ": ".
            segs.push(Seg::new(": ", colon));
            segs.push(Seg::new(g3.as_str(), val));
            return highlight(segs, ranges);
        }
    } else if let Some(c) = key_rx().captures(l) {
        if ranges.is_empty() {
            let g1 = c.get(1).expect("indent");
            let g2 = c.get(2).expect("key");
            let mut segs = Vec::new();
            if !g1.as_str().is_empty() {
                segs.push(Seg::new(g1.as_str(), val));
            }
            segs.push(Seg::new(g2.as_str(), key));
            // yamlKeyFmt drops whitespace after the colon.
            segs.push(Seg::new(":", colon));
            return segs;
        }
    }
    if l.is_empty() {
        return Vec::new();
    }
    highlight(vec![Seg::new(l, val)], ranges)
}

impl View for DetailsView {
    fn owns_frame(&self) -> bool {
        true
    }
    fn id(&self) -> ViewId {
        self.id
    }

    fn name(&self) -> String {
        self.title.clone()
    }

    /// details.go / live_view.go SetTitleColor(tcell.ColorAqua).
    fn title_color(&self, _: &Styles) -> Color {
        Color::LightCyan
    }

    fn title(&self) -> String {
        if matches!(self.source, Source::Cow) {
            return " Error ".into();
        }
        util::plain(&util::markup_line(
            &self.title_markup(),
            util::title_base(&self.styles),
        ))
    }

    fn hints(&self) -> Vec<Hint> {
        let mut hh = vec![Hint::new("esc", "Back").hidden()];
        if matches!(self.source, Source::Cow) {
            return hh;
        }
        hh.extend([
            Hint::new("q", "Back").hidden(),
            Hint::new("ctrl-s", "Save").hidden(),
            Hint::new("c", "Copy"),
            Hint::new("f", "Toggle FullScreen"),
        ]);
        if self.searchable() {
            hh.push(Hint::new("n", "Next Match"));
            hh.push(Hint::new("shift-n", "Prev Match"));
        }
        if matches!(self.source, Source::Values { .. }) {
            hh.push(Hint::new("r", "Toggle Auto-Refresh"));
            if !self.readonly {
                hh.push(Hint::new("e", "Edit"));
            }
            hh.push(Hint::new("v", "Toggle All Values"));
        }
        if self.is_live() || self.source == Source::Release {
            hh.push(Hint::new("r", "Toggle Auto-Refresh"));
            if !self.readonly {
                hh.push(Hint::new("e", "Edit"));
            }
            if self.title == YAML_ACTION {
                hh.push(Hint::new("m", "Toggle ManagedFields"));
            }
            hh.push(Hint::new("x", "Toggle Decode"));
        }
        util::sort_hints(&mut hh);
        hh
    }

    fn start(&mut self, ctx: &mut Ctx) -> Vec<ViewCmd> {
        self.styles = ctx.styles.clone();
        if self.is_live() {
            self.restart(ctx);
        }
        Vec::new()
    }

    fn stop(&mut self, _ctx: &mut Ctx) {
        self.stop_watch();
    }

    fn handle_key(&mut self, key: KeyEvent, ctx: &mut Ctx) -> Vec<ViewCmd> {
        let k = util::key_label(&key);
        if matches!(self.source, Source::Cow) {
            return match k.as_str() {
                "esc" => vec![ViewCmd::Pop],
                _ => {
                    self.pane.scroll_key(&k);
                    Vec::new()
                }
            };
        }
        match k.as_str() {
            "esc" | "q" => {
                self.query.clear();
                vec![ViewCmd::Pop]
            }
            "/" => vec![ViewCmd::OpenFilterPrompt],
            "ctrl-s" => vec![self.save(ctx)],
            "c" => vec![util::copy_to_clipboard(&self.export_text(true))],
            "f" => {
                self.fullscreen = !self.fullscreen;
                Vec::new()
            }
            "n" | "shift-n" if self.searchable() => {
                if self.query.is_empty() || self.matches.is_empty() {
                    return Vec::new();
                }
                let max = self.matches.len();
                self.current = if k == "n" {
                    (self.current + 1) % max
                } else if self.current == 0 {
                    max - 1
                } else {
                    self.current - 1
                };
                self.render_lines();
                self.reveal_current();
                Vec::new()
            }
            "v" => {
                let Source::Values {
                    all,
                    user,
                    computed,
                } = &mut self.source
                else {
                    self.pane.scroll_key(&k);
                    return Vec::new();
                };
                *all = !*all;
                let body = if *all { computed.clone() } else { user.clone() };
                self.set_text(&body);
                vec![util::flash(FlashLevel::Info, "Values toggled")]
            }
            "m" if self.source == Source::Release && self.title == YAML_ACTION => {
                self.managed = !self.managed;
                vec![util::flash(FlashLevel::Info, "toggled managed fields")]
            }
            "x" | "e" if self.source == Source::Release => Vec::new(),
            // Values never re-fetch, so auto-refresh only flips the flag.
            "r" if matches!(self.source, Source::Values { .. } | Source::Release) => {
                self.auto_refresh = !self.auto_refresh;
                let s = if self.auto_refresh {
                    "enabled"
                } else {
                    "disabled"
                };
                vec![util::flash(
                    FlashLevel::Info,
                    format!("Auto-refresh is {s}"),
                )]
            }
            "r" if self.is_live() => {
                self.auto_refresh = !self.auto_refresh;
                if self.auto_refresh {
                    self.restart(ctx);
                    vec![util::flash(FlashLevel::Info, "Auto-refresh is enabled")]
                } else {
                    self.stop_watch();
                    vec![util::flash(FlashLevel::Info, "Auto-refresh is disabled")]
                }
            }
            "e" if self.is_live() && !self.readonly => self.edit(ctx),
            "m" if self.is_live() && self.title == YAML_ACTION => {
                self.managed = !self.managed;
                self.restart(ctx);
                vec![util::flash(FlashLevel::Info, "toggled managed fields")]
            }
            "x" if self.is_live() => {
                self.decode = !self.decode;
                self.restart(ctx);
                Vec::new()
            }
            _ => {
                self.pane.scroll_key(&k);
                Vec::new()
            }
        }
    }

    /// tview TextView.MouseHandler: the wheel scrolls the text under the
    /// pointer; a click only focuses (already focused here).
    fn on_mouse(&mut self, m: crate::ui::Mouse, _area: Rect, _ctx: &mut Ctx) -> Vec<ViewCmd> {
        match m {
            crate::ui::Mouse::ScrollUp { x, y } => {
                self.pane.wheel(x, y, true);
            }
            crate::ui::Mouse::ScrollDown { x, y } => {
                self.pane.wheel(x, y, false);
            }
            crate::ui::Mouse::Click { .. } => {}
        }
        Vec::new()
    }

    fn on_event(&mut self, ev: &AppEvent, ctx: &mut Ctx) -> Vec<ViewCmd> {
        match ev {
            AppEvent::Text { view, text } if *view == self.id => {
                let lines: Vec<String> = text.split('\n').map(String::from).collect();
                if lines == self.lines && self.error.is_none() {
                    return Vec::new();
                }
                let was_empty = self.lines.is_empty() || self.error.is_some();
                self.error = None;
                self.pane.center = false;
                self.lines = lines;
                self.apply_filter();
                if was_empty && self.matches.is_empty() {
                    self.pane.scroll_to_beginning();
                }
                Vec::new()
            }
            AppEvent::Error { view, msg } if *view == self.id => {
                self.error = Some(msg.clone());
                Vec::new()
            }
            AppEvent::Custom { view, payload } if *view == self.id => {
                if payload.is::<Refetch>() {
                    self.restart(ctx);
                }
                if let Some(EditAllowed(ok)) = payload.downcast_ref::<EditAllowed>() {
                    if !ok {
                        let gvr = self.gvr().map(|g| g.to_string()).unwrap_or_default();
                        return vec![util::flash(
                            FlashLevel::Err,
                            format!("current user can't edit resource {gvr}"),
                        )];
                    }
                    return self.run_edit(ctx);
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn on_filter(&mut self, text: &str, _ctx: &mut Ctx) -> Vec<ViewCmd> {
        self.query = text.to_string();
        self.apply_filter();
        Vec::new()
    }

    fn draw(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &Ctx) {
        // Details.StylesChanged: re-colorize with the new skin.
        if self.styles != ctx.styles {
            self.styles = ctx.styles.clone();
            self.render_lines();
        }
        let s = &self.styles;
        let buf = frame.buffer_mut();
        let body = s.body();
        let inner = if self.fullscreen {
            util::fill(buf, area, body);
            area
        } else {
            // Title color aqua (SetTitleColor): what `-` falls back to.
            let base = util::title_base(s).fg(Color::LightCyan);
            let title = if matches!(self.source, Source::Cow) {
                util::markup_line(" Error ", base)
            } else {
                util::markup_line(&self.title_markup(), base)
            };
            // The focus sits on the inner text view, so the frame keeps the
            // plain border color (tview Box.HasFocus is false).
            let inner = util::draw_box(buf, area, &title, body.fg(s.border), body);
            util::pad_x(inner, 1)
        };
        if let Some(err) = &self.error {
            let w = if matches!(self.source, Source::Cow) {
                (area.x + area.width) as usize / 2
            } else {
                (area.x + area.width) as usize
            };
            let mut pane = TextPane::new(true);
            pane.center = true;
            pane.set_lines(util::cow_talk(err, w, body));
            pane.draw(buf, inner);
            return;
        }
        self.pane.draw(buf, inner);
    }

    fn is_fullscreen(&self) -> bool {
        self.fullscreen
    }

    fn filter_text(&self) -> String {
        self.query.clone()
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
    use std::sync::{Arc, RwLock};
    use tokio::sync::mpsc;

    fn ctx_with(f: FakeAccessor) -> (Ctx, mpsc::UnboundedReceiver<AppEvent>) {
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

    fn fake() -> FakeAccessor {
        let f = FakeAccessor::new();
        f.add(
            "v1/pods",
            obj(serde_json::json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"nginx","namespace":"default"}})),
        );
        f
    }

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn render(v: &mut dyn View, ctx: &Ctx, w: u16, h: u16) -> ratatui::buffer::Buffer {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| v.draw(f, f.area(), ctx)).unwrap();
        t.backend().buffer().clone()
    }

    fn row(b: &ratatui::buffer::Buffer, y: u16) -> String {
        (0..b.area.width).map(|x| b[(x, y)].symbol()).collect()
    }

    #[test]
    fn yaml_colorizer() {
        let s = Styles::default();
        let base = s.body();
        let l = colorize_yaml_line("  name: fred", &[], &s, base);
        assert_eq!(util::plain(&l), "  name: fred");
        assert_eq!(l[1].style.fg, Some(s.yaml_key));
        assert!(l[1].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(l[2].text, ": ");
        assert_eq!(l[2].style.fg, Some(s.yaml_colon));
        assert_eq!(l[3].style.fg, Some(s.yaml_value));
        let l = colorize_yaml_line("metadata:", &[], &s, base);
        assert_eq!(l[0].style.fg, Some(s.yaml_key));
        assert_eq!(l[1].text, ":");
        let l = colorize_yaml_line("- item", &[], &s, base);
        assert_eq!(l[0].style.fg, Some(s.yaml_value));
        // a match in the key breaks key coloring, like k9s region tags.
        let l = colorize_yaml_line("name: fred", &[(0, 2, true)], &s, base);
        assert_eq!(l[0].text, "na");
        // tview highlights by swapping the text's fore and background.
        assert_eq!(l[0].style.bg, Some(s.yaml_value));
        assert_eq!(l[1].style.fg, Some(s.yaml_value));
        // a match in the value keeps colors.
        let l = colorize_yaml_line("name: fred", &[(6, 8, true)], &s, base);
        assert_eq!(l[0].style.fg, Some(s.yaml_key));
        assert_eq!(l[2].text, "fr");
        assert_eq!(l[2].style.bg, Some(s.yaml_value));
    }

    #[test]
    fn highlight_swaps_colors() {
        let st = Style::default()
            .fg(Color::Rgb(0xFF, 0xFF, 0xE0))
            .bg(Color::Black);
        let h = highlighted(st);
        assert_eq!(
            (h.fg, h.bg),
            (Some(Color::Black), Some(Color::Rgb(0xFF, 0xFF, 0xE0)))
        );
        // Default background: black under light text, white under dark.
        let h = highlighted(Style::default().fg(Color::Rgb(0xFF, 0xFF, 0xE0)));
        assert_eq!(h.fg, Some(Color::Black));
        let h = highlighted(Style::default().fg(Color::Rgb(0, 0, 0x80)));
        assert_eq!(h.fg, Some(Color::White));
    }

    #[tokio::test]
    async fn describe_fetch_render_and_search() {
        let (mut ctx, mut rx) = ctx_with(fake());
        let mut v = DetailsView::live(
            &mut ctx,
            "Describe",
            Gvr::parse("v1/pods"),
            "default/nginx".into(),
            false,
        );
        v.start(&mut ctx);
        let ev = rx.recv().await.unwrap();
        v.on_event(&ev, &mut ctx);
        assert_eq!(v.title(), " Describe(default/nginx) ");
        let b = render(&mut v, &ctx, 60, 8);
        assert!(row(&b, 0).contains(" Describe(default/nginx) "));
        assert!(row(&b, 1).contains("Name:         nginx"));
        assert_eq!(b[(2, 1)].fg, ctx.styles.yaml_key);
        assert_eq!(b[(16, 1)].fg, ctx.styles.yaml_value);

        v.on_filter("nginx|default", &mut ctx);
        assert_eq!(v.title(), " Describe(default/nginx) </nginx|default[1:2]> ");
        let b = render(&mut v, &ctx, 60, 8);
        assert_eq!(b[(16, 1)].bg, ctx.styles.yaml_value);
        assert_eq!(b[(16, 1)].fg, ctx.styles.bg);
        assert_eq!(b[(16, 2)].bg, ctx.styles.bg);
        v.handle_key(key(KeyCode::Char('n')), &mut ctx);
        assert_eq!(v.title(), " Describe(default/nginx) </nginx|default[2:2]> ");
        let b = render(&mut v, &ctx, 60, 8);
        assert_eq!(b[(16, 2)].bg, ctx.styles.yaml_value);
        v.handle_key(
            KeyEvent::new(KeyCode::Char('N'), KeyModifiers::SHIFT),
            &mut ctx,
        );
        assert_eq!(v.current, 0);
        let cmds = v.handle_key(key(KeyCode::Esc), &mut ctx);
        assert!(matches!(cmds[0], ViewCmd::Pop));
    }

    #[tokio::test]
    async fn yaml_hints_fullscreen_and_errors() {
        let (mut ctx, mut rx) = ctx_with(fake());
        let mut v = DetailsView::live(
            &mut ctx,
            YAML_ACTION,
            Gvr::parse("v1/pods"),
            "default/nginx".into(),
            true,
        );
        let names: Vec<String> = v.hints().iter().map(|h| h.description.clone()).collect();
        assert!(names.contains(&"Toggle ManagedFields".to_string()));
        assert!(names.contains(&"Toggle Auto-Refresh".to_string()));
        v.start(&mut ctx);
        let ev = rx.recv().await.unwrap();
        v.on_event(&ev, &mut ctx);
        let b = render(&mut v, &ctx, 50, 10);
        assert!(row(&b, 1).contains("apiVersion: v1"));
        v.handle_key(key(KeyCode::Char('f')), &mut ctx);
        assert!(v.is_fullscreen());
        let b = render(&mut v, &ctx, 50, 10);
        assert!(row(&b, 0).starts_with("apiVersion: v1"));
        let cmds = v.handle_key(key(KeyCode::Char('m')), &mut ctx);
        assert!(
            matches!(&cmds[0], ViewCmd::Flash(FlashLevel::Info, m) if m == "toggled managed fields")
        );
        v.on_event(
            &AppEvent::Error {
                view: v.id(),
                msg: "boom".into(),
            },
            &mut ctx,
        );
        let b = render(&mut v, &ctx, 50, 10);
        assert!(row(&b, 1).contains("Ruroh? boom"));
    }

    #[tokio::test]
    async fn export_text_matches_tview_get_text_and_sanitize_esc() {
        let (mut ctx, _rx) = ctx_with(fake());
        let mut v = DetailsView::live(
            &mut ctx,
            "Describe",
            Gvr::parse("v1/nodes"),
            "n1".into(),
            false,
        );
        v.set_text("Msg:  Added: [a/b c], Removed: [], Tainted: true\nk: [x] [y]\nz: []");
        // Live saves/copies: Escape then greedy sanitizeEsc (k9s quirks kept).
        assert_eq!(
            v.export_text(true),
            "Msg:  Added: [a/b c], Removed: ], Tainted: true\nk: [x[] [y]\nz: []"
        );
        assert_eq!(
            v.export_text(false),
            "Msg:  Added: [a/b c], Removed: [], Tainted: true\nk: [x[] [y[]\nz: []"
        );
        // colorizeYAML rewrites separators: key-only lines lose trailing blanks.
        v.set_text("Annotations:  a: \n              a/job-tracking: \n  k:\tv");
        assert_eq!(
            v.export_text(true),
            "Annotations:  a: \n              a/job-tracking:\n  k: v"
        );
        // Plain static text drops color tags instead of escaping.
        let v = DetailsView::fixed(
            &mut ctx,
            "t",
            "s".into(),
            false,
            true,
            "[red::b]hi[-::-] [x]",
        );
        assert_eq!(v.export_text(false), "hi [x]");
    }

    /// editRes: patch access first; denied flashes, allowed suspends to edit.
    #[tokio::test]
    async fn edit_checks_patch_access() {
        let f = fake();
        f.denied_patch
            .lock()
            .unwrap()
            .push(("v1/pods".into(), "default/nginx".into()));
        let (mut ctx, mut rx) = ctx_with(f);
        let mut v = DetailsView::live(
            &mut ctx,
            "YAML",
            Gvr::parse("v1/pods"),
            "default/nginx".into(),
            true,
        );
        assert!(v.handle_key(key(KeyCode::Char('e')), &mut ctx).is_empty());
        let cmds = loop {
            let ev = rx.recv().await.unwrap();
            if matches!(&ev, AppEvent::Custom { payload, .. } if payload.is::<EditAllowed>()) {
                break v.on_event(&ev, &mut ctx);
            }
        };
        assert!(
            matches!(&cmds[..], [ViewCmd::Flash(FlashLevel::Err, m)] if m == "current user can't edit resource v1/pods"),
            "{}",
            cmds.len()
        );
        let (mut ctx, mut rx) = ctx_with(fake());
        let mut v = DetailsView::live(
            &mut ctx,
            "YAML",
            Gvr::parse("v1/pods"),
            "default/nginx".into(),
            true,
        );
        v.handle_key(key(KeyCode::Char('e')), &mut ctx);
        let cmds = loop {
            let ev = rx.recv().await.unwrap();
            if matches!(&ev, AppEvent::Custom { payload, .. } if payload.is::<EditAllowed>()) {
                break v.on_event(&ev, &mut ctx);
            }
        };
        assert!(matches!(&cmds[..], [ViewCmd::Edit { .. }]));
    }

    #[tokio::test]
    async fn auto_refresh_toggle_and_save() {
        let dir = tempfile::tempdir().unwrap();
        let (mut ctx, mut rx) = ctx_with(fake());
        ctx.config.write().unwrap().k9s.screen_dump_dir = dir.path().display().to_string();
        ctx.cluster_info.cluster = "c1".into();
        ctx.cluster_info.context = "ctx:1".into();
        let mut v = DetailsView::live(
            &mut ctx,
            "Describe",
            Gvr::parse("v1/pods"),
            "default/nginx".into(),
            false,
        );
        let cmds = v.handle_key(key(KeyCode::Char('r')), &mut ctx);
        assert!(matches!(&cmds[0], ViewCmd::Flash(_, m) if m == "Auto-refresh is enabled"));
        let ev = rx.recv().await.unwrap();
        v.on_event(&ev, &mut ctx);
        let cmds = v.handle_key(key(KeyCode::Char('r')), &mut ctx);
        assert!(matches!(&cmds[0], ViewCmd::Flash(_, m) if m == "Auto-refresh is disabled"));
        let cmds = v.handle_key(
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
            &mut ctx,
        );
        assert!(
            matches!(&cmds[0], ViewCmd::Flash(FlashLevel::Info, m) if m == "File \"default-nginx--describe\" saved successfully!")
        );
        let sub = dir.path().join("c1").join("ctx-1");
        let files: Vec<_> = std::fs::read_dir(&sub).unwrap().collect();
        assert_eq!(files.len(), 1);
        let content = std::fs::read_to_string(files[0].as_ref().unwrap().path()).unwrap();
        assert!(content.starts_with("Name:         nginx"));
    }

    #[test]
    fn static_text_and_cow() {
        let (mut ctx, _rx) = ctx_with(fake());
        let mut v =
            DetailsView::fixed(&mut ctx, "Results", "bench".into(), false, false, "a: b\nc");
        assert_eq!(v.title(), " Results(bench) ");
        let b = render(&mut v, &ctx, 30, 5);
        assert_eq!(b[(2, 1)].fg, ctx.styles.fg);
        assert!(!v.hints().iter().any(|h| h.mnemonic == "n"));
        let mut c = DetailsView::cow(&mut ctx, "");
        let b = render(&mut c, &ctx, 80, 12);
        assert!(row(&b, 0).contains(" Error "));
        assert!(row(&b, 2).contains("Ruroh? Nothing to report here."));
    }

    /// secret.go/node.go open their details with contentYAML.
    #[test]
    fn decoder_and_drain_text_are_yaml() {
        let (mut ctx, _rx) = ctx_with(fake());
        for name in ["Secret Decoder", "Drain Progress"] {
            let mut v =
                crate::ui::views::text(&mut ctx, name.into(), "x".into(), "pass: hunter2".into());
            let b = render(v.as_mut(), &ctx, 40, 5);
            assert_eq!(b[(2, 1)].fg, ctx.styles.yaml_key, "{name}");
        }
    }

    #[test]
    fn secret_decoding() {
        let o = obj(
            serde_json::json!({"apiVersion":"v1","kind":"Secret","metadata":{"name":"s","namespace":"default"},"data":{"pwd":"aGVsbG8="}}),
        );
        let y = decode_secret_yaml(o.clone(), false).unwrap();
        assert!(y.contains("pwd: hello"));
        let d = decode_secret_description("Name: s\nData\n====\npwd: 5 bytes", &o).unwrap();
        assert_eq!(d, "Name: s\nData\n====\npwd: hello");
    }
}
