//! The cow (k9s view/cow.go and ui/dialog/error.go cowTalk).

use std::any::Any;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::Frame;

use super::text::{draw_segs, seg, Align, Seg};
use crate::ui::{Ctx, Hint, View, ViewCmd, ViewId};

pub(crate) const COW: [&str; 5] = [
    r"\   ^__^            ",
    r" \  (oo)\_______    ",
    r"    (__)\       )\/\",
    r"        ||----w |   ",
    r"        ||     ||   ",
];

/// dialog/error.go cowTalk: plain text for the error dialog.
pub(crate) fn dialog_cow_talk(says: &str) -> String {
    let msg = format!("< Ruroh? {} >", says.strip_suffix('\n').unwrap_or(says));
    let mut buff = vec![msg];
    buff.extend(COW.iter().map(|s| s.to_string()));
    buff.join("\n")
}

/// view/cow.go cowTalk: styled lines for the cow view.
pub(crate) fn cow_talk(says: &str, w: i32) -> Vec<Vec<Seg>> {
    let red = Style::default().fg(Color::LightRed);
    let bold = red.add_modifier(Modifier::BOLD);
    let n = says.chars().count() + 8;
    let mut out = vec![
        vec![seg(format!(" {}", "─".repeat(n)), red)],
        vec![
            seg("< ", red),
            seg(format!("Ruroh? {says}"), bold),
            seg(" >", red),
        ],
        vec![seg(format!(" {}", "─".repeat(n)), red)],
    ];
    let mut r = w / 2 - 8;
    if r < 0 {
        r = w / 2;
    }
    let spacer = " ".repeat(r as usize);
    for s in COW {
        out.push(vec![seg(format!("{spacer}{s}"), bold)]);
    }
    out
}

/// tview's default Box rect width when the cow is initialized: talk() uses
/// `(x+w)/2` of that rect.
const INIT_WIDTH: i32 = 15;

struct Cow {
    id: ViewId,
    says: String,
}

/// The cow viewer (view/cow.go): an "Error" text view with a talking cow.
pub fn cow(ctx: &mut Ctx, says: String) -> Box<dyn View> {
    Box::new(Cow {
        id: ctx.new_view_id(),
        says,
    })
}

impl View for Cow {
    fn id(&self) -> ViewId {
        self.id
    }
    fn name(&self) -> String {
        "cow".into()
    }
    fn title(&self) -> String {
        "Error".into()
    }
    fn hints(&self) -> Vec<Hint> {
        vec![Hint::new("esc", "Back").hidden()]
    }
    fn handle_key(&mut self, key: KeyEvent, _ctx: &mut Ctx) -> Vec<ViewCmd> {
        if key.code == KeyCode::Esc {
            return vec![ViewCmd::Pop];
        }
        Vec::new()
    }
    fn draw(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &Ctx) {
        let says = if self.says.is_empty() {
            "Nothing to report here. Please move along..."
        } else {
            self.says.as_str()
        };
        let buf = frame.buffer_mut();
        let bg = ctx.styles.bg;
        for (i, line) in cow_talk(says, INIT_WIDTH / 2).into_iter().enumerate() {
            let y = area.y + i as u16;
            if y >= area.bottom() {
                break;
            }
            let line: Vec<Seg> = line
                .into_iter()
                .map(|s| Seg {
                    style: s.style.bg(bg),
                    ..s
                })
                .collect();
            draw_segs(buf, area.x, y, area.width, &line, Align::Center, false);
        }
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

    fn text(l: &[Seg]) -> String {
        l.iter().map(|s| s.text.clone()).collect()
    }

    #[test]
    fn cow_view_talk() {
        let lines = cow_talk("blee", 7);
        assert_eq!(text(&lines[0]), format!(" {}", "─".repeat(12)));
        assert_eq!(text(&lines[1]), "< Ruroh? blee >");
        assert_eq!(text(&lines[2]), format!(" {}", "─".repeat(12)));
        assert_eq!(text(&lines[3]), format!("   {}", COW[0]));
        assert_eq!(lines.len(), 8);
        assert!(lines[1][1].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(lines[1][0].style.fg, Some(Color::LightRed));
        let wide = cow_talk("x", 40);
        assert_eq!(text(&wide[3]), format!("{}{}", " ".repeat(12), COW[0]));
    }

    #[test]
    fn dialog_talk() {
        assert_eq!(
            dialog_cow_talk("oops\n"),
            format!("< Ruroh? oops >\n{}", COW.join("\n"))
        );
    }
}
