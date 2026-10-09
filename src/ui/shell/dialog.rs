//! Modal dialogs (k9s ui/dialog/*, view/pf_dialog.go, view/drain_dialog.go,
//! view/scale_extender.go) rendered like tview's ModalForm.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};

use super::cow::dialog_cow_talk;
use super::text::{draw_segs, fill, seg, width, word_wrap, Align};
use crate::config::Styles;
use crate::k8s::{fqn, DeleteOptions, DrainOptions, PortForwardSpec, Propagation};
use crate::ui::{Ctx, Dialog, FlashLevel, ViewCmd};

/// tview's ModalForm minimum text width.
const BUTTONS_WIDTH: u16 = 50;
const AQUA: Color = Color::LightCyan;
const ORANGE_RED: Color = Color::Rgb(0xFF, 0x45, 0x00);
const PROPAGATIONS: [&str; 4] = ["Background", "Foreground", "Orphan", "None"];

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Item {
    Input {
        label: String,
        value: String,
        width: u16,
        numeric: bool,
        placeholder: String,
    },
    Checkbox {
        label: String,
        checked: bool,
    },
    DropDown {
        label: String,
        options: Vec<String>,
        selected: usize,
        open: bool,
        cursor: usize,
    },
}

impl Item {
    fn input(label: &str, value: impl Into<String>, width: u16) -> Item {
        Item::Input {
            label: label.into(),
            value: value.into(),
            width,
            numeric: false,
            placeholder: String::new(),
        }
    }
    fn label(&self) -> &str {
        match self {
            Item::Input { label, .. }
            | Item::Checkbox { label, .. }
            | Item::DropDown { label, .. } => label,
        }
    }
    fn value(&self) -> String {
        match self {
            Item::Input { value, .. } => value.clone(),
            Item::Checkbox { checked, .. } => checked.to_string(),
            Item::DropDown {
                options, selected, ..
            } => options.get(*selected).cloned().unwrap_or_default(),
        }
    }
    fn checked(&self) -> bool {
        matches!(self, Item::Checkbox { checked: true, .. })
    }
}

/// A live dialog: the contract `Dialog` plus form state.
pub(crate) struct DialogState {
    pub title: String,
    pub text: String,
    text_color: Option<Color>,
    pub items: Vec<Item>,
    pub buttons: Vec<String>,
    /// Focus index over items followed by buttons.
    pub focus: usize,
    kind: Option<Dialog>,
    /// Drain options tracked from valid field edits.
    drain: DrainOptions,
    /// Flash message the app should show (empty text clears the flash).
    pub pending_flash: Option<(FlashLevel, String)>,
}

