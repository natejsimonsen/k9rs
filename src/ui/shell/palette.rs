//! Terminal color fitting, as tcell does it.
//!
//! tcell only sends 24-bit colors when the terminal supports them
//! (terminfo.LookupTerminfo + tscreen.go). Otherwise every RGB color is
//! replaced by the closest palette entry (colorfit.go FindColor, CIE76
//! distance in Lab space). Inside tmux (TERM=tmux-256color, which has no
//! built-in tcell entry) k9s therefore draws with the 256-color palette.

use std::collections::HashMap;
use std::sync::Mutex;

use ratatui::buffer::Buffer;
use ratatui::style::Color;

/// How the terminal takes colors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ColorMode {
    pub truecolor: bool,
    /// Palette size (terminfo `colors`); 0 means no color support.
    pub colors: u16,
}

/// tcell built-in terminfo entries (terminfo/extended): name, aliases,
/// colors, TrueColor flag.
const BUILTIN: &[(&str, &[&str], u16, bool)] = &[
    ("aixterm", &[], 8, false),
    ("alacritty", &[], 256, false),
    ("ansi", &[], 8, false),
    ("beterm", &[], 8, false),
    ("cygwin", &[], 8, false),
    ("dtterm", &[], 8, false),
    ("eterm", &[], 0, false),
    ("eterm-color", &[], 8, false),
    ("gnome", &[], 8, false),
    ("gnome-256color", &[], 256, false),
    ("hpterm", &["X-hpterm"], 0, false),
    ("konsole", &[], 8, false),
    ("konsole-256color", &[], 256, false),
    ("kterm", &[], 8, false),
    ("linux", &[], 8, false),
    ("pcansi", &[], 8, false),
    ("rxvt", &[], 8, false),
    ("rxvt-256color", &[], 256, false),
    ("rxvt-88color", &[], 88, false),
    ("rxvt-unicode", &[], 88, false),
    ("rxvt-unicode-256color", &[], 256, false),
    ("screen", &[], 8, false),
    ("screen-256color", &[], 256, false),
    ("st", &[], 8, true),
    ("st-256color", &[], 256, true),
    ("sun", &["sun1", "sun2"], 0, false),
    ("sun-color", &[], 8, false),
    ("termite", &[], 256, false),
    ("tmux", &[], 8, false),
    ("vt100", &["vt100-am"], 0, false),
    ("vt102", &[], 0, false),
    ("vt220", &["vt200"], 0, false),
    ("vt320", &["vt300"], 0, false),
    ("vt400", &["vt400-24", "dec-vt400"], 0, false),
    ("vt420", &[], 0, false),
    ("vt52", &[], 0, false),
    ("wy50", &["wyse50"], 0, false),
    ("wy60", &["wyse60"], 0, false),
    ("wy99-ansi", &[], 0, false),
    ("wy99a-ansi", &[], 0, false),
    ("xfce", &[], 8, false),
    ("xterm", &["xterm-debian"], 8, false),
    ("xterm-88color", &[], 88, false),
    ("xterm-256color", &[], 256, false),
    ("xterm-ghostty", &["ghostty"], 256, true),
    ("xterm-kitty", &[], 256, true),
    ("xterm-termite", &[], 256, false),
];

fn builtin(name: &str) -> Option<(u16, bool)> {
    BUILTIN
        .iter()
        .find(|(n, aa, _, _)| *n == name || aa.contains(&name))
        .map(|(_, _, c, t)| (*c, *t))
}

