//! The `:` command interpreter (k9s view/cmd/interpreter.go + suggestions).
//!
//! Grammar (k9s): `<alias|gvr> [ns | -n ns] [@context] [/filter] [k=v,..|'sel'|-l sel] [-f fuzzy]`.
//! Words containing `=`, `==`, `!=`, ` in `, ` notin ` are label selectors;
//! a single-quoted span is taken as the selector verbatim.
//!
//! [`parse`] follows k9s `view.Command.run`/`specialCmd` dispatch:
//! - `cow msg`                → gvr `cow`, args `[msg]` (error `` Invalid command. Use `cow xxx` ``)
//! - `q`, `q!`, `qa`, `quit`, `exit` → gvr `quit`
//! - `?`, `h`, `help`         → gvr `help`
//! - `a`, `alias`, `aliases`  → gvr `aliases` (+ `/filter`)
//! - `x`/`xr`/`xray res [ns]` → gvr `xrays`, args `[resolved res gvr]`, namespace
//! - `can [u|g|s]:subject`    → gvr `policy`, args `[category, subject]`
//! - `ctx`/`context`/`contexts [name]` → gvr `contexts`, context = name if given
//! - `ns`/`namespace`/`namespaces` with a name → pods in that namespace
//!   (gvr `v1/pods`); without one → the namespaces resource
//! - `dir`/`dirs`/`d`/`ls path` → gvr `dirs`, args `[path]`
//! - `-`                      → gvr `last` (k9rs: same as the `-` key, last view)
//! - anything else resolves through the alias map (aliases may expand to
//!   full command lines); unknown → `` `foo` command not found ``.
//!
//! `pf`, `sd`, `bench`, `pulses`/`pu`/`hz`, `users`, `groups`, `wk`, `helm`
//! are plain aliases to pseudo-resource GVRs (`portforwards`, `screendumps`,
//! `benchmarks`, `pulses`, `users`, `groups`, `workloads`, `helm`).

pub mod history;
pub mod interpreter;
pub mod suggest;

pub use history::{History, MAX_HISTORY};
pub use interpreter::{is_k8s_res, lookup_alias, resolve, Args, Interpreter};
pub use suggest::{
    should_add_suggest, suggest_command, suggest_filter, suggest_sub_command, FishBuff,
};

use anyhow::anyhow;

use crate::config::Aliases;

/// Special command GVR names produced by [`parse`].
pub const QUIT_CMD: &str = "quit";
pub const HELP_CMD: &str = "help";
pub const ALIASES_CMD: &str = "aliases";
pub const COW_CMD: &str = "cow";
pub const XRAY_CMD: &str = "xrays";
pub const POLICY_CMD: &str = "policy";
pub const CONTEXTS_CMD: &str = "contexts";
pub const DIRS_CMD: &str = "dirs";
pub const LAST_CMD: &str = "last";

/// Resources supported by xray (view.allowedXRay).
const XRAY_ALLOWED: [&str; 6] = [
    "v1/pods",
    "v1/services",
    "apps/v1/deployments",
    "apps/v1/daemonsets",
    "apps/v1/statefulsets",
    "apps/v1/replicasets",
];

/// Parsed `:` command.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Command {
    /// Resolved GVR string (or special command name such as `quit`, `help`).
    pub gvr: String,
    /// The raw alias/resource token typed by the user (lowercased).
    pub alias: String,
    pub namespace: Option<String>,
    pub context: Option<String>,
    pub filter: Option<String>,
    /// Canonical label selector (apimachinery `String()` form).
    pub label_selector: Option<String>,
    pub fuzzy: Option<String>,
    /// Trailing positional args (e.g. `xray po ns`, `dir /tmp`, `cow hi`).
    pub args: Vec<String>,
}

/// Parse and resolve `input` against `aliases`.
pub fn parse(input: &str, aliases: &Aliases) -> anyhow::Result<Command> {
    interpret(input, aliases).map(|(c, _)| c)
}

