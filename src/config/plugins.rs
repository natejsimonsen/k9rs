//! plugins.yaml (config/plugin.go): global and context plugins files plus
//! plugin snippets from the XDG `k9s/plugins` dirs.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

use super::k9s::json;
use super::k9s::yaml::{de_bool, de_str, de_str_vec, sorted_map};
use super::paths::Locations;

/// Scope matching every view.
pub const ALL_SCOPES: &str = "all";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Plugin {
    #[serde(deserialize_with = "de_str_vec")]
    pub scopes: Vec<String>,
    #[serde(deserialize_with = "de_str_vec")]
    pub args: Vec<String>,
    #[serde(deserialize_with = "de_str")]
    pub short_cut: String,
    #[serde(rename = "override", deserialize_with = "de_bool")]
    pub override_: bool,
    #[serde(deserialize_with = "de_str_vec")]
    pub pipes: Vec<String>,
    #[serde(deserialize_with = "de_str")]
    pub description: String,
    #[serde(deserialize_with = "de_str")]
    pub command: String,
    #[serde(deserialize_with = "de_bool")]
    pub confirm: bool,
    #[serde(deserialize_with = "de_bool")]
    pub background: bool,
    #[serde(deserialize_with = "de_bool")]
    pub dangerous: bool,
    #[serde(deserialize_with = "de_bool")]
    pub overwrite_output: bool,
}

impl std::fmt::Display for Plugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "[{}] {}({})",
            self.short_cut,
            self.command,
            self.args.join(" ")
        )
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Plugins {
    #[serde(rename = "plugins", serialize_with = "sorted_map", default)]
    pub plugins: BTreeMap<String, Plugin>,
}

/// Whether a plugin with `scopes` applies to a view known by `aliases`
/// (view.inScope): `all`, or any scope equal to one of the view aliases.
pub fn in_scope(scopes: &[String], aliases: &[String]) -> bool {
    if scopes.iter().any(|s| s == ALL_SCOPES) {
        return true;
    }
    scopes.iter().any(|s| aliases.contains(s))
}

fn join_errs(errs: Vec<String>) -> Result<()> {
    if errs.is_empty() {
        Ok(())
    } else {
        Err(anyhow!(errs.join("\n")))
    }
}

impl Plugins {
    /// Load `<config>/plugins.yaml` plus `<xdg>/k9s/plugins/*.yaml`.
    pub fn load() -> Result<Plugins> {
        let mut p = Plugins::default();
        p.load_all(&Locations::current(), None, true)?;
        Ok(p)
    }

    /// Plugins.Load: the global file, the optional context file and, when
    /// `load_extra`, every YAML file under the XDG plugin dirs. Problems are
    /// joined into the error; valid plugins are kept.
    pub fn load_all(
        &mut self,
        locs: &Locations,
        context_file: Option<&Path>,
        load_extra: bool,
    ) -> Result<()> {
        let mut errs = Vec::new();
        if let Err(e) = self.load_file(&locs.plugins_file) {
            errs.push(e.to_string());
        }
        if let Some(p) = context_file {
            if let Err(e) = self.load_file(p) {
                errs.push(e.to_string());
            }
        }
        if load_extra {
            for dir in locs.plugin_dirs() {
                if let Err(e) = self.load_dir(&dir) {
                    errs.push(e.to_string());
                }
            }
        }
        join_errs(errs)
    }

    /// Load a single plugins file.
    pub fn load_from(path: &Path) -> Result<Plugins> {
        let mut p = Plugins::default();
        p.load_file(path)?;
        Ok(p)
    }

