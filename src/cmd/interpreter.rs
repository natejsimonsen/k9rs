//! Port of k9s `view/cmd` Interpreter + args, and `config.Aliases.Resolve`.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use regex::Regex;

use crate::config::Aliases;
use crate::model::labels::{parse_selector, Selector};

pub(crate) const NS_KEY: &str = "ns";
pub(crate) const TOPIC_KEY: &str = "topic";
pub(crate) const FILTER_KEY: &str = "filter";
pub(crate) const FUZZY_KEY: &str = "fuzzy";
pub(crate) const LABEL_KEY: &str = "labels";
pub(crate) const CONTEXT_KEY: &str = "context";

const FUZZY_FLAG: &str = "-f";
const FILTER_FLAG: &str = "/";
const CONTEXT_FLAG: &str = "@";
const NS_FLAG: &str = "-n";
const LABEL_FLAG: &str = "-l";
const LABEL_FLAGS: [&str; 5] = ["=", "==", "!=", " in ", " notin "];

const COW_CMD: &str = "cow";
const CAN_CMD: &str = "can";
const CONTEXT_CMDS: [&str; 3] = ["ctx", "context", "contexts"];
const NAMESPACE_CMDS: [&str; 3] = ["ns", "namespace", "namespaces"];
const DIR_CMDS: [&str; 4] = ["dir", "dirs", "d", "ls"];
const BAIL_CMDS: [&str; 6] = ["q", "q!", "qa", "Q", "quit", "exit"];
const HELP_CMDS: [&str; 3] = ["?", "h", "help"];
const ALIAS_CMDS: [&str; 3] = ["a", "alias", "aliases"];
const XRAY_CMDS: [&str; 3] = ["x", "xr", "xray"];

/// Parsed prompt arguments keyed by `ns`, `topic`, `filter`, `fuzzy`,
/// `labels`, `context` (k9s `cmd.args`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Args(pub BTreeMap<String, String>);

impl Args {
    fn get(&self, k: &str) -> Option<&String> {
        self.0.get(k)
    }
    fn set(&mut self, k: &str, v: String) {
        self.0.insert(k.to_string(), v);
    }

    /// True when a filter, fuzzy filter or label selector is present.
    pub fn has_filters(&self) -> bool {
        [FILTER_KEY, FUZZY_KEY, LABEL_KEY]
            .iter()
            .any(|k| self.0.contains_key(*k))
    }
}

impl std::fmt::Display for Args {
    /// args.String(): values in key order, decorated with their flag.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ss: Vec<String> = self
            .0
            .iter()
            .map(|(k, v)| match k.as_str() {
                LABEL_KEY => format!("'{v}'"),
                FILTER_KEY => format!("{FILTER_FLAG}{v}"),
                CONTEXT_KEY => format!("{CONTEXT_FLAG}{v}"),
                _ => v.clone(),
            })
            .collect();
        f.write_str(&ss.join(" "))
    }
}

fn is_label_arg(a: &str) -> bool {
    LABEL_FLAGS.iter().any(|f| a.contains(f))
}

/// Tracks a `:` prompt line (k9s `cmd.Interpreter`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Interpreter {
    line: String,
    cmd: String,
    aliases: Vec<String>,
    args: Args,
}

impl Interpreter {
    pub fn new(line: &str) -> Self {
        let mut i = Interpreter {
            line: line.to_string(),
            ..Default::default()
        };
        i.grok();
        i
    }

    fn grok(&mut self) {
        let Some(first) = self.line.split_whitespace().next().map(str::to_string) else {
            return;
        };
        self.cmd = first.to_lowercase();
        let mut lbls = String::new();
        let mut line = self.line.replacen(&first, "", 1).trim().to_string();
        if line.contains('\'') {
            match quote_indices(&line) {
                Some((start, end)) => {
                    lbls = line[start + 1..end].to_string();
                    line = line
                        .replacen(&format!("'{lbls}'"), "", 1)
                        .trim()
                        .to_string();
                }
                None => {
                    tracing::error!("Unmatched single quote in command line: {}", self.line);
                    line.clear();
                }
            }
        }
        let mut ff: Vec<String> = line.split_whitespace().map(str::to_string).collect();
        if !lbls.is_empty() {
            ff.push(lbls);
        }
        self.args = self.new_args(&ff);
    }