impl DialogState {
    pub fn new(d: Dialog, ctx: &Ctx) -> DialogState {
        let mut st = DialogState {
            title: String::new(),
            text: String::new(),
            text_color: None,
            items: Vec::new(),
            buttons: vec!["Cancel".into(), "OK".into()],
            focus: 0,
            kind: None,
            drain: DrainOptions::default(),
            pending_flash: None,
        };
        match &d {
            Dialog::Confirm { title, msg, .. } => {
                st.title = format!("<{title}>");
                st.text = msg.clone();
            }
            Dialog::Delete { msg, .. } => {
                st.title = "<Delete>".into();
                st.text = msg.clone();
                st.items = vec![
                    Item::DropDown {
                        label: "Propagation:".into(),
                        options: PROPAGATIONS.iter().map(|s| s.to_string()).collect(),
                        selected: 0,
                        open: false,
                        cursor: 0,
                    },
                    Item::Checkbox {
                        label: "Force:".into(),
                        checked: false,
                    },
                ];
                st.focus = 2;
            }
            Dialog::Error { msg } => {
                st.title = "<error>".into();
                st.text = dialog_cow_talk(msg);
                st.text_color = Some(ORANGE_RED);
                st.buttons = vec!["Dismiss".into()];
            }
            Dialog::Scale { msg, current, .. } => {
                st.title = "<Scale>".into();
                st.text = msg.clone();
                st.items = vec![Item::Input {
                    label: "Replicas:".into(),
                    value: current.to_string(),
                    width: 4,
                    numeric: true,
                    placeholder: String::new(),
                }];
                st.buttons = vec!["OK".into(), "Cancel".into()];
            }
            Dialog::PortForward { spec, ports, .. } => {
                st.title = "<PortForward>".into();
                let mut msg = fqn(&spec.namespace, &spec.pod);
                if ports.len() > 1 {
                    msg.push_str("\n\nExposed Ports:\n");
                    msg.push_str(&ports.join("\n"));
                }
                st.text = msg;
                let p1 = if let Some(p) = ports.first() {
                    p.clone()
                } else if spec.container_port > 0 {
                    format!("{}::{}", spec.container, spec.container_port)
                } else {
                    String::new()
                };
                let p2 = if spec.local_port > 0 {
                    spec.local_port.to_string()
                } else {
                    extract_port(&p1)
                };
                let field_len = 30.max(p1.len()) as u16;
                let address = if spec.address.is_empty() {
                    ctx.config
                        .read()
                        .map(|c| c.k9s.port_forward_address.clone())
                        .unwrap_or_else(|_| "localhost".into())
                } else {
                    spec.address.clone()
                };
                st.items = vec![
                    Item::Input {
                        label: "Container Port:".into(),
                        value: p1.clone(),
                        width: field_len,
                        numeric: false,
                        placeholder: "Enter a container name::port".into(),
                    },
                    Item::Input {
                        label: "Local Port:".into(),
                        value: p2,
                        width: field_len,
                        numeric: false,
                        placeholder: "Enter a local port".into(),
                    },
                    Item::input("Address:", address, field_len),
                ];
                st.buttons = vec!["OK".into(), "Cancel".into()];
            }
            Dialog::Drain { node, .. } => {
                st.title = "<Drain>".into();
                st.text = format!("Drain {node}?");
                let o = DrainOptions::default();
                st.items = vec![
                    Item::input("GracePeriod:", o.grace_period_seconds.to_string(), 0),
                    Item::input("Timeout:", go_duration(o.timeout_secs), 0),
                    Item::Checkbox {
                        label: "Ignore DaemonSets:".into(),
                        checked: o.ignore_all_daemon_sets,
                    },
                    Item::Checkbox {
                        label: "Delete EmptyDir Data:".into(),
                        checked: o.delete_empty_dir_data,
                    },
                    Item::Checkbox {
                        label: "Force:".into(),
                        checked: o.force,
                    },
                    Item::Checkbox {
                        label: "Disable Eviction:".into(),
                        checked: o.disable_eviction,
                    },
                ];
                st.drain = o;
            }
            Dialog::Prompt {
                title,
                label,
                value,
                ..
            } => {
                st.title = format!("<{title}>");
                st.items = vec![Item::input(label, value.clone(), 40)];
                st.buttons = vec!["OK".into(), "Cancel".into()];
            }
        }
        st.kind = Some(d);
        st
    }

    fn slots(&self) -> usize {
        self.items.len() + self.buttons.len()
    }

    fn focused_button(&self) -> Option<&str> {
        self.focus
            .checked_sub(self.items.len())
            .and_then(|i| self.buttons.get(i))
            .map(|s| s.as_str())
    }

    fn next(&mut self) {
        self.focus = (self.focus + 1) % self.slots();
    }

    fn prev(&mut self) {
        self.focus = (self.focus + self.slots() - 1) % self.slots();
    }