    /// Plugins.load: a full plugins file, a single plugin snippet (named after
    /// the file) or a multi snippets file. Missing files are fine.
    pub fn load_file(&mut self, path: &Path) -> Result<()> {
        if !path.exists() {
            return Ok(());
        }
        let bb = std::fs::read_to_string(path)?;
        let scheme = match json::validate_plugins(&bb) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "Plugin schema validation failed");
                return Err(anyhow!(
                    "plugin validation failed for {}: {e}",
                    path.display()
                ));
            }
        };
        let v = super::k9s::yaml::parse_value(&bb)?;
        let unmarshal =
            |e: serde_yaml::Error| anyhow!("plugin unmarshal failed for {}: {e}", path.display());
        match scheme {
            json::PLUGIN_SCHEMA => {
                let o: Plugin = serde_yaml::from_value(v).map_err(unmarshal)?;
                let name = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.plugins.insert(name, o);
            }
            json::PLUGINS_SCHEMA => {
                let oo: Plugins = serde_yaml::from_value(v).map_err(unmarshal)?;
                self.plugins.extend(oo.plugins);
            }
            _ => {
                let oo: BTreeMap<String, Plugin> = serde_yaml::from_value(v).map_err(unmarshal)?;
                self.plugins.extend(oo);
            }
        }
        Ok(())
    }

    /// Plugins.loadDir: every `.yaml`/`.yml` file below `dir`, following
    /// symlinks (loops are skipped).
    pub fn load_dir(&mut self, dir: &Path) -> Result<()> {
        if !dir.exists() {
            return Ok(());
        }
        let mut errs = Vec::new();
        let mut seen = HashSet::new();
        let mut files = Vec::new();
        walk(dir, &mut seen, &mut files);
        for f in files {
            if let Err(e) = self.load_file(&f) {
                errs.push(e.to_string());
            }
        }
        join_errs(errs)
    }

    /// Plugins applicable to a view known by `scopes` (its aliases): plugin
    /// scopes match one of them or `all`. Sorted by plugin name.
    pub fn for_scope(&self, scopes: &[String]) -> Vec<(String, Plugin)> {
        self.plugins
            .iter()
            .filter(|(_, p)| in_scope(&p.scopes, scopes))
            .map(|(k, p)| (k.clone(), p.clone()))
            .collect()
    }

    /// `for_scope` minus dangerous plugins when read only (view.pluginActions).
    pub fn for_view(&self, aliases: &[String], read_only: bool) -> Vec<(String, Plugin)> {
        self.for_scope(aliases)
            .into_iter()
            .filter(|(_, p)| !(read_only && p.dangerous))
            .collect()
    }
}

fn is_yaml_file(p: &Path) -> bool {
    matches!(p.extension().and_then(|e| e.to_str()), Some("yml" | "yaml"))
}