    fn new_args(&self, aa: &[String]) -> Args {
        let mut args = Args::default();
        let mut i = 0;
        while i < aa.len() {
            let a = aa[i].trim();
            if a.starts_with(FUZZY_FLAG) {
                if a == FUZZY_FLAG {
                    i += 1;
                    if let Some(n) = aa.get(i) {
                        args.set(FUZZY_KEY, n.trim().to_lowercase());
                    }
                } else {
                    args.set(FUZZY_KEY, a[2..].to_lowercase());
                }
            } else if a.starts_with(FILTER_FLAG) {
                if self.is_dir_cmd() {
                    if args.get(TOPIC_KEY).is_none() {
                        args.set(TOPIC_KEY, a.to_string());
                    }
                } else {
                    args.set(FILTER_KEY, a[1..].to_lowercase());
                }
            } else if let Some(c) = a.strip_prefix(CONTEXT_FLAG) {
                args.set(CONTEXT_KEY, c.to_string());
            } else if a == NS_FLAG && i + 1 < aa.len() {
                // k9rs: explicit `-n ns`.
                i += 1;
                args.set(NS_KEY, aa[i].trim().to_lowercase());
            } else if a == LABEL_FLAG && i + 1 < aa.len() {
                // k9rs: explicit `-l selector`.
                i += 1;
                args.set(LABEL_KEY, aa[i].trim().to_lowercase());
            } else if is_label_arg(a) {
                let l = a.strip_prefix(LABEL_FLAG).unwrap_or(a);
                args.set(LABEL_KEY, l.to_lowercase());
            } else if self.is_context_cmd() {
                args.set(CONTEXT_KEY, a.to_string());
            } else if self.is_dir_cmd() {
                if args.get(TOPIC_KEY).is_none() {
                    args.set(TOPIC_KEY, a.to_string());
                }
            } else if self.is_xray_cmd() {
                if args.get(TOPIC_KEY).is_some() {
                    args.set(NS_KEY, a.to_lowercase());
                } else {
                    args.set(TOPIC_KEY, a.to_lowercase());
                }
            } else {
                args.set(NS_KEY, a.to_lowercase());
            }
            i += 1;
        }
        args
    }

    /// Parsed arguments.
    pub fn args(&self) -> &Args {
        &self.args
    }

    /// Clears the namespace argument, if any.
    pub fn clear_ns(&mut self) {
        self.switch_ns("");
    }

    /// Replaces the namespace argument with `ns` (appends when missing).
    pub fn switch_ns(&mut self, ns: &str) {
        if let Some(ons) = self.ns_arg() {
            let line = self
                .line
                .replacen(&format!(" {ons}"), &format!(" {ns}"), 1)
                .trim()
                .to_string();
            self.reset(&line, "");
            return;
        }
        if !ns.is_empty() {
            let line = format!("{} {ns}", self.line.trim());
            self.reset(&line, "");
        }
    }

    /// Takes `p`'s command and overlays its args (Interpreter.Merge).
    pub fn merge(&mut self, p: &Interpreter) {
        self.cmd = p.cmd.clone();
        for (k, v) in &p.args.0 {
            self.args.0.insert(k.clone(), v.clone());
        }
        self.line = format!("{} {}", self.cmd, self.args);
    }

    /// Takes `c1`'s command and its non-empty args (Interpreter.Amend).
    pub fn amend(&mut self, c1: &Interpreter) {
        self.cmd = c1.cmd.clone();
        for (k, v) in &c1.args.0 {
            if !v.is_empty() {
                self.args.0.insert(k.clone(), v.clone());
            }
        }
    }

    /// Re-parses with a new line, remembering `alias` (Interpreter.Reset).
    pub fn reset(&mut self, line: &str, alias: &str) -> &mut Self {
        self.line = line.to_string();
        self.grok();
        if !alias.is_empty() && alias != self.cmd && !self.aliases.iter().any(|a| a == alias) {
            self.aliases.push(alias.to_string());
        }
        self
    }

    pub fn has_ns(&self) -> bool {
        self.args.get(NS_KEY).is_some_and(|n| !n.is_empty())
    }
    /// The lowercased command token.
    pub fn cmd(&self) -> &str {
        &self.cmd
    }
    /// Aliases seen while resolving this command.
    pub fn aliases(&self) -> &[String] {
        &self.aliases
    }
    /// The line minus the command token.
    pub fn args_line(&self) -> String {
        self.line.replacen(&self.cmd, "", 1).trim().to_string()
    }
    pub fn is_blank(&self) -> bool {
        self.line.is_empty()
    }
    /// The trimmed prompt line.
    pub fn get_line(&self) -> String {
        self.line.trim().to_string()
    }