/// terminfo.LookupTerminfo, minus the dynamic fallback: (colors, truecolor).
fn lookup(name: &str, colorterm: &str, tcell_tc: &str) -> Option<(u16, bool)> {
    if name.is_empty() {
        return None;
    }
    let mut add_tc = matches!(colorterm, "truecolor" | "24bit" | "24-bit");
    let mut found = builtin(name);
    if let Some((_, true)) = found {
        add_tc = true;
    } else if found.is_none() {
        if let Some(base) = name.strip_suffix("-truecolor") {
            for s in ["-256color", "-88color", "-color", ""] {
                if let Some(t) = lookup(&format!("{base}{s}"), "", "") {
                    found = Some(t);
                    add_tc = true;
                    break;
                }
            }
        }
    }
    if found.is_none() {
        if let Some(base) = name.strip_suffix("-256color") {
            for s in ["-88color", "-color"] {
                if lookup(&format!("{base}{s}"), "", "").is_some() {
                    found = Some((256, false));
                    break;
                }
            }
        }
    }
    let (colors, _) = found?;
    match tcell_tc {
        "" => {}
        "disable" => add_tc = false,
        _ => add_tc = true,
    }
    Some((colors, add_tc))
}

/// tcell's choice for a terminal. `infocmp_colors` reads `colors#N` for
/// terminals tcell has no built-in entry for (terminfo/dynamic). Entries
/// loaded that way never get 24-bit color sequences.
pub(crate) fn detect(
    term: &str,
    colorterm: &str,
    tcell_tc: &str,
    infocmp_colors: impl FnOnce(&str) -> Option<u16>,
) -> ColorMode {
    let (colors, mut truecolor) = match lookup(term, colorterm, tcell_tc) {
        Some(t) => t,
        // No terminfo at all: tcell fails to start. Keep colors as they are.
        None => match infocmp_colors(term) {
            Some(c) => (c, false),
            None => {
                return ColorMode {
                    truecolor: true,
                    colors: 256,
                }
            }
        },
    };
    if tcell_tc == "disable" {
        truecolor = false;
    }
    let colors = if colors < 8 { 0 } else { colors };
    ColorMode { truecolor, colors }
}

/// Mode for the current process environment.
pub(crate) fn detect_env() -> ColorMode {
    let env = |k: &str| std::env::var(k).unwrap_or_default();
    detect(
        &env("TERM"),
        &env("COLORTERM"),
        &env("TCELL_TRUECOLOR"),
        |t| {
            let out = std::process::Command::new("infocmp")
                .args(["-1", t])
                .stderr(std::process::Stdio::null())
                .output()
                .ok()?;
            if !out.status.success() {
                return None;
            }
            String::from_utf8_lossy(&out.stdout)
                .split([',', '\n'])
                .find_map(|f| f.trim().strip_prefix("colors#")?.parse().ok())
        },
    )
}

/// tcell palette color RGB (color.go ColorValues).
fn palette_rgb(i: u8) -> u32 {
    const BASIC: [u32; 16] = [
        0x000000, 0x800000, 0x008000, 0x808000, 0x000080, 0x800080, 0x008080, 0xC0C0C0, 0x808080,
        0xFF0000, 0x00FF00, 0xFFFF00, 0x0000FF, 0xFF00FF, 0x00FFFF, 0xFFFFFF,
    ];
    const LEVELS: [u32; 6] = [0x00, 0x5F, 0x87, 0xAF, 0xD7, 0xFF];
    match i {
        0..=15 => BASIC[i as usize],
        16..=231 => {
            let n = (i - 16) as usize;
            (LEVELS[n / 36] << 16) | (LEVELS[(n / 6) % 6] << 8) | LEVELS[n % 6]
        }
        _ => {
            let v = 8 + 10 * (i as u32 - 232);
            (v << 16) | (v << 8) | v
        }
    }
}

/// Palette index of a ratatui named/indexed color.
fn index_of(c: Color) -> Option<u8> {
    Some(match c {
        Color::Black => 0,
        Color::Red => 1,
        Color::Green => 2,
        Color::Yellow => 3,
        Color::Blue => 4,
        Color::Magenta => 5,
        Color::Cyan => 6,
        Color::Gray => 7,
        Color::DarkGray => 8,
        Color::LightRed => 9,
        Color::LightGreen => 10,
        Color::LightYellow => 11,
        Color::LightBlue => 12,
        Color::LightMagenta => 13,
        Color::LightCyan => 14,
        Color::White => 15,
        Color::Indexed(i) => i,
        _ => return None,
    })
}

