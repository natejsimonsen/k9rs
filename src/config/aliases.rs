//! aliases.yaml (config/alias.go): alias -> GVR string, with k9s built-ins,
//! plus the aliases k9s derives from API discovery (dao/alias.go).

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::k9s::yaml::sorted_map;
use super::paths::{self, Locations};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Aliases {
    /// alias → gvr string (e.g. `po` → `v1/pods`).
    #[serde(rename = "aliases", serialize_with = "sorted_map", default)]
    pub aliases: BTreeMap<String, String>,
}

/// k9s pseudo resources that count as real resources (client.reservedGVRs).
const RESERVED_GVRS: &[&str] = &[
    "cpu",
    "memory",
    "workloads",
    "containers",
    "contexts",
    "references",
    "pulses",
    "scans",
    "dirs",
    "portforwards",
    "screendumps",
    "benchmarks",
    "aliases",
    "xrays",
    "help",
    "quit",
    "helm",
    "helm-history",
    "rbac",
    "policy",
    "users",
    "groups",
];

/// Group/versions whose resources get plural/singular aliases (dao.stdGroups).
const STD_GROUPS: &[&str] = &[
    "apps/v1",
    "autoscaling/v1",
    "autoscaling/v2",
    "autoscaling/v2beta1",
    "autoscaling/v2beta2",
    "batch/v1",
    "batch/v1beta1",
    "extensions/v1beta1",
    "policy/v1beta1",
    "policy/v1",
    "v1",
];

/// GVRs k9s skips during discovery (dao.deprecatedGVRs).
const DEPRECATED_GVRS: &[&str] = &["v1/events", "extensions/v1beta1/ingresses"];

/// client.GVR.IsK8sRes: a `[group/]version/resource` (no spaces) or a k9s
/// reserved resource.
pub fn is_k8s_res(gvr: &str) -> bool {
    (!gvr.contains(' ') && gvr.contains('/') && !gvr.contains(" /")) || RESERVED_GVRS.contains(&gvr)
}

/// dao.isStandardGroup.
pub fn is_standard_group(gv: &str) -> bool {
    STD_GROUPS.contains(&gv) || gv.contains(".k8s.io")
}

/// client.GVR.AsResourceName: `res` or `res.version.group`.
pub fn as_resource_name(gvr: &crate::k8s::Gvr) -> String {
    if gvr.group.is_empty() {
        gvr.resource.clone()
    } else {
        format!("{}.{}.{}", gvr.resource, gvr.version, gvr.group)
    }
}

impl Aliases {
    /// Built-in aliases (config.Aliases.loadDefaultAliases).
    pub fn defaults() -> Aliases {
        let mut a = Aliases::default();
        a.load_default_aliases();
        a
    }

    fn declare(&mut self, gvr: &str, aliases: &[&str]) {
        self.aliases.insert(gvr.to_string(), gvr.to_string());
        for al in aliases {
            self.aliases.insert(al.to_string(), gvr.to_string());
        }
    }

    fn load_default_aliases(&mut self) {
        self.declare("help", &["h", "?"]);
        self.declare("quit", &["q", "q!", "qa", "Q"]);
        self.declare("aliases", &["alias", "a"]);
        self.declare("helm", &["charts", "chart", "hm"]);
        self.declare("dirs", &["dir", "d"]);
        self.declare("contexts", &["context", "ctx"]);
        self.declare("users", &["user", "usr"]);
        self.declare("groups", &["group", "grp"]);
        self.declare("portforwards", &["portforward", "pf"]);
        self.declare("benchmarks", &["benchmark", "bench"]);
        self.declare("screendumps", &["screendump", "sd"]);
        self.declare("pulses", &["pulse", "pu", "hz"]);
        self.declare("xrays", &["xray", "x"]);
        self.declare("workloads", &["workload", "wk"]);
    }

    /// Load `<config>/aliases.yaml` (created from the template if missing)
    /// merged over the defaults.
    pub fn load() -> Result<Aliases> {
        Aliases::load_with(&Locations::current(), None)
    }