    /// Handle a key. `None` keeps the dialog open; `Some(cmds)` closes it.
    pub fn handle_key(&mut self, key: KeyEvent, ctx: &mut Ctx) -> Option<Vec<ViewCmd>> {
        let n_items = self.items.len();
        // Open drop down list captures keys.
        if let Some(Item::DropDown {
            open: true,
            cursor,
            selected,
            options,
            ..
        }) = self.items.get_mut(self.focus)
        {
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => *cursor = cursor.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    *cursor = (*cursor + 1).min(options.len().saturating_sub(1))
                }
                KeyCode::Enter | KeyCode::Char(' ') => {
                    *selected = *cursor;
                    self.close_dropdown();
                }
                KeyCode::Esc | KeyCode::Tab => self.close_dropdown(),
                _ => {}
            }
            return None;
        }
        match key.code {
            KeyCode::Esc => return Some(Vec::new()),
            KeyCode::Tab => {
                self.next();
                return None;
            }
            KeyCode::BackTab => {
                self.prev();
                return None;
            }
            _ => {}
        }
        if self.focus < n_items {
            return self.item_key(key, ctx);
        }
        match key.code {
            KeyCode::Right | KeyCode::Down => self.next(),
            KeyCode::Left | KeyCode::Up => self.prev(),
            KeyCode::Enter | KeyCode::Char(' ') => {
                let b = self.focused_button().unwrap_or_default().to_string();
                return self.press(&b, ctx);
            }
            _ => {}
        }
        None
    }

    fn close_dropdown(&mut self) {
        if let Some(Item::DropDown { open, .. }) = self.items.get_mut(self.focus) {
            *open = false;
        }
    }

    fn item_key(&mut self, key: KeyEvent, ctx: &mut Ctx) -> Option<Vec<ViewCmd>> {
        let idx = self.focus;
        let mut changed = false;
        match &mut self.items[idx] {
            Item::Input { value, numeric, .. } => match key.code {
                KeyCode::Enter | KeyCode::Down => self.next(),
                KeyCode::Up => self.prev(),
                KeyCode::Backspace | KeyCode::Delete => {
                    value.pop();
                    changed = true;
                }
                KeyCode::Char('u') | KeyCode::Char('w')
                    if key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    value.clear();
                    changed = true;
                }
                KeyCode::Char(c)
                    if !key.modifiers.contains(KeyModifiers::CONTROL) && !c.is_control() =>
                {
                    let mut v = value.clone();
                    v.push(c);
                    if !*numeric || v.parse::<i64>().is_ok() {
                        *value = v;
                        changed = true;
                    }
                }
                _ => {}
            },
            Item::Checkbox { checked, .. } => match key.code {
                KeyCode::Enter | KeyCode::Char(' ') => *checked = !*checked,
                KeyCode::Down => self.next(),
                KeyCode::Up => self.prev(),
                _ => {}
            },
            Item::DropDown {
                open,
                cursor,
                selected,
                ..
            } => match key.code {
                KeyCode::Enter | KeyCode::Char(' ') => {
                    *open = true;
                    *cursor = *selected;
                }
                KeyCode::Down => self.next(),
                KeyCode::Up => self.prev(),
                _ => {}
            },
        }
        if changed {
            return self.on_change(idx, ctx);
        }
        None
    }

    /// Field change hooks (pf local port follow, drain live validation).
    fn on_change(&mut self, idx: usize, _ctx: &mut Ctx) -> Option<Vec<ViewCmd>> {
        match self.kind {
            Some(Dialog::PortForward { .. }) if idx == 0 => {
                let p = extract_port(&self.items[0].value());
                if let Item::Input { value, .. } = &mut self.items[1] {
                    *value = p;
                }
                None
            }
            Some(Dialog::Drain { .. }) if idx <= 1 => {
                let v = self.items[idx].value();
                let res = if idx == 0 {
                    v.parse::<i64>()
                        .map(|n| self.drain.grace_period_seconds = n)
                        .map_err(|_| {
                            format!(
                                "strconv.Atoi: parsing {}: invalid syntax",
                                super::text::go_quote(&v)
                            )
                        })
                } else {
                    parse_go_duration(&v)
                        .map(|n| self.drain.timeout_secs = n)
                        .map_err(|_| {
                            format!("time: invalid duration {}", super::text::go_quote(&v))
                        })
                };
                self.pending_flash = Some(match res {
                    Ok(()) => (FlashLevel::Info, String::new()),
                    Err(e) => (FlashLevel::Err, e),
                });
                None
            }
            _ => None,
        }
    }

    fn press(&mut self, button: &str, ctx: &mut Ctx) -> Option<Vec<ViewCmd>> {
        match button {
            "Cancel" | "Dismiss" => Some(Vec::new()),
            "OK" => self.ok(ctx),
            _ => None,
        }
    }

    fn ok(&mut self, ctx: &mut Ctx) -> Option<Vec<ViewCmd>> {
        let kind = self.kind.take()?;
        match kind {
            Dialog::Confirm { on_ok, .. } => Some(on_ok(ctx)),
            Dialog::Delete { on_ok, .. } => {
                let prop = match self.items[0].value().as_str() {
                    "Background" => Some(Propagation::Background),
                    "Foreground" => Some(Propagation::Foreground),
                    "Orphan" => Some(Propagation::Orphan),
                    _ => None,
                };
                let force = self.items[1].checked();
                let opts = DeleteOptions {
                    propagation: prop,
                    grace: force.then_some(0),
                    force,
                };
                Some(on_ok(ctx, opts))
            }
            Dialog::Error { .. } => Some(Vec::new()),
            Dialog::Scale { on_ok, .. } => {
                let v = self.items[0].value();
                match v.parse::<i32>() {
                    Ok(n) => Some(on_ok(ctx, n)),
                    Err(_) => Some(vec![ViewCmd::Flash(
                        FlashLevel::Err,
                        format!(
                            "strconv.Atoi: parsing {}: invalid syntax",
                            super::text::go_quote(&v)
                        ),
                    )]),
                }
            }
            Dialog::PortForward { spec, ports, on_ok } => {
                let co = self.items[0].value();
                let lo = self.items[1].value();
                let address = self.items[2].value();
                if co.is_empty() || lo.is_empty() {
                    self.kind = Some(Dialog::PortForward { spec, ports, on_ok });
                    self.pending_flash =
                        Some((FlashLevel::Err, "container to local port mismatch".into()));
                    return None;
                }
                match to_tunnel(&address, &co, &lo, &spec) {
                    Ok(s) => Some(on_ok(ctx, s)),
                    Err(e) => {
                        self.kind = Some(Dialog::PortForward { spec, ports, on_ok });
                        self.pending_flash = Some((FlashLevel::Err, e));
                        None
                    }
                }
            }
            Dialog::Drain { on_ok, .. } => {
                let mut o = self.drain.clone();
                o.ignore_all_daemon_sets = self.items[2].checked();
                o.delete_empty_dir_data = self.items[3].checked();
                o.force = self.items[4].checked();
                o.disable_eviction = self.items[5].checked();
                Some(on_ok(ctx, o))
            }
            Dialog::Prompt { on_ok, .. } => {
                let v = self.items[0].value();
                Some(on_ok(ctx, v))
            }
        }
    }

    /// Draw the modal centered on `screen`; returns the input cursor if any.
    pub fn draw(&self, buf: &mut Buffer, screen: Rect, styles: &Styles) -> Option<Position> {
        let mut w = (screen.width / 3).max(BUTTONS_WIDTH);
        let lines = word_wrap(&self.text, w.saturating_sub(2) as usize);
        let has_text = !self.text.is_empty();
        let text_rows = if has_text { lines.len() as u16 + 1 } else { 0 };
        let items = self.items.len() as u16;
        let mut h = 2 + 2 + text_rows + items + if items > 0 { 1 } else { 0 } + 1;
        w += 2;
        w = w.min(screen.width);
        h = h.min(screen.height);
        let x = screen.x + (screen.width - w) / 2;
        let y = screen.y + (screen.height - h) / 2;
        let area = Rect::new(x, y, w, h);
        let body = Style::default().fg(styles.dialog_fg).bg(styles.dialog_bg);
        fill(buf, area, body);
        let bs = Style::default().fg(styles.border).bg(styles.dialog_bg);
        if w < 4 || h < 3 {
            return None;
        }
        let (l, r, t, b) = (area.left(), area.right() - 1, area.top(), area.bottom() - 1);
        let mut put = |x: u16, y: u16, s: &str| {
            if let Some(c) = buf.cell_mut((x, y)) {
                c.set_symbol(s);
                c.set_style(bs);
            }
        };
        for xx in l + 1..r {
            put(xx, t, "─");
            put(xx, b, "─");
        }
        for yy in t + 1..b {
            put(l, yy, "│");
            put(r, yy, "│");
        }
        put(l, t, "┌");
        put(r, t, "┐");
        put(l, b, "└");
        put(r, b, "┘");
        draw_segs(
            buf,
            l + 1,
            t,
            w - 2,
            &[seg(
                self.title.clone(),
                Style::default().fg(AQUA).bg(styles.dialog_bg),
            )],
            Align::Center,
            false,
        );
        let ix = l + 2;
        let iw = w.saturating_sub(4);
        let mut yy = t + 2;
        let text_style = Style::default()
            .fg(self.text_color.unwrap_or(styles.dialog_fg))
            .bg(styles.dialog_bg);
        if has_text {
            for line in &lines {
                if yy >= b {
                    break;
                }
                draw_segs(
                    buf,
                    ix,
                    yy,
                    iw,
                    &[seg(line.clone(), text_style)],
                    Align::Center,
                    false,
                );
                yy += 1;
            }
            yy += 1;
        }
        let label_w = self
            .items
            .iter()
            .map(|i| width(i.label()))
            .max()
            .unwrap_or(0) as u16;
        let mut cursor = None;
        let mut dropdown: Option<(u16, u16, &Item)> = None;
        for (i, item) in self.items.iter().enumerate() {
            if yy >= b {
                break;
            }
            let focused = self.focus == i;
            let label = Style::default()
                .fg(styles.dialog_label_fg)
                .bg(styles.dialog_bg);
            draw_segs(
                buf,
                ix,
                yy,
                iw,
                &[seg(item.label(), label)],
                Align::Left,
                false,
            );
            let fx = ix + label_w + 1;
            let fw = (ix + iw).saturating_sub(fx);
            let field = Style::default()
                .fg(styles.dialog_field_fg)
                .bg(styles.dialog_bg);
            let inverted = Style::default()
                .fg(styles.dialog_bg)
                .bg(styles.dialog_field_fg);
            match item {
                Item::Input {
                    value,
                    width: fwid,
                    placeholder,
                    ..
                } => {
                    let fw = if *fwid == 0 { fw } else { (*fwid).min(fw) };
                    if value.is_empty() && !placeholder.is_empty() {
                        draw_segs(
                            buf,
                            fx,
                            yy,
                            fw,
                            &[seg(placeholder.clone(), field.add_modifier(Modifier::DIM))],
                            Align::Left,
                            false,
                        );
                    } else {
                        draw_segs(
                            buf,
                            fx,
                            yy,
                            fw,
                            &[seg(value.clone(), field)],
                            Align::Left,
                            false,
                        );
                    }
                    if focused {
                        let cx =
                            (fx as usize + width(value)).min((fx + fw.saturating_sub(1)) as usize);
                        cursor = Some(Position::new(cx as u16, yy));
                    }
                }
                Item::Checkbox { checked, .. } => {
                    let mark = if *checked { "X" } else { " " };
                    let st = if focused { inverted } else { field };
                    draw_segs(buf, fx, yy, fw, &[seg(mark, st)], Align::Left, false);
                }
                Item::DropDown {
                    options,
                    selected,
                    open,
                    ..
                } => {
                    let v = options.get(*selected).cloned().unwrap_or_default();
                    let st = if focused && !open { inverted } else { field };
                    draw_segs(buf, fx, yy, fw, &[seg(v, st)], Align::Left, false);
                    if *open {
                        dropdown = Some((fx, yy + 1, item));
                    }
                }
            }
            yy += 1;
        }
        // Buttons row.
        let by = b.saturating_sub(2);
        let total: u16 = self
            .buttons
            .iter()
            .map(|s| width(s) as u16 + 4)
            .sum::<u16>()
            + 2 * self.buttons.len().saturating_sub(1) as u16;
        let mut bx = ix + iw.saturating_sub(total) / 2;
        for (i, label) in self.buttons.iter().enumerate() {
            let focused = self.focus == self.items.len() + i;
            let st = if focused {
                Style::default()
                    .fg(styles.dialog_button_focus_fg)
                    .bg(styles.dialog_button_focus_bg)
            } else {
                Style::default()
                    .fg(styles.dialog_button_fg)
                    .bg(styles.dialog_button_bg)
            };
            let bw = width(label) as u16 + 4;
            draw_segs(
                buf,
                bx,
                by,
                bw,
                &[seg(format!("  {label}  "), st)],
                Align::Left,
                false,
            );
            bx += bw + 2;
        }
        if let Some((
            dx,
            dy,
            Item::DropDown {
                options, cursor: c, ..
            },
        )) = dropdown
        {
            let ow = options.iter().map(|o| width(o)).max().unwrap_or(0) as u16 + 1;
            for (i, o) in options.iter().enumerate() {
                let y = dy + i as u16;
                if y >= screen.bottom() {
                    break;
                }
                let st = if i == *c {
                    Style::default()
                        .fg(styles.dialog_button_focus_fg)
                        .bg(styles.dialog_button_focus_bg)
                } else {
                    Style::default().fg(styles.dialog_fg).bg(styles.dialog_bg)
                };
                fill(buf, Rect::new(dx, y, ow, 1), st);
                draw_segs(buf, dx, y, ow, &[seg(o.clone(), st)], Align::Left, false);
            }
        }
        cursor
    }
}