/// Like [`parse`], also returning the resolved interpreter; its
/// `get_line()` is the canonical line k9s pushes to the command history
/// (e.g. `po fred` → `v1/pods fred`).
pub fn interpret(input: &str, aliases: &Aliases) -> anyhow::Result<(Command, Interpreter)> {
    let mut p = Interpreter::new(input);
    let alias = p.cmd().to_string();
    let mut c = Command {
        alias: alias.clone(),
        ..Default::default()
    };

    if p.is_cow_cmd() {
        let msg = p
            .cow_arg()
            .ok_or_else(|| anyhow!("Invalid command. Use `cow xxx`"))?;
        c.gvr = COW_CMD.into();
        c.args = vec![msg];
        return Ok((c, p));
    }
    if p.is_bail_cmd() {
        c.gvr = QUIT_CMD.into();
        return Ok((c, p));
    }
    if p.is_help_cmd() {
        c.gvr = HELP_CMD.into();
        return Ok((c, p));
    }
    if p.is_alias_cmd() {
        c.gvr = ALIASES_CMD.into();
        c.filter = p.filter_arg();
        return Ok((c, p));
    }
    if p.is_xray_cmd() {
        let (arg, ns) = p
            .xray_args()
            .ok_or_else(|| anyhow!("invalid command. use `xray xxx`"))?;
        let gvr = resolve(&mut Interpreter::new(&arg), aliases)
            .ok_or_else(|| anyhow!("invalid resource name: {arg:?}"))?;
        if !XRAY_ALLOWED.contains(&gvr.as_str()) {
            return Err(anyhow!("unsupported resource {arg:?}"));
        }
        c.gvr = XRAY_CMD.into();
        c.args = vec![gvr];
        c.namespace = (!ns.is_empty()).then_some(ns);
        return Ok((c, p));
    }
    if p.is_rbac_cmd() {
        let (cat, sub) = p
            .rbac_args()
            .ok_or_else(|| anyhow!("Invalid command. Use `can [u|g|s]:xxx`"))?;
        c.gvr = POLICY_CMD.into();
        c.args = vec![cat, sub];
        return Ok((c, p));
    }
    if p.is_context_cmd() {
        let ct = p
            .context_arg()
            .ok_or_else(|| anyhow!("invalid command use `context xxx`"))?;
        c.gvr = CONTEXTS_CMD.into();
        c.context = (!ct.is_empty()).then_some(ct);
        c.filter = p.filter_arg();
        return Ok((c, p));
    }
    if p.is_namespace_cmd() {
        if let Some(ns) = p.ns_arg() {
            p.reset("v1/pods", "");
            p.switch_ns(&ns);
        }
    } else if p.is_dir_cmd() {
        let d = p
            .dir_arg()
            .ok_or_else(|| anyhow!("Invalid command. Use `dir xxx`"))?;
        c.gvr = DIRS_CMD.into();
        c.args = vec![d];
        return Ok((c, p));
    }

    let gvr = match resolve(&mut p, aliases) {
        Some(g) => g,
        None if p.cmd() == "-" => {
            c.gvr = LAST_CMD.into();
            return Ok((c, p));
        }
        None => return Err(anyhow!("`{}` command not found", p.cmd())),
    };
    c.gvr = gvr;
    c.namespace = p.ns_arg();
    c.context = p.has_context();
    c.filter = p.filter_arg();
    c.fuzzy = p.fuzzy_arg();
    c.label_selector = match p.labels_selector() {
        Ok(sel) if !sel.is_empty() => Some(sel.to_string()),
        Ok(_) => None,
        Err(e) => {
            tracing::error!("Unable to grok labels selector: {e}");
            None
        }
    };
    Ok((c, p))
}

