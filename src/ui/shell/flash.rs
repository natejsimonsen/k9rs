//! Flash message line (k9s ui/flash.go + model/flash.go).

use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

use super::text::{draw_segs, fill, seg, Align};
use crate::config::Styles;
use crate::ui::FlashLevel;

/// k9s model.DefaultFlashDelay.
pub(crate) const FLASH_DELAY: Duration = Duration::from_secs(6);

const EMO_HAPPY: &str = "😎";
const EMO_DOH: &str = "😗";
const EMO_RED: &str = "😡";

/// tcell ColorNavajoWhite / ColorOrange / ColorOrangeRed.
const NAVAJO_WHITE: Color = Color::Rgb(0xFF, 0xDE, 0xAD);
const ORANGE: Color = Color::Rgb(0xFF, 0xA5, 0x00);
const ORANGE_RED: Color = Color::Rgb(0xFF, 0x45, 0x00);

#[derive(Clone, Debug, Default)]
pub(crate) struct Flash {
    msg: Option<(FlashLevel, String, Instant)>,
}

impl Flash {
    pub fn set(&mut self, level: FlashLevel, text: impl Into<String>) {
        let text = text.into();
        if text.is_empty() {
            self.msg = None;
            return;
        }
        self.msg = Some((level, text, Instant::now()));
    }

    pub fn clear(&mut self) {
        self.msg = None;
    }

    #[cfg(test)]
    pub fn current(&self) -> Option<(FlashLevel, &str)> {
        self.msg.as_ref().map(|(l, t, _)| (*l, t.as_str()))
    }

    /// Clear the message once its delay elapsed. Returns true when cleared.
    pub fn expire(&mut self, now: Instant) -> bool {
        if let Some((_, _, at)) = &self.msg {
            if now.duration_since(*at) >= FLASH_DELAY {
                self.msg = None;
                return true;
            }
        }
        false
    }

    pub fn draw(&self, buf: &mut Buffer, area: Rect, styles: &Styles, no_icons: bool) {
        fill(buf, area, Style::default().bg(styles.bg));
        let Some((level, text, _)) = &self.msg else {
            return;
        };
        let emoji = if no_icons { "" } else { flash_emoji(*level) };
        let line = format!("{emoji} {text}");
        let style = Style::default().fg(flash_color(*level)).bg(styles.bg);
        // Border padding (0, 0, 1, 1).
        let inner_x = area.x.saturating_add(1);
        let inner_w = area.width.saturating_sub(2);
        let first = line.lines().next().unwrap_or_default().to_string();
        draw_segs(
            buf,
            inner_x,
            area.y,
            inner_w,
            &[seg(first, style)],
            Align::Center,
            false,
        );
    }
}

fn flash_emoji(l: FlashLevel) -> &'static str {
    match l {
        FlashLevel::Warn => EMO_DOH,
        FlashLevel::Err => EMO_RED,
        FlashLevel::Info => EMO_HAPPY,
    }
}

fn flash_color(l: FlashLevel) -> Color {
    match l {
        FlashLevel::Warn => ORANGE,
        FlashLevel::Err => ORANGE_RED,
        FlashLevel::Info => NAVAJO_WHITE,
    }
}

#[cfg(test)]
mod tests {
    use super::super::text::testutil::*;
    use super::*;

    #[test]
    fn renders_levels() {
        let styles = Styles::default();
        let mut f = Flash::default();
        f.set(FlashLevel::Err, "boom");
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 1));
        {
            let a = buf.area;
            f.draw(&mut buf, a, &styles, false)
        };
        let (x, y) = find(&buf, "boom").unwrap();
        assert_eq!(fg(&buf, x, y), ORANGE_RED);
        assert!(row(&buf, 0).contains("😡 boom"));

        f.set(FlashLevel::Info, "ok");
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 1));
        {
            let a = buf.area;
            f.draw(&mut buf, a, &styles, true)
        };
        let (x, y) = find(&buf, "ok").unwrap();
        assert_eq!(fg(&buf, x, y), NAVAJO_WHITE);
        assert!(!row(&buf, 0).contains('😎'));

        f.set(FlashLevel::Warn, "hmm");
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 1));
        {
            let a = buf.area;
            f.draw(&mut buf, a, &styles, false)
        };
        assert!(row(&buf, 0).contains("😗 hmm"));
    }

    #[test]
    fn expires() {
        let mut f = Flash::default();
        f.set(FlashLevel::Info, "x");
        let now = Instant::now();
        assert!(!f.expire(now));
        assert!(f.expire(now + FLASH_DELAY + Duration::from_millis(1)));
        assert!(f.current().is_none());
        f.set(FlashLevel::Info, "");
        assert!(f.current().is_none());
    }
}
