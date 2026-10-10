//! Resolved UI styles (shared contract). `Default` is the k9s built-in skin
//! (config/styles.go newStyle). Skin colors are always exact RGB, even the
//! 16 basic names (config.Color.Color() is GetColor(..).TrueColor()); the
//! screen fits them to the palette when the terminal lacks 24-bit color.

use ratatui::style::{Color, Style};

use super::skin::Skin;
use crate::render::Palette;

#[derive(Clone, Debug, PartialEq)]
pub struct Styles {
    /// The raw skin these styles were resolved from.
    pub skin: Skin,
    // body
    pub bg: Color,
    pub fg: Color,
    pub logo: Color,
    pub logo_msg: Color,
    pub logo_info: Color,
    pub logo_warn: Color,
    pub logo_error: Color,
    // frame
    pub border: Color,
    pub border_focus: Color,
    pub title_fg: Color,
    pub title_bg: Color,
    pub title_highlight: Color,
    pub title_counter: Color,
    pub title_filter: Color,
    pub menu_fg: Color,
    /// Menu description text style (frame.menu.fgStyle, dim by default).
    pub menu_fg_style: ratatui::style::Modifier,
    pub menu_key: Color,
    pub menu_num_key: Color,
    pub crumbs_fg: Color,
    pub crumbs_bg: Color,
    pub crumbs_active: Color,
    /// Row status colors (frame.status).
    pub palette: Palette,
    // info (header cluster info)
    pub info_fg: Color,
    pub info_section: Color,
    pub info_cpu: Color,
    pub info_mem: Color,
    pub info_k9s_rev: Color,
    // prompt
    pub prompt_fg: Color,
    pub prompt_bg: Color,
    pub prompt_suggest: Color,
    pub prompt_border_command: Color,
    pub prompt_border_default: Color,
    // help
    pub help_fg: Color,
    pub help_bg: Color,
    pub help_section: Color,
    pub help_key: Color,
    pub help_num_key: Color,
    // table
    pub table_fg: Color,
    pub table_bg: Color,
    pub table_cursor_fg: Color,
    pub table_cursor_bg: Color,
    pub table_mark: Color,
    pub table_header_fg: Color,
    pub table_header_bg: Color,
    pub table_sorter: Color,
    // xray
    pub xray_fg: Color,
    pub xray_bg: Color,
    pub xray_cursor: Color,
    pub xray_cursor_text: Color,
    pub xray_graphic: Color,
    // yaml
    pub yaml_key: Color,
    pub yaml_colon: Color,
    pub yaml_value: Color,
    // logs
    pub logs_fg: Color,
    pub logs_bg: Color,
    pub log_indicator_fg: Color,
    pub log_indicator_bg: Color,
    pub log_toggle_on: Color,
    pub log_toggle_off: Color,
    // dialog
    pub dialog_fg: Color,
    pub dialog_bg: Color,
    pub dialog_button_fg: Color,
    pub dialog_button_bg: Color,
    pub dialog_button_focus_fg: Color,
    pub dialog_button_focus_bg: Color,
    pub dialog_label_fg: Color,
    pub dialog_field_fg: Color,
    // picker (container/port pickers)
    pub picker_main: Color,
    pub picker_focus: Color,
    pub picker_shortcut: Color,
    // charts (pulses)
    pub chart_bg: Color,
    pub chart_dial_bg: Color,
    pub chart_focus_fg: Color,
    pub chart_focus_bg: Color,
    pub chart_default_dial: Vec<Color>,
    pub chart_default_chart: Vec<Color>,
    /// Keyed by `cpu` / `mem`.
    pub chart_resource_colors: std::collections::BTreeMap<String, Vec<Color>>,
}