    pub fn is_cow_cmd(&self) -> bool {
        self.cmd == COW_CMD
    }
    pub fn is_help_cmd(&self) -> bool {
        HELP_CMDS.contains(&self.cmd.as_str())
    }
    pub fn is_bail_cmd(&self) -> bool {
        BAIL_CMDS.contains(&self.cmd.as_str())
    }
    pub fn is_alias_cmd(&self) -> bool {
        ALIAS_CMDS.contains(&self.cmd.as_str())
    }
    pub fn is_xray_cmd(&self) -> bool {
        XRAY_CMDS.contains(&self.cmd.as_str())
    }
    pub fn is_context_cmd(&self) -> bool {
        CONTEXT_CMDS.contains(&self.cmd.as_str())
    }
    pub fn is_namespace_cmd(&self) -> bool {
        NAMESPACE_CMDS.contains(&self.cmd.as_str())
    }
    pub fn is_dir_cmd(&self) -> bool {
        DIR_CMDS.contains(&self.cmd.as_str())
    }
    pub fn is_rbac_cmd(&self) -> bool {
        self.cmd == CAN_CMD
    }

    /// Context argument for `ctx xxx` or any line holding `@xxx`.
    pub fn context_arg(&self) -> Option<String> {
        if self.is_context_cmd() || self.line.contains(CONTEXT_FLAG) {
            return Some(self.args.get(CONTEXT_KEY).cloned().unwrap_or_default());
        }
        None
    }
    /// Removes the context argument.
    pub fn reset_context_arg(&mut self) {
        self.args.0.remove(CONTEXT_KEY);
    }
    pub fn dir_arg(&self) -> Option<String> {
        if !self.is_dir_cmd() {
            return None;
        }
        self.args.get(TOPIC_KEY).filter(|d| !d.is_empty()).cloned()
    }
    pub fn cow_arg(&self) -> Option<String> {
        if !self.is_cow_cmd() {
            return None;
        }
        self.args.get(NS_KEY).filter(|m| !m.is_empty()).cloned()
    }
    /// `can [u|g|s]:subject` → (category, subject).
    pub fn rbac_args(&self) -> Option<(String, String)> {
        if !self.is_rbac_cmd() {
            return None;
        }
        static RX: OnceLock<Regex> = OnceLock::new();
        let rx = RX.get_or_init(|| Regex::new(r"^can\s+([ugs]):\s*([\w:-]+)\s*$").unwrap());
        let c = rx.captures(&self.line)?;
        Some((c[1].to_string(), c[2].to_string()))
    }
    /// `xray res [ns]` → (res, ns or "").
    pub fn xray_args(&self) -> Option<(String, String)> {
        if !self.is_xray_cmd() {
            return None;
        }
        let gvr = self.args.get(TOPIC_KEY)?;
        Some((
            gvr.clone(),
            self.args.get(NS_KEY).cloned().unwrap_or_default(),
        ))
    }
    pub fn filter_arg(&self) -> Option<String> {
        self.args.get(FILTER_KEY).filter(|f| !f.is_empty()).cloned()
    }
    pub fn fuzzy_arg(&self) -> Option<String> {
        self.args.get(FUZZY_KEY).filter(|f| !f.is_empty()).cloned()
    }
    /// Namespace argument, when present and non blank.
    pub fn ns_arg(&self) -> Option<String> {
        self.args.get(NS_KEY).filter(|n| !n.is_empty()).cloned()
    }
    pub fn has_context(&self) -> Option<String> {
        self.args
            .get(CONTEXT_KEY)
            .filter(|c| !c.is_empty())
            .cloned()
    }
    /// Raw label selector argument ("" when none).
    pub fn labels_arg(&self) -> &str {
        self.args.get(LABEL_KEY).map(String::as_str).unwrap_or("")
    }
    /// Parsed label selector (empty selector when none).
    pub fn labels_selector(&self) -> Result<Selector, String> {
        parse_selector(self.labels_arg())
    }
}

fn quote_indices(s: &str) -> Option<(usize, usize)> {
    let mut it = s.match_indices('\'').map(|(i, _)| i);
    let start = it.next()?;
    let end = it.next()?;
    Some((start, end))
}