fn walk(dir: &Path, seen: &mut HashSet<PathBuf>, files: &mut Vec<PathBuf>) {
    let canon = match std::fs::canonicalize(dir) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(path = %dir.display(), error = %e, "Error walking plugins dir - skipping node");
            return;
        }
    };
    if !seen.insert(canon) {
        return;
    }
    let mut entries: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(rd) => rd.filter_map(|e| e.ok().map(|e| e.path())).collect(),
        Err(e) => {
            tracing::warn!(path = %dir.display(), error = %e, "Error walking plugins dir - skipping node");
            return;
        }
    };
    entries.sort();
    for p in entries {
        // metadata follows symlinks.
        match std::fs::metadata(&p) {
            Ok(m) if m.is_dir() => walk(&p, seen, files),
            Ok(_) if is_yaml_file(&p) => files.push(p),
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(path = %p.display(), error = %e, "Error walking plugins dir - skipping node")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn td(p: &str) -> PathBuf {
        format!(
            "{}/src/config/testdata/config/plugins/{p}",
            env!("CARGO_MANIFEST_DIR")
        )
        .into()
    }

    fn sv(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn blah() -> Plugin {
        Plugin {
            scopes: sv(&["po", "dp"]),
            args: sv(&["-n", "$NAMESPACE", "-boolean"]),
            short_cut: "shift-s".into(),
            description: "blee".into(),
            command: "duh".into(),
            confirm: true,
            ..Plugin::default()
        }
    }

    fn snippet1() -> Plugin {
        Plugin {
            overwrite_output: true,
            ..blah()
        }
    }

    fn all_dir() -> BTreeMap<String, Plugin> {
        [
            ("snippet.1".to_string(), snippet1()),
            (
                "snippet.2".to_string(),
                Plugin {
                    scopes: sv(&["svc", "ing"]),
                    args: sv(&["-n", "$NAMESPACE", "-oyaml"]),
                    short_cut: "shift-r".into(),
                    description: "bla".into(),
                    command: "duha".into(),
                    background: true,
                    ..Plugin::default()
                },
            ),
            (
                "crapola".to_string(),
                Plugin {
                    short_cut: "Shift-1".into(),
                    command: "crapola".into(),
                    description: "crapola".into(),
                    scopes: sv(&["pods"]),
                    ..Plugin::default()
                },
            ),
            (
                "bozo".to_string(),
                Plugin {
                    short_cut: "Shift-2".into(),
                    description: "bozo".into(),
                    command: "bozo".into(),
                    scopes: sv(&["pods", "svc"]),
                    ..Plugin::default()
                },
            ),
        ]
        .into_iter()
        .collect()
    }

    #[test]
    fn load_kinds() {
        let p = Plugins::load_from(&td("dir/snippet.1.yaml")).unwrap();
        assert_eq!(
            p.plugins,
            [("snippet.1".to_string(), snippet1())]
                .into_iter()
                .collect()
        );

        let p = Plugins::load_from(&td("dir/snippet.multi.yaml")).unwrap();
        let mut e = all_dir();
        e.remove("snippet.1");
        e.remove("snippet.2");
        assert_eq!(p.plugins, e);

        let p = Plugins::load_from(&td("plugins.yaml")).unwrap();
        assert_eq!(
            p.plugins,
            [("blah".to_string(), blah())].into_iter().collect()
        );

        let p = Plugins::load_from(&td("plugins-bozo.yaml")).unwrap();
        assert!(p.plugins.is_empty());
    }

    #[test]
    fn load_invalid() {
        let path = td("plugins-toast.yaml");
        let mut p = Plugins::default();
        let err = p.load_file(&path).unwrap_err().to_string();
        assert_eq!(
            err,
            format!(
                "plugin validation failed for {}: scopes is required\nAdditional property plugins is not allowed\ncommand is required\ndescription is required\nscopes is required\nshortCut is required\ncommand is required\ndescription is required\nscopes is required\nshortCut is required",
                path.display()
            )
        );
        assert!(p.plugins.is_empty());
    }

    #[test]
    fn load_file_and_dir() {
        let mut p = Plugins::default();
        p.load_file(&td("plugins.yaml")).unwrap();
        p.load_dir(Path::new("/random/dir/not/exist")).unwrap();
        assert_eq!(p.plugins.len(), 1);
        p.load_dir(&td("dir")).unwrap();
        let mut e = all_dir();
        e.insert("blah".into(), blah());
        assert_eq!(p.plugins, e);
    }

    #[cfg(unix)]
    #[test]
    fn load_symlinks() {
        let tmp = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(td("plugins.yaml"), tmp.path().join("plugins-symlink.yaml"))
            .unwrap();
        std::os::unix::fs::symlink(td("dir"), tmp.path().join("plugins-dir-symlink")).unwrap();
        let lp = tmp.path().join("loop");
        std::fs::create_dir(&lp).unwrap();
        std::os::unix::fs::symlink(&lp, lp.join("self")).unwrap();
        let mut p = Plugins::default();
        p.load_dir(tmp.path()).unwrap();
        let mut e = all_dir();
        e.insert("blah".into(), blah());
        assert_eq!(p.plugins, e);
    }

    #[test]
    fn load_all_layers() {
        use crate::config::paths::{MapEnv, Xdg, K9S_ENV_CONFIG_DIR};
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("cfg");
        let data = tmp.path().join("data");
        std::fs::create_dir_all(data.join("k9s/plugins")).unwrap();
        std::fs::create_dir_all(&cfg).unwrap();
        std::fs::copy(td("plugins.yaml"), cfg.join("plugins.yaml")).unwrap();
        std::fs::copy(
            td("dir/snippet.2.yaml"),
            data.join("k9s/plugins/snippet.2.yaml"),
        )
        .unwrap();
        let ctx = tmp.path().join("ctx-plugins.yaml");
        std::fs::write(
            &ctx,
            "plugins:\n  blah:\n    shortCut: Ctrl-B\n    description: ctx\n    scopes: [all]\n    command: ctx\n    dangerous: true\n    args: [1, \"-x\"]\n",
        )
        .unwrap();
        let env = MapEnv::new(&[
            (K9S_ENV_CONFIG_DIR, cfg.to_str().unwrap()),
            ("XDG_DATA_HOME", data.to_str().unwrap()),
            ("XDG_DATA_DIRS", "/nonexistent-k9rs"),
        ]);
        let locs = Locations::resolve(&env, &Xdg::resolve(&env, Path::new("/h"), false));
        let mut p = Plugins::default();
        p.load_all(&locs, Some(&ctx), false).unwrap();
        assert_eq!(p.plugins.len(), 1);
        assert_eq!(p.plugins["blah"].command, "ctx");
        assert_eq!(p.plugins["blah"].args, vec!["1", "-x"]);
        p.load_all(&locs, Some(&ctx), true).unwrap();
        assert_eq!(p.plugins.len(), 2);
        assert!(p.plugins.contains_key("snippet.2"));

        // Scopes.
        let po = sv(&["po", "pod", "pods", "v1/pods"]);
        let names: Vec<String> = p.for_scope(&po).into_iter().map(|(k, _)| k).collect();
        assert_eq!(names, vec!["blah"]);
        let svc = sv(&["svc", "service", "v1/services"]);
        let names: Vec<String> = p.for_scope(&svc).into_iter().map(|(k, _)| k).collect();
        assert_eq!(names, vec!["blah", "snippet.2"]);
        let names: Vec<String> = p.for_view(&svc, true).into_iter().map(|(k, _)| k).collect();
        assert_eq!(names, vec!["snippet.2"]);
        assert_eq!(
            p.plugins["snippet.2"].to_string(),
            "[shift-r] duha(-n $NAMESPACE -oyaml)"
        );
    }

    #[test]
    fn scope_matching() {
        assert!(in_scope(&sv(&["all"]), &[]));
        assert!(in_scope(&sv(&["po", "dp"]), &sv(&["dp"])));
        assert!(!in_scope(&sv(&["po"]), &sv(&["svc"])));
        assert!(!in_scope(&sv(&["PO"]), &sv(&["po"])));
        assert!(!in_scope(&[], &sv(&["po"])));
    }
}