/// go-colorful Color.Lab (sRGB, D65). Constants kept verbatim.
#[allow(clippy::excessive_precision)]
fn lab(rgb: u32) -> (f64, f64, f64) {
    fn lin(v: f64) -> f64 {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    }
    fn f(t: f64) -> f64 {
        if t > 6.0 / 29.0 * 6.0 / 29.0 * 6.0 / 29.0 {
            t.cbrt()
        } else {
            t / 3.0 * 29.0 / 6.0 * 29.0 / 6.0 + 4.0 / 29.0
        }
    }
    let ch = |s: u32| lin(((rgb >> s) & 0xFF) as f64 / 255.0);
    let (r, g, b) = (ch(16), ch(8), ch(0));
    let x = 0.41239079926595948 * r + 0.35758433938387796 * g + 0.18048078840183429 * b;
    let y = 0.21263900587151036 * r + 0.71516867876775593 * g + 0.072192315360733715 * b;
    let z = 0.019330818715591851 * r + 0.11919477979462599 * g + 0.95053215224966058 * b;
    let fy = f(y / 1.0);
    (
        1.16 * fy - 0.16,
        5.0 * (f(x / 0.95047) - fy),
        2.0 * (fy - f(z / 1.08883)),
    )
}

/// CIE lightness (0..1) of a color, as go-colorful Hcl; `None` for the
/// terminal default.
pub(crate) fn lightness(c: Color) -> Option<f64> {
    let rgb = match c {
        Color::Rgb(r, g, b) => ((r as u32) << 16) | ((g as u32) << 8) | b as u32,
        _ => palette_rgb(index_of(c)?),
    };
    Some(lab(rgb).0)
}

/// colorfit.go FindColor over the first `n` palette entries.
fn find_color(rgb: u32, n: u16) -> u8 {
    let (l1, a1, b1) = lab(rgb);
    let mut best = (0u8, f64::INFINITY);
    for i in 0..n.min(256) {
        let (l2, a2, b2) = lab(palette_rgb(i as u8));
        let mut d = ((l1 - l2).powi(2) + (a1 - a2).powi(2) + (b1 - b2).powi(2)).sqrt();
        if d.is_nan() {
            d = f64::INFINITY;
        }
        if i == 0 || d < best.1 {
            best = (i as u8, d);
        }
    }
    best.0
}

/// A tcell named color (ColorAliceBlue.., what GetColor returns for W3C
/// names in markup and code). These are palette entries past 255, not RGB,
/// so sendFgBg fits them to the 256-color palette even with 24-bit color.
/// Skin colors differ: k9s turns them into RGB (Color.TrueColor()).
pub(crate) fn named(rgb: u32) -> Color {
    static CACHE: std::sync::LazyLock<Mutex<HashMap<u32, u8>>> =
        std::sync::LazyLock::new(Default::default);
    let mut c = CACHE.lock().expect("named colors");
    Color::Indexed(*c.entry(rgb).or_insert_with(|| find_color(rgb, 256)))
}

/// Maps colors the way tcell's sendFgBg does for a given mode.
pub(crate) struct Fitter {
    mode: ColorMode,
    cache: Mutex<HashMap<Color, Color>>,
}