/// pf_dialog.go extractPort.
pub(crate) fn extract_port(p: &str) -> String {
    let tokens: Vec<&str> = p.split("::").collect();
    if tokens.len() < 2 {
        for t in p.split(',') {
            if t.trim().parse::<i64>().is_err() {
                return String::new();
            }
        }
        return p.to_string();
    }
    tokens[1].to_string()
}

/// port.ToTunnels for a single tunnel.
fn to_tunnel(
    address: &str,
    co: &str,
    lo: &str,
    spec: &PortForwardSpec,
) -> Result<PortForwardSpec, String> {
    let pp: Vec<&str> = co.split(',').collect();
    let lps: Vec<&str> = lo.split(',').collect();
    if pp.len() != lps.len() {
        return Err(format!(
            "spec to local port count mismatch. Expected {} but got {}",
            pp.len(),
            lps.len()
        ));
    }
    let p = pp[0].trim();
    let (container, port) = match p.split_once("::") {
        Some((c, port)) => (c.to_string(), port),
        None => (spec.container.clone(), p),
    };
    let port = port.split('/').next().unwrap_or(port);
    let cport: u16 = port
        .parse()
        .map_err(|_| format!("invalid port-forward specification {p}"))?;
    let lport: u16 = lps[0]
        .trim()
        .parse()
        .map_err(|_| format!("invalid local port {}", lps[0]))?;
    Ok(PortForwardSpec {
        namespace: spec.namespace.clone(),
        pod: spec.pod.clone(),
        container,
        address: address.to_string(),
        local_port: lport,
        container_port: cport,
    })
}

