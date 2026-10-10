//! Plugins and hotkeys (k9s view/actions.go, view/env.go, view/helpers.go
//! k8sEnv/defaultEnv, view/exec.go run/execute).
//!
//! Views that own a selection (the resource browser, containers, logs) call
//! [`plugin_hints`] to list plugin shortcuts in their menu and
//! [`plugin_for_key`] + [`run_plugin`] when a key matches, passing the
//! selected row as a [`Selection`].

use std::collections::BTreeMap;
use std::sync::OnceLock;

use crossterm::event::KeyEvent;
use regex::Regex;

use super::text::{go_quote, truncate};
use super::SHELL_VIEW_ID;
use crate::config::Plugin;
use crate::model::NA_VALUE;
use crate::ui::keys::{key_name, parse_key, same_key};
use crate::ui::{AppEvent, Ctx, Dialog, Hint, ViewCmd};

/// k9s view.AllScopes.
const ALL_SCOPES: &str = "all";
const OUTPUT_PREFIX: &str = "[output]";

/// The selected item a plugin runs against.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    /// FQN of the selected item (`ns/name` or `name`); plugins do nothing when empty.
    pub path: String,
    /// `$POD` (container views).
    pub pod: Option<String>,
    /// `$CONTAINER` (container and log views).
    pub container: Option<String>,
    /// `$FILTER` (log view filter).
    pub filter: Option<String>,
    /// Visible columns of the selected row: `(header name, value)` → `$COL-<NAME>`.
    pub columns: Vec<(String, String)>,
}

/// k8sEnv: CONTEXT, CLUSTER, USER, GROUPS, KUBECONFIG.
/// k8sEnv GROUPS: the impersonation groups, else n/a.
pub(crate) fn groups_env(groups: &[String]) -> String {
    if groups.is_empty() {
        NA_VALUE.to_string()
    } else {
        groups.join(",")
    }
}

pub fn k8s_env(ctx: &Ctx) -> BTreeMap<String, String> {
    let na = |s: &str| {
        if s.is_empty() {
            NA_VALUE.to_string()
        } else {
            s.to_string()
        }
    };
    let context = if ctx.cluster_info.context.is_empty() {
        ctx.accessor.active_context()
    } else {
        ctx.cluster_info.context.clone()
    };
    let kubeconfig = ctx
        .accessor
        .shell_env()
        .into_iter()
        .find(|(k, _)| k == "KUBECONFIG")
        .map(|(_, v)| v)
        .or_else(|| std::env::var("KUBECONFIG").ok())
        .unwrap_or_default();
    BTreeMap::from([
        ("CONTEXT".to_string(), na(&context)),
        ("CLUSTER".to_string(), na(&ctx.cluster_info.cluster)),
        ("USER".to_string(), na(&ctx.cluster_info.user)),
        (
            "GROUPS".to_string(),
            groups_env(&ctx.accessor.impersonate_groups()),
        ),
        ("KUBECONFIG".to_string(), kubeconfig),
    ])
}

/// defaultEnv plus the container/log extras.
pub fn plugin_env(ctx: &Ctx, sel: &Selection) -> BTreeMap<String, String> {
    let mut env = k8s_env(ctx);
    let (ns, name) = crate::k8s::namespaced(&sel.path);
    env.insert("NAMESPACE".into(), ns.to_string());
    env.insert("NAME".into(), name.to_string());
    if let Some(p) = &sel.pod {
        env.insert("POD".into(), p.clone());
    }
    if let Some(c) = &sel.container {
        env.insert("CONTAINER".into(), c.clone());
    }
    if let Some(f) = &sel.filter {
        env.insert("FILTER".into(), f.clone());
    }
    for (k, v) in &sel.columns {
        env.insert(format!("COL-{k}"), v.clone());
    }
    env
}

fn env_rx() -> &'static Regex {
    static RX: OnceLock<Regex> = OnceLock::new();
    RX.get_or_init(|| {
        Regex::new(r"(\$(!?)([\w\-]+))|(\$\{(!?)([\w\-%/: ]+)})").expect("valid env regex")
    })
}