impl Fitter {
    pub(crate) fn new(mode: ColorMode) -> Fitter {
        Fitter {
            mode,
            cache: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn fit(&self, c: Color) -> Color {
        let rgb = match c {
            Color::Rgb(r, g, b) if !self.mode.truecolor => {
                ((r as u32) << 16) | ((g as u32) << 8) | b as u32
            }
            _ => match index_of(c) {
                Some(i) if self.mode.colors > 0 && u16::from(i) >= self.mode.colors => {
                    palette_rgb(i)
                }
                _ => return c,
            },
        };
        if self.mode.colors == 0 {
            return Color::Reset;
        }
        let mut cache = self.cache.lock().expect("color cache");
        *cache
            .entry(c)
            .or_insert_with(|| Color::Indexed(find_color(rgb, self.mode.colors)))
    }

    /// Fit every cell of a frame buffer.
    pub(crate) fn apply(&self, buf: &mut Buffer) {
        if self.mode.truecolor && self.mode.colors >= 256 {
            return;
        }
        for cell in buf.content.iter_mut() {
            cell.fg = self.fit(cell.fg);
            cell.bg = self.fit(cell.bg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_infocmp(_: &str) -> Option<u16> {
        None
    }

    #[test]
    fn detects_like_tcell() {
        // tmux-256color has no built-in entry: infocmp, no 24-bit color.
        assert_eq!(
            detect("tmux-256color", "truecolor", "", |_| Some(256)),
            ColorMode {
                truecolor: false,
                colors: 256
            }
        );
        // Built-in entry + COLORTERM: 24-bit.
        assert_eq!(
            detect("xterm-256color", "truecolor", "", no_infocmp),
            ColorMode {
                truecolor: true,
                colors: 256
            }
        );
        assert!(!detect("xterm-256color", "", "", no_infocmp).truecolor);
        assert!(!detect("xterm-256color", "truecolor", "disable", no_infocmp).truecolor);
        assert!(detect("xterm-256color", "", "on", no_infocmp).truecolor);
        // Entries flagged TrueColor, aliases, and -truecolor names.
        assert!(detect("ghostty", "", "", no_infocmp).truecolor);
        assert!(detect("xterm-truecolor", "", "", no_infocmp).truecolor);
        assert_eq!(detect("xterm", "", "", no_infocmp).colors, 8);
        assert_eq!(detect("vt100", "", "", no_infocmp).colors, 0);
    }

    #[test]
    fn named_colors_fit_to_256_palette() {
        // What k9s draws for [cadetblue::b] / ColorOrange with COLORTERM=truecolor.
        assert_eq!(named(0x5F9EA0), Color::Indexed(73));
        assert_eq!(named(0xFFA500), Color::Indexed(214));
        // Constants hard coded elsewhere (flash, header, dialogs, renderers).
        for (rgb, idx) in [
            (0xFFDEAD, 223),
            (0xFF4500, 202),
            (0x87CEEB, 117),
            (0x98FB98, 121),
            (0xC71585, 162),
            (0x7CFC00, 118),
            (0x00FA9A, 49),
        ] {
            assert_eq!(named(rgb), Color::Indexed(idx), "{rgb:06x}");
        }
    }

    #[test]
    fn fits_rgb_to_palette() {
        let f = Fitter::new(ColorMode {
            truecolor: false,
            colors: 256,
        });
        // What k9s draws in tmux: orange -> 214, dodgerblue -> 33,
        // cadetblue -> 73, white -> 15.
        assert_eq!(f.fit(Color::Rgb(0xFF, 0xA5, 0x00)), Color::Indexed(214));
        assert_eq!(f.fit(Color::Rgb(0x1E, 0x90, 0xFF)), Color::Indexed(33));
        assert_eq!(f.fit(Color::Rgb(0x5F, 0x9E, 0xA0)), Color::Indexed(73));
        assert_eq!(f.fit(Color::Rgb(0xFF, 0xFF, 0xFF)), Color::Indexed(15));
        assert_eq!(f.fit(Color::White), Color::White);
        assert_eq!(f.fit(Color::Reset), Color::Reset);

        let tc = Fitter::new(ColorMode {
            truecolor: true,
            colors: 256,
        });
        assert_eq!(tc.fit(Color::Rgb(1, 2, 3)), Color::Rgb(1, 2, 3));

        let eight = Fitter::new(ColorMode {
            truecolor: false,
            colors: 8,
        });
        assert_eq!(eight.fit(Color::LightRed), Color::Indexed(1));
        assert_eq!(eight.fit(Color::Green), Color::Green);
    }
}
