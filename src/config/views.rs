//! views.yaml (config/views.go): per-GVR custom columns, plus the column
//! spec parser (render/cust_col.go) for `NAME`, `NAME|WR`,
//! `NAME:.json.path|T` style specs.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

use super::k9s::yaml::{de_null, de_str_vec, sorted_map};
use super::paths;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ViewSetting {
    /// Column specs, e.g. `NAME`, `AGE|T`, `LABEL:.metadata.labels.app|W`, `STATUS|H`.
    #[serde(deserialize_with = "de_str_vec")]
    pub columns: Vec<String>,
    #[serde(deserialize_with = "de_null")]
    pub sort_column: String,
}

impl ViewSetting {
    /// Whether custom columns are set.
    pub fn has_cols(&self) -> bool {
        !self.columns.is_empty()
    }

    /// Neither columns nor sort column.
    pub fn is_blank(&self) -> bool {
        self.columns.is_empty() && self.sort_column.is_empty()
    }

    /// Sort column spec `name:asc|desc` → (name, ascending).
    pub fn sort_col(&self) -> Result<(String, bool)> {
        if self.sort_column.is_empty() {
            return Err(anyhow!("no sort column specified"));
        }
        let tt: Vec<&str> = self.sort_column.split(':').collect();
        if tt.len() < 2 {
            return Err(anyhow!(
                "invalid sort column spec: {:?}. must be col-name:asc|desc",
                self.sort_column
            ));
        }
        Ok((tt[0].to_string(), tt[1] == "asc"))
    }

    /// Parse every column spec.
    pub fn column_defs(&self) -> Result<Vec<ColumnDef>> {
        self.columns.iter().map(|c| parse_column_spec(c)).collect()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CustomView {
    /// gvr string → setting.
    #[serde(
        rename = "views",
        serialize_with = "sorted_map",
        default,
        deserialize_with = "de_null"
    )]
    pub views: BTreeMap<String, ViewSetting>,
}

impl CustomView {
    /// Load `<config>/views.yaml` (missing file gives no views).
    pub fn load() -> Result<CustomView> {
        CustomView::load_from(&paths::views_file())
    }

    pub fn load_from(path: &Path) -> Result<CustomView> {
        let mut v = CustomView::default();
        v.load_file(path)?;
        Ok(v)
    }

    /// CustomView.Load: replace the views with the content of `path`.
    pub fn load_file(&mut self, path: &Path) -> Result<()> {
        if !path.exists() {
            return Ok(());
        }
        let bb = std::fs::read_to_string(path)?;
        if let Err(e) = super::k9s::json::validate(super::k9s::json::VIEWS_SCHEMA, &bb) {
            tracing::warn!(path = %path.display(), error = %e, "Validation failed. Please update your config and restart!");
        }
        let v = super::k9s::yaml::parse_value(&bb)?;
        let input: CustomView = if v.is_null() {
            CustomView::default()
        } else {
            serde_yaml::from_value(v)?
        };
        self.views = input.views;
        Ok(())
    }

    /// Clear all views.
    pub fn reset(&mut self) {
        self.views.clear();
    }

    /// CustomView.getVS: the setting for a view command `gvr` (`v1/pods`,
    /// an alias, or `cmd ns`) in namespace `ns`. Keys may carry a namespace
    /// regex: `v1/pods@kube-.*`.
    pub fn get_vs(&self, gvr: &str, ns: &str) -> Option<ViewSetting> {
        let ns = if crate::k8s::is_all_namespaces(ns) {
            crate::k8s::NAMESPACE_ALL
        } else {
            ns
        };
        let k = gvr;
        let mut keys: Vec<&String> = self.views.keys().collect();
        keys.sort();
        keys.reverse();
        for key in keys {
            if !key.starts_with(gvr) && !gvr.starts_with(key.as_str()) {
                continue;
            }
            if key.contains('@') {
                let tt: Vec<&str> = key.split('@').collect();
                if tt.len() != 2 {
                    continue;
                }
                let mut nsk = gvr.to_string();
                if !ns.is_empty() {
                    nsk.push('@');
                    nsk.push_str(ns);
                }
                if let Ok(rx) = regex::Regex::new(tt[1]) {
                    if rx.is_match(&nsk) {
                        return self.views.get(key).cloned();
                    }
                }
            } else if k.starts_with(key.as_str()) {
                let kk: Vec<&str> = k.split_whitespace().collect();
                if kk.len() == 2 {
                    if let Some(v) = self.views.get(&format!("{}@{}", kk[0], kk[1])) {
                        return Some(v.clone());
                    }
                }
                return self.views.get(key).cloned();
            } else if key == k {
                return self.views.get(key).cloned();
            }
        }
        None
    }
}