/// view/env.go Env.Substitute.
pub fn substitute(env: &BTreeMap<String, String>, arg: &str) -> String {
    let mut matches: Vec<(String, String, bool)> = env_rx()
        .captures_iter(arg)
        .map(|c| {
            let all = c.get(0).map(|m| m.as_str()).unwrap_or_default().to_string();
            if c.get(1).is_some() {
                let inv = c.get(2).map(|m| m.as_str()) == Some("!");
                let key = c.get(3).map(|m| m.as_str()).unwrap_or_default().to_string();
                (all, key, inv)
            } else {
                let inv = c.get(5).map(|m| m.as_str()) == Some("!");
                let key = c.get(6).map(|m| m.as_str()).unwrap_or_default().to_string();
                (all, key, inv)
            }
        })
        .collect();
    matches.sort_by_key(|m| std::cmp::Reverse(m.0.len()));
    let mut out = arg.to_string();
    for (all, key, inverse) in matches {
        let Some(v) = env.get(&key.to_uppercase()) else {
            tracing::warn!(key, "no k9rs environment matching key");
            continue;
        };
        let v = match parse_go_bool(v) {
            Some(b) => (if inverse { !b } else { b }).to_string(),
            None => v.clone(),
        };
        out = out.replace(&all, &v);
    }
    out
}

/// strconv.ParseBool.
fn parse_go_bool(s: &str) -> Option<bool> {
    match s {
        "1" | "t" | "T" | "TRUE" | "true" | "True" => Some(true),
        "0" | "f" | "F" | "FALSE" | "false" | "False" => Some(false),
        _ => None,
    }
}

fn in_scope(scopes: &[String], aliases: &[String]) -> bool {
    scopes.iter().any(|s| s == ALL_SCOPES) || scopes.iter().any(|s| aliases.contains(s))
}

/// Plugins applicable to a view whose resource answers to `aliases`
/// (sorted by plugin name; dangerous plugins are dropped in read-only mode).
pub fn plugins_for(ctx: &Ctx, aliases: &[String]) -> Vec<(String, Plugin)> {
    ctx.plugins
        .plugins
        .iter()
        .filter(|(_, p)| in_scope(&p.scopes, aliases) && !(ctx.readonly && p.dangerous))
        .filter(|(_, p)| parse_key(&p.short_cut).is_some())
        .map(|(k, p)| (k.clone(), p.clone()))
        .collect()
}

/// Menu hints for the applicable plugins.
pub fn plugin_hints(ctx: &Ctx, aliases: &[String]) -> Vec<Hint> {
    plugins_for(ctx, aliases)
        .into_iter()
        .filter_map(|(_, p)| {
            let k = parse_key(&p.short_cut)?;
            Some(Hint::new(key_name(&k), p.description))
        })
        .collect()
}

/// The plugin bound to `key` for this view, if any.
pub fn plugin_for_key(ctx: &Ctx, aliases: &[String], key: &KeyEvent) -> Option<Plugin> {
    plugins_for(ctx, aliases)
        .into_iter()
        .find(|(_, p)| parse_key(&p.short_cut).is_some_and(|k| same_key(&k, key)))
        .map(|(_, p)| p)
}

/// Run `plugin` against `sel` (view/actions.go pluginAction): substitutes
/// `$VARS` in the args, asks for confirmation when configured, then either
/// suspends the UI to run it in the foreground or runs it in the background.
pub fn run_plugin(ctx: &Ctx, plugin: &Plugin, sel: &Selection) -> Vec<ViewCmd> {
    if sel.path.is_empty() {
        return Vec::new();
    }
    let env = plugin_env(ctx, sel);
    let args: Vec<String> = plugin.args.iter().map(|a| substitute(&env, a)).collect();
    if plugin.confirm {
        let msg = format!("Run?\n{} {}", plugin.command, args.join(" "));
        let p = plugin.clone();
        return vec![ViewCmd::Dialog(Dialog::Confirm {
            title: format!("Confirm {}", plugin.description),
            msg,
            on_ok: Box::new(move |ctx: &mut Ctx| launch(ctx, &p, args)),
        })];
    }
    launch(ctx, plugin, args)
}

/// exec.Cmd.String(): resolved binary path plus args.
pub(crate) fn cmd_string(bin: &str, args: &[String]) -> String {
    let path = which::which(bin)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| bin.to_string());
    if args.is_empty() {
        path
    } else {
        format!("{path} {}", args.join(" "))
    }
}