    /// Aliases.Load: defaults, then the global file, then the optional
    /// context specific file.
    pub fn load_with(locs: &Locations, context_file: Option<&Path>) -> Result<Aliases> {
        let mut a = Aliases::default();
        a.reload_with(locs, context_file)?;
        Ok(a)
    }

    /// Aliases.Load over the current map (command.Reset without nuke keeps
    /// what is there; call on a cleared map to nuke).
    pub fn reload_with(&mut self, locs: &Locations, context_file: Option<&Path>) -> Result<()> {
        self.load_default_aliases();
        let a = self;
        let global = match locs.ensure_aliases_cfg_file() {
            Ok(f) => f,
            Err(e) => {
                tracing::error!(error = %e, "Unable to gen config aliases");
                locs.config_dir.join("aliases.yaml")
            }
        };
        a.load_file(&global)?;
        if let Some(p) = context_file {
            a.load_file(p)?;
        }
        Ok(())
    }

    /// Defaults merged with a single file.
    pub fn load_from(path: &Path) -> Result<Aliases> {
        let mut a = Aliases::defaults();
        a.load_file(path)?;
        Ok(a)
    }

    /// Aliases.LoadFile: merge the aliases of `path` (missing file is fine).
    pub fn load_file(&mut self, path: &Path) -> Result<()> {
        if !path.exists() {
            return Ok(());
        }
        let bb = std::fs::read_to_string(path)?;
        if let Err(e) = super::k9s::json::validate(super::k9s::json::ALIASES_SCHEMA, &bb) {
            tracing::warn!(error = %e, "Aliases validation failed");
        }
        let v = super::k9s::yaml::parse_value(&bb)?;
        if let Some(m) = v.get("aliases").and_then(|m| m.as_mapping()) {
            for (k, val) in m {
                let k = scalar_string(k);
                let val = match val {
                    serde_yaml::Value::Null => String::new(),
                    other => scalar_string(other),
                };
                self.aliases.insert(k, val);
            }
        } else if let Some(other) = v.get("aliases").filter(|v| !v.is_null()) {
            anyhow::bail!("cannot unmarshal aliases: expected a mapping, got {other:?}");
        }
        Ok(())
    }

    /// Save to `<config>/aliases.yaml`.
    pub fn save(&self) -> Result<()> {
        self.save_to(&paths::aliases_file())
    }

    /// Save to a given file.
    pub fn save_to(&self, path: &Path) -> Result<()> {
        paths::ensure_dir_path(path)?;
        let s = super::k9s::yaml::to_string(self)?;
        paths::write_file(path, s.as_bytes())
    }

    /// Add discovered API resources as aliases to their GVR (dao.Alias.load):
    /// resource name forms, plural/singular for standard groups, short names,
    /// lowercased kind for CRDs, and the GVR string itself. First definition
    /// wins.
    pub fn add_resources(&mut self, metas: &[crate::k8s::ResourceMeta]) {
        let mut std = Vec::new();
        let mut crds = Vec::new();
        for m in metas {
            let gvr = &m.gvr;
            if gvr.version.is_empty() || m.categories.iter().any(|c| c == "k9s") {
                continue;
            }
            let s = gvr.to_string();
            if DEPRECATED_GVRS.contains(&s.as_str()) {
                continue;
            }
            if m.categories.iter().any(|c| c == "crd") || !is_standard_group(&gvr.gv()) {
                crds.push(m);
            } else {
                std.push(m);
            }
        }
        std.sort_by_key(|m| m.gvr.to_string());
        crds.sort_by_key(|m| m.gvr.to_string());
        for m in std {
            let gvr = m.gvr.to_string();
            let singular = singular_name(m);
            self.define(&gvr, &[&as_resource_name(&m.gvr)]);
            if is_standard_group(&m.gvr.gv()) && m.gvr.group != "metrics.k8s.io" {
                self.define(&gvr, &[&m.gvr.resource, &singular]);
            }
            let shorts: Vec<&str> = m.short_names.iter().map(String::as_str).collect();
            self.define(&gvr, &shorts);
            self.define(&gvr, &[&gvr]);
        }
        for m in crds {
            let gvr = m.gvr.to_string();
            let singular = singular_name(m);
            self.define(&gvr, &[&m.kind.to_lowercase(), &m.gvr.resource]);
            self.define(&gvr, &[&singular]);
            let shorts: Vec<&str> = m.short_names.iter().map(String::as_str).collect();
            self.define(&gvr, &shorts);
            self.define(&gvr, &[&gvr]);
            self.define(&gvr, &[&format!("{}.{}", m.gvr.resource, m.gvr.group)]);
        }
    }