/// Go time.Duration.String for whole seconds.
pub(crate) fn go_duration(secs: i64) -> String {
    if secs == 0 {
        return "0s".into();
    }
    let neg = secs < 0;
    let s = secs.unsigned_abs();
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    if h > 0 {
        out.push_str(&format!("{h}h{m}m{sec}s"));
    } else if m > 0 {
        out.push_str(&format!("{m}m{sec}s"));
    } else {
        out.push_str(&format!("{sec}s"));
    }
    out
}

/// Parse a Go duration (`5m`, `1h30m`, `90s`, `1.5h`) into whole seconds.
pub(crate) fn parse_go_duration(s: &str) -> Result<i64, ()> {
    let s = s.trim();
    if s.is_empty() {
        return Err(());
    }
    if s == "0" {
        return Ok(0);
    }
    let (neg, mut rest) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let mut total = 0f64;
    while !rest.is_empty() {
        let n_end = rest
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .ok_or(())?;
        if n_end == 0 {
            return Err(());
        }
        let n: f64 = rest[..n_end].parse().map_err(|_| ())?;
        rest = &rest[n_end..];
        let u_end = rest
            .find(|c: char| c.is_ascii_digit() || c == '.')
            .unwrap_or(rest.len());
        let mult = match &rest[..u_end] {
            "ns" => 1e-9,
            "us" | "µs" => 1e-6,
            "ms" => 1e-3,
            "s" => 1.0,
            "m" => 60.0,
            "h" => 3600.0,
            _ => return Err(()),
        };
        total += n * mult;
        rest = &rest[u_end..];
    }
    let secs = total as i64;
    Ok(if neg { -secs } else { secs })
}