/// Single-quote for `sh` (empty tokens stay as empty args, like Go's split).
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// argv for the plugin, piping through `pipes` via the shell when set.
pub(crate) fn plugin_argv(p: &Plugin, args: &[String]) -> Vec<String> {
    let valid: Vec<&String> = p
        .pipes
        .iter()
        .filter(|s| s.split(' ').count() >= 2)
        .collect();
    if valid.is_empty() {
        let mut v = vec![p.command.clone()];
        v.extend(args.iter().cloned());
        return v;
    }
    let mut line = std::iter::once(p.command.as_str())
        .chain(args.iter().map(|s| s.as_str()))
        .map(|s| {
            shlex::try_quote(s)
                .map(|c| c.into_owned())
                .unwrap_or_else(|_| s.to_string())
        })
        .collect::<Vec<_>>()
        .join(" ");
    // exec.go: each pipe is split on spaces and run directly (no shell
    // expansion), so every token is quoted for `sh`.
    for pp in valid {
        line.push_str(" | ");
        let toks: Vec<String> = pp.split(' ').map(sh_quote).collect();
        line.push_str(&toks.join(" "));
    }
    vec!["sh".into(), "-c".into(), line]
}

fn launch(ctx: &Ctx, p: &Plugin, args: Vec<String>) -> Vec<ViewCmd> {
    let argv = plugin_argv(p, &args);
    let opts = format!("{} {}", p.command, args.join(" "));
    if p.background {
        let tx = ctx.sender();
        let overwrite = p.overwrite_output;
        let cmd_str = cmd_string(&p.command, &args);
        let mut cmd = tokio::process::Command::new(&argv[0]);
        cmd.args(&argv[1..])
            .envs(crate::k8s::client::editor_env())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        match cmd.spawn() {
            Err(e) => {
                return vec![ViewCmd::Flash(
                    crate::ui::FlashLevel::Err,
                    format!("Exec failed {}: {e}", go_quote(&opts)),
                )];
            }
            Ok(child) => {
                ctx.spawn(async move {
                    let out = match child.wait_with_output().await {
                        Ok(o) => o,
                        Err(e) => {
                            tracing::error!(error = %e, "plugin command failed");
                            return;
                        }
                    };
                    if !out.status.success() {
                        tracing::error!(status = %out.status, "plugin command failed");
                        return;
                    }
                    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
                    if let Some(msg) = background_flash(&stdout, &cmd_str, overwrite) {
                        let _ = tx.send(AppEvent::Info {
                            view: SHELL_VIEW_ID,
                            msg,
                        });
                    }
                });
                return Vec::new();
            }
        }
    }
    let cmd_str = cmd_string(&p.command, &args);
    vec![ViewCmd::Suspend {
        argv,
        banner: String::new(),
        env: crate::k8s::client::editor_env(),
        on_done: Some(Box::new(move |_ctx: &mut Ctx, status| {
            foreground_done(&cmd_str, status)
        })),
    }]
}

/// Background plugin flash (exec.go pipe statuses + pluginAction): every
/// stdout line as `[output] l`, then the completion status; the last one
/// is flashed, or the first output line alone with overwriteOutput.
pub(crate) fn background_flash(stdout: &str, cmd_str: &str, overwrite: bool) -> Option<String> {
    let mut statuses: Vec<String> = stdout
        .split('\n')
        .filter(|l| !l.is_empty())
        .map(|l| format!("{OUTPUT_PREFIX} {l}"))
        .collect();
    statuses.push(format!(
        "Command completed successfully: {}",
        go_quote(&truncate(cmd_str, 20))
    ));
    if overwrite {
        return statuses
            .iter()
            .find(|s| s.contains(OUTPUT_PREFIX))
            .map(|s| s.trim_start_matches(OUTPUT_PREFIX).trim().to_string());
    }
    statuses
        .last()
        .map(|s| format!("Plugin command launched successfully: {}", go_quote(s)))
}

/// Foreground plugin exit (exec.go pipe + pluginAction).
pub(crate) fn foreground_done(cmd_str: &str, status: std::process::ExitStatus) -> Vec<ViewCmd> {
    if status.success() {
        let st = format!("Command completed successfully: {}", go_quote(cmd_str));
        return vec![ViewCmd::Flash(
            crate::ui::FlashLevel::Info,
            format!("Plugin command launched successfully: {}", go_quote(&st)),
        )];
    }
    match status.code() {
        // Killed by a signal: k9s ignores it.
        None => Vec::new(),
        Some(code) => vec![ViewCmd::Dialog(Dialog::Error {
            msg: format!("command failed. Check k9rs logs: exit status {code}"),
        })],
    }
}

/// The hotkey bound to `key`, if any.
pub(crate) fn hotkey_for_key(ctx: &Ctx, key: &KeyEvent) -> Option<crate::config::HotKey> {
    ctx.hotkeys
        .hot_keys
        .values()
        .find(|hk| parse_key(&hk.short_cut).is_some_and(|k| same_key(&k, key)))
        .cloned()
}

