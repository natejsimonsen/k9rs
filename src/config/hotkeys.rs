//! hotkeys.yaml (config/hotkey.go).

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::k9s::yaml::{de_bool, de_null, sorted_map};
use super::paths::Locations;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HotKey {
    #[serde(deserialize_with = "de_null")]
    pub short_cut: String,
    #[serde(rename = "override", deserialize_with = "de_bool")]
    pub override_: bool,
    #[serde(deserialize_with = "de_null")]
    pub description: String,
    #[serde(deserialize_with = "de_null")]
    pub command: String,
    #[serde(deserialize_with = "de_bool")]
    pub keep_history: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HotKeys {
    #[serde(
        rename = "hotKeys",
        serialize_with = "sorted_map",
        default,
        deserialize_with = "de_null"
    )]
    pub hot_keys: BTreeMap<String, HotKey>,
}

impl HotKeys {
    /// Load `<config>/hotkeys.yaml`.
    pub fn load() -> Result<HotKeys> {
        HotKeys::load_with(&Locations::current(), None)
    }

    /// HotKeys.Load: global file, then the optional context specific file
    /// (its entries override global ones with the same name).
    pub fn load_with(locs: &Locations, context_file: Option<&Path>) -> Result<HotKeys> {
        let mut h = HotKeys::default();
        h.load_file(&locs.hotkeys_file)?;
        if let Some(p) = context_file {
            h.load_file(p)?;
        }
        Ok(h)
    }

    /// Load a single file.
    pub fn load_from(path: &Path) -> Result<HotKeys> {
        let mut h = HotKeys::default();
        h.load_file(path)?;
        Ok(h)
    }

    /// HotKeys.LoadHotKeys: merge `path` (missing file is fine).
    pub fn load_file(&mut self, path: &Path) -> Result<()> {
        if !path.exists() {
            return Ok(());
        }
        let bb = std::fs::read_to_string(path)?;
        if let Err(e) = super::k9s::json::validate(super::k9s::json::HOTKEYS_SCHEMA, &bb) {
            tracing::warn!(path = %path.display(), error = %e, "Validation failed. Please update your config and restart.");
        }
        let v = super::k9s::yaml::parse_value(&bb)?;
        if v.is_null() {
            return Ok(());
        }
        let hh: HotKeys = serde_yaml::from_value(v)?;
        self.hot_keys.extend(hh.hot_keys);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::paths::{MapEnv, Xdg, K9S_ENV_CONFIG_DIR};

    #[test]
    fn load() {
        let p = format!(
            "{}/src/config/testdata/config/hotkeys/hotkeys.yaml",
            env!("CARGO_MANIFEST_DIR")
        );
        let h = HotKeys::load_from(Path::new(&p)).unwrap();
        assert_eq!(h.hot_keys.len(), 1);
        let k = &h.hot_keys["pods"];
        assert_eq!(k.short_cut, "shift-0");
        assert_eq!(k.description, "Launch pod view");
        assert_eq!(k.command, "pods");
        assert!(k.keep_history);
        assert!(!k.override_);
    }

    #[test]
    fn global_then_context() {
        let tmp = tempfile::tempdir().unwrap();
        let env = MapEnv::new(&[(K9S_ENV_CONFIG_DIR, tmp.path().to_str().unwrap())]);
        let locs = Locations::resolve(&env, &Xdg::resolve(&env, Path::new("/h"), false));
        std::fs::write(
            &locs.hotkeys_file,
            "hotKeys:\n  a:\n    shortCut: Shift-1\n    command: po\n  b:\n    shortCut: Shift-2\n    command: dp\n",
        )
        .unwrap();
        let ctx = tmp.path().join("ctx-hotkeys.yaml");
        std::fs::write(
            &ctx,
            "hotKeys:\n  b:\n    shortCut: Shift-3\n    command: svc\n    override: true\n",
        )
        .unwrap();
        let h = HotKeys::load_with(&locs, Some(&ctx)).unwrap();
        assert_eq!(h.hot_keys["a"].command, "po");
        assert_eq!(h.hot_keys["b"].command, "svc");
        assert!(h.hot_keys["b"].override_);
        // Missing files are fine; the template only has comments.
        assert!(HotKeys::load_with(&locs, Some(&tmp.path().join("nope.yaml"))).is_ok());
        std::fs::write(&locs.hotkeys_file, "hotKeys:\n  # Examples...\n").unwrap();
        assert!(HotKeys::load_with(&locs, None).unwrap().hot_keys.is_empty());
        std::fs::write(&locs.hotkeys_file, "hotKeys: [\n").unwrap();
        assert!(HotKeys::load_with(&locs, None).is_err());
    }
}