/// k9s built-in aliases (config.Aliases.loadDefaultAliases), used when the
/// alias map lacks a key.
const BUILTIN_ALIASES: &[(&str, &[&str])] = &[
    ("help", &["help", "h", "?"]),
    ("quit", &["quit", "q", "q!", "qa", "Q"]),
    ("aliases", &["aliases", "alias", "a"]),
    ("helm", &["helm", "charts", "chart", "hm"]),
    ("dirs", &["dirs", "dir", "d"]),
    ("contexts", &["contexts", "context", "ctx"]),
    ("users", &["users", "user", "usr"]),
    ("groups", &["groups", "group", "grp"]),
    ("portforwards", &["portforwards", "portforward", "pf"]),
    ("benchmarks", &["benchmarks", "benchmark", "bench"]),
    ("screendumps", &["screendumps", "screendump", "sd"]),
    ("pulses", &["pulses", "pulse", "pu", "hz"]),
    ("xrays", &["xrays", "xray", "x"]),
    ("workloads", &["workloads", "workload", "wk"]),
];

/// Built-in alias names.
pub fn builtin_alias_names() -> impl Iterator<Item = &'static str> {
    BUILTIN_ALIASES
        .iter()
        .flat_map(|(_, aa)| aa.iter().copied())
}

fn builtin_alias(a: &str) -> Option<&'static str> {
    BUILTIN_ALIASES
        .iter()
        .find(|(_, aa)| aa.contains(&a))
        .map(|(g, _)| *g)
}