#[cfg(test)]
mod tests {
    use super::super::text::testutil::*;
    use super::*;
    use std::sync::{Arc, Mutex};

    fn ctx() -> Ctx {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let acc: crate::k8s::SharedAccessor = Arc::new(crate::k8s::fake::FakeAccessor::new());
        Ctx::new(
            acc,
            Default::default(),
            Styles::default(),
            "default".into(),
            tx,
        )
    }

    fn k(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    #[test]
    fn confirm_ok_and_cancel() {
        let mut c = ctx();
        let hit = Arc::new(Mutex::new(false));
        let h2 = hit.clone();
        let mut d = DialogState::new(
            Dialog::Confirm {
                title: "Confirm Delete".into(),
                msg: "Delete it?".into(),
                on_ok: Box::new(move |_| {
                    *h2.lock().unwrap() = true;
                    vec![ViewCmd::Flash(FlashLevel::Info, "done".into())]
                }),
            },
            &c,
        );
        assert_eq!(d.title, "<Confirm Delete>");
        assert_eq!(d.focus, 0, "Cancel focused first");
        let st = Styles::default();
        let mut buf = Buffer::empty(Rect::new(0, 0, 100, 30));
        {
            let a = buf.area;
            d.draw(&mut buf, a, &st)
        };
        let (x, y) = find(&buf, "<Confirm Delete>").unwrap();
        assert_eq!(fg(&buf, x, y), AQUA);
        assert!(find(&buf, "Delete it?").is_some());
        let (x, y) = find(&buf, "Cancel").unwrap();
        assert_eq!(bg(&buf, x, y), st.dialog_button_focus_bg);
        let (x, y) = find(&buf, "OK").unwrap();
        assert_eq!(bg(&buf, x, y), st.dialog_button_bg);
        // Box is centered: 52 wide.
        let (bx, _) = find(&buf, "┌").unwrap();
        assert_eq!(bx, 24);

        assert!(d.handle_key(k(KeyCode::Right), &mut c).is_none());
        let out = d.handle_key(k(KeyCode::Enter), &mut c).unwrap();
        assert!(*hit.lock().unwrap());
        assert!(matches!(
            out.as_slice(),
            [ViewCmd::Flash(FlashLevel::Info, _)]
        ));

        let mut d = DialogState::new(
            Dialog::Confirm {
                title: "t".into(),
                msg: "m".into(),
                on_ok: Box::new(|_| panic!("not accepted")),
            },
            &c,
        );
        assert!(d.handle_key(k(KeyCode::Enter), &mut c).unwrap().is_empty());
        let mut d = DialogState::new(
            Dialog::Confirm {
                title: "t".into(),
                msg: "m".into(),
                on_ok: Box::new(|_| panic!("not accepted")),
            },
            &c,
        );
        assert!(d.handle_key(k(KeyCode::Esc), &mut c).unwrap().is_empty());
    }

    #[test]
    fn delete_with_propagation_and_force() {
        let mut c = ctx();
        let got = Arc::new(Mutex::new(None));
        let g2 = got.clone();
        let mut d = DialogState::new(
            Dialog::Delete {
                msg: "Delete pod default/nginx?".into(),
                on_ok: Box::new(move |_, o| {
                    *g2.lock().unwrap() = Some(o);
                    vec![]
                }),
            },
            &c,
        );
        let st = Styles::default();
        let mut buf = Buffer::empty(Rect::new(0, 0, 100, 30));
        {
            let a = buf.area;
            d.draw(&mut buf, a, &st)
        };
        let (x, y) = find(&buf, "Propagation:").unwrap();
        assert_eq!(fg(&buf, x, y), st.dialog_label_fg);
        assert!(row(&buf, y).contains("Propagation: Background"));
        assert!(row(&buf, y + 1).contains("Force:"));
        assert_eq!(d.focus, 2);
        // Back to the drop down, pick Orphan.
        d.handle_key(k(KeyCode::BackTab), &mut c);
        d.handle_key(k(KeyCode::BackTab), &mut c);
        assert_eq!(d.focus, 0);
        d.handle_key(k(KeyCode::Enter), &mut c);
        let mut buf = Buffer::empty(Rect::new(0, 0, 100, 30));
        {
            let a = buf.area;
            d.draw(&mut buf, a, &st)
        };
        assert!(find(&buf, "Foreground").is_some(), "list open");
        d.handle_key(k(KeyCode::Down), &mut c);
        d.handle_key(k(KeyCode::Down), &mut c);
        d.handle_key(k(KeyCode::Enter), &mut c);
        assert_eq!(d.items[0].value(), "Orphan");
        d.handle_key(k(KeyCode::Tab), &mut c);
        d.handle_key(k(KeyCode::Char(' ')), &mut c);
        assert!(d.items[1].checked());
        d.handle_key(k(KeyCode::Tab), &mut c);
        d.handle_key(k(KeyCode::Tab), &mut c);
        assert_eq!(d.focused_button(), Some("OK"));
        assert!(d.handle_key(k(KeyCode::Enter), &mut c).is_some());
        assert_eq!(
            *got.lock().unwrap(),
            Some(DeleteOptions {
                propagation: Some(Propagation::Orphan),
                grace: Some(0),
                force: true
            })
        );
    }

    #[test]
    fn error_dialog_has_cow() {
        let mut c = ctx();
        let mut d = DialogState::new(
            Dialog::Error {
                msg: "`bozo` command not found".into(),
            },
            &c,
        );
        let st = Styles::default();
        let mut buf = Buffer::empty(Rect::new(0, 0, 100, 30));
        {
            let a = buf.area;
            d.draw(&mut buf, a, &st)
        };
        assert!(find(&buf, "<error>").is_some());
        let (x, y) = find(&buf, "< Ruroh? `bozo` command not found >").unwrap();
        assert_eq!(fg(&buf, x, y), ORANGE_RED);
        assert!(find(&buf, "(oo)").is_some());
        assert!(find(&buf, "Dismiss").is_some());
        assert!(d.handle_key(k(KeyCode::Enter), &mut c).unwrap().is_empty());
    }

    #[test]
    fn scale_numeric_input() {
        let mut c = ctx();
        let got = Arc::new(Mutex::new(0));
        let g2 = got.clone();
        let mut d = DialogState::new(
            Dialog::Scale {
                msg: "Scale deployment default/nginx?".into(),
                current: 3,
                on_ok: Box::new(move |_, n| {
                    *g2.lock().unwrap() = n;
                    vec![]
                }),
            },
            &c,
        );
        d.handle_key(k(KeyCode::Char('x')), &mut c);
        assert_eq!(d.items[0].value(), "3");
        d.handle_key(k(KeyCode::Backspace), &mut c);
        d.handle_key(k(KeyCode::Char('1')), &mut c);
        d.handle_key(k(KeyCode::Char('2')), &mut c);
        let st = Styles::default();
        let mut buf = Buffer::empty(Rect::new(0, 0, 100, 30));
        let cur = {
            let a = buf.area;
            d.draw(&mut buf, a, &st)
        };
        assert!(cur.is_some());
        assert!(find(&buf, "Replicas: 12").is_some());
        assert!(find(&buf, "Scale deployment default/nginx?").is_some());
        d.handle_key(k(KeyCode::Enter), &mut c);
        assert_eq!(d.focused_button(), Some("OK"));
        assert!(d.handle_key(k(KeyCode::Enter), &mut c).is_some());
        assert_eq!(*got.lock().unwrap(), 12);
    }

    #[test]
    fn port_forward_form() {
        let mut c = ctx();
        c.config.write().unwrap().k9s.port_forward_address = "localhost".into();
        let got = Arc::new(Mutex::new(None));
        let g2 = got.clone();
        let mut d = DialogState::new(
            Dialog::PortForward {
                spec: PortForwardSpec {
                    namespace: "default".into(),
                    pod: "nginx".into(),
                    container: "nginx".into(),
                    ..Default::default()
                },
                ports: vec!["nginx::80".into(), "sidecar::9090".into()],
                on_ok: Box::new(move |_, s| {
                    *g2.lock().unwrap() = Some(s);
                    vec![]
                }),
            },
            &c,
        );
        assert!(d
            .text
            .starts_with("default/nginx\n\nExposed Ports:\nnginx::80"));
        assert_eq!(d.items[1].value(), "80");
        assert_eq!(d.items[2].value(), "localhost");
        // Edit container port -> local port follows.
        d.handle_key(k(KeyCode::Backspace), &mut c);
        d.handle_key(k(KeyCode::Backspace), &mut c);
        d.handle_key(k(KeyCode::Char('9')), &mut c);
        d.handle_key(k(KeyCode::Char('1')), &mut c);
        assert_eq!(d.items[1].value(), "91");
        d.focus = 3;
        assert!(d.handle_key(k(KeyCode::Enter), &mut c).is_some());
        let s = got.lock().unwrap().clone().unwrap();
        assert_eq!(
            (s.container.as_str(), s.container_port, s.local_port),
            ("nginx", 91, 91)
        );
        assert_eq!(s.address, "localhost");

        // Empty ports flash an error and stay open.
        let mut d = DialogState::new(
            Dialog::PortForward {
                spec: PortForwardSpec::default(),
                ports: vec![],
                on_ok: Box::new(|_, _| panic!("no")),
            },
            &c,
        );
        d.focus = 3;
        assert!(d.handle_key(k(KeyCode::Enter), &mut c).is_none());
        assert_eq!(
            d.pending_flash.take(),
            Some((FlashLevel::Err, "container to local port mismatch".into()))
        );
    }

    #[test]
    fn drain_form() {
        let mut c = ctx();
        let got = Arc::new(Mutex::new(None));
        let g2 = got.clone();
        let mut d = DialogState::new(
            Dialog::Drain {
                node: "n1".into(),
                on_ok: Box::new(move |_, o| {
                    *g2.lock().unwrap() = Some(o);
                    vec![]
                }),
            },
            &c,
        );
        assert_eq!(d.text, "Drain n1?");
        assert_eq!(d.items[1].value(), "5m0s");
        d.handle_key(k(KeyCode::Backspace), &mut c);
        d.handle_key(k(KeyCode::Backspace), &mut c);
        d.handle_key(k(KeyCode::Char('3')), &mut c);
        assert_eq!(
            d.pending_flash.take(),
            Some((FlashLevel::Info, String::new()))
        );
        d.handle_key(k(KeyCode::Tab), &mut c);
        d.handle_key(k(KeyCode::Char('x')), &mut c);
        assert!(matches!(d.pending_flash.take(), Some((FlashLevel::Err, _))));
        for _ in 0..5 {
            d.handle_key(k(KeyCode::Backspace), &mut c);
        }
        d.handle_key(k(KeyCode::Char('2')), &mut c);
        d.handle_key(k(KeyCode::Char('m')), &mut c);
        d.focus = 4;
        d.handle_key(k(KeyCode::Char(' ')), &mut c);
        d.focus = 7;
        assert_eq!(d.focused_button(), Some("OK"));
        d.handle_key(k(KeyCode::Enter), &mut c);
        let o = got.lock().unwrap().clone().unwrap();
        assert_eq!(o.grace_period_seconds, 3);
        assert_eq!(o.timeout_secs, 120);
        assert!(o.force);
        assert!(o.ignore_all_daemon_sets);
    }

    #[test]
    fn prompt_dialog() {
        let mut c = ctx();
        let got = Arc::new(Mutex::new(String::new()));
        let g2 = got.clone();
        let mut d = DialogState::new(
            Dialog::Prompt {
                title: "Set Image".into(),
                label: "nginx:".into(),
                value: "nginx:1.0".into(),
                on_ok: Box::new(move |_, v| {
                    *g2.lock().unwrap() = v;
                    vec![]
                }),
            },
            &c,
        );
        d.handle_key(k(KeyCode::Char('1')), &mut c);
        d.handle_key(k(KeyCode::Tab), &mut c);
        d.handle_key(k(KeyCode::Enter), &mut c);
        assert_eq!(*got.lock().unwrap(), "nginx:1.01");
    }

    #[test]
    fn helpers() {
        assert_eq!(extract_port("nginx::80"), "80");
        assert_eq!(extract_port("80,81"), "80,81");
        assert_eq!(extract_port("abc"), "");
        assert_eq!(go_duration(300), "5m0s");
        assert_eq!(go_duration(3661), "1h1m1s");
        assert_eq!(go_duration(-1), "-1s");
        assert_eq!(parse_go_duration("1h30m"), Ok(5400));
        assert_eq!(parse_go_duration("90s"), Ok(90));
        assert!(parse_go_duration("bogus").is_err());
        assert!(parse_go_duration("5").is_err());
    }
}
