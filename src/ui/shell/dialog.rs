//! Modal dialogs (k9s ui/dialog/*, view/pf_dialog.go, view/drain_dialog.go,
//! view/scale_extender.go) rendered like tview's ModalForm.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};

use super::cow::dialog_cow_talk;
use super::text::{draw_segs, fill, seg, width, word_wrap_tagged, Align};
use crate::config::Styles;
use crate::k8s::{fqn, DeleteOptions, DrainOptions, PortForwardSpec, Propagation};
use crate::ui::{Ctx, Dialog, FlashLevel, ViewCmd};

const AQUA: Color = Color::LightCyan;
const ORANGE_RED: Color = Color::Indexed(202);
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
    /// Last draw geometry for mouse hits.
    hits: std::cell::RefCell<Hits>,
    /// tview focus left the modal (a click elsewhere): nothing is drawn
    /// focused and no input cursor shows.
    pub blurred: bool,
    /// ui.ModalList (dialog.ShowSelection): list items and the current one.
    list: Option<(Vec<String>, usize)>,
}

/// The modal box, each focusable element (focus index -> rect) and an open
/// drop down's options.
#[derive(Default)]
struct Hits {
    area: Rect,
    elems: Vec<(usize, Rect)>,
    options: Vec<(usize, Rect)>,
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
            hits: Default::default(),
            blurred: false,
            list: None,
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
            Dialog::ConfirmAck { title, msg, .. } => {
                st.title = format!("<{title}>");
                st.text = msg.clone();
                st.text_color = Some(ctx.styles.dialog_fg);
                st.items = vec![Item::input("Confirm:", "", 30)];
            }
            Dialog::Launching { title, msg, .. } => {
                st.title = format!("<{title}>");
                st.text = msg.clone();
                st.text_color = Some(ctx.styles.dialog_fg);
                st.buttons = vec!["Cancel".into()];
            }
            Dialog::Transfer {
                pod,
                containers,
                retries,
                ..
            } => {
                st.title = "<Transfer>".into();
                st.text = "Download Files".into();
                if containers.len() > 1 {
                    st.text
                        .push_str(&format!("\nAvailable Containers:{}", containers.join(",")));
                }
                st.text_color = Some(ctx.styles.dialog_fg);
                st.items = vec![
                    Item::Checkbox {
                        label: "Download:".into(),
                        checked: true,
                    },
                    Item::input("From:", pod.clone(), 40),
                    Item::input("To:", "", 40),
                    Item::Checkbox {
                        label: "NoPreserve:".into(),
                        checked: false,
                    },
                    Item::input(
                        "Container:",
                        containers.first().cloned().unwrap_or_default(),
                        30,
                    ),
                    Item::input("Retries:", retries.to_string(), 30),
                ];
            }
            Dialog::Restart {
                title,
                msg,
                field_manager,
                ..
            } => {
                st.title = format!("<{title}>");
                st.text = msg.clone();
                st.items = vec![Item::input("FieldManager:", field_manager.clone(), 40)];
                // f.SetFocus(1): the Cancel button.
                st.focus = 1;
            }
            Dialog::SetImage {
                msg, containers, ..
            } => {
                st.title = "<Set image>".into();
                st.text = msg.clone();
                // ModalForm text color is never set: tview.Styles.PrimaryTextColor.
                st.text_color = Some(ctx.styles.fg);
                st.items = containers
                    .iter()
                    .map(|(name, image, _)| Item::input(name, image.clone(), 0))
                    .collect();
                st.buttons = vec!["OK".into(), "Cancel".into()];
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
            Dialog::PortForward {
                spec,
                ports,
                prefill,
                ..
            } => {
                st.title = "<PortForward>".into();
                let mut msg = fqn(&spec.namespace, &spec.pod);
                if ports.len() > 1 {
                    msg.push_str("\n\nExposed Ports:\n");
                    msg.push_str(&ports.join("\n"));
                }
                st.text = msg;
                let (p1, p2) = prefill.clone();
                let field_len = 30.max(p1.len()) as u16;
                let address = if spec.address.is_empty() {
                    ctx.config
                        .read()
                        .map(|c| c.k9s.port_forward_address.clone())
                        .unwrap_or_else(|_| "localhost".into())
                } else {
                    spec.address.clone()
                };
                // pf_dialog.go only sets placeholders on fields that start empty.
                let hint = |v: &str, h: &str| {
                    if v.is_empty() {
                        h.into()
                    } else {
                        String::new()
                    }
                };
                st.items = vec![
                    Item::Input {
                        label: "Container Port:".into(),
                        placeholder: hint(&p1, "Enter a container name::port"),
                        value: p1.clone(),
                        width: field_len,
                        numeric: false,
                    },
                    Item::Input {
                        label: "Local Port:".into(),
                        placeholder: hint(&p2, "Enter a local port"),
                        value: p2,
                        width: field_len,
                        numeric: false,
                    },
                    Item::input("Address:", address, field_len),
                ];
                st.buttons = vec!["OK".into(), "Cancel".into()];
            }
            Dialog::Drain { node, .. } => {
                st.title = "<Drain>".into();
                st.text = format!("Drain {node}?");
                // view/node.go drainCmd: only GracePeriod and Timeout are set.
                let o = DrainOptions {
                    grace_period_seconds: -1,
                    timeout_secs: 5,
                    ignore_all_daemon_sets: false,
                    delete_empty_dir_data: false,
                    force: false,
                    disable_eviction: false,
                };
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
            Dialog::Rename { name, .. } => {
                st.title = "<Rename>".into();
                st.text = format!("Rename context {}?", super::text::go_quote(name));
                st.items = vec![Item::input("New name:", name.clone(), 0)];
                st.buttons = vec!["OK".into(), "Cancel".into()];
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
            Dialog::Selection { title, options, .. } => {
                st.title = format!("<{title}>");
                st.buttons = Vec::new();
                // ShowSelection adds every option twice (k9s quirk).
                let items = options
                    .iter()
                    .flat_map(|o| [o.clone(), o.clone()])
                    .collect();
                st.list = Some((items, 0));
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
        if self.list.is_some() {
            return self.list_key(key, ctx);
        }
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
            KeyCode::Esc => return Some(self.cancelled(ctx)),
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

    /// tview List.InputHandler (wrap around, Enter/space select, Esc done).
    fn list_key(&mut self, key: KeyEvent, ctx: &mut Ctx) -> Option<Vec<ViewCmd>> {
        let (n, cur) = self
            .list
            .as_ref()
            .map(|(l, c)| (l.len() as i64, *c as i64))?;
        if key.code == KeyCode::Esc {
            return Some(Vec::new());
        }
        if n == 0 {
            return None;
        }
        let mut c = cur;
        match key.code {
            KeyCode::Tab | KeyCode::Down | KeyCode::Right => c += 1,
            KeyCode::BackTab | KeyCode::Up | KeyCode::Left => c -= 1,
            KeyCode::Home => c = 0,
            KeyCode::End => c = n - 1,
            KeyCode::PageDown => c += n,
            KeyCode::PageUp => c -= n,
            KeyCode::Enter | KeyCode::Char(' ') => return self.select(cur as usize, ctx),
            _ => {}
        }
        if c < 0 {
            c = n - 1;
        } else if c >= n {
            c = 0;
        }
        if let Some((_, cc)) = self.list.as_mut() {
            *cc = c as usize;
        }
        None
    }

    fn select(&mut self, i: usize, ctx: &mut Ctx) -> Option<Vec<ViewCmd>> {
        match self.kind.take()? {
            Dialog::Selection { on_select, .. } => Some(on_select(ctx, i)),
            _ => Some(Vec::new()),
        }
    }

    /// ui.ModalList.Draw: list width + 4, items + 4 rows, centered; framed
    /// with a focused border and an aqua title.
    fn draw_list(&self, buf: &mut Buffer, screen: Rect, styles: &Styles) {
        let Some((items, cur)) = &self.list else {
            return;
        };
        let w = items.iter().map(|i| width(i) as u16 + 2).max().unwrap_or(0) + 2;
        let h = items.len() as u16 + 4;
        let (w, h) = (w.min(screen.width), h.min(screen.height));
        let x = screen.x + (screen.width - w) / 2;
        let y = screen.y + (screen.height - h) / 2;
        let area = Rect::new(x, y, w, h);
        let mut hits = Hits {
            area,
            ..Default::default()
        };
        fill(buf, area, Style::default().bg(styles.bg));
        if w < 4 || h < 3 {
            *self.hits.borrow_mut() = hits;
            return;
        }
        let bc = if self.blurred {
            styles.border
        } else {
            styles.border_focus
        };
        let bs = Style::default().fg(bc).bg(styles.bg);
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
                Style::default().fg(AQUA).bg(styles.bg),
            )],
            Align::Center,
            false,
        );
        let (ix, iw) = (l + 2, w.saturating_sub(4));
        for (i, item) in items.iter().enumerate() {
            let yy = t + 2 + i as u16;
            if yy >= b {
                break;
            }
            let st = if i == *cur {
                Style::default()
                    .fg(styles.dialog_button_focus_fg)
                    .bg(styles.dialog_button_focus_bg)
            } else {
                Style::default().fg(styles.fg).bg(styles.bg)
            };
            draw_segs(
                buf,
                ix,
                yy,
                iw,
                &[seg(item.clone(), st)],
                Align::Left,
                false,
            );
            hits.elems.push((i, Rect::new(ix, yy, iw, 1)));
        }
        *self.hits.borrow_mut() = hits;
    }

    /// Whether `(x, y)` falls on the modal box of the last draw.
    pub fn contains(&self, x: u16, y: u16) -> bool {
        self.hits.borrow().area.contains(Position::new(x, y))
    }

    /// tview Form.MouseHandler for a left click inside the modal: an open
    /// drop down list picks the clicked option (or closes); buttons press,
    /// checkboxes toggle, drop downs open, inputs take focus; anywhere else
    /// keeps the current focus. Same return contract as `handle_key`.
    pub fn click(&mut self, x: u16, y: u16, ctx: &mut Ctx) -> Option<Vec<ViewCmd>> {
        let p = Position::new(x, y);
        // tview List.MouseHandler: a click on an item selects it.
        if self.list.is_some() {
            let hit = self
                .hits
                .borrow()
                .elems
                .iter()
                .find(|(_, r)| r.contains(p))
                .map(|(i, _)| *i);
            return match hit {
                Some(i) => {
                    if let Some((_, c)) = self.list.as_mut() {
                        *c = i;
                    }
                    self.select(i, ctx)
                }
                None => None,
            };
        }
        let (elem, option) = {
            let h = self.hits.borrow();
            (
                h.elems.iter().find(|(_, r)| r.contains(p)).map(|(i, _)| *i),
                h.options
                    .iter()
                    .find(|(_, r)| r.contains(p))
                    .map(|(i, _)| *i),
            )
        };
        if let Some(Item::DropDown {
            open: true,
            selected,
            ..
        }) = self.items.get_mut(self.focus)
        {
            if let Some(o) = option {
                *selected = o;
            }
            self.close_dropdown();
            return None;
        }
        let i = elem?;
        self.focus = i;
        if i >= self.items.len() {
            let b = self.focused_button().unwrap_or_default().to_string();
            return self.press(&b, ctx);
        }
        match &mut self.items[i] {
            Item::Checkbox { checked, .. } => *checked = !*checked,
            Item::DropDown {
                open,
                cursor,
                selected,
                ..
            } => {
                *open = true;
                *cursor = *selected;
            }
            Item::Input { .. } => {}
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
                KeyCode::Enter | KeyCode::Char(' ') => {
                    *checked = !*checked;
                    changed = true;
                }
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
            // Download toggles the message and swaps From/To.
            Some(Dialog::Transfer { .. }) if idx == 0 => {
                let (from, to) = (self.items[1].value(), self.items[2].value());
                // ShowUploads re-sets opts.Message, dropping the containers line.
                self.text = if self.items[0].checked() {
                    "Download Files".into()
                } else {
                    "Upload Files".into()
                };
                for (i, v) in [(1, to), (2, from)] {
                    if let Item::Input { value, .. } = &mut self.items[i] {
                        *value = v;
                    }
                }
                None
            }
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

    /// Cancel/Esc: a Launching prompt cancels its action first.
    fn cancelled(&mut self, ctx: &mut Ctx) -> Vec<ViewCmd> {
        match self.kind.take() {
            Some(Dialog::Launching {
                cancel, on_cancel, ..
            }) => {
                cancel.cancel();
                on_cancel(ctx)
            }
            other => {
                self.kind = other;
                Vec::new()
            }
        }
    }

    fn press(&mut self, button: &str, ctx: &mut Ctx) -> Option<Vec<ViewCmd>> {
        match button {
            "Cancel" | "Dismiss" => Some(self.cancelled(ctx)),
            "OK" => self.ok(ctx),
            _ => None,
        }
    }

    fn ok(&mut self, ctx: &mut Ctx) -> Option<Vec<ViewCmd>> {
        let kind = self.kind.take()?;
        match kind {
            Dialog::Confirm { on_ok, .. } => Some(on_ok(ctx)),
            Dialog::Launching { .. } => Some(Vec::new()),
            Dialog::ConfirmAck {
                title,
                msg,
                accept,
                on_ok,
            } => {
                // OK is inert until the text matches.
                if self.items[0].value() != accept {
                    self.kind = Some(Dialog::ConfirmAck {
                        title,
                        msg,
                        accept,
                        on_ok,
                    });
                    return None;
                }
                Some(on_ok(ctx))
            }
            Dialog::Transfer {
                pod,
                containers,
                retries,
                mut on_ok,
            } => {
                // Retries keeps its last valid number (strconv.Atoi on change).
                let retries = self.items[5].value().parse().unwrap_or(retries);
                let args = crate::ui::TransferArgs {
                    download: self.items[0].checked(),
                    from: self.items[1].value(),
                    to: self.items[2].value(),
                    no_preserve: self.items[3].checked(),
                    container: self.items[4].value(),
                    retries,
                };
                match on_ok(ctx, args) {
                    Ok(cmds) => Some(cmds),
                    Err(e) => {
                        self.kind = Some(Dialog::Transfer {
                            pod,
                            containers,
                            retries,
                            on_ok,
                        });
                        self.pending_flash = Some((FlashLevel::Err, e));
                        None
                    }
                }
            }
            Dialog::SetImage {
                containers, on_ok, ..
            } => {
                // imageFormSpec.modified: trimmed, non-empty and different.
                let specs = containers
                    .iter()
                    .zip(&self.items)
                    .filter_map(|((name, old, init), item)| {
                        let img = item.value().trim().to_string();
                        (!img.is_empty() && img != *old).then(|| crate::k8s::ImageSpec {
                            name: name.clone(),
                            docker_image: img,
                            init: *init,
                        })
                    })
                    .collect();
                Some(on_ok(ctx, specs))
            }
            Dialog::Restart { on_ok, .. } => {
                let fm = self.items[0].value();
                Some(on_ok(ctx, fm))
            }
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
            Dialog::PortForward {
                spec,
                ports,
                prefill,
                on_ok,
            } => {
                let co = self.items[0].value();
                let lo = self.items[1].value();
                let address = self.items[2].value();
                // pf_dialog OK, then startFwdCB: errors flash and keep the form.
                let res = if co.is_empty() || lo.is_empty() {
                    Err("container to local port mismatch".to_string())
                } else {
                    to_tunnels(&address, &co, &lo, &spec).and_then(|tt| {
                        check_tunnels(&tt, &fqn(&spec.namespace, &spec.pod), ctx)?;
                        Ok(tt)
                    })
                };
                match res {
                    Ok(tt) => Some(on_ok(ctx, tt)),
                    Err(e) => {
                        self.kind = Some(Dialog::PortForward {
                            spec,
                            ports,
                            prefill,
                            on_ok,
                        });
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
            Dialog::Prompt { on_ok, .. } | Dialog::Rename { on_ok, .. } => {
                let v = self.items[0].value();
                Some(on_ok(ctx, v))
            }
            Dialog::Selection { .. } => Some(Vec::new()),
        }
    }

    /// Draw the modal centered on `screen`; returns the input cursor if any.
    pub fn draw(&self, buf: &mut Buffer, screen: Rect, styles: &Styles) -> Option<Position> {
        if self.list.is_some() {
            self.draw_list(buf, screen, styles);
            return None;
        }
        // tview ModalForm: text width is a third of the screen, at least the buttons row.
        let buttons_w: u16 = self
            .buttons
            .iter()
            .map(|s| width(s) as u16 + 4)
            .sum::<u16>()
            + 2 * self.buttons.len().saturating_sub(1) as u16;
        let mut w = (screen.width / 3).max(buttons_w);
        // Frame.AddText prints with color tags; WordWrap skips them.
        let lines = word_wrap_tagged(
            &self.text,
            w as usize,
            &crate::ui::views::print_tags(&self.text),
        );
        let has_text = !self.text.is_empty();
        let items = self.items.len() as u16;
        // tview ModalForm: text lines + items + one row per button + 5.
        let mut h = lines.len() as u16 + items + self.buttons.len() as u16 + 5;
        w += 4;
        w = w.min(screen.width);
        h = h.min(screen.height);
        let x = screen.x + (screen.width - w) / 2;
        let y = screen.y + (screen.height - h) / 2;
        let area = Rect::new(x, y, w, h);
        let mut hits = Hits {
            area,
            ..Default::default()
        };
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
                    &crate::ui::views::print_markup(line, text_style)
                        .into_iter()
                        .map(|(t, st)| seg(t, st))
                        .collect::<Vec<_>>(),
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
            let focused = !self.blurred && self.focus == i;
            hits.elems.push((i, Rect::new(ix, yy, iw, 1)));
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
        // Buttons row: tview Form centers them with one blank column between.
        // Form leaves button colors unset (BOZO), so unfocused buttons use
        // tview.Styles (body fg/bg); k9s only sets the activated colors.
        let by = b.saturating_sub(2);
        let total: u16 = self
            .buttons
            .iter()
            .map(|s| width(s) as u16 + 5)
            .sum::<u16>()
            .saturating_sub(1);
        let mut bx = (ix + ix + iw).saturating_sub(total) / 2;
        for (i, label) in self.buttons.iter().enumerate() {
            let focused = !self.blurred && self.focus == self.items.len() + i;
            let st = if focused {
                Style::default()
                    .fg(styles.dialog_button_focus_fg)
                    .bg(styles.dialog_button_focus_bg)
            } else {
                Style::default().fg(styles.fg).bg(styles.bg)
            };
            let bw = width(label) as u16 + 4;
            hits.elems
                .push((self.items.len() + i, Rect::new(bx, by, bw, 1)));
            draw_segs(
                buf,
                bx,
                by,
                bw,
                &[seg(format!("  {label}  "), st)],
                Align::Left,
                false,
            );
            bx += bw + 1;
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
                hits.options.push((i, Rect::new(dx, y, ow, 1)));
                draw_segs(buf, dx, y, ow, &[seg(o.clone(), st)], Align::Left, false);
            }
        }
        *self.hits.borrow_mut() = hits;
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
fn to_tunnels(
    address: &str,
    co: &str,
    lo: &str,
    spec: &PortForwardSpec,
) -> Result<Vec<PortForwardSpec>, String> {
    let pp: Vec<&str> = co.split(',').collect();
    let lps: Vec<&str> = lo.split(',').collect();
    if pp.len() != lps.len() {
        return Err(format!(
            "spec to local port count mismatch. Expected {} but got {}",
            pp.len(),
            lps.len()
        ));
    }
    pp.iter()
        .zip(&lps)
        .map(|(p, l)| {
            let (container, cport) = parse_pf(p)?;
            let lport: u16 = l
                .trim()
                .parse()
                .map_err(|_| format!("invalid local port {l}"))?;
            Ok(PortForwardSpec {
                namespace: spec.namespace.clone(),
                pod: spec.pod.clone(),
                container: container.unwrap_or_else(|| spec.container.clone()),
                address: address.to_string(),
                local_port: lport,
                container_port: cport,
            })
        })
        .collect()
}

/// port.ParsePF + PFAnn.PortNum: `[container::][local:]port[/num]` to
/// (container, container port number).
fn parse_pf(p: &str) -> Result<(Option<String>, u16), String> {
    static PLAIN: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\A(\d*):?(\d*|[\w-]*)\z").unwrap());
    static FULL: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"\A([\w-]+)::(\d*):?(\d*|[\w-]*)/?(\d+)?\z").unwrap()
    });
    let t = p.trim();
    let (container, port, num) = if let Some(m) = PLAIN.captures(t).filter(|_| !t.is_empty()) {
        let port = if m[2].is_empty() { &m[1] } else { &m[2] };
        (None, port.to_string(), String::new())
    } else if let Some(m) = FULL.captures(t) {
        let port = if m[3].is_empty() { &m[2] } else { &m[3] };
        let num = m.get(4).map(|n| n.as_str().to_string()).unwrap_or_default();
        (Some(m[1].to_string()), port.to_string(), num)
    } else {
        return Err(format!("invalid port-forward specification {p}"));
    };
    port.parse()
        .or_else(|_| num.parse())
        .map(|n| (container, n))
        .map_err(|_| "no port number assigned".to_string())
}

/// port.IsPortFree: listen on address:port (Go resolves to IPv4 first).
pub(crate) fn port_free(address: &str, port: u16) -> bool {
    use std::net::ToSocketAddrs as _;
    let Ok(addrs) = (address, port).to_socket_addrs() else {
        return false;
    };
    let addrs: Vec<_> = addrs.collect();
    let Some(a) = addrs.iter().find(|a| a.is_ipv4()).or(addrs.first()) else {
        return false;
    };
    std::net::TcpListener::bind(a).is_ok()
}

/// startFwdCB: every local port must be free and no tunnel may exist yet.
fn check_tunnels(tt: &[PortForwardSpec], path: &str, ctx: &Ctx) -> Result<(), String> {
    for t in tt {
        if !port_free(&t.address, t.local_port) {
            return Err(format!("port {} is not available on host", t.local_port));
        }
    }
    if tt.iter().any(|t| ctx.port_forwards.contains_key(&t.id())) {
        return Err(format!("port-forward is already active on pod {path}"));
    }
    Ok(())
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
        // Idle buttons use the body colors (tview Form drops button colors),
        // one column after the focused one.
        let (x, y) = find(&buf, "OK").unwrap();
        // tview Form ignores the skin's button colors for unfocused buttons.
        assert_eq!(bg(&buf, x, y), st.bg);
        assert_eq!(fg(&buf, x, y), st.fg);
        // Centered with one blank column between buttons: "  Cancel   OK  ".
        let (cx, _) = find(&buf, "Cancel").unwrap();
        assert_eq!(x - cx, 11);
        // Box is centered: tview ModalForm is screen/3 + 4 = 37 wide.
        let (bx, by) = find(&buf, "┌").unwrap();
        assert_eq!(bx, 31);
        // Height: 1 text line + 2 buttons + 5; a blank row precedes the buttons.
        let (_, bottom) = find(&buf, "└").unwrap();
        assert_eq!(bottom - by + 1, 8);
        let (_, cy) = find(&buf, "Cancel").unwrap();
        assert_eq!(cy, bottom - 2);

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

    /// dialog.ShowConfirmAck: OK only acts once the magic text is typed.
    #[test]
    fn confirm_ack_needs_text() {
        let mut c = ctx();
        let hit = Arc::new(Mutex::new(false));
        let h2 = hit.clone();
        let mut d = DialogState::new(
            Dialog::ConfirmAck {
                title: "Sanitize".into(),
                msg: "Please enter Yes Please! to proceed.".into(),
                accept: "Yes Please!".into(),
                on_ok: Box::new(move |_| {
                    *h2.lock().unwrap() = true;
                    vec![]
                }),
            },
            &c,
        );
        assert_eq!(d.title, "<Sanitize>");
        assert_eq!(d.buttons, ["Cancel", "OK"]);
        assert_eq!(d.focus, 0);
        d.focus = 2;
        assert!(d.handle_key(k(KeyCode::Enter), &mut c).is_none());
        assert!(d.pending_flash.is_none() && !*hit.lock().unwrap());
        d.focus = 0;
        for ch in "Yes Please!".chars() {
            d.handle_key(k(KeyCode::Char(ch)), &mut c);
        }
        d.focus = 2;
        assert!(d.handle_key(k(KeyCode::Enter), &mut c).is_some());
        assert!(*hit.lock().unwrap());
    }

    /// dialog.ShowUploads: Download toggles the message and swaps From/To;
    /// a failed Ack keeps the form open with a flash.
    #[test]
    fn transfer_form() {
        let mut c = ctx();
        let got = Arc::new(Mutex::new(Vec::new()));
        let g2 = got.clone();
        let mut d = DialogState::new(
            Dialog::Transfer {
                pod: "default/p:".into(),
                containers: vec!["a".into(), "b".into()],
                retries: 999,
                on_ok: Box::new(move |_, args| {
                    g2.lock().unwrap().push(args.clone());
                    if args.download {
                        Ok(vec![])
                    } else {
                        Err("stat x: no such file or directory".into())
                    }
                }),
            },
            &c,
        );
        assert_eq!(d.title, "<Transfer>");
        assert_eq!(d.text, "Download Files\nAvailable Containers:a,b");
        assert_eq!(d.buttons, ["Cancel", "OK"]);
        assert_eq!(d.items[1].value(), "default/p:");
        assert_eq!(d.items[4].value(), "a");
        assert_eq!(d.items[5].value(), "999");
        // Toggle Download off: Upload message, From/To swapped.
        d.handle_key(k(KeyCode::Char(' ')), &mut c);
        assert_eq!(d.text, "Upload Files");
        assert_eq!(
            (d.items[1].value(), d.items[2].value()),
            (String::new(), "default/p:".into())
        );
        d.focus = 7;
        assert!(d.handle_key(k(KeyCode::Enter), &mut c).is_none());
        assert!(matches!(d.pending_flash.take(), Some((FlashLevel::Err, _))));
        d.focus = 0;
        d.handle_key(k(KeyCode::Char(' ')), &mut c);
        d.focus = 7;
        assert!(d.handle_key(k(KeyCode::Enter), &mut c).is_some());
        let last = got.lock().unwrap().last().cloned().unwrap();
        assert!(last.download && !last.no_preserve);
        assert_eq!(
            (last.from.as_str(), last.container.as_str(), last.retries),
            ("default/p:", "a", 999)
        );
    }

    /// tview Frame prints color tags in dialog text; `[list]` is not a tag.
    #[test]
    fn dialog_text_markup() {
        let c = ctx();
        let d = DialogState::new(
            Dialog::Confirm {
                title: "Confirm Rollback".into(),
                msg: "RollingBack chart [yellow::b]alpha[-::-] to release <[orangered::b]1[-::-]>? [list]".into(),
                on_ok: Box::new(|_| vec![]),
            },
            &c,
        );
        let st = Styles::default();
        let mut buf = Buffer::empty(Rect::new(0, 0, 160, 30));
        let area = buf.area;
        d.draw(&mut buf, area, &st);
        let (x, y) = find(&buf, "RollingBack chart alpha to release <1>? [list]").unwrap();
        let ax = x + "RollingBack chart ".len() as u16;
        let yellow = crate::ui::views::print_markup("[yellow::]x", Style::default())[0]
            .1
            .fg;
        assert_eq!(Some(fg(&buf, ax, y)), yellow);
        assert!(buf[(ax, y)]
            .modifier
            .contains(ratatui::style::Modifier::BOLD));
        assert_eq!(fg(&buf, x, y), st.dialog_fg);
    }

    /// view/image_extender.go: one input per container labelled by name;
    /// only changed images are submitted.
    #[test]
    fn set_image_form() {
        let mut c = ctx();
        let got = Arc::new(Mutex::new(None));
        let g2 = got.clone();
        let mut d = DialogState::new(
            Dialog::SetImage {
                msg: "Set image apps/v1/deployments default/web".into(),
                containers: vec![
                    ("init".into(), "busybox:1".into(), true),
                    ("web".into(), "nginx:1".into(), false),
                ],
                on_ok: Box::new(move |_, specs| {
                    *g2.lock().unwrap() = Some(specs);
                    vec![]
                }),
            },
            &c,
        );
        assert_eq!(d.title, "<Set image>");
        assert_eq!(d.buttons, ["OK", "Cancel"]);
        let st = Styles::default();
        let mut buf = Buffer::empty(Rect::new(0, 0, 160, 45));
        let area = buf.area;
        d.draw(&mut buf, area, &st);
        let (x, y) = find(&buf, "Set image apps").unwrap();
        assert_eq!(fg(&buf, x, y), st.fg);
        assert!(find(&buf, "init busybox:1").is_some());
        assert!(find(&buf, "web  nginx:1").is_some());
        // Edit the second container, then OK.
        d.handle_key(k(KeyCode::Tab), &mut c);
        d.handle_key(k(KeyCode::Backspace), &mut c);
        d.handle_key(k(KeyCode::Char('2')), &mut c);
        d.handle_key(k(KeyCode::Tab), &mut c);
        assert!(d.handle_key(k(KeyCode::Enter), &mut c).is_some());
        let specs = got.lock().unwrap().clone().unwrap();
        assert_eq!(
            specs,
            vec![crate::k8s::ImageSpec {
                name: "web".into(),
                docker_image: "nginx:2".into(),
                init: false,
            }]
        );
    }

    /// dialog.ShowSelection / ui.ModalList: options listed twice, centered
    /// box (width = longest + 4, height = items + 4), focus-colored border,
    /// wrap-around keys, Enter/space/click pick the list index, Esc closes.
    #[test]
    fn selection_modal_list() {
        let mut c = ctx();
        let picked: Arc<Mutex<Option<usize>>> = Default::default();
        let mk = |p: Arc<Mutex<Option<usize>>>| Dialog::Selection {
            title: "Jump To".into(),
            options: vec!["0: ReplicaSet".into(), "1: Job".into()],
            on_select: Box::new(move |_, i| {
                *p.lock().unwrap() = Some(i);
                Vec::new()
            }),
        };
        let st = Styles::default();
        let d = DialogState::new(mk(picked.clone()), &c);
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 20));
        {
            let a = buf.area;
            d.draw(&mut buf, a, &st);
        }
        let rows = all_rows(&buf);
        let (tx, ty) = find(&buf, "<Jump To>").unwrap();
        assert_eq!(fg(&buf, tx, ty), AQUA);
        // 13 + 4 wide, 4 + 4 high, centered.
        let (bx, by) = ((80 - 17) / 2, (20 - 8) / 2);
        assert_eq!(rows[by as usize].chars().nth(bx as usize), Some('┌'));
        assert_eq!(rows[by as usize].chars().nth(bx as usize + 16), Some('┐'));
        assert_eq!(fg(&buf, bx, by), st.border_focus);
        assert_eq!(
            rows[by as usize + 2].trim_matches(|c| c == ' ' || c == '│'),
            "0: ReplicaSet"
        );
        assert_eq!(
            rows[by as usize + 3].trim_matches(|c| c == ' ' || c == '│'),
            "0: ReplicaSet"
        );
        assert_eq!(
            rows[by as usize + 4].trim_matches(|c| c == ' ' || c == '│'),
            "1: Job"
        );
        assert_eq!(bg(&buf, bx + 2, by + 2), st.dialog_button_focus_bg);
        // Up wraps to the last item; Enter picks its index.
        let mut d = DialogState::new(mk(picked.clone()), &c);
        assert!(d.handle_key(k(KeyCode::Up), &mut c).is_none());
        assert!(d.handle_key(k(KeyCode::Enter), &mut c).is_some());
        assert_eq!(*picked.lock().unwrap(), Some(3));
        let mut d = DialogState::new(mk(picked.clone()), &c);
        d.handle_key(k(KeyCode::Down), &mut c);
        assert!(d.handle_key(k(KeyCode::Char(' ')), &mut c).is_some());
        assert_eq!(*picked.lock().unwrap(), Some(1));
        let mut d = DialogState::new(mk(picked.clone()), &c);
        assert!(d.handle_key(k(KeyCode::Char('x')), &mut c).is_none());
        assert!(d.handle_key(k(KeyCode::Esc), &mut c).unwrap().is_empty());
        // A click on an item picks it.
        let mut d = DialogState::new(mk(picked.clone()), &c);
        {
            let a = buf.area;
            d.draw(&mut buf, a, &st);
        }
        assert!(d.click(bx + 3, by + 4, &mut c).is_some());
        assert_eq!(*picked.lock().unwrap(), Some(2));
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
        // Text wraps at screen/3, so use a wide screen to keep the message on one line.
        let mut buf = Buffer::empty(Rect::new(0, 0, 160, 30));
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
                    // pf_choices fills the preferred port.
                    container_port: 80,
                    local_port: 80,
                    ..Default::default()
                },
                ports: vec!["nginx::80(http)".into(), "sidecar::9090".into()],
                prefill: ("nginx::80".into(), "80".into()),
                on_ok: Box::new(move |_, s| {
                    *g2.lock().unwrap() = Some(s);
                    vec![]
                }),
            },
            &c,
        );
        assert!(d
            .text
            .starts_with("default/nginx\n\nExposed Ports:\nnginx::80(http)"));
        // PFAnn.AsSpec drops the port name.
        assert_eq!(d.items[0].value(), "nginx::80");
        // Placeholders only for fields that start empty.
        assert!(matches!(&d.items[1], Item::Input { placeholder, .. } if placeholder.is_empty()));
        assert_eq!(d.items[1].value(), "80");
        assert_eq!(d.items[2].value(), "localhost");
        // Edit container port -> local port follows.
        d.handle_key(k(KeyCode::Backspace), &mut c);
        d.handle_key(k(KeyCode::Backspace), &mut c);
        for ch in "29091,sidecar::9090".chars() {
            d.handle_key(k(KeyCode::Char(ch)), &mut c);
        }
        d.focus = 1;
        for _ in 0..20 {
            d.handle_key(k(KeyCode::Backspace), &mut c);
        }
        for ch in "29091,29090".chars() {
            d.handle_key(k(KeyCode::Char(ch)), &mut c);
        }
        d.focus = 3;
        assert!(d.handle_key(k(KeyCode::Enter), &mut c).is_some());
        // ToTunnels keeps every comma-separated tunnel.
        let s = got.lock().unwrap().clone().unwrap();
        let tt: Vec<_> = s
            .iter()
            .map(|t| (t.container.as_str(), t.container_port, t.local_port))
            .collect();
        assert_eq!(tt, [("nginx", 29091, 29091), ("sidecar", 9090, 29090)]);
        assert_eq!(s[0].address, "localhost");