/// k9s pseudo resources that count as resources (client.reservedGVRs).
const RESERVED_GVRS: [&str; 22] = [
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

/// client.GVR.IsK8sRes: a real (or reserved pseudo) resource, not an alias
/// to a command line.
pub fn is_k8s_res(raw: &str) -> bool {
    (!raw.contains(' ') && raw.contains('/') && !raw.contains(" /")) || RESERVED_GVRS.contains(&raw)
}

/// Alias lookup: the alias map first, then k9s built-ins, then a GVR string
/// that some alias already points to (k9s registers every GVR under itself).
pub fn lookup_alias(aliases: &Aliases, a: &str) -> Option<String> {
    if let Some(g) = aliases.aliases.get(a) {
        return Some(g.clone());
    }
    if let Some(g) = builtin_alias(a) {
        return Some(g.to_string());
    }
    if is_k8s_res(a) && aliases.aliases.values().any(|v| v == a) {
        return Some(a.to_string());
    }
    None
}

const MAX_ALIAS_DEPTH: usize = 32;

/// config.Aliases.Resolve: resolve the command token of `p` to a GVR string,
/// expanding command-line aliases (e.g. `pp: pod default`) and rewriting `p`
/// to the canonical line (`v1/pods default`).
pub fn resolve(p: &mut Interpreter, aliases: &Aliases) -> Option<String> {
    let mut gvr = lookup_alias(aliases, p.cmd())?;
    if is_k8s_res(&gvr) {
        let cmd = p.cmd().to_string();
        let line = p.get_line().replacen(&cmd, &gvr, 1);
        p.reset(&line, &cmd);
        return Some(gvr);
    }
    let mut depth = 0;
    while !is_k8s_res(&gvr) {
        depth += 1;
        if depth > MAX_ALIAS_DEPTH {
            return None;
        }
        let mut ap = Interpreter::new(&gvr);
        gvr = lookup_alias(aliases, ap.cmd())?;
        ap.merge(p);
        let ap_cmd = ap.cmd().to_string();
        let line = ap.get_line().replacen(&ap_cmd, &gvr, 1);
        p.reset(&line, &ap_cmd);
    }
    Some(gvr)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(pairs: &[(&str, &str)]) -> Args {
        Args(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    #[test]
    fn flags_new() {
        // view/cmd/args_test.go TestFlagsNew
        let s = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let po = Interpreter::new("po");
        let cases: Vec<(Interpreter, Vec<String>, Args)> = vec![
            (po.clone(), vec![], args(&[])),
            (po.clone(), s(&["ns1"]), args(&[(NS_KEY, "ns1")])),
            (po.clone(), s(&[" ns1 "]), args(&[(NS_KEY, "ns1")])),
            (po.clone(), s(&["/fred"]), args(&[(FILTER_KEY, "fred")])),
            (po.clone(), s(&["/!fred"]), args(&[(FILTER_KEY, "!fred")])),
            (po.clone(), s(&["-f", "fred"]), args(&[(FUZZY_KEY, "fred")])),
            (po.clone(), s(&["-ffred"]), args(&[(FUZZY_KEY, "fred")])),
            (
                po.clone(),
                s(&["/fred", "  ns1 "]),
                args(&[(NS_KEY, "ns1"), (FILTER_KEY, "fred")]),
            ),
            (
                po.clone(),
                s(&["app=fred"]),
                args(&[(LABEL_KEY, "app=fred")]),
            ),
            (po.clone(), s(&["="]), args(&[(LABEL_KEY, "=")])),
            (
                po.clone(),
                s(&["app=fred,blee=duh"]),
                args(&[(LABEL_KEY, "app=fred,blee=duh")]),
            ),
            (
                po.clone(),
                s(&["a=b,c=d", "  ns1  "]),
                args(&[(LABEL_KEY, "a=b,c=d"), (NS_KEY, "ns1")]),
            ),
            (
                po.clone(),
                s(&["app=fred", "ns1", "-f", "blee", "/zorg"]),
                args(&[
                    (FILTER_KEY, "zorg"),
                    (FUZZY_KEY, "blee"),
                    (LABEL_KEY, "app=fred"),
                    (NS_KEY, "ns1"),
                ]),
            ),
            (
                po.clone(),
                s(&["app=fred", "ns1", "-f", "blee", "/zorg", "@ctx1"]),
                args(&[
                    (FILTER_KEY, "zorg"),
                    (FUZZY_KEY, "blee"),
                    (LABEL_KEY, "app=fred"),
                    (NS_KEY, "ns1"),
                    (CONTEXT_KEY, "ctx1"),
                ]),
            ),
            (
                po.clone(),
                s(&["app=fred", "ns1", "-f", "blee", "/zorg", "@zorg fred"]),
                args(&[
                    (FILTER_KEY, "zorg"),
                    (FUZZY_KEY, "blee"),
                    (LABEL_KEY, "app=fred"),
                    (NS_KEY, "ns1"),
                    (CONTEXT_KEY, "zorg fred"),
                ]),
            ),
            (
                po.clone(),
                s(&["@ctx1", "app=fred", "ns1", "-f", "blee", "/zorg"]),
                args(&[
                    (FILTER_KEY, "zorg"),
                    (FUZZY_KEY, "blee"),
                    (LABEL_KEY, "app=fred"),
                    (NS_KEY, "ns1"),
                    (CONTEXT_KEY, "ctx1"),
                ]),
            ),
            (
                po.clone(),
                s(&["app=fred", "ns1", "-f", "blee", "/zorg", "@Dev"]),
                args(&[
                    (FILTER_KEY, "zorg"),
                    (FUZZY_KEY, "blee"),
                    (LABEL_KEY, "app=fred"),
                    (NS_KEY, "ns1"),
                    (CONTEXT_KEY, "Dev"),
                ]),
            ),
            (
                Interpreter::new("ctx"),
                s(&["Dev"]),
                args(&[(CONTEXT_KEY, "Dev")]),
            ),
            (Interpreter::new("apply -f"), vec![], args(&[])),
            // k9rs extensions.
            (po.clone(), s(&["-n", "Fred"]), args(&[(NS_KEY, "fred")])),
            (
                po.clone(),
                s(&["-l", "app=x"]),
                args(&[(LABEL_KEY, "app=x")]),
            ),
            (po.clone(), s(&["-lapp=x"]), args(&[(LABEL_KEY, "app=x")])),
        ];
        for (i, aa, e) in cases {
            assert_eq!(i.new_args(&aa), e, "{aa:?}");
        }
    }

    #[test]
    fn flags_has_filters() {
        let po = Interpreter::new("po");
        let s = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        for (aa, ok) in [
            (s(&[]), false),
            (s(&["ns1"]), false),
            (s(&["/fred"]), true),
            (s(&["/!fred"]), true),
            (s(&["-f", "fred"]), true),
            (s(&["/fred", "ns1"]), true),
            (s(&["app=fred"]), true),
            (s(&["app=fred,blee=duh"]), true),
            (s(&["app=fred", "ns1"]), true),
        ] {
            assert_eq!(po.new_args(&aa).has_filters(), ok, "{aa:?}");
        }
    }

    #[test]
    fn args_string() {
        let a = args(&[
            (FILTER_KEY, "zorg"),
            (LABEL_KEY, "app=fred"),
            (NS_KEY, "ns1"),
            (CONTEXT_KEY, "ctx1"),
        ]);
        assert_eq!(a.to_string(), "@ctx1 /zorg 'app=fred' ns1");
    }

    #[test]
    fn rbac_cmd() {
        for (cmd, e) in [
            ("", None),
            ("can u:fernand", Some(("u", "fernand"))),
            ("can   u:  fernand  ", Some(("u", "fernand"))),
            ("canopy u:bozo", None),
            ("can u:", None),
            ("can bozo", None),
            ("can u:bozo", Some(("u", "bozo"))),
            ("can g:bozo", Some(("g", "bozo"))),
            ("can s:bozo", Some(("s", "bozo"))),
            (
                "can s:kube-system:default",
                Some(("s", "kube-system:default")),
            ),
        ] {
            let p = Interpreter::new(cmd);
            let e = e.map(|(a, b)| (a.to_string(), b.to_string()));
            assert_eq!(p.rbac_args(), e, "{cmd}");
            if e.is_some() {
                assert!(p.is_rbac_cmd());
            }
        }
    }

    #[test]
    fn ns_cmd() {
        for (cmd, e) in [
            ("", None),
            ("pod fred", Some("fred")),
            ("pod      fred   ", Some("fred")),
            ("Deploy", None),
            ("DEPLOY Fred", Some("fred")),
            ("pod", None),
            ("pod app=blee fred @zorg", Some("fred")),
            ("pod app=blee blee @zorg", Some("blee")),
            ("pod -n fred", Some("fred")),
        ] {
            assert_eq!(Interpreter::new(cmd).ns_arg().as_deref(), e, "{cmd}");
        }
    }

    #[test]
    fn switch_ns() {
        for (cmd, ns, e) in [
            ("", "", ""),
            ("pod fred", "", "pod"),
            ("pod fred", "fred", "pod fred"),
            ("pod", "blee", "pod blee"),
            ("pod app=blee fred @zorg", "blee", "pod app=blee blee @zorg"),
            ("pod app=zorg zorg @zorg", "blee", "pod app=zorg blee @zorg"),
            ("pod app=blee @zorg", "blee", "pod app=blee @zorg blee"),
        ] {
            let mut p = Interpreter::new(cmd);
            p.switch_ns(ns);
            assert_eq!(p.get_line(), e, "{cmd} -> {ns}");
        }
    }

    #[test]
    fn clear_ns() {
        for (cmd, e) in [
            ("", ""),
            ("pod fred", "pod"),
            ("pod", "pod"),
            ("pod app=blee @zorg zorg", "pod app=blee @zorg"),
            ("pod app=blee @zorg", "pod app=blee @zorg"),
        ] {
            let mut p = Interpreter::new(cmd);
            p.clear_ns();
            assert_eq!(p.get_line(), e, "{cmd}");
        }
    }

    #[test]
    fn filter_cmd() {
        for (cmd, e) in [
            ("", None),
            ("pod /fred", Some("fred")),
            ("POD /FRED", Some("fred")),
            ("pod /fred ns1", Some("fred")),
            ("pod ns1 /fred", Some("fred")),
            ("pod ns1 /fred app=blee,fred=zorg", Some("fred")),
            ("pod /cilium kube-system", Some("cilium")),
        ] {
            assert_eq!(Interpreter::new(cmd).filter_arg().as_deref(), e, "{cmd}");
        }
    }

    #[test]
    fn label_cmd() {
        for (cmd, e) in [
            ("", Ok("")),
            ("pod fred=blee", Ok("fred=blee")),
            ("pod fred=blee,zorg=duh", Ok("fred=blee,zorg=duh")),
            (
                "pod 'fred in (blee,zorg),blee notin (zorg)'",
                Ok("blee notin (zorg),fred in (blee,zorg)"),
            ),
            ("pod ns-1", Ok("")),
            ("pod fred=blee,zorg=duh ns1", Ok("fred=blee,zorg=duh")),
            ("pod   fred=blee   ", Ok("fred=blee")),
            ("POD  FRED=BLEE   ", Ok("fred=blee")),
            (
                "pod =blee",
                Err("found '=', expected: !, identifier, or 'end of string'"),
            ),
            // grokLabels
            ("po @fred", Ok("")),
            ("po a=b,b=c @fred", Ok("a=b,b=c")),
            ("po 'a=b,b=c' @fred", Ok("a=b,b=c")),
            ("po 'a=b @fred", Ok("")),
            (
                "po 'a in (b,c),b notin (c,z)' fred'",
                Ok("a in (b,c),b notin (c,z)"),
            ),
            ("po -l app=fred", Ok("app=fred")),
        ] {
            let got = Interpreter::new(cmd)
                .labels_selector()
                .map(|s| s.to_string());
            assert_eq!(got, e.map(String::from).map_err(String::from), "{cmd}");
        }
    }

    #[test]
    fn xray_cmd() {
        for (cmd, e) in [
            ("", None),
            ("xray po", Some(("po", ""))),
            ("xray po ns1", Some(("po", "ns1"))),
            ("x DP NS1", Some(("dp", "ns1"))),
            ("xrayzor po", None),
            ("xray", None),
        ] {
            let e = e.map(|(a, b)| (a.to_string(), b.to_string()));
            assert_eq!(Interpreter::new(cmd).xray_args(), e, "{cmd}");
        }
    }

    #[test]
    fn dir_cmd() {
        for (cmd, e) in [
            ("", None),
            ("dir dir1", Some("dir1")),
            ("dir dir1 ns1", Some("dir1")),
            ("dirdel dir1", None),
            ("dir", None),
            ("dir DirName", Some("DirName")),
            ("dir /tmp", Some("/tmp")),
            ("ls /tmp /blee", Some("/tmp")),
        ] {
            assert_eq!(Interpreter::new(cmd).dir_arg().as_deref(), e, "{cmd}");
        }
    }

    #[test]
    fn context_cmd() {
        for (cmd, ok, ctx) in [
            ("", false, ""),
            ("context ctx1", true, "ctx1"),
            ("ctx ctx1", true, "ctx1"),
            ("ctxto ctx1", false, ""),
            ("ctx Dev", true, "Dev"),
            ("ctx kind-fred", true, "kind-fred"),
            ("contexts", true, ""),
        ] {
            let p = Interpreter::new(cmd);
            assert_eq!(p.is_context_cmd(), ok, "{cmd}");
            if ok {
                assert_eq!(p.context_arg().as_deref(), Some(ctx), "{cmd}");
            }
        }
        // `@ctx` on a resource command.
        assert_eq!(
            Interpreter::new("po @fred").context_arg().as_deref(),
            Some("fred")
        );
        assert_eq!(Interpreter::new("po").context_arg(), None);
        assert_eq!(
            Interpreter::new("po @fred").has_context().as_deref(),
            Some("fred")
        );
    }

    #[test]
    fn special_cmds() {
        for (cmd, help, bail, alias, cow) in [
            ("", false, false, false, false),
            ("help", true, false, false, false),
            ("?", true, false, false, false),
            ("h", true, false, false, false),
            ("helpme", false, false, false, false),
            ("hozer", false, false, false, false),
            ("quit", false, true, false, false),
            ("q", false, true, false, false),
            ("q!", false, true, false, false),
            ("qa", false, true, false, false),
            ("exit", false, true, false, false),
            ("zorg", false, false, false, false),
            ("quitter", false, false, false, false),
            ("alias", false, false, true, false),
            ("a", false, false, true, false),
            ("aliases", false, false, true, false),
            ("abba", false, false, false, false),
            ("cow", false, false, false, true),
            ("cow bumblebeetuna", false, false, false, true),
            ("cowdy", false, false, false, false),
        ] {
            let p = Interpreter::new(cmd);
            assert_eq!(p.is_help_cmd(), help, "{cmd}");
            assert_eq!(p.is_bail_cmd(), bail, "{cmd}");
            assert_eq!(p.is_alias_cmd(), alias, "{cmd}");
            assert_eq!(p.is_cow_cmd(), cow, "{cmd}");
        }
        assert_eq!(
            Interpreter::new("cow Moo").cow_arg().as_deref(),
            Some("moo")
        );
        assert_eq!(Interpreter::new("cow").cow_arg(), None);
        assert!(Interpreter::new("ns").is_namespace_cmd());
    }

    #[test]
    fn merge_and_amend() {
        let mut a = Interpreter::new("pod default app=fred @fred");
        let p = Interpreter::new("ppc /fred @bozo ns-1");
        a.merge(&p);
        assert_eq!(a.cmd(), "ppc");
        assert_eq!(a.get_line(), "ppc @bozo /fred 'app=fred' ns-1");

        let mut b = Interpreter::new("po ns1 /fred");
        b.amend(&Interpreter::new("dp @ctx"));
        assert_eq!(b.cmd(), "dp");
        assert_eq!(b.has_context().as_deref(), Some("ctx"));
        assert_eq!(b.ns_arg().as_deref(), Some("ns1"));
        // The line is untouched by amend, so the new cmd is not found in it.
        assert_eq!(b.args_line(), "po ns1 /fred");
        assert_eq!(Interpreter::new("po ns1 /fred").args_line(), "ns1 /fred");
        assert!(!b.is_blank());
        assert!(Interpreter::new("").is_blank());
    }

    fn test_aliases() -> Aliases {
        let mut a = Aliases::default();
        a.define("v1/pods", &["po", "pipo", "pod", "v1/pods", "pods"]);
        a.define("workloads", &["workloads", "workload", "wkl"]);
        a.define("pod default", &["pp"]);
        a.define("pipo a=b,b=c default", &["ppo"]);
        a.define("pod default app=fred @fred", &["ppc"]);
        a.define("pod /cilium kube-system", &["pc"]);
        a.define("pod 'app in (be,fe)'", &["ppp"]);
        a.define("pp /zorg", &["ppz"]);
        a.define("loop1 x", &["loop2"]);
        a.define("loop2 y", &["loop1"]);
        a
    }

    #[test]
    fn alias_resolve() {
        // config/alias_test.go TestAliasResolve
        let a = test_aliases();
        for (exp, e) in [
            ("v1/pods", Some(("v1/pods", "v1/pods"))),
            ("pod", Some(("v1/pods", "v1/pods"))),
            ("pods", Some(("v1/pods", "v1/pods"))),
            ("po", Some(("v1/pods", "v1/pods"))),
            (
                "po 'a in (b,c)' @zorb bozo",
                Some(("v1/pods", "v1/pods 'a in (b,c)' @zorb bozo")),
            ),
            ("pipo", Some(("v1/pods", "v1/pods"))),
            ("zorg", None),
            ("wkl", Some(("workloads", "workloads"))),
            ("pp", Some(("v1/pods", "v1/pods default"))),
            ("ppo", Some(("v1/pods", "v1/pods 'a=b,b=c' default"))),
            ("ppc", Some(("v1/pods", "v1/pods @fred 'app=fred' default"))),
            (
                "po /fred @bozo ns-1",
                Some(("v1/pods", "v1/pods /fred @bozo ns-1")),
            ),
            (
                "pipo /fred @bozo ns-1",
                Some(("v1/pods", "v1/pods /fred @bozo ns-1")),
            ),
            (
                "ppc /fred @bozo ns-1",
                Some(("v1/pods", "v1/pods @bozo /fred 'app=fred' ns-1")),
            ),
            ("pc", Some(("v1/pods", "v1/pods /cilium kube-system"))),
            ("ppp", Some(("v1/pods", "v1/pods 'app in (be,fe)'"))),
            // Nested aliases.
            ("ppz", Some(("v1/pods", "v1/pods /zorg default"))),
            // Cycles give up.
            ("loop1", None),
            // Built-ins work without an alias map entry.
            ("ctx", Some(("contexts", "contexts"))),
            ("hz", Some(("pulses", "pulses"))),
        ] {
            let mut p = Interpreter::new(exp);
            let got = resolve(&mut p, &a);
            match e {
                Some((gvr, line)) => {
                    assert_eq!(got.as_deref(), Some(gvr), "{exp}");
                    assert_eq!(p.get_line(), Interpreter::new(line).get_line(), "{exp}");
                }
                None => assert_eq!(got, None, "{exp}"),
            }
        }
    }

    #[test]
    fn k8s_res() {
        assert!(is_k8s_res("v1/pods"));
        assert!(is_k8s_res("apps/v1/deployments"));
        assert!(is_k8s_res("contexts"));
        assert!(!is_k8s_res("pod default"));
        assert!(!is_k8s_res("v1/pods /fred"));
        assert!(!is_k8s_res("pod"));
    }
}