    /// Aliases.Get.
    pub fn get(&self, alias: &str) -> Option<&str> {
        self.aliases.get(alias).map(String::as_str)
    }

    /// Resolve an alias (or raw GVR string) to a GVR string, following
    /// alias-to-alias definitions (`pp: pod default`) like Aliases.Resolve.
    pub fn resolve(&self, alias: &str) -> Option<String> {
        let cmd = first_token(alias);
        let mut gvr = self.get(&cmd).or_else(|| self.get(alias))?.to_string();
        if is_k8s_res(&gvr) {
            return Some(gvr);
        }
        // Guard against alias cycles (k9s would spin forever).
        for _ in 0..64 {
            if is_k8s_res(&gvr) {
                return Some(gvr);
            }
            let next = first_token(&gvr);
            gvr = self.get(&next)?.to_string();
        }
        None
    }

    /// All aliases mapping to `gvr` (sorted).
    pub fn aliases_for(&self, gvr: &str) -> Vec<String> {
        self.aliases
            .iter()
            .filter(|(_, v)| v.as_str() == gvr)
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// GVR → its aliases (Aliases.ShortNames), aliases sorted.
    pub fn short_names(&self) -> BTreeMap<String, Vec<String>> {
        let mut m: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (a, g) in &self.aliases {
            m.entry(g.clone()).or_default().push(a.clone());
        }
        m
    }

    /// Remove all aliases.
    pub fn clear(&mut self) {
        self.aliases.clear();
    }

    /// Aliases.Define: add aliases for `gvr`; existing and blank aliases are
    /// left alone.
    pub fn define(&mut self, gvr: &str, aliases: &[&str]) {
        for a in aliases {
            if a.is_empty() || self.aliases.contains_key(*a) {
                continue;
            }
            self.aliases.insert(a.to_string(), gvr.to_string());
        }
    }
}

fn singular_name(m: &crate::k8s::ResourceMeta) -> String {
    if m.singular_name.is_empty() {
        m.kind.to_lowercase()
    } else {
        m.singular_name.clone()
    }
}

fn first_token(s: &str) -> String {
    s.split_whitespace().next().unwrap_or("").to_lowercase()
}

fn scalar_string(v: &serde_yaml::Value) -> String {
    match v {
        serde_yaml::Value::String(s) => s.clone(),
        serde_yaml::Value::Bool(b) => b.to_string(),
        serde_yaml::Value::Number(n) => n.to_string(),
        serde_yaml::Value::Null => String::new(),
        other => serde_yaml::to_string(other)
            .unwrap_or_default()
            .trim()
            .to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::paths::{MapEnv, Xdg, K9S_ENV_CONFIG_DIR};
    use crate::k8s::{Gvr, ResourceMeta};

    fn td(p: &str) -> std::path::PathBuf {
        format!(
            "{}/src/config/testdata/config/aliases/{p}",
            env!("CARGO_MANIFEST_DIR")
        )
        .into()
    }

    fn test_aliases() -> Aliases {
        let mut a = Aliases::default();
        for (k, v) in [
            ("a1", "gvr1"),
            ("a11", "gvr1"),
            ("a2", "gvr2"),
            ("a3", "gvr3"),
        ] {
            a.aliases.insert(k.into(), v.into());
        }
        a
    }

    #[test]
    fn clear_keys_short_names() {
        let mut a = test_aliases();
        assert_eq!(
            a.aliases.keys().collect::<Vec<_>>(),
            vec!["a1", "a11", "a2", "a3"]
        );
        let ss = a.short_names();
        assert_eq!(ss.len(), 3);
        assert_eq!(ss["gvr1"], vec!["a1", "a11"]);
        assert_eq!(ss["gvr2"], vec!["a2"]);
        assert_eq!(a.aliases_for("gvr1"), vec!["a1", "a11"]);
        a.clear();
        assert!(a.aliases.is_empty());
    }

    #[test]
    fn define_first_wins() {
        let mut a = Aliases::default();
        a.define("one", &["blee", "duh"]);
        a.define("two", &["blee", "duh", "fred", "zorg", ""]);
        assert_eq!(a.get("blee"), Some("one"));
        assert_eq!(a.get("duh"), Some("one"));
        assert_eq!(a.get("fred"), Some("two"));
        assert_eq!(a.get("zorg"), Some("two"));
        assert_eq!(a.get(""), None);
    }

    #[test]
    fn defaults_count() {
        let a = Aliases::defaults();
        assert_eq!(a.aliases.len(), 46);
        assert_eq!(a.get("?"), Some("help"));
        assert_eq!(a.get("q!"), Some("quit"));
        assert_eq!(a.get("hz"), Some("pulses"));
        assert_eq!(a.get("ctx"), Some("contexts"));
        assert_eq!(a.get("wk"), Some("workloads"));
        assert_eq!(a.get("chart"), Some("helm"));
    }

    #[test]
    fn load() {
        // TestAliasesLoad: defaults + <config>/aliases.yaml + plain.yaml.
        let dir = td("");
        let env = MapEnv::new(&[(K9S_ENV_CONFIG_DIR, dir.to_str().unwrap())]);
        let locs = Locations::resolve(&env, &Xdg::resolve(&env, Path::new("/home/x"), false));
        let a = Aliases::load_with(&locs, Some(&td("plain.yaml"))).unwrap();
        assert_eq!(a.aliases.len(), 55);
        assert_eq!(a.get("dp"), Some("apps/v1/deployments"));
        assert_eq!(a.get("pe"), Some("v1/pods"));
        assert_eq!(a.get("np"), Some("networking.k8s.io/v1/networkpolicies"));
    }

    #[test]
    fn load_creates_template() {
        let tmp = tempfile::tempdir().unwrap();
        let env = MapEnv::new(&[(K9S_ENV_CONFIG_DIR, tmp.path().to_str().unwrap())]);
        let locs = Locations::resolve(&env, &Xdg::resolve(&env, Path::new("/home/x"), false));
        let a = Aliases::load_with(&locs, None).unwrap();
        assert!(tmp.path().join("aliases.yaml").exists());
        assert_eq!(a.get("dp"), Some("deployments"));
        assert_eq!(a.aliases.len(), 54);
    }

    #[test]
    fn save_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("test-aliases/aliases.yaml");
        let a = test_aliases();
        a.save_to(&p).unwrap();
        assert_eq!(
            std::fs::read_to_string(&p).unwrap(),
            "aliases:\n  a1: gvr1\n  a2: gvr2\n  a3: gvr3\n  a11: gvr1\n"
        );
        let mut b = Aliases::default();
        b.load_file(&p).unwrap();
        assert_eq!(b, a);
    }

    #[test]
    fn resolve() {
        let mut a = Aliases::default();
        a.define("v1/pods", &["po", "pipo", "pod"]);
        a.define("v1/pods", &["v1/pods"]);
        a.define("v1/pods", &["pods"]);
        a.define("workloads", &["workloads", "workload", "wkl"]);
        a.define("pod default", &["pp"]);
        a.define("pipo a=b,b=c default", &["ppo"]);
        a.define("pod default app=fred @fred", &["ppc"]);
        a.define("pod /cilium kube-system", &["pc"]);
        a.define("pod 'app in (be,fe)'", &["ppp"]);
        for (exp, e) in [
            ("v1/pods", Some("v1/pods")),
            ("pod", Some("v1/pods")),
            ("pods", Some("v1/pods")),
            ("po", Some("v1/pods")),
            ("po 'a in (b,c)' @zorb bozo", Some("v1/pods")),
            ("pipo", Some("v1/pods")),
            ("zorg", None),
            ("wkl", Some("workloads")),
            ("pp", Some("v1/pods")),
            ("ppo", Some("v1/pods")),
            ("ppc", Some("v1/pods")),
            ("pc", Some("v1/pods")),
            ("ppp", Some("v1/pods")),
            ("PO", Some("v1/pods")),
        ] {
            assert_eq!(a.resolve(exp).as_deref(), e, "{exp}");
        }
        // Cycles do not hang.
        a.define("loop x", &["loop"]);
        assert_eq!(a.resolve("loop"), None);
    }

    fn meta(gvr: &str, kind: &str, singular: &str, shorts: &[&str], cats: &[&str]) -> ResourceMeta {
        ResourceMeta {
            gvr: Gvr::parse(gvr),
            kind: kind.into(),
            singular_name: singular.into(),
            short_names: shorts.iter().map(|s| s.to_string()).collect(),
            namespaced: true,
            verbs: vec![],
            categories: cats.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn add_resources() {
        let mut a = Aliases::defaults();
        a.add_resources(&[
            meta("v1/pods", "Pod", "pod", &["po"], &[]),
            meta("apps/v1/deployments", "Deployment", "", &["deploy"], &[]),
            meta("metrics.k8s.io/v1beta1/pods", "PodMetrics", "", &[], &[]),
            meta("v1/events", "Event", "event", &["ev"], &[]),
            meta("events.k8s.io/v1/events", "Event", "event", &["ev"], &[]),
            meta(
                "rbac.authorization.k8s.io/v1/clusterroles",
                "ClusterRole",
                "clusterrole",
                &[],
                &[],
            ),
            meta(
                "cilium.io/v2/ciliumnetworkpolicies",
                "CiliumNetworkPolicy",
                "ciliumnetworkpolicy",
                &["cnp"],
                &["crd"],
            ),
            meta("contexts", "Context", "context", &["ctx"], &["k9s"]),
        ]);
        assert_eq!(a.get("pod"), Some("v1/pods"));
        assert_eq!(a.get("pods"), Some("v1/pods"));
        assert_eq!(a.get("po"), Some("v1/pods"));
        assert_eq!(a.get("v1/pods"), Some("v1/pods"));
        assert_eq!(a.get("deployments"), Some("apps/v1/deployments"));
        assert_eq!(a.get("deployment"), Some("apps/v1/deployments"));
        assert_eq!(a.get("deploy"), Some("apps/v1/deployments"));
        assert_eq!(a.get("deployments.v1.apps"), Some("apps/v1/deployments"));
        // metrics pods do not take over `pods`.
        assert_eq!(
            a.get("pods.v1beta1.metrics.k8s.io"),
            Some("metrics.k8s.io/v1beta1/pods")
        );
        // deprecated v1/events skipped.
        assert_eq!(a.get("ev"), Some("events.k8s.io/v1/events"));
        assert_eq!(a.get("v1/events"), None);
        assert_eq!(
            a.get("clusterroles"),
            Some("rbac.authorization.k8s.io/v1/clusterroles")
        );
        // CRD aliases.
        let cnp = Some("cilium.io/v2/ciliumnetworkpolicies");
        assert_eq!(a.get("ciliumnetworkpolicy"), cnp);
        assert_eq!(a.get("ciliumnetworkpolicies"), cnp);
        assert_eq!(a.get("cnp"), cnp);
        assert_eq!(a.get("ciliumnetworkpolicies.cilium.io"), cnp);
        // k9s pseudo resources are untouched.
        assert_eq!(a.get("ctx"), Some("contexts"));
        // Template style aliases resolve through discovery.
        a.aliases.insert("dp".into(), "deployments".into());
        assert_eq!(a.resolve("dp").as_deref(), Some("apps/v1/deployments"));
    }

    #[test]
    fn k8s_res() {
        assert!(is_k8s_res("v1/pods"));
        assert!(is_k8s_res("workloads"));
        assert!(!is_k8s_res("pod default"));
        assert!(!is_k8s_res("deployments"));
        assert!(is_standard_group("networking.k8s.io/v1"));
        assert!(!is_standard_group("cilium.io/v2"));
    }
}