        // Empty ports flash an error and stay open.
        let mut d = DialogState::new(
            Dialog::PortForward {
                spec: PortForwardSpec::default(),
                ports: vec![],
                prefill: Default::default(),
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

    /// port.ParsePF + PortNum and startFwdCB checks keep the form open.
    #[test]
    fn port_forward_specs_and_checks() {
        let spec = PortForwardSpec {
            namespace: "default".into(),
            pod: "p".into(),
            container: "web".into(),
            ..Default::default()
        };
        assert_eq!(parse_pf("web::80"), Ok((Some("web".into()), 80)));
        assert_eq!(parse_pf("8080:80"), Ok((None, 80)));
        assert_eq!(parse_pf("web::http/8080"), Ok((Some("web".into()), 8080)));
        assert_eq!(parse_pf("web::http"), Err("no port number assigned".into()));
        assert_eq!(
            parse_pf("web::80(http)"),
            Err("invalid port-forward specification web::80(http)".into())
        );
        assert_eq!(
            to_tunnels("localhost", "web::80,web::81", "8080", &spec).unwrap_err(),
            "spec to local port count mismatch. Expected 2 but got 1"
        );
        // A busy host port is refused.
        let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let busy = l.local_addr().unwrap().port();
        let mut c = ctx();
        let mut d = DialogState::new(
            Dialog::PortForward {
                spec,
                ports: vec![],
                prefill: ("web::80".into(), busy.to_string()),
                on_ok: Box::new(|_, _| panic!("no")),
            },
            &c,
        );
        d.items[2] = Item::input("Address:", "localhost", 30);
        d.focus = 3;
        assert!(d.handle_key(k(KeyCode::Enter), &mut c).is_none());
        assert_eq!(
            d.pending_flash.take(),
            Some((
                FlashLevel::Err,
                format!("port {busy} is not available on host")
            ))
        );
    }

    #[test]
    fn mouse_clicks_toggle_and_press() {
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
        let st = Styles::default();
        let mut buf = Buffer::empty(Rect::new(0, 0, 160, 45));
        let a = buf.area;
        d.draw(&mut buf, a, &st);
        let elem = |d: &DialogState, i: usize| {
            let h = d.hits.borrow();
            h.elems.iter().find(|(j, _)| *j == i).unwrap().1
        };
        // Checkbox (item 4, Force): a click focuses and toggles it.
        let r = elem(&d, 4);
        assert!(d.contains(r.x, r.y));
        assert!(!d.contains(0, 0));
        assert!(d.click(r.x + 1, r.y, &mut c).is_none());
        assert_eq!(d.focus, 4);
        // Clicking empty form space keeps the focus.
        let area = d.hits.borrow().area;
        assert!(d.click(area.x + 1, area.y + 1, &mut c).is_none());
        assert_eq!(d.focus, 4);
        // OK button presses.
        let ok = d.items.len() + 1;
        let r = elem(&d, ok);
        assert!(d.click(r.x + 2, r.y, &mut c).is_some());
        assert!(got.lock().unwrap().clone().unwrap().force);
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
        // view/node.go drainCmd defaults: 5s timeout, all checkboxes off.
        assert_eq!(d.items[1].value(), "5s");
        assert!(!d.items[2].checked() && !d.items[3].checked());
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
        assert!(!o.ignore_all_daemon_sets);
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
