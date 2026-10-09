//! Skins (config/styles.go + config/color.go): skins/*.yaml → ratatui
//! styles. `Styles` (styles.rs) is the resolved palette the UI draws with.
//!
//! Colors follow tcell: W3C names map to exact RGB, the 16 basic names
//! (`black` .. `white`) map to the terminal palette (ratatui named colors),
//! `#rrggbb` is RGB and anything else (`default`, `-`, unknown) is the
//! terminal default (`Color::Reset`).

use std::path::Path;

use anyhow::Result;
use ratatui::style::Color;

pub use super::styles::Styles;
use serde::{Deserialize, Serialize};

use super::k9s::{json, yaml};
use super::paths::{Env, Locations};
use crate::render::Palette;

/// The skins shipped in the k9s repo (`skins/*.yaml`), by name.
pub const BUNDLED_SKINS: &[(&str, &str)] = &[
    ("axual", include_str!("skins/axual.yaml")),
    ("black-and-wtf", include_str!("skins/black-and-wtf.yaml")),
    ("dracula", include_str!("skins/dracula.yaml")),
    (
        "everforest-dark",
        include_str!("skins/everforest-dark.yaml"),
    ),
    (
        "everforest-light",
        include_str!("skins/everforest-light.yaml"),
    ),
    (
        "gruvbox-dark-hard",
        include_str!("skins/gruvbox-dark-hard.yaml"),
    ),
    ("gruvbox-dark", include_str!("skins/gruvbox-dark.yaml")),
    (
        "gruvbox-light-hard",
        include_str!("skins/gruvbox-light-hard.yaml"),
    ),
    ("gruvbox-light", include_str!("skins/gruvbox-light.yaml")),
    (
        "gruvbox-material-dark-hard",
        include_str!("skins/gruvbox-material-dark-hard.yaml"),
    ),
    (
        "gruvbox-material-dark-medium",
        include_str!("skins/gruvbox-material-dark-medium.yaml"),
    ),
    (
        "gruvbox-material-dark-soft",
        include_str!("skins/gruvbox-material-dark-soft.yaml"),
    ),
    (
        "gruvbox-material-light-hard",
        include_str!("skins/gruvbox-material-light-hard.yaml"),
    ),
    (
        "gruvbox-material-light-medium",
        include_str!("skins/gruvbox-material-light-medium.yaml"),
    ),
    (
        "gruvbox-material-light-soft",
        include_str!("skins/gruvbox-material-light-soft.yaml"),
    ),
    ("in-the-navy", include_str!("skins/in-the-navy.yaml")),
    ("kanagawa", include_str!("skins/kanagawa.yaml")),
    ("kiss", include_str!("skins/kiss.yaml")),
    ("monokai", include_str!("skins/monokai.yaml")),
    ("narsingh", include_str!("skins/narsingh.yaml")),
    ("nightfox", include_str!("skins/nightfox.yaml")),
    ("nord", include_str!("skins/nord.yaml")),
    ("one-dark", include_str!("skins/one-dark.yaml")),
    ("red", include_str!("skins/red.yaml")),
    ("rose-pine-dawn", include_str!("skins/rose-pine-dawn.yaml")),
    ("rose-pine-moon", include_str!("skins/rose-pine-moon.yaml")),
    ("rose-pine", include_str!("skins/rose-pine.yaml")),
    ("snazzy", include_str!("skins/snazzy.yaml")),
    ("solarized-16", include_str!("skins/solarized-16.yaml")),
    ("solarized-dark", include_str!("skins/solarized-dark.yaml")),
    (
        "solarized-light",
        include_str!("skins/solarized-light.yaml"),
    ),
    ("stock", include_str!("skins/stock.yaml")),
    ("transparent", include_str!("skins/transparent.yaml")),
    ("vercel", include_str!("skins/vercel.yaml")),
];