const fn rgb(hex: u32) -> Color {
    Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

// tcell basic names, as TrueColor() values.
const BLACK: Color = rgb(0x000000);
const GREEN: Color = rgb(0x008000);
const GRAY: Color = rgb(0x808080);
const RED: Color = rgb(0xFF0000);
const YELLOW: Color = rgb(0xFFFF00);
const FUCHSIA: Color = rgb(0xFF00FF);
const AQUA: Color = rgb(0x00FFFF);
const WHITE: Color = rgb(0xFFFFFF);
// W3C extended names (exact RGB, as tcell).
const CADETBLUE: Color = rgb(0x5F9EA0);
const DARKSLATEBLUE: Color = rgb(0x483D8B);
const DODGERBLUE: Color = rgb(0x1E90FF);
const SEAGREEN: Color = rgb(0x2E8B57);
const PALEGREEN: Color = rgb(0x98FB98);
const ORANGERED: Color = rgb(0xFF4500);
const GOLDENROD: Color = rgb(0xDAA520);
const ORANGE: Color = rgb(0xFFA500);
const MEDIUMVIOLETRED: Color = rgb(0xC71585);
const LIGHTSKYBLUE: Color = rgb(0x87CEFA);
const GREENYELLOW: Color = rgb(0xADFF2F);
const DARKORANGE: Color = rgb(0xFF8C00);
const MEDIUMPURPLE: Color = rgb(0x9370DB);
const LIGHTSLATEGRAY: Color = rgb(0x778899);
const LIMEGREEN: Color = rgb(0x32CD32);
const STEELBLUE: Color = rgb(0x4682B4);
const PAPAYAWHIP: Color = rgb(0xFFEFD5);
const LAWNGREEN: Color = rgb(0x7CFC00);
const DARKTURQUOISE: Color = rgb(0x00CED1);

impl Default for Styles {
    fn default() -> Self {
        Styles {
            skin: Skin::default_skin(),
            bg: BLACK,
            fg: CADETBLUE,
            logo: ORANGE,
            logo_msg: WHITE,
            logo_info: GREEN,
            logo_warn: MEDIUMVIOLETRED,
            logo_error: RED,
            border: DODGERBLUE,
            border_focus: LIGHTSKYBLUE,
            title_fg: AQUA,
            title_bg: BLACK,
            title_highlight: FUCHSIA,
            title_counter: PAPAYAWHIP,
            title_filter: SEAGREEN,
            menu_fg: WHITE,
            menu_fg_style: ratatui::style::Modifier::DIM,
            menu_key: DODGERBLUE,
            menu_num_key: FUCHSIA,
            crumbs_fg: BLACK,
            crumbs_bg: AQUA,
            crumbs_active: ORANGE,
            palette: Palette::default(),
            info_fg: ORANGE,
            info_section: WHITE,
            info_cpu: LAWNGREEN,
            info_mem: DARKTURQUOISE,
            info_k9s_rev: AQUA,
            prompt_fg: CADETBLUE,
            prompt_bg: BLACK,
            prompt_suggest: DODGERBLUE,
            prompt_border_command: AQUA,
            prompt_border_default: SEAGREEN,
            help_fg: CADETBLUE,
            help_bg: BLACK,
            help_section: GREEN,
            help_key: DODGERBLUE,
            help_num_key: FUCHSIA,
            table_fg: AQUA,
            table_bg: BLACK,
            table_cursor_fg: BLACK,
            table_cursor_bg: AQUA,
            table_mark: PALEGREEN,
            table_header_fg: WHITE,
            table_header_bg: BLACK,
            table_sorter: AQUA,
            xray_fg: AQUA,
            xray_bg: BLACK,
            xray_cursor: DODGERBLUE,
            xray_cursor_text: BLACK,
            xray_graphic: CADETBLUE,
            yaml_key: STEELBLUE,
            yaml_colon: WHITE,
            yaml_value: PAPAYAWHIP,
            logs_fg: LIGHTSKYBLUE,
            logs_bg: BLACK,
            log_indicator_fg: DODGERBLUE,
            log_indicator_bg: BLACK,
            log_toggle_on: LIMEGREEN,
            log_toggle_off: GRAY,
            dialog_fg: CADETBLUE,
            dialog_bg: BLACK,
            dialog_button_fg: BLACK,
            dialog_button_bg: DARKSLATEBLUE,
            dialog_button_focus_fg: BLACK,
            dialog_button_focus_bg: DODGERBLUE,
            dialog_label_fg: WHITE,
            dialog_field_fg: WHITE,
            picker_main: WHITE,
            picker_focus: AQUA,
            picker_shortcut: AQUA,
            chart_bg: BLACK,
            chart_dial_bg: BLACK,
            chart_focus_fg: WHITE,
            chart_focus_bg: ORANGE,
            chart_default_dial: vec![PALEGREEN, ORANGERED],
            chart_default_chart: vec![PALEGREEN, ORANGERED],
            chart_resource_colors: [
                ("cpu".to_string(), vec![DODGERBLUE, DARKSLATEBLUE]),
                ("mem".to_string(), vec![YELLOW, GOLDENROD]),
            ]
            .into_iter()
            .collect(),
        }
    }
}

impl Styles {
    /// Base body style.
    pub fn body(&self) -> Style {
        Style::default().fg(self.fg).bg(self.bg)
    }
}

/// Default row status palette (frame.status in the default skin).
pub(crate) fn default_palette() -> Palette {
    Palette {
        std: LIGHTSKYBLUE,
        add: DODGERBLUE,
        modify: GREENYELLOW,
        err: ORANGERED,
        highlight: AQUA,
        kill: MEDIUMPURPLE,
        completed: LIGHTSLATEGRAY,
        pending: DARKORANGE,
    }
}