/// Column alignment (tview.AlignLeft/Center/Right).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

/// A parsed column spec (render.colDef).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ColumnDef {
    pub name: String,
    /// JSONPath (`{.metadata.name}`) or empty to reuse a standard column.
    pub spec: String,
    pub align: Align,
    /// `W`: only shown in wide mode.
    pub wide: bool,
    /// `S`: always shown (cancels wide).
    pub show: bool,
    /// `H`: hidden.
    pub hide: bool,
    /// `T`: age column.
    pub time: bool,
    /// `N`: number/capacity column (right aligned).
    pub capacity: bool,
    pub mx: bool,
    pub mxc: bool,
    pub mxm: bool,
}

impl ColumnDef {
    /// JSONPath spec when given.
    pub fn json_path(&self) -> Option<&str> {
        (!self.spec.is_empty()).then_some(self.spec.as_str())
    }

    /// A jq expression (render.isJQSpec): more than two `|` separated parts.
    pub fn is_jq(&self) -> bool {
        self.spec.split('|').count() > 2
    }
}

/// Parse a views.yaml column spec `NAME[:jsonpath][|ATTRS]` (render.parse).
pub fn parse_column_spec(s: &str) -> Result<ColumnDef> {
    static RX: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let rx = RX.get_or_init(|| {
        regex::Regex::new(r"^([A-Za-z0-9_\t\n\f\r %/-]+):?((?s:.)*?)\|?([NTWSLRH]{0,3})$")
            .expect("column spec regex")
    });
    let Some(mm) = rx.captures(s) else {
        return Err(anyhow!("invalid column definition {s:?}"));
    };
    let spec = relaxed_json_path_expression(mm.get(2).map_or("", |m| m.as_str()))?;
    let mut c = ColumnDef {
        name: mm[1].to_string(),
        spec,
        ..ColumnDef::default()
    };
    for b in mm.get(3).map_or("", |m| m.as_str()).chars() {
        match b {
            'H' => c.hide = true,
            'W' => {
                c.wide = true;
                c.show = false;
            }
            'S' => {
                c.show = true;
                c.wide = false;
            }
            'L' => c.align = Align::Left,
            'R' => c.align = Align::Right,
            'T' => c.time = true,
            'N' => {
                c.capacity = true;
                c.align = Align::Right;
            }
            other => tracing::warn!(attr = %other, "Unknown column attribute"),
        }
    }
    Ok(c)
}