/// Autocomplete suggestions for the partial `input` (k9s suggestion logic):
/// alias names, then namespaces for resource commands, contexts for `ctx`/`@`.
/// Returns completion suffixes, sorted. See [`suggest_command`] for the
/// empty-input history fallback.
pub fn suggest(
    input: &str,
    aliases: &Aliases,
    namespaces: &[String],
    contexts: &[String],
) -> Vec<String> {
    suggest::suggest(input, aliases, namespaces, contexts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aliases() -> Aliases {
        let mut a = Aliases::default();
        a.define("v1/pods", &["po", "pod", "pods", "v1/pods"]);
        a.define(
            "v1/namespaces",
            &["ns", "namespace", "namespaces", "v1/namespaces"],
        );
        a.define(
            "apps/v1/deployments",
            &["dp", "deploy", "apps/v1/deployments"],
        );
        a.define("v1/services", &["svc", "v1/services"]);
        a.define("v1/configmaps", &["cm"]);
        a.define("pod default app=fred @fred", &["ppc"]);
        a
    }

    fn cmd(gvr: &str, alias: &str) -> Command {
        Command {
            gvr: gvr.into(),
            alias: alias.into(),
            ..Default::default()
        }
    }

    #[test]
    fn parse_resources() {
        let a = aliases();
        assert_eq!(parse("po", &a).unwrap(), cmd("v1/pods", "po"));
        assert_eq!(parse("PODS", &a).unwrap(), cmd("v1/pods", "pods"));
        assert_eq!(
            parse("dp kube-system", &a).unwrap(),
            Command {
                namespace: Some("kube-system".into()),
                ..cmd("apps/v1/deployments", "dp")
            }
        );
        assert_eq!(
            parse("po -n Fred", &a).unwrap(),
            Command {
                namespace: Some("fred".into()),
                ..cmd("v1/pods", "po")
            }
        );
        assert_eq!(
            parse("po /Fred app=blee,x=y @Dev -f zorg ns1", &a).unwrap(),
            Command {
                namespace: Some("ns1".into()),
                context: Some("Dev".into()),
                filter: Some("fred".into()),
                label_selector: Some("app=blee,x=y".into()),
                fuzzy: Some("zorg".into()),
                ..cmd("v1/pods", "po")
            }
        );
        assert_eq!(
            parse("po -l 'app in (b,a)'", &a).unwrap(),
            Command {
                label_selector: Some("app in (a,b)".into()),
                ..cmd("v1/pods", "po")
            }
        );
        // Bad selectors are dropped, like k9s.
        assert_eq!(parse("po =blee", &a).unwrap(), cmd("v1/pods", "po"));
        assert_eq!(
            parse("apps/v1/deployments", &a).unwrap().gvr,
            "apps/v1/deployments"
        );
        assert_eq!(
            parse("ppc /x", &a).unwrap(),
            Command {
                namespace: Some("default".into()),
                context: Some("fred".into()),
                filter: Some("x".into()),
                label_selector: Some("app=fred".into()),
                ..cmd("v1/pods", "ppc")
            }
        );
    }

    #[test]
    fn parse_errors() {
        let a = aliases();
        assert_eq!(
            parse("zorg", &a).unwrap_err().to_string(),
            "`zorg` command not found"
        );
        assert_eq!(
            parse("ZORG blee", &a).unwrap_err().to_string(),
            "`zorg` command not found"
        );
        assert_eq!(
            parse("", &a).unwrap_err().to_string(),
            "`` command not found"
        );
        assert_eq!(
            parse("cow", &a).unwrap_err().to_string(),
            "Invalid command. Use `cow xxx`"
        );
        assert_eq!(
            parse("dir", &a).unwrap_err().to_string(),
            "Invalid command. Use `dir xxx`"
        );
        assert_eq!(
            parse("xray", &a).unwrap_err().to_string(),
            "invalid command. use `xray xxx`"
        );
        assert_eq!(
            parse("xray zorg", &a).unwrap_err().to_string(),
            "invalid resource name: \"zorg\""
        );
        assert_eq!(
            parse("xray cm", &a).unwrap_err().to_string(),
            "unsupported resource \"cm\""
        );
        assert_eq!(
            parse("can bozo", &a).unwrap_err().to_string(),
            "Invalid command. Use `can [u|g|s]:xxx`"
        );
    }

    #[test]
    fn parse_specials() {
        let a = aliases();
        for q in ["q", "q!", "qa", "quit", "exit", "Q"] {
            assert_eq!(parse(q, &a).unwrap().gvr, QUIT_CMD, "{q}");
        }
        for h in ["?", "h", "help"] {
            assert_eq!(parse(h, &a).unwrap().gvr, HELP_CMD, "{h}");
        }
        let al = parse("alias /po", &a).unwrap();
        assert_eq!(al.gvr, ALIASES_CMD);
        assert_eq!(al.filter.as_deref(), Some("po"));
        assert_eq!(parse("a", &a).unwrap().gvr, ALIASES_CMD);

        let c = parse("cow Hello", &a).unwrap();
        assert_eq!(
            (c.gvr.as_str(), c.args.clone()),
            (COW_CMD, vec!["hello".to_string()])
        );

        let x = parse("xray dp kube-system", &a).unwrap();
        assert_eq!(x.gvr, XRAY_CMD);
        assert_eq!(x.args, vec!["apps/v1/deployments".to_string()]);
        assert_eq!(x.namespace.as_deref(), Some("kube-system"));
        let x = parse("x po", &a).unwrap();
        assert_eq!((x.args[0].as_str(), x.namespace), ("v1/pods", None));

        let r = parse("can u:fernand", &a).unwrap();
        assert_eq!(r.gvr, POLICY_CMD);
        assert_eq!(r.args, vec!["u".to_string(), "fernand".to_string()]);

        let ctx = parse("ctx", &a).unwrap();
        assert_eq!((ctx.gvr.as_str(), ctx.context), (CONTEXTS_CMD, None));
        let ctx = parse("context Kind-Dev", &a).unwrap();
        assert_eq!(ctx.context.as_deref(), Some("Kind-Dev"));

        let ns = parse("ns", &a).unwrap();
        assert_eq!(ns.gvr, "v1/namespaces");
        let ns = parse("ns kube-system", &a).unwrap();
        assert_eq!(ns.gvr, "v1/pods");
        assert_eq!(ns.alias, "ns");
        assert_eq!(ns.namespace.as_deref(), Some("kube-system"));

        let d = parse("dir /tmp/Blee", &a).unwrap();
        assert_eq!(
            (d.gvr.as_str(), d.args.clone()),
            (DIRS_CMD, vec!["/tmp/Blee".to_string()])
        );
        assert_eq!(parse("ls ./x", &a).unwrap().args, vec!["./x".to_string()]);

        assert_eq!(parse("-", &a).unwrap().gvr, LAST_CMD);
    }

    #[test]
    fn parse_builtin_pseudo_resources() {
        let a = Aliases::default();
        for (input, gvr) in [
            ("pf", "portforwards"),
            ("sd", "screendumps"),
            ("screendumps", "screendumps"),
            ("bench", "benchmarks"),
            ("pulses", "pulses"),
            ("pulse", "pulses"),
            ("pu", "pulses"),
            ("hz", "pulses"),
            ("users", "users"),
            ("usr", "users"),
            ("groups", "groups"),
            ("grp", "groups"),
            ("wk", "workloads"),
            ("hm", "helm"),
        ] {
            assert_eq!(parse(input, &a).unwrap().gvr, gvr, "{input}");
        }
        assert_eq!(
            parse("rbac", &a).unwrap_err().to_string(),
            "`rbac` command not found"
        );
    }

    #[test]
    fn interpret_canonical_line() {
        let a = aliases();
        let (_, p) = interpret("po Fred", &a).unwrap();
        assert_eq!(p.get_line(), "v1/pods Fred");
        let (_, p) = interpret("ns blee", &a).unwrap();
        assert_eq!(p.get_line(), "v1/pods blee");
        let (_, p) = interpret("ppc", &a).unwrap();
        assert_eq!(p.get_line(), "v1/pods @fred 'app=fred' default");
    }

    #[test]
    fn suggest_wrapper() {
        let a = aliases();
        let nss = vec!["default".to_string(), "kube-system".to_string()];
        assert_eq!(suggest("dep", &a, &nss, &[]), vec!["loy".to_string()]);
        assert_eq!(
            suggest("dp k", &a, &nss, &[]),
            vec!["ube-system".to_string()]
        );
    }
}