/// Raw skin file (`k9s:` root). Field names match the upstream YAML schema.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Skin {
    pub k9s: SkinK9s,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SkinK9s {
    pub body: BodySkin,
    pub prompt: PromptSkin,
    pub help: HelpSkin,
    pub frame: FrameSkin,
    pub info: InfoSkin,
    pub views: ViewsSkin,
    pub dialog: DialogSkin,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BodySkin {
    pub fg_color: String,
    pub bg_color: String,
    pub logo_color: String,
    pub logo_color_msg: String,
    pub logo_color_info: String,
    pub logo_color_warn: String,
    pub logo_color_error: String,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PromptSkin {
    pub fg_color: String,
    pub bg_color: String,
    pub suggest_color: String,
    pub border: PromptBorder,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PromptBorder {
    pub command: String,
    pub default: String,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HelpSkin {
    pub fg_color: String,
    pub bg_color: String,
    pub section_color: String,
    pub key_color: String,
    pub num_key_color: String,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FrameSkin {
    pub title: TitleSkin,
    pub border: BorderSkin,
    pub menu: MenuSkin,
    pub crumbs: CrumbsSkin,
    pub status: StatusSkin,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TitleSkin {
    pub fg_color: String,
    pub bg_color: String,
    pub highlight_color: String,
    pub counter_color: String,
    pub filter_color: String,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BorderSkin {
    pub fg_color: String,
    pub focus_color: String,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MenuSkin {
    pub fg_color: String,
    /// Text style: `normal`, `bold` or `dim` (config.TextStyle).
    pub fg_style: String,
    pub key_color: String,
    pub num_key_color: String,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CrumbsSkin {
    pub fg_color: String,
    pub bg_color: String,
    pub active_color: String,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct StatusSkin {
    pub new_color: String,
    pub modify_color: String,
    pub add_color: String,
    pub pending_color: String,
    pub error_color: String,
    pub highlight_color: String,
    pub kill_color: String,
    pub completed_color: String,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct InfoSkin {
    pub section_color: String,
    pub fg_color: String,
    pub cpu_color: String,
    pub mem_color: String,
    pub k9s_rev_color: String,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ViewsSkin {
    pub table: TableSkin,
    pub xray: XraySkin,
    pub charts: ChartsSkin,
    pub yaml: YamlSkin,
    pub picker: PickerSkin,
    pub logs: LogsSkin,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TableSkin {
    pub fg_color: String,
    pub bg_color: String,
    pub cursor_fg_color: String,
    pub cursor_bg_color: String,
    pub mark_color: String,
    pub header: TableHeaderSkin,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TableHeaderSkin {
    pub fg_color: String,
    pub bg_color: String,
    pub sorter_color: String,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct XraySkin {
    pub fg_color: String,
    pub bg_color: String,
    pub cursor_color: String,
    pub cursor_text_color: String,
    pub graphic_color: String,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ChartsSkin {
    pub bg_color: String,
    pub dial_bg_color: String,
    pub chart_bg_color: String,
    pub default_dial_colors: Vec<String>,
    pub default_chart_colors: Vec<String>,
    pub resource_colors: std::collections::BTreeMap<String, Vec<String>>,
    pub focus_fg_color: String,
    pub focus_bg_color: String,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct YamlSkin {
    pub key_color: String,
    pub value_color: String,
    pub colon_color: String,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PickerSkin {
    pub main_color: String,
    pub focus_color: String,
    pub shortcut_color: String,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LogsSkin {
    pub fg_color: String,
    pub bg_color: String,
    pub indicator: LogIndicatorSkin,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LogIndicatorSkin {
    pub fg_color: String,
    pub bg_color: String,
    pub toggle_on_color: String,
    pub toggle_off_color: String,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DialogSkin {
    pub fg_color: String,
    pub bg_color: String,
    pub button_fg_color: String,
    pub button_bg_color: String,
    pub button_focus_fg_color: String,
    pub button_focus_bg_color: String,
    pub label_fg_color: String,
    pub field_fg_color: String,
}

impl MenuSkin {
    /// TextStyle.ToShortString: `-` normal, `b` bold, `d` dim (also the default).
    pub fn fg_style_short(&self) -> &'static str {
        match self.fg_style.as_str() {
            "normal" => "-",
            "bold" => "b",
            _ => "d",
        }
    }
}

fn s(v: &str) -> String {
    v.to_string()
}

fn colors(v: &[String]) -> Vec<Color> {
    v.iter().map(|c| parse_color(c)).collect()
}

impl Styles {
    /// Resolve a skin to concrete styles. `Styles::from_skin(Skin::default_skin())`
    /// matches `Styles::default()` (apart from the stored raw skin).
    pub fn from_skin(skin: Skin) -> Styles {
        let k = &skin.k9s;
        let c = parse_color;
        let st = &k.frame.status;
        let ch = &k.views.charts;
        Styles {
            bg: c(&k.body.bg_color),
            fg: c(&k.body.fg_color),
            logo: c(&k.body.logo_color),
            logo_msg: c(&k.body.logo_color_msg),
            logo_info: c(&k.body.logo_color_info),
            logo_warn: c(&k.body.logo_color_warn),
            logo_error: c(&k.body.logo_color_error),
            border: c(&k.frame.border.fg_color),
            border_focus: c(&k.frame.border.focus_color),
            title_fg: c(&k.frame.title.fg_color),
            title_bg: c(&k.frame.title.bg_color),
            title_highlight: c(&k.frame.title.highlight_color),
            title_counter: c(&k.frame.title.counter_color),
            title_filter: c(&k.frame.title.filter_color),
            menu_fg: c(&k.frame.menu.fg_color),
            menu_fg_style: match k.frame.menu.fg_style_short() {
                "-" => ratatui::style::Modifier::empty(),
                "b" => ratatui::style::Modifier::BOLD,
                _ => ratatui::style::Modifier::DIM,
            },
            menu_key: c(&k.frame.menu.key_color),
            menu_num_key: c(&k.frame.menu.num_key_color),
            crumbs_fg: c(&k.frame.crumbs.fg_color),
            crumbs_bg: c(&k.frame.crumbs.bg_color),
            crumbs_active: c(&k.frame.crumbs.active_color),
            palette: Palette {
                std: c(&st.new_color),
                add: c(&st.add_color),
                modify: c(&st.modify_color),
                err: c(&st.error_color),
                highlight: c(&st.highlight_color),
                kill: c(&st.kill_color),
                completed: c(&st.completed_color),
                pending: c(&st.pending_color),
            },
            info_fg: c(&k.info.fg_color),
            info_section: c(&k.info.section_color),
            info_cpu: c(&k.info.cpu_color),
            info_mem: c(&k.info.mem_color),
            info_k9s_rev: c(&k.info.k9s_rev_color),
            prompt_fg: c(&k.prompt.fg_color),
            prompt_bg: c(&k.prompt.bg_color),
            prompt_suggest: c(&k.prompt.suggest_color),
            prompt_border_command: c(&k.prompt.border.command),
            prompt_border_default: c(&k.prompt.border.default),
            help_fg: c(&k.help.fg_color),
            help_bg: c(&k.help.bg_color),
            help_section: c(&k.help.section_color),
            help_key: c(&k.help.key_color),
            help_num_key: c(&k.help.num_key_color),
            table_fg: c(&k.views.table.fg_color),
            table_bg: c(&k.views.table.bg_color),
            table_cursor_fg: c(&k.views.table.cursor_fg_color),
            table_cursor_bg: c(&k.views.table.cursor_bg_color),
            table_mark: c(&k.views.table.mark_color),
            table_header_fg: c(&k.views.table.header.fg_color),
            table_header_bg: c(&k.views.table.header.bg_color),
            table_sorter: c(&k.views.table.header.sorter_color),
            xray_fg: c(&k.views.xray.fg_color),
            xray_bg: c(&k.views.xray.bg_color),
            xray_cursor: c(&k.views.xray.cursor_color),
            xray_cursor_text: c(&k.views.xray.cursor_text_color),
            xray_graphic: c(&k.views.xray.graphic_color),
            yaml_key: c(&k.views.yaml.key_color),
            yaml_colon: c(&k.views.yaml.colon_color),
            yaml_value: c(&k.views.yaml.value_color),
            logs_fg: c(&k.views.logs.fg_color),
            logs_bg: c(&k.views.logs.bg_color),
            log_indicator_fg: c(&k.views.logs.indicator.fg_color),
            log_indicator_bg: c(&k.views.logs.indicator.bg_color),
            log_toggle_on: c(&k.views.logs.indicator.toggle_on_color),
            log_toggle_off: c(&k.views.logs.indicator.toggle_off_color),
            dialog_fg: c(&k.dialog.fg_color),
            dialog_bg: c(&k.dialog.bg_color),
            dialog_button_fg: c(&k.dialog.button_fg_color),
            dialog_button_bg: c(&k.dialog.button_bg_color),
            dialog_button_focus_fg: c(&k.dialog.button_focus_fg_color),
            dialog_button_focus_bg: c(&k.dialog.button_focus_bg_color),
            dialog_label_fg: c(&k.dialog.label_fg_color),
            dialog_field_fg: c(&k.dialog.field_fg_color),
            picker_main: c(&k.views.picker.main_color),
            picker_focus: c(&k.views.picker.focus_color),
            picker_shortcut: c(&k.views.picker.shortcut_color),
            chart_bg: c(&ch.bg_color),
            chart_dial_bg: c(&ch.dial_bg_color),
            chart_focus_fg: c(&ch.focus_fg_color),
            chart_focus_bg: c(&ch.focus_bg_color),
            chart_default_dial: colors(&ch.default_dial_colors),
            chart_default_chart: colors(&ch.default_chart_colors),
            chart_resource_colors: ch
                .resource_colors
                .iter()
                .map(|(k, v)| (k.clone(), colors(v)))
                .collect(),
            skin,
        }
    }

    /// Load `<config>/skins/<name>.yaml` over the default skin.
    pub fn load_skin(name: &str) -> Result<Styles> {
        let path = Locations::current().skin_file_from_name(name);
        Ok(Styles::from_skin(Skin::load_from(&path)?))
    }

    /// Invert light<->dark (the `--invert` flag), keeping hues (Oklch).
    pub fn invert(&mut self) {
        let base = if self.skin == Skin::default() {
            Skin::default_skin()
        } else {
            self.skin.clone()
        };
        *self = Styles::from_skin(base.inverted());
    }

    /// Resolve the active skin like k9s (ui.Configurator.loadSkinFile):
    /// `$K9S_SKIN`, then the context skin, then `k9s.ui.skin` (each only if
    /// its file exists in the skins dir), inverted when asked.
    pub fn load_active(
        locs: &Locations,
        env: &dyn Env,
        ui_skin: &str,
        ctx_skin: Option<&str>,
        invert: bool,
    ) -> Styles {
        let Some(name) = active_skin(locs, env, ui_skin, ctx_skin) else {
            tracing::debug!("No custom skin found. Using stock skin");
            return stock(invert);
        };
        let file = locs.skin_file_from_name(&name);
        tracing::debug!(skin = %file.display(), "Loading skin file");
        match Skin::load_from(&file) {
            Ok(sk) => Styles::from_skin(if invert { sk.inverted() } else { sk }),
            Err(e) => {
                let missing = e
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound);
                if missing {
                    tracing::warn!(skin = %name, dir = %locs.skins_dir.display(), error = %e, "Skin file not found in skins dir");
                    stock(invert)
                } else {
                    // k9s keeps the current (stock) styles, not inverted.
                    tracing::error!(path = %name, error = %e, "Failed to parse skin file");
                    stock(false)
                }
            }
        }
    }
}

fn stock(invert: bool) -> Styles {
    let sk = Skin::default_skin();
    Styles::from_skin(if invert { sk.inverted() } else { sk })
}

/// The active skin name (ui.Configurator.activeSkin).
pub fn active_skin(
    locs: &Locations,
    env: &dyn Env,
    ui_skin: &str,
    ctx_skin: Option<&str>,
) -> Option<String> {
    let exists = |n: &str| locs.skin_file_from_name(n).exists();
    if let Some(sk) = env.get_set("K9S_SKIN") {
        if exists(&sk) {
            tracing::debug!(skin = %sk, "Loading env skin");
            return Some(sk);
        }
    }
    if let Some(sk) = ctx_skin.filter(|s| !s.is_empty()) {
        if exists(sk) {
            tracing::debug!(skin = %sk, "Loading context skin");
            return Some(sk.to_string());
        }
    }
    if !ui_skin.is_empty() && exists(ui_skin) {
        tracing::debug!(skin = %ui_skin, "Loading global skin");
        return Some(ui_skin.to_string());
    }
    None
}

impl Skin {
    /// Built-in default skin values (config.newStyle in styles.go).
    pub fn default_skin() -> Skin {
        Skin {
            k9s: SkinK9s {
                body: BodySkin {
                    fg_color: s("cadetblue"),
                    bg_color: s("black"),
                    logo_color: s("orange"),
                    logo_color_msg: s("white"),
                    logo_color_info: s("green"),
                    logo_color_warn: s("mediumvioletred"),
                    logo_color_error: s("red"),
                },
                prompt: PromptSkin {
                    fg_color: s("cadetblue"),
                    bg_color: s("black"),
                    suggest_color: s("dodgerblue"),
                    border: PromptBorder {
                        command: s("aqua"),
                        default: s("seagreen"),
                    },
                },
                help: HelpSkin {
                    fg_color: s("cadetblue"),
                    bg_color: s("black"),
                    section_color: s("green"),
                    key_color: s("dodgerblue"),
                    num_key_color: s("fuchsia"),
                },
                frame: FrameSkin {
                    title: TitleSkin {
                        fg_color: s("aqua"),
                        bg_color: s("black"),
                        highlight_color: s("fuchsia"),
                        counter_color: s("papayawhip"),
                        filter_color: s("seagreen"),
                    },
                    border: BorderSkin {
                        fg_color: s("dodgerblue"),
                        focus_color: s("lightskyblue"),
                    },
                    menu: MenuSkin {
                        fg_color: s("white"),
                        fg_style: String::new(),
                        key_color: s("dodgerblue"),
                        num_key_color: s("fuchsia"),
                    },
                    crumbs: CrumbsSkin {
                        fg_color: s("black"),
                        bg_color: s("aqua"),
                        active_color: s("orange"),
                    },
                    status: StatusSkin {
                        new_color: s("lightskyblue"),
                        modify_color: s("greenyellow"),
                        add_color: s("dodgerblue"),
                        pending_color: s("darkorange"),
                        error_color: s("orangered"),
                        highlight_color: s("aqua"),
                        kill_color: s("mediumpurple"),
                        completed_color: s("lightslategray"),
                    },
                },
                info: InfoSkin {
                    section_color: s("white"),
                    fg_color: s("orange"),
                    cpu_color: s("lawngreen"),
                    mem_color: s("darkturquoise"),
                    k9s_rev_color: s("aqua"),
                },
                views: ViewsSkin {
                    table: TableSkin {
                        fg_color: s("aqua"),
                        bg_color: s("black"),
                        cursor_fg_color: s("black"),
                        cursor_bg_color: s("aqua"),
                        mark_color: s("palegreen"),
                        header: TableHeaderSkin {
                            fg_color: s("white"),
                            bg_color: s("black"),
                            sorter_color: s("aqua"),
                        },
                    },
                    xray: XraySkin {
                        fg_color: s("aqua"),
                        bg_color: s("black"),
                        cursor_color: s("dodgerblue"),
                        cursor_text_color: s("black"),
                        graphic_color: s("cadetblue"),
                    },
                    charts: ChartsSkin {
                        bg_color: s("black"),
                        dial_bg_color: s("black"),
                        chart_bg_color: s("black"),
                        default_dial_colors: vec![s("palegreen"), s("orangered")],
                        default_chart_colors: vec![s("palegreen"), s("orangered")],
                        resource_colors: [
                            (s("cpu"), vec![s("dodgerblue"), s("darkslateblue")]),
                            (s("mem"), vec![s("yellow"), s("goldenrod")]),
                        ]
                        .into_iter()
                        .collect(),
                        focus_fg_color: s("white"),
                        focus_bg_color: s("orange"),
                    },
                    yaml: YamlSkin {
                        key_color: s("steelblue"),
                        value_color: s("papayawhip"),
                        colon_color: s("white"),
                    },
                    picker: PickerSkin {
                        main_color: s("white"),
                        focus_color: s("aqua"),
                        shortcut_color: s("aqua"),
                    },
                    logs: LogsSkin {
                        fg_color: s("lightskyblue"),
                        bg_color: s("black"),
                        indicator: LogIndicatorSkin {
                            fg_color: s("dodgerblue"),
                            bg_color: s("black"),
                            toggle_on_color: s("limegreen"),
                            toggle_off_color: s("gray"),
                        },
                    },
                },
                dialog: DialogSkin {
                    fg_color: s("cadetblue"),
                    bg_color: s("black"),
                    button_fg_color: s("black"),
                    button_bg_color: s("darkslateblue"),
                    button_focus_fg_color: s("black"),
                    button_focus_bg_color: s("dodgerblue"),
                    label_fg_color: s("white"),
                    field_fg_color: s("white"),
                },
            },
        }
    }

    /// Load a skin file over the default skin (Styles.Load): the file must
    /// pass the skin schema; missing values keep their defaults.
    pub fn load_from(path: &Path) -> Result<Skin> {
        Skin::load_over(&Skin::default_skin(), path)
    }

    /// Load a skin file over `base`.
    pub fn load_over(base: &Skin, path: &Path) -> Result<Skin> {
        let bb = std::fs::read_to_string(path)?;
        Skin::parse_over(base, &bb)
    }

    /// Parse skin YAML over `base`.
    pub fn parse_over(base: &Skin, bb: &str) -> Result<Skin> {
        json::validate(json::SKIN_SCHEMA, bb)?;
        let over = yaml::parse_value(bb)?;
        let mut v = serde_yaml::to_value(base)?;
        if let Some(k) = over.get("k9s") {
            if let Some(root) = v.get_mut("k9s") {
                yaml::merge_value(root, k.clone());
            }
        }
        Ok(serde_yaml::from_value(v)?)
    }

    /// A bundled k9s skin by name (`dracula`, `nord`, ...), over the default.
    pub fn bundled(name: &str) -> Option<Skin> {
        BUNDLED_SKINS
            .iter()
            .find(|(n, _)| *n == name)
            .and_then(|(_, src)| Skin::parse_over(&Skin::default_skin(), src).ok())
    }

    /// Every color inverted (Style.Invert).
    pub fn inverted(&self) -> Skin {
        let mut sk = self.clone();
        let k = &mut sk.k9s;
        let inv = |c: &mut String| *c = invert_color(c);
        for c in [
            &mut k.body.fg_color,
            &mut k.body.bg_color,
            &mut k.body.logo_color,
            &mut k.body.logo_color_msg,
            &mut k.body.logo_color_info,
            &mut k.body.logo_color_warn,
            &mut k.body.logo_color_error,
            &mut k.prompt.fg_color,
            &mut k.prompt.bg_color,
            &mut k.prompt.suggest_color,
            &mut k.prompt.border.command,
            &mut k.prompt.border.default,
            &mut k.help.fg_color,
            &mut k.help.bg_color,
            &mut k.help.section_color,
            &mut k.help.key_color,
            &mut k.help.num_key_color,
            &mut k.frame.title.fg_color,
            &mut k.frame.title.bg_color,
            &mut k.frame.title.highlight_color,
            &mut k.frame.title.counter_color,
            &mut k.frame.title.filter_color,
            &mut k.frame.border.fg_color,
            &mut k.frame.border.focus_color,
            &mut k.frame.menu.fg_color,
            &mut k.frame.menu.key_color,
            &mut k.frame.menu.num_key_color,
            &mut k.frame.crumbs.fg_color,
            &mut k.frame.crumbs.bg_color,
            &mut k.frame.crumbs.active_color,
            &mut k.frame.status.new_color,
            &mut k.frame.status.modify_color,
            &mut k.frame.status.add_color,
            &mut k.frame.status.pending_color,
            &mut k.frame.status.error_color,
            &mut k.frame.status.highlight_color,
            &mut k.frame.status.kill_color,
            &mut k.frame.status.completed_color,
            &mut k.info.section_color,
            &mut k.info.fg_color,
            &mut k.info.cpu_color,
            &mut k.info.mem_color,
            &mut k.info.k9s_rev_color,
            &mut k.views.table.fg_color,
            &mut k.views.table.bg_color,
            &mut k.views.table.cursor_fg_color,
            &mut k.views.table.cursor_bg_color,
            &mut k.views.table.mark_color,
            &mut k.views.table.header.fg_color,
            &mut k.views.table.header.bg_color,
            &mut k.views.table.header.sorter_color,
            &mut k.views.xray.fg_color,
            &mut k.views.xray.bg_color,
            &mut k.views.xray.cursor_color,
            &mut k.views.xray.cursor_text_color,
            &mut k.views.xray.graphic_color,
            &mut k.views.charts.bg_color,
            &mut k.views.charts.dial_bg_color,
            &mut k.views.charts.chart_bg_color,
            &mut k.views.charts.focus_fg_color,
            &mut k.views.charts.focus_bg_color,
            &mut k.views.yaml.key_color,
            &mut k.views.yaml.value_color,
            &mut k.views.yaml.colon_color,
            &mut k.views.picker.main_color,
            &mut k.views.picker.focus_color,
            &mut k.views.picker.shortcut_color,
            &mut k.views.logs.fg_color,
            &mut k.views.logs.bg_color,
            &mut k.views.logs.indicator.fg_color,
            &mut k.views.logs.indicator.bg_color,
            &mut k.views.logs.indicator.toggle_on_color,
            &mut k.views.logs.indicator.toggle_off_color,
            &mut k.dialog.fg_color,
            &mut k.dialog.bg_color,
            &mut k.dialog.button_fg_color,
            &mut k.dialog.button_bg_color,
            &mut k.dialog.button_focus_fg_color,
            &mut k.dialog.button_focus_bg_color,
            &mut k.dialog.label_fg_color,
            &mut k.dialog.field_fg_color,
        ] {
            inv(c);
        }
        for c in k
            .views
            .charts
            .default_dial_colors
            .iter_mut()
            .chain(k.views.charts.default_chart_colors.iter_mut())
            .chain(k.views.charts.resource_colors.values_mut().flatten())
        {
            inv(c);
        }
        sk
    }
}

// ---- colors -----------------------------------------------------------------

/// tcell's 16 basic colors: (name, palette color, rgb).
const BASIC: &[(&str, Color, u32)] = &[
    ("black", Color::Black, 0x000000),
    ("maroon", Color::Red, 0x800000),
    ("green", Color::Green, 0x008000),
    ("olive", Color::Yellow, 0x808000),
    ("navy", Color::Blue, 0x000080),
    ("purple", Color::Magenta, 0x800080),
    ("teal", Color::Cyan, 0x008080),
    ("silver", Color::Gray, 0xc0c0c0),
    ("gray", Color::DarkGray, 0x808080),
    ("grey", Color::DarkGray, 0x808080),
    ("red", Color::LightRed, 0xff0000),
    ("lime", Color::LightGreen, 0x00ff00),
    ("yellow", Color::LightYellow, 0xffff00),
    ("blue", Color::LightBlue, 0x0000ff),
    ("fuchsia", Color::LightMagenta, 0xff00ff),
    ("aqua", Color::LightCyan, 0x00ffff),
    ("white", Color::White, 0xffffff),
];

/// tcell's W3C extended color names.
const W3C: &[(&str, u32)] = &[
    ("aliceblue", 0xf0f8ff),
    ("antiquewhite", 0xfaebd7),
    ("aquamarine", 0x7fffd4),
    ("azure", 0xf0ffff),
    ("beige", 0xf5f5dc),
    ("bisque", 0xffe4c4),
    ("blanchedalmond", 0xffebcd),
    ("blueviolet", 0x8a2be2),
    ("brown", 0xa52a2a),
    ("burlywood", 0xdeb887),
    ("cadetblue", 0x5f9ea0),
    ("chartreuse", 0x7fff00),
    ("chocolate", 0xd2691e),
    ("coral", 0xff7f50),
    ("cornflowerblue", 0x6495ed),
    ("cornsilk", 0xfff8dc),
    ("crimson", 0xdc143c),
    ("darkblue", 0x00008b),
    ("darkcyan", 0x008b8b),
    ("darkgoldenrod", 0xb8860b),
    ("darkgray", 0xa9a9a9),
    ("darkgreen", 0x006400),
    ("darkkhaki", 0xbdb76b),
    ("darkmagenta", 0x8b008b),
    ("darkolivegreen", 0x556b2f),
    ("darkorange", 0xff8c00),
    ("darkorchid", 0x9932cc),
    ("darkred", 0x8b0000),
    ("darksalmon", 0xe9967a),
    ("darkseagreen", 0x8fbc8f),
    ("darkslateblue", 0x483d8b),
    ("darkslategray", 0x2f4f4f),
    ("darkturquoise", 0x00ced1),
    ("darkviolet", 0x9400d3),
    ("deeppink", 0xff1493),
    ("deepskyblue", 0x00bfff),
    ("dimgray", 0x696969),
    ("dodgerblue", 0x1e90ff),
    ("firebrick", 0xb22222),
    ("floralwhite", 0xfffaf0),
    ("forestgreen", 0x228b22),
    ("gainsboro", 0xdcdcdc),
    ("ghostwhite", 0xf8f8ff),
    ("gold", 0xffd700),
    ("goldenrod", 0xdaa520),
    ("greenyellow", 0xadff2f),
    ("honeydew", 0xf0fff0),
    ("hotpink", 0xff69b4),
    ("indianred", 0xcd5c5c),
    ("indigo", 0x4b0082),
    ("ivory", 0xfffff0),
    ("khaki", 0xf0e68c),
    ("lavender", 0xe6e6fa),
    ("lavenderblush", 0xfff0f5),
    ("lawngreen", 0x7cfc00),
    ("lemonchiffon", 0xfffacd),
    ("lightblue", 0xadd8e6),
    ("lightcoral", 0xf08080),
    ("lightcyan", 0xe0ffff),
    ("lightgoldenrodyellow", 0xfafad2),
    ("lightgray", 0xd3d3d3),
    ("lightgreen", 0x90ee90),
    ("lightpink", 0xffb6c1),
    ("lightsalmon", 0xffa07a),
    ("lightseagreen", 0x20b2aa),
    ("lightskyblue", 0x87cefa),
    ("lightslategray", 0x778899),
    ("lightsteelblue", 0xb0c4de),
    ("lightyellow", 0xffffe0),
    ("limegreen", 0x32cd32),
    ("linen", 0xfaf0e6),
    ("mediumaquamarine", 0x66cdaa),
    ("mediumblue", 0x0000cd),
    ("mediumorchid", 0xba55d3),
    ("mediumpurple", 0x9370db),
    ("mediumseagreen", 0x3cb371),
    ("mediumslateblue", 0x7b68ee),
    ("mediumspringgreen", 0x00fa9a),
    ("mediumturquoise", 0x48d1cc),
    ("mediumvioletred", 0xc71585),
    ("midnightblue", 0x191970),
    ("mintcream", 0xf5fffa),
    ("mistyrose", 0xffe4e1),
    ("moccasin", 0xffe4b5),
    ("navajowhite", 0xffdead),
    ("oldlace", 0xfdf5e6),
    ("olivedrab", 0x6b8e23),
    ("orange", 0xffa500),
    ("orangered", 0xff4500),
    ("orchid", 0xda70d6),
    ("palegoldenrod", 0xeee8aa),
    ("palegreen", 0x98fb98),
    ("paleturquoise", 0xafeeee),
    ("palevioletred", 0xdb7093),
    ("papayawhip", 0xffefd5),
    ("peachpuff", 0xffdab9),
    ("peru", 0xcd853f),
    ("pink", 0xffc0cb),
    ("plum", 0xdda0dd),
    ("powderblue", 0xb0e0e6),
    ("rebeccapurple", 0x663399),
    ("rosybrown", 0xbc8f8f),
    ("royalblue", 0x4169e1),
    ("saddlebrown", 0x8b4513),
    ("salmon", 0xfa8072),
    ("sandybrown", 0xf4a460),
    ("seagreen", 0x2e8b57),
    ("seashell", 0xfff5ee),
    ("sienna", 0xa0522d),
    ("skyblue", 0x87ceeb),
    ("slateblue", 0x6a5acd),
    ("slategray", 0x708090),
    ("snow", 0xfffafa),
    ("springgreen", 0x00ff7f),
    ("steelblue", 0x4682b4),
    ("tan", 0xd2b48c),
    ("thistle", 0xd8bfd8),
    ("tomato", 0xff6347),
    ("turquoise", 0x40e0d0),
    ("violet", 0xee82ee),
    ("wheat", 0xf5deb3),
    ("whitesmoke", 0xf5f5f5),
    ("yellowgreen", 0x9acd32),
    ("darkgrey", 0xa9a9a9),
    ("darkslategrey", 0x2f4f4f),
    ("dimgrey", 0x696969),
    ("lightgrey", 0xd3d3d3),
    ("lightslategrey", 0x778899),
    ("slategrey", 0x708090),
];

const fn rgb(hex: u32) -> Color {
    Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

fn parse_hex(s: &str) -> Option<u32> {
    if s.len() != 7 || !s.starts_with('#') {
        return None;
    }
    let v = i64::from_str_radix(&s[1..], 16).ok()?;
    (0..=0xff_ffff).contains(&v).then_some(v as u32)
}

/// Parse a k9s color string (tcell.GetColor): names (`white`, `dodgerblue`),
/// `#rrggbb`; `default`, `-`, `transparent` and anything unknown are the
/// terminal default.
pub fn parse_color(s: &str) -> Color {
    if let Some((_, c, _)) = BASIC.iter().find(|(n, _, _)| *n == s) {
        return *c;
    }
    if let Some((_, v)) = W3C.iter().find(|(n, _)| *n == s) {
        return rgb(*v);
    }
    match parse_hex(s) {
        Some(v) => rgb(v),
        None => Color::Reset,
    }
}

/// RGB value of a color string, `None` for the terminal default
/// (tcell Color.TrueColor().Hex()).
pub fn color_rgb(s: &str) -> Option<u32> {
    if s == "default" {
        return None;
    }
    if let Some((_, _, v)) = BASIC.iter().find(|(n, _, _)| *n == s) {
        return Some(*v);
    }
    if let Some((_, v)) = W3C.iter().find(|(n, _)| *n == s) {
        return Some(*v);
    }
    parse_hex(s)
}

/// config.Color.String: hex as is, `-` for default/unknown, else `#rrggbb`.
pub fn color_string(s: &str) -> String {
    if s.len() == 7 && s.starts_with('#') {
        return s.to_string();
    }
    match color_rgb(s) {
        Some(v) => format!("#{v:06x}"),
        None => "-".into(),
    }
}

// go-colorful Oklab/Oklch conversions.

#[derive(Clone, Copy, Debug)]
struct Rgb(f64, f64, f64);

fn linearize(v: f64) -> f64 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn delinearize(v: f64) -> f64 {
    if v <= 0.0031308 {
        12.92 * v
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

impl Rgb {
    fn from_hex(v: u32) -> Rgb {
        Rgb(
            ((v >> 16) & 0xff) as f64 / 255.0,
            ((v >> 8) & 0xff) as f64 / 255.0,
            (v & 0xff) as f64 / 255.0,
        )
    }

    #[allow(clippy::excessive_precision)]
    fn ok_lch(self) -> (f64, f64, f64) {
        let (r, g, b) = (linearize(self.0), linearize(self.1), linearize(self.2));
        let x = 0.41239079926595948 * r + 0.35758433938387796 * g + 0.18048078840183429 * b;
        let y = 0.21263900587151036 * r + 0.71516867876775593 * g + 0.072192315360733715 * b;
        let z = 0.019330818715591851 * r + 0.11919477979462599 * g + 0.95053215224966058 * b;
        let l_ = (0.8189330101 * x + 0.3618667424 * y - 0.1288597137 * z).cbrt();
        let m_ = (0.0329845436 * x + 0.9293118715 * y + 0.0361456387 * z).cbrt();
        let s_ = (0.0482003018 * x + 0.2643662691 * y + 0.6338517070 * z).cbrt();
        let l = 0.2104542553 * l_ + 0.7936177850 * m_ - 0.0040720468 * s_;
        let a = 1.9779984951 * l_ - 2.4285922050 * m_ + 0.4505937099 * s_;
        let bb = 0.0259040371 * l_ + 0.7827717662 * m_ - 0.8086757660 * s_;
        let c = (a * a + bb * bb).sqrt();
        let mut h = bb.atan2(a);
        if h < 0.0 {
            h += 2.0 * std::f64::consts::PI;
        }
        (l, c, h * 180.0 / std::f64::consts::PI)
    }

    #[allow(clippy::excessive_precision)]
    fn from_ok_lch(l: f64, c: f64, h: f64) -> Rgb {
        let h = h * std::f64::consts::PI / 180.0;
        let a = c * h.cos();
        let b = c * h.sin();
        let l_ = 0.9999999984505196 * l + 0.39633779217376774 * a + 0.2158037580607588 * b;
        let m_ = 1.0000000088817607 * l - 0.10556134232365633 * a - 0.0638541747717059 * b;
        let s_ = 1.0000000546724108 * l - 0.08948418209496574 * a - 1.2914855378640917 * b;
        let ll = l_ * l_ * l_;
        let m = m_ * m_ * m_;
        let s = s_ * s_ * s_;
        let x = 1.2268798733741557 * ll - 0.5578149965554813 * m + 0.28139105017721583 * s;
        let y = -0.04057576262431372 * ll + 1.1122868293970594 * m - 0.07171106666151701 * s;
        let z = -0.07637294974672142 * ll - 0.4214933239627914 * m + 1.5869240244272418 * s;
        let r = 3.2409699419045214 * x - 1.5373831775700935 * y - 0.49861076029300328 * z;
        let g = -0.96924363628087983 * x + 1.8759675015077207 * y + 0.041555057407175613 * z;
        let bl = 0.055630079696993609 * x - 0.20397695888897657 * y + 1.0569715142428786 * z;
        Rgb(delinearize(r), delinearize(g), delinearize(bl))
    }

    fn is_valid(self) -> bool {
        (0.0..=1.0).contains(&self.0)
            && (0.0..=1.0).contains(&self.1)
            && (0.0..=1.0).contains(&self.2)
    }

    fn clamped(self) -> Rgb {
        let c = |v: f64| v.clamp(0.0, 1.0);
        Rgb(c(self.0), c(self.1), c(self.2))
    }

    fn hex(self) -> String {
        let q = |v: f64| (v * 255.0 + 0.5) as u8;
        format!("#{:02x}{:02x}{:02x}", q(self.0), q(self.1), q(self.2))
    }
}

fn max_chroma_for_lh(l: f64, h: f64) -> f64 {
    let (mut lo, mut hi) = (0.0, 0.4);
    while hi - lo > 0.001 {
        let mid = (lo + hi) / 2.0;
        if Rgb::from_ok_lch(l, mid, h).is_valid() {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

const CHROMA_PRESERVE_FACTOR: f64 = 0.5;

fn closest_l_for_chroma(target_l: f64, c: f64, h: f64) -> f64 {
    if max_chroma_for_lh(target_l, h) >= c {
        return target_l;
    }
    if target_l < 0.5 {
        let mut ll = target_l;
        while ll <= 0.5 {
            if max_chroma_for_lh(ll, h) >= c {
                return ll;
            }
            ll += 0.01;
        }
        let mut ll = 0.51;
        while ll <= 0.95 {
            if max_chroma_for_lh(ll, h) >= c {
                return ll;
            }
            ll += 0.01;
        }
    } else {
        let mut ll = target_l;
        while ll >= 0.5 {
            if max_chroma_for_lh(ll, h) >= c {
                return ll;
            }
            ll -= 0.01;
        }
        let mut ll = 0.49;
        while ll >= 0.05 {
            if max_chroma_for_lh(ll, h) >= c {
                return ll;
            }
            ll -= 0.01;
        }
    }
    target_l
}

/// config.Color.InvertColor: invert Oklch lightness keeping hue and as much
/// chroma as the sRGB gamut allows. `default`, `-` and blank are unchanged.
pub fn invert_color(c: &str) -> String {
    if c == "default" || c == "-" || c.is_empty() {
        return c.to_string();
    }
    let Some(hex) = color_rgb(c) else {
        return c.to_string();
    };
    let (l, ch, h) = Rgb::from_hex(hex).ok_lch();
    if ch < 0.01 {
        return Rgb::from_ok_lch(1.0 - l, 0.0, h).clamped().hex();
    }
    let target_l = 1.0 - l;
    let min_c = ch * CHROMA_PRESERVE_FACTOR;
    let actual_l = closest_l_for_chroma(target_l, min_c, h);
    let max_c = max_chroma_for_lh(actual_l, h);
    let actual_c = if max_c < ch { max_c } else { ch };
    Rgb::from_ok_lch(actual_l, actual_c, h).clamped().hex()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::paths::{MapEnv, Xdg, K9S_ENV_CONFIG_DIR};

    fn td(p: &str) -> std::path::PathBuf {
        format!(
            "{}/src/config/testdata/config/skins/{p}",
            env!("CARGO_MANIFEST_DIR")
        )
        .into()
    }

    #[test]
    fn default_skin_matches_default_styles() {
        let st = Styles::from_skin(Skin::default_skin());
        assert_eq!(st.skin, Skin::default_skin());
        let d = Styles {
            skin: Skin::default_skin(),
            ..Styles::default()
        };
        assert_eq!(st, d);
    }

    #[test]
    fn stock_template_matches_default_skin() {
        let stock =
            Skin::parse_over(&Skin::default(), crate::config::paths::STOCK_SKIN_TPL).unwrap();
        let mut d = Skin::default_skin();
        // The stock template has no info cpu/mem/rev colors.
        d.k9s.info.cpu_color.clear();
        d.k9s.info.mem_color.clear();
        d.k9s.info.k9s_rev_color.clear();
        assert_eq!(stock, d);
    }

    #[test]
    fn colors() {
        assert_eq!(parse_color("blah"), Color::Reset);
        assert_eq!(parse_color("default"), Color::Reset);
        assert_eq!(parse_color("-"), Color::Reset);
        assert_eq!(parse_color("transparent"), Color::Reset);
        assert_eq!(parse_color(""), Color::Reset);
        assert_eq!(parse_color("blue"), Color::LightBlue);
        assert_eq!(parse_color("green"), Color::Green);
        assert_eq!(parse_color("gray"), Color::DarkGray);
        assert_eq!(parse_color("aqua"), Color::LightCyan);
        assert_eq!(parse_color("fuchsia"), Color::LightMagenta);
        assert_eq!(parse_color("dodgerblue"), Color::Rgb(0x1e, 0x90, 0xff));
        assert_eq!(parse_color("lightslategrey"), Color::Rgb(0x77, 0x88, 0x99));
        assert_eq!(parse_color("#ffffff"), Color::Rgb(255, 255, 255));
        assert_eq!(parse_color("#FF0000"), Color::Rgb(255, 0, 0));
        assert_eq!(parse_color("#fff"), Color::Reset);
        assert_eq!(parse_color("#gggggg"), Color::Reset);
        assert_eq!(parse_color("12"), Color::Reset);
        assert_eq!(parse_color("White"), Color::Reset);
        let mut names: Vec<&str> = W3C
            .iter()
            .map(|(n, _)| *n)
            .chain(BASIC.iter().map(|(n, _, _)| *n))
            .collect();
        let n = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), n);
    }

    #[test]
    fn color_strings() {
        for (c, e) in [
            ("", "-"),
            ("default", "-"),
            ("-", "-"),
            ("blue", "#0000ff"),
            ("lightgray", "#d3d3d3"),
            ("#00ff00", "#00ff00"),
            ("blah", "-"),
        ] {
            assert_eq!(color_string(c), e, "{c}");
        }
    }

    #[test]
    fn inversion() {
        for (c, e) in [
            ("default", "default"),
            ("-", "-"),
            ("", ""),
            ("#000000", "#ffffff"),
            ("#ffffff", "#000000"),
            ("#ff0000", "#7e0000"),
            ("#0000ff", "#1f5bff"),
            ("#00ff00", "#006600"),
            ("#ffff00", "#656501"),
            ("#00ffff", "#016464"),
            ("#333333", "#989898"),
            ("#cccccc", "#0c0c0c"),
            ("white", "#000000"),
            ("blah", "blah"),
        ] {
            assert_eq!(invert_color(c), e, "{c}");
        }
        for c in [
            "#000000", "#ffffff", "#808080", "#333333", "#cccccc", "#555555", "#636363",
        ] {
            assert_eq!(invert_color(&invert_color(c)), c, "{c}");
        }
        assert_eq!(invert_color("#636363"), "#636363");
        // Hue preserved.
        for c in ["#ff0000", "#0000ff", "#800080"] {
            let (_, _, h0) = Rgb::from_hex(color_rgb(c).unwrap()).ok_lch();
            let inv = invert_color(c);
            let (_, ci, hi) = Rgb::from_hex(color_rgb(&inv).unwrap()).ok_lch();
            if ci > 0.05 {
                let mut d = (h0 - hi).abs();
                if d > 180.0 {
                    d = 360.0 - d;
                }
                assert!(d < 1.0, "{c} -> {inv}: {h0} vs {hi}");
            }
        }
    }

    #[test]
    fn styles_invert() {
        let mut st = Styles::default();
        st.invert();
        assert_eq!(st.bg, Color::Rgb(255, 255, 255));
        assert_eq!(st.skin.k9s.body.bg_color, "#ffffff");
        assert_eq!(st.chart_default_dial.len(), 2);
        assert!(matches!(st.fg, Color::Rgb(..)));
        let mut t = Styles::from_skin(Skin::bundled("transparent").unwrap());
        t.invert();
        assert_eq!(t.bg, Color::Reset);
    }

    #[test]
    fn skin_happy() {
        let sk = Skin::load_from(&td("black-and-wtf.yaml")).unwrap();
        let st = Styles::from_skin(sk.clone());
        assert_eq!(color_string(&sk.k9s.body.fg_color), "#ffffff");
        assert_eq!(color_string(&sk.k9s.body.bg_color), "#000000");
        assert_eq!(color_string(&sk.k9s.views.table.bg_color), "#000000");
        assert_eq!(st.fg, Color::White);
        assert_eq!(st.bg, Color::Black);
    }

    #[test]
    fn skin_load_errors() {
        assert!(Skin::load_from(&td("blee.yaml")).is_err());
        let e = Skin::load_from(&td("boarked.yaml"))
            .unwrap_err()
            .to_string();
        assert_eq!(
            e,
            "Additional property bgColor is not allowed\nAdditional property fgColor is not allowed\nAdditional property logoColor is not allowed\nInvalid type. Expected: object, given: array"
        );
        // `body:` with no value fails the schema, like k9s.
        assert_eq!(
            Skin::load_from(&td("empty.yaml")).unwrap_err().to_string(),
            "Invalid type. Expected: object, given: null"
        );
        assert_eq!(
            Skin::parse_over(&Skin::default_skin(), "")
                .unwrap_err()
                .to_string(),
            "Invalid type. Expected: object, given: null"
        );
        assert_eq!(
            Skin::parse_over(&Skin::default_skin(), "k9s: {}\n").unwrap(),
            Skin::default_skin()
        );
    }

    #[test]
    fn all_bundled_skins_parse() {
        assert_eq!(BUNDLED_SKINS.len(), 34);
        for (name, src) in BUNDLED_SKINS {
            let sk = Skin::parse_over(&Skin::default_skin(), src)
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            let st = Styles::from_skin(sk.clone());
            let _ = st.body();
            let _ = sk.inverted();
        }
        let d = Styles::from_skin(Skin::bundled("dracula").unwrap());
        assert_eq!(d.bg, Color::Rgb(0x28, 0x2a, 0x36));
        let t = Styles::from_skin(Skin::bundled("transparent").unwrap());
        assert_eq!(t.bg, Color::Reset);
        assert!(Skin::bundled("nope").is_none());
    }

    #[test]
    fn active_skin_resolution() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().to_str().unwrap();
        let env = MapEnv::new(&[(K9S_ENV_CONFIG_DIR, cfg)]);
        let locs = Locations::resolve(&env, &Xdg::resolve(&env, Path::new("/h"), false));
        std::fs::create_dir_all(&locs.skins_dir).unwrap();
        for (n, src) in BUNDLED_SKINS
            .iter()
            .filter(|(n, _)| ["dracula", "nord", "kiss"].contains(n))
        {
            std::fs::write(locs.skins_dir.join(format!("{n}.yaml")), src).unwrap();
        }
        std::fs::write(locs.skins_dir.join("bad.yaml"), "k9s:\n  bodys: {}\n").unwrap();
        assert_eq!(active_skin(&locs, &env, "", None), None);
        assert_eq!(
            active_skin(&locs, &env, "kiss", None).as_deref(),
            Some("kiss")
        );
        assert_eq!(
            active_skin(&locs, &env, "kiss", Some("nord")).as_deref(),
            Some("nord")
        );
        assert_eq!(
            active_skin(&locs, &env, "kiss", Some("missing")).as_deref(),
            Some("kiss")
        );
        let env2 = MapEnv::new(&[(K9S_ENV_CONFIG_DIR, cfg), ("K9S_SKIN", "dracula")]);
        assert_eq!(
            active_skin(&locs, &env2, "kiss", Some("nord")).as_deref(),
            Some("dracula")
        );
        let env3 = MapEnv::new(&[(K9S_ENV_CONFIG_DIR, cfg), ("K9S_SKIN", "zorg")]);
        assert_eq!(
            active_skin(&locs, &env3, "", Some("nord")).as_deref(),
            Some("nord")
        );

        let st = Styles::load_active(&locs, &env, "", None, false);
        assert_eq!(st, Styles::default());
        let st = Styles::load_active(&locs, &env2, "", None, false);
        assert_eq!(st.bg, Color::Rgb(0x28, 0x2a, 0x36));
        let inv = Styles::load_active(&locs, &env, "", None, true);
        assert_eq!(inv.bg, Color::Rgb(255, 255, 255));
        // A broken skin falls back to stock, not inverted.
        let bad = Styles::load_active(&locs, &env, "bad", None, true);
        assert_eq!(bad.bg, Color::Black);
    }

    #[test]
    fn menu_style() {
        let mut m = MenuSkin::default();
        assert_eq!(m.fg_style_short(), "d");
        m.fg_style = "bold".into();
        assert_eq!(m.fg_style_short(), "b");
        m.fg_style = "normal".into();
        assert_eq!(m.fg_style_short(), "-");
    }
}