/// gotoCmd(r, command, "", !keepHistory): a `:` command, keeping the view
/// stack with keepHistory.
pub(crate) fn hotkey_goto(command: String, keep_history: bool) -> ViewCmd {
    if keep_history {
        ViewCmd::Goto {
            cmd: command,
            path: String::new(),
        }
    } else {
        ViewCmd::Command(command)
    }
}

/// The `:` command a hotkey runs, with `$VARS` substituted (hotKeyActions).
pub(crate) fn hotkey_command(ctx: &Ctx, command: &str) -> String {
    let mut env = k8s_env(ctx);
    env.insert("NAMESPACE".into(), ctx.namespace.clone());
    substitute(&env, command)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Styles;
    use crossterm::event::{KeyCode, KeyModifiers};
    use std::sync::Arc;

    fn ctx() -> (Ctx, tokio::sync::mpsc::UnboundedReceiver<AppEvent>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let acc: crate::k8s::SharedAccessor = Arc::new(crate::k8s::fake::FakeAccessor::new());
        let mut c = Ctx::new(
            acc,
            Default::default(),
            Styles::default(),
            "default".into(),
            tx,
        );
        c.cluster_info.context = "kind".into();
        c.cluster_info.cluster = "kind-cluster".into();
        c.cluster_info.user = "admin".into();
        (c, rx)
    }

    fn plugin(scopes: &[&str], short: &str) -> Plugin {
        Plugin {
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
            short_cut: short.into(),
            description: "Do it".into(),
            command: "echo".into(),
            args: vec!["$NAMESPACE".into(), "${NAME}".into(), "$COL-STATUS".into()],
            ..Default::default()
        }
    }

    #[test]
    fn substitution() {
        let env = BTreeMap::from([
            ("NAME".to_string(), "nginx".to_string()),
            ("NAMESPACE".to_string(), "default".to_string()),
            ("FLAG".to_string(), "true".to_string()),
        ]);
        assert_eq!(substitute(&env, "$NAMESPACE/$NAME"), "default/nginx");
        assert_eq!(substitute(&env, "${name}-x"), "nginx-x");
        assert_eq!(
            substitute(&env, "$!FLAG ${!flag} $FLAG"),
            "false false true"
        );
        assert_eq!(substitute(&env, "$MISSING"), "$MISSING");
        assert_eq!(substitute(&env, "plain"), "plain");
    }

    #[test]
    fn env_contents() {
        let (c, _rx) = ctx();
        let env = plugin_env(
            &c,
            &Selection {
                path: "default/nginx".into(),
                container: Some("app".into()),
                columns: vec![("STATUS".into(), "Running".into())],
                ..Default::default()
            },
        );
        assert_eq!(env["NAMESPACE"], "default");
        assert_eq!(env["NAME"], "nginx");
        assert_eq!(env["CONTEXT"], "kind");
        assert_eq!(env["CLUSTER"], "kind-cluster");
        assert_eq!(env["USER"], "admin");
        assert_eq!(env["GROUPS"], "n/a");
        assert_eq!(env["CONTAINER"], "app");
        assert_eq!(env["COL-STATUS"], "Running");
        assert!(!env.contains_key("POD"));
    }

    #[test]
    fn scopes_hints_and_keys() {
        let (mut c, _rx) = ctx();
        c.plugins
            .plugins
            .insert("a".into(), plugin(&["pods"], "Shift-D"));
        c.plugins
            .plugins
            .insert("b".into(), plugin(&["all"], "Ctrl-T"));
        c.plugins.plugins.insert("c".into(), plugin(&["svc"], "x"));
        let mut danger = plugin(&["all"], "Shift-K");
        danger.dangerous = true;
        c.plugins.plugins.insert("d".into(), danger);
        c.plugins
            .plugins
            .insert("e".into(), plugin(&["all"], "Bogus-Key"));
        let aliases = vec!["po".to_string(), "pods".to_string()];
        let names: Vec<String> = plugins_for(&c, &aliases).into_iter().map(|p| p.0).collect();
        assert_eq!(names, ["a", "b", "d"]);
        c.readonly = true;
        let names: Vec<String> = plugins_for(&c, &aliases).into_iter().map(|p| p.0).collect();
        assert_eq!(names, ["a", "b"]);
        let hints = plugin_hints(&c, &aliases);
        assert_eq!(hints[0], Hint::new("shift-d", "Do it"));
        assert_eq!(hints[1], Hint::new("ctrl-t", "Do it"));
        let k = KeyEvent::new(KeyCode::Char('D'), KeyModifiers::SHIFT);
        assert!(plugin_for_key(&c, &aliases, &k).is_some());
        let k = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE);
        assert!(plugin_for_key(&c, &aliases, &k).is_none());
    }

    #[test]
    fn foreground_plugin_suspends() {
        let (c, _rx) = ctx();
        let p = plugin(&["all"], "Shift-D");
        let sel = Selection {
            path: "default/nginx".into(),
            columns: vec![("STATUS".into(), "Running".into())],
            ..Default::default()
        };
        let out = run_plugin(&c, &p, &sel);
        match out.as_slice() {
            [ViewCmd::Suspend { argv, env, .. }] => {
                assert_eq!(argv, &["echo", "default", "nginx", "Running"]);
                assert!(env.is_empty());
            }
            _ => panic!("expected suspend"),
        }
        assert!(run_plugin(&c, &p, &Selection::default()).is_empty());
    }

    #[test]
    fn confirm_and_pipes() {
        let (mut c, _rx) = ctx();
        let mut p = plugin(&["all"], "Shift-D");
        p.confirm = true;
        let sel = Selection {
            path: "default/nginx".into(),
            ..Default::default()
        };
        let mut out = run_plugin(&c, &p, &sel);
        let Some(ViewCmd::Dialog(Dialog::Confirm { title, msg, on_ok })) = out.pop() else {
            panic!("expected confirm");
        };
        assert_eq!(title, "Confirm Do it");
        assert_eq!(msg, "Run?\necho default nginx $COL-STATUS");
        let after = on_ok(&mut c);
        assert!(matches!(after.as_slice(), [ViewCmd::Suspend { .. }]));

        p.confirm = false;
        p.pipes = vec!["grep nginx".into(), "bad".into()];
        let out = run_plugin(&c, &p, &sel);
        match out.as_slice() {
            [ViewCmd::Suspend { argv, .. }] => {
                assert_eq!(argv[0], "sh");
                // Pipe tokens run as-is, like exec.go (was shell-expanded).
                assert_eq!(argv[2], "echo default nginx '$COL-STATUS' | 'grep' 'nginx'");
            }
            _ => panic!("expected suspend"),
        }
        p.pipes = vec!["grep it's  $HOME *".into()];
        let out = run_plugin(&c, &p, &sel);
        let [ViewCmd::Suspend { argv, .. }] = out.as_slice() else {
            panic!()
        };
        assert!(
            argv[2].ends_with(r#"| 'grep' 'it'\''s' '' '$HOME' '*'"#),
            "{}",
            argv[2]
        );
        assert_eq!(groups_env(&[]), "n/a");
        assert_eq!(groups_env(&["a".into(), "b".into()]), "a,b");
    }

    #[test]
    fn background_flash_statuses() {
        // Last status wins: the completion line, command truncated to 20.
        assert_eq!(
            background_flash("a\nb\n", "/usr/bin/printenv HOME", false).unwrap(),
            "Plugin command launched successfully: \"Command completed successfully: \\\"/usr/bin/printenv H…\\\"\""
        );
        // overwriteOutput: the first output line, bare.
        assert_eq!(background_flash("a\nb\n", "x", true).as_deref(), Some("a"));
        assert_eq!(background_flash("", "x", true), None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn background_plugin_flashes() {
        let (c, mut rx) = ctx();
        let mut p = plugin(&["all"], "Shift-D");
        p.background = true;
        p.args = vec!["hello".into()];
        let sel = Selection {
            path: "default/nginx".into(),
            ..Default::default()
        };
        assert!(run_plugin(&c, &p, &sel).is_empty());
        let ev = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        match ev {
            AppEvent::Info { view, msg } => {
                assert_eq!(view, SHELL_VIEW_ID);
                assert!(
                    msg.starts_with(
                        "Plugin command launched successfully: \"Command completed successfully:"
                    ),
                    "{msg}"
                );
            }
            other => panic!("unexpected {other:?}"),
        }
        p.overwrite_output = true;
        assert!(run_plugin(&c, &p, &sel).is_empty());
        let ev = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(ev, AppEvent::Info { msg, .. } if msg == "hello"));
    }

    #[test]
    fn hotkey_substitution() {
        let (c, _rx) = ctx();
        assert_eq!(hotkey_command(&c, "pods -n $NAMESPACE"), "pods -n default");
    }
}