/// kubectl get.RelaxedJSONPathExpression: `a.b`, `.a.b`, `{a.b}` → `{.a.b}`.
pub fn relaxed_json_path_expression(p: &str) -> Result<String> {
    if p.is_empty() {
        return Ok(String::new());
    }
    static RX: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let rx = RX.get_or_init(|| {
        regex::Regex::new(r"^\{\.?([^{}]+)\}$|^\.?([^{}]+)$").expect("jsonpath regex")
    });
    let Some(m) = rx.captures(p) else {
        return Err(anyhow!(
            "unexpected path string, expected a 'name1.name2' or '.name1.name2' or '{{name1.name2}}' or '{{.name1.name2}}'"
        ));
    };
    let field = m
        .get(1)
        .filter(|g| !g.as_str().is_empty())
        .or_else(|| m.get(2))
        .map_or("", |g| g.as_str());
    Ok(format!("{{.{field}}}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn td() -> std::path::PathBuf {
        format!(
            "{}/src/config/testdata/config/views/views.yaml",
            env!("CARGO_MANIFEST_DIR")
        )
        .into()
    }

    fn sv(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn load() {
        let v = CustomView::load_from(&td()).unwrap();
        assert_eq!(
            v.views["v1/pods"].columns,
            sv(&["NAMESPACE", "NAME", "AGE", "IP"])
        );
        assert_eq!(
            v.views["v1/pods@default"].columns,
            sv(&["NAME", "IP", "AGE"])
        );
        assert!(CustomView::load_from(Path::new("/nope/views.yaml"))
            .unwrap()
            .views
            .is_empty());
        let mut v = v;
        v.reset();
        assert!(v.views.is_empty());
    }

    #[test]
    fn get_vs() {
        let v = CustomView::load_from(&td()).unwrap();
        let cols = |gvr: &str, ns: &str| v.get_vs(gvr, ns).map(|s| s.columns);
        assert_eq!(cols("", ""), None);
        assert_eq!(cols("zorg", ""), None);
        assert_eq!(
            cols("v1/pods", ""),
            Some(sv(&["NAMESPACE", "NAME", "AGE", "IP"]))
        );
        assert_eq!(cols("v1/pods", "default"), Some(sv(&["NAME", "IP", "AGE"])));
        assert_eq!(cols("v1/pods", "ns-fred"), Some(sv(&["AGE", "NAME", "IP"])));
        assert_eq!(cols("bozo", ""), Some(sv(&["DUH", "BLAH", "BLEE"])));
        assert_eq!(
            cols("v1/pods", "zorg"),
            Some(sv(&["NAMESPACE", "NAME", "AGE", "IP"]))
        );
        assert_eq!(cols("v1/services", "zorg"), None);
        // `cmd ns` form.
        assert_eq!(
            cols("v1/pods default", ""),
            Some(sv(&["NAME", "IP", "AGE"]))
        );
    }

    #[test]
    fn equals_and_sort() {
        let a = ViewSetting {
            columns: sv(&["A", "B", "C"]),
            ..ViewSetting::default()
        };
        assert_eq!(a, a.clone());
        assert_ne!(
            a,
            ViewSetting {
                columns: sv(&["C", "A", "B"]),
                ..ViewSetting::default()
            }
        );
        assert_ne!(a, ViewSetting::default());
        assert_eq!(ViewSetting::default(), ViewSetting::default());
        assert!(ViewSetting::default().is_blank());
        assert!(a.has_cols());
        assert_eq!(
            ViewSetting::default().sort_col().unwrap_err().to_string(),
            "no sort column specified"
        );
        let s = ViewSetting {
            sort_column: "AGE".into(),
            ..ViewSetting::default()
        };
        assert_eq!(
            s.sort_col().unwrap_err().to_string(),
            "invalid sort column spec: \"AGE\". must be col-name:asc|desc"
        );
        let s = ViewSetting {
            sort_column: "AGE:asc".into(),
            ..ViewSetting::default()
        };
        assert_eq!(s.sort_col().unwrap(), ("AGE".to_string(), true));
        let s = ViewSetting {
            sort_column: "NAME:desc".into(),
            ..ViewSetting::default()
        };
        assert_eq!(s.sort_col().unwrap(), ("NAME".to_string(), false));
    }

    fn def(name: &str, spec: &str) -> ColumnDef {
        ColumnDef {
            name: name.into(),
            spec: spec.into(),
            ..ColumnDef::default()
        }
    }

    #[test]
    fn column_specs() {
        assert_eq!(
            parse_column_spec("").unwrap_err().to_string(),
            "invalid column definition \"\""
        );
        assert_eq!(parse_column_spec("fred").unwrap(), def("fred", ""));
        assert_eq!(
            parse_column_spec("fred|W").unwrap(),
            ColumnDef {
                wide: true,
                ..def("fred", "")
            }
        );
        assert_eq!(
            parse_column_spec("fred|WH").unwrap(),
            ColumnDef {
                wide: true,
                hide: true,
                ..def("fred", "")
            }
        );
        assert_eq!(
            parse_column_spec("fred|S").unwrap(),
            ColumnDef {
                show: true,
                ..def("fred", "")
            }
        );
        assert_eq!(
            parse_column_spec("AGE|TR").unwrap(),
            ColumnDef {
                align: Align::Right,
                time: true,
                ..def("AGE", "")
            }
        );
        assert_eq!(
            parse_column_spec("fred|WR").unwrap(),
            ColumnDef {
                align: Align::Right,
                wide: true,
                ..def("fred", "")
            }
        );
        let cil = "{.spec.addresses[?(@.type == 'CiliumInternalIP')].ip}";
        assert_eq!(
            parse_column_spec("BLEE:.spec.addresses[?(@.type == 'CiliumInternalIP')].ip").unwrap(),
            def("BLEE", cil)
        );
        assert_eq!(
            parse_column_spec("BLEE:.spec.addresses[?(@.type == 'CiliumInternalIP')].ip|WR")
                .unwrap(),
            ColumnDef {
                align: Align::Right,
                wide: true,
                ..def("BLEE", cil)
            }
        );
        assert_eq!(
            parse_column_spec("fred:.metadata.name|NW").unwrap(),
            ColumnDef {
                align: Align::Right,
                capacity: true,
                wide: true,
                ..def("fred", "{.metadata.name}")
            }
        );
        assert_eq!(
            parse_column_spec("fred:.metadata.name|RW").unwrap(),
            ColumnDef {
                align: Align::Right,
                wide: true,
                ..def("fred", "{.metadata.name}")
            }
        );
        assert_eq!(
            parse_column_spec("fred:.metadata.name|T").unwrap(),
            ColumnDef {
                time: true,
                ..def("fred", "{.metadata.name}")
            }
        );
        assert_eq!(
            parse_column_spec("fred:.metadata.name").unwrap(),
            def("fred", "{.metadata.name}")
        );
        assert_eq!(
            parse_column_spec("fred:.metadata.name|W").unwrap(),
            ColumnDef {
                wide: true,
                ..def("fred", "{.metadata.name}")
            }
        );
        assert_eq!(
            parse_column_spec("fred||.metadata.name|W").unwrap(),
            ColumnDef {
                wide: true,
                ..def("fred", "{.||.metadata.name}")
            }
        );
        assert_eq!(
            parse_column_spec(":.metadata.name.fred|TW")
                .unwrap_err()
                .to_string(),
            "invalid column definition \":.metadata.name.fred|TW\""
        );
        assert_eq!(
            parse_column_spec(r"fred:.metadata.name.k8s:fred\.blee|TW").unwrap(),
            ColumnDef {
                time: true,
                wide: true,
                ..def("fred", r"{.metadata.name.k8s:fred\.blee}")
            }
        );
        assert_eq!(
            parse_column_spec("fred|T").unwrap(),
            ColumnDef {
                time: true,
                ..def("fred", "")
            }
        );
        assert_eq!(
            parse_column_spec("LABELS:{.metadata.labels}").unwrap(),
            def("LABELS", "{.metadata.labels}")
        );
        let jq = parse_column_spec("IMG:.spec.containers | map(.image) | join(\",\")").unwrap();
        assert!(jq.is_jq());
        assert_eq!(jq.json_path(), Some(jq.spec.as_str()));
        assert_eq!(def("x", "").json_path(), None);
        assert!(parse_column_spec("X:{a}{b}").is_err());
        let vs = ViewSetting {
            columns: sv(&["NAME", "AGE|T"]),
            ..ViewSetting::default()
        };
        assert_eq!(vs.column_defs().unwrap().len(), 2);
    }
}
