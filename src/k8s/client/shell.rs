//! kubectl shell-outs (k9s view/exec.go runK, view/pod.go computeShellArgs,
//! view/browser.go editRes).
//!
//! k9s passes `--context`, `--kubeconfig` and `--token` on the kubectl
//! command line. k9rs never does: credentials in argv leak through the
//! process list. Instead, when kubectl would otherwise pick the wrong
//! context or miss an override, a private override kubeconfig is written and
//! put first in `KUBECONFIG` (kubectl merges the files and the first one
//! wins for current-context).

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{json, Value};

use super::config::LoadedConfig;
use crate::k8s::{is_cluster_scoped, namespaced, Gvr};

/// k9s view.shellCheck.
pub(crate) const SHELL_CHECK: &str = "command -v bash >/dev/null && exec bash || exec sh";
/// k9s view.powerShell.
const POWER_SHELL: &str = "powershell";
/// Name of the user and context defined in the override kubeconfig.
pub(crate) const OVERRIDE_NAME: &str = "k9rs-override";

/// runK: `kubectl <cmd> [--insecure-skip-tls-verify] <rest>`.
fn run_k(insecure: bool, args: Vec<String>) -> Vec<String> {
    let mut out = vec!["kubectl".to_string()];
    let mut it = args.into_iter();
    if let Some(first) = it.next() {
        out.push(first);
    }
    if insecure {
        out.push("--insecure-skip-tls-verify".into());
    }
    out.extend(it);
    out
}

/// Full `kubectl exec` argv for a pod shell. `shell` selects the command:
/// "" / "sh" / "linux" use k9s' bash-or-sh probe, "powershell" / "windows"
/// run powershell, anything else is split shell-style and run verbatim
/// (k9s shellPod command/args).
pub(crate) fn exec_argv(insecure: bool, ns: &str, pod: &str, co: &str, shell: &str) -> Vec<String> {
    let mut args = vec!["exec".to_string(), "-it".to_string()];
    if !ns.is_empty() {
        args.extend(["-n".into(), ns.to_string()]);
    }
    args.push(pod.to_string());
    if !co.is_empty() {
        args.extend(["-c".into(), co.to_string()]);
    }
    args.push("--".into());
    match shell.trim() {
        "" | "sh" | "linux" => args.extend(["sh".into(), "-c".into(), SHELL_CHECK.into()]),
        "powershell" | "windows" => args.push(POWER_SHELL.into()),
        custom => match shlex::split(custom) {
            Some(tokens) if !tokens.is_empty() => args.extend(tokens),
            _ => args.push(custom.to_string()),
        },
    }
    run_k(insecure, args)
}

/// GVR.AsResourceName: `resource.version.group` or bare resource for core.
pub(crate) fn as_resource_name(gvr: &Gvr) -> String {
    if gvr.g().is_empty() {
        gvr.r().to_string()
    } else {
        format!("{}.{}.{}", gvr.r(), gvr.v(), gvr.g())
    }
}

/// editRes argv: `kubectl edit <res.v.g>/<name> [-n ns]`.
pub(crate) fn edit_argv(insecure: bool, gvr: &Gvr, path: &str) -> Vec<String> {
    let (ns, n) = namespaced(path);
    let ns = if is_cluster_scoped(ns) { "" } else { ns };
    let mut args = vec![
        "edit".to_string(),
        format!("{}/{}", as_resource_name(gvr), n),
    ];
    if !ns.is_empty() {
        args.extend(["-n".into(), ns.to_string()]);
    }
    run_k(insecure, args)
}

fn non_empty(v: &Option<String>) -> Option<&str> {
    v.as_deref().filter(|s| !s.is_empty())
}

/// Content of the override kubeconfig, or `None` when kubectl's defaults
/// already match the active connection.
pub(crate) fn override_kubeconfig(cfg: &LoadedConfig) -> Option<Value> {
    if cfg.in_cluster {
        return None;
    }
    let active = cfg.current_context_name().ok().filter(|c| !c.is_empty())?;
    let token = non_empty(&cfg.opts.token);
    let impersonate = non_empty(&cfg.opts.impersonate);
    let groups = &cfg.opts.impersonate_groups;
    let needs_user = token.is_some() || impersonate.is_some() || !groups.is_empty();
    if !needs_user {
        if active == cfg.file_current_context() {
            return None;
        }
        return Some(json!({"apiVersion": "v1", "kind": "Config", "current-context": active}));
    }
    let mut user = match token {
        Some(t) => json!({ "token": t }),
        // Impersonation only: keep the original credentials of the active user.
        None => cfg
            .current_auth_info()
            .and_then(|a| serde_json::to_value(a).ok())
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({})),
    };
    if let Some(u) = impersonate {
        user["as"] = json!(u);
    }
    if !groups.is_empty() {
        user["as-groups"] = json!(groups);
    }
    let mut context = json!({
        "cluster": cfg.current_cluster_name().unwrap_or_default(),
        "user": OVERRIDE_NAME,
    });
    let ns = cfg.current_namespace();
    if !ns.is_empty() {
        context["namespace"] = json!(ns);
    }
    Some(json!({
        "apiVersion": "v1",
        "kind": "Config",
        "current-context": OVERRIDE_NAME,
        "users": [{"name": OVERRIDE_NAME, "user": user}],
        "contexts": [{"name": OVERRIDE_NAME, "context": context}],
    }))
}

/// A private override kubeconfig: a 0600 file in a 0700 temp dir, removed on drop.
#[derive(Debug)]
pub(crate) struct OverrideFile {
    /// Owns the temp dir; dropping it deletes the dir and the file.
    _dir: tempfile::TempDir,
    path: PathBuf,
}

impl OverrideFile {
    pub(crate) fn write(content: &Value) -> Result<OverrideFile> {
        let dir = tempfile::Builder::new()
            .prefix("k9rs-kubeconfig-")
            .tempdir()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))?;
        }
        let path = dir.path().join("config");
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&path)?;
        f.write_all(serde_yaml::to_string(content)?.as_bytes())?;
        Ok(OverrideFile { _dir: dir, path })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    #[cfg(test)]
    pub(crate) fn dir(&self) -> &Path {
        self._dir.path()
    }
}

/// Env overrides for kubectl shell-outs: KUBECONFIG (override file first,
/// then the original kubeconfig files) and KUBE_EDITOR from K9S_EDITOR
/// (resolved binary + args) like k9s execute().
pub(crate) fn shell_env(
    override_path: Option<&Path>,
    kubeconfig_paths: &[String],
    explicit: bool,
    lookup: impl Fn(&str) -> Option<String>,
) -> Vec<(String, String)> {
    let mut env = Vec::new();
    let mut parts: Vec<String> = Vec::new();
    if let Some(p) = override_path {
        parts.push(p.display().to_string());
    }
    if override_path.is_some() || explicit {
        parts.extend(kubeconfig_paths.iter().cloned());
    }
    if !parts.is_empty() {
        env.push(("KUBECONFIG".to_string(), parts.join(":")));
    }
    if let Some(ed) = lookup("K9S_EDITOR").filter(|e| !e.is_empty()) {
        let mut tokens: Vec<String> = ed.split(' ').map(str::to_string).collect();
        if let Ok(bin) = which::which(&tokens[0]) {
            tokens[0] = bin.display().to_string();
            env.push(("KUBE_EDITOR".to_string(), tokens.join(" ")));
        }
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::k8s::client::ConnectOptions;
    use kube::config::Kubeconfig;

    const KC: &str = r#"
apiVersion: v1
kind: Config
current-context: ctx-a
clusters:
- name: cl-a
  cluster:
    server: https://a
- name: cl-b
  cluster:
    server: https://b
users:
- name: u-a
  user:
    token: secret-a
contexts:
- name: ctx-a
  context:
    cluster: cl-a
    user: u-a
    namespace: team-a
- name: ctx-b
  context:
    cluster: cl-b
    user: u-a
"#;

    fn cfg(opts: ConnectOptions) -> LoadedConfig {
        LoadedConfig::from_kubeconfig(Kubeconfig::from_yaml(KC).unwrap(), opts)
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn exec_argv_has_no_connection_flags() {
        assert_eq!(
            exec_argv(false, "default", "nginx", "c1", ""),
            s(&[
                "kubectl",
                "exec",
                "-it",
                "-n",
                "default",
                "nginx",
                "-c",
                "c1",
                "--",
                "sh",
                "-c",
                SHELL_CHECK
            ])
        );
        assert_eq!(
            exec_argv(true, "", "nginx", "", "sh"),
            s(&[
                "kubectl",
                "exec",
                "--insecure-skip-tls-verify",
                "-it",
                "nginx",
                "--",
                "sh",
                "-c",
                SHELL_CHECK
            ])
        );
        assert_eq!(
            exec_argv(false, "ns", "po", "co", "powershell"),
            s(&[
                "kubectl",
                "exec",
                "-it",
                "-n",
                "ns",
                "po",
                "-c",
                "co",
                "--",
                "powershell"
            ])
        );
        assert_eq!(
            exec_argv(false, "ns", "po", "co", "/bin/zsh -l"),
            s(&["kubectl", "exec", "-it", "-n", "ns", "po", "-c", "co", "--", "/bin/zsh", "-l"])
        );
    }

    #[test]
    fn edit_argv_shapes() {
        assert_eq!(
            edit_argv(false, &Gvr::parse("apps/v1/deployments"), "default/nginx"),
            s(&[
                "kubectl",
                "edit",
                "deployments.v1.apps/nginx",
                "-n",
                "default"
            ])
        );
        assert_eq!(
            edit_argv(true, &Gvr::parse("v1/nodes"), "n1"),
            s(&["kubectl", "edit", "--insecure-skip-tls-verify", "nodes/n1"])
        );
        assert_eq!(
            edit_argv(false, &Gvr::parse("v1/namespaces"), "-/kube-system"),
            s(&["kubectl", "edit", "namespaces/kube-system"])
        );
        assert_eq!(as_resource_name(&Gvr::parse("v1/pods")), "pods");
    }

    #[test]
    fn no_override_when_defaults_match() {
        assert_eq!(override_kubeconfig(&cfg(ConnectOptions::default())), None);
        let c = cfg(ConnectOptions {
            context: Some("ctx-a".into()),
            ..Default::default()
        });
        assert_eq!(override_kubeconfig(&c), None);
    }

    #[test]
    fn override_for_context_switch() {
        let c = cfg(ConnectOptions::default()).switched("ctx-b").unwrap();
        assert_eq!(
            override_kubeconfig(&c),
            Some(json!({"apiVersion": "v1", "kind": "Config", "current-context": "ctx-b"}))
        );
    }

    #[test]
    fn override_for_token() {
        let c = cfg(ConnectOptions {
            context: Some("ctx-b".into()),
            namespace: Some("ns1".into()),
            token: Some("tok".into()),
            impersonate: Some("jane".into()),
            impersonate_groups: vec!["g1".into(), "g2".into()],
            ..Default::default()
        });
        assert_eq!(
            override_kubeconfig(&c),
            Some(json!({
                "apiVersion": "v1", "kind": "Config", "current-context": "k9rs-override",
                "users": [{"name": "k9rs-override", "user": {"token": "tok", "as": "jane", "as-groups": ["g1", "g2"]}}],
                "contexts": [{"name": "k9rs-override", "context": {"cluster": "cl-b", "user": "k9rs-override", "namespace": "ns1"}}]
            }))
        );
    }

    #[test]
    fn override_for_impersonation_keeps_credentials() {
        let c = cfg(ConnectOptions {
            impersonate: Some("jane".into()),
            ..Default::default()
        });
        let v = override_kubeconfig(&c).unwrap();
        assert_eq!(v["current-context"], json!("k9rs-override"));
        assert_eq!(v["users"][0]["user"]["token"], json!("secret-a"));
        assert_eq!(v["users"][0]["user"]["as"], json!("jane"));
        assert_eq!(
            v["contexts"][0]["context"],
            json!({"cluster": "cl-a", "user": "k9rs-override", "namespace": "team-a"})
        );
    }

    #[test]
    fn override_file_is_private_and_removed() {
        let content = json!({"apiVersion": "v1", "kind": "Config", "current-context": "ctx-b"});
        let f = OverrideFile::write(&content).unwrap();
        let path = f.path().to_path_buf();
        let dir = f.dir().to_path_buf();
        let back: Value = serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(back, content);
        // kube can read it as a kubeconfig.
        assert_eq!(
            Kubeconfig::read_from(&path)
                .unwrap()
                .current_context
                .as_deref(),
            Some("ctx-b")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        drop(f);
        assert!(!path.exists());
        assert!(!dir.exists());
    }

    #[test]
    fn env() {
        let paths = s(&["/k/one", "/k/two"]);
        assert_eq!(shell_env(None, &paths, false, |_| None), vec![]);
        assert_eq!(
            shell_env(None, &paths, true, |_| None),
            vec![("KUBECONFIG".to_string(), "/k/one:/k/two".to_string())]
        );
        assert_eq!(
            shell_env(Some(Path::new("/tmp/o/config")), &paths, false, |_| None),
            vec![(
                "KUBECONFIG".to_string(),
                "/tmp/o/config:/k/one:/k/two".to_string()
            )]
        );
        let env = shell_env(None, &[], false, |k| {
            (k == "K9S_EDITOR").then(|| "sh -x".to_string())
        });
        assert_eq!(env.len(), 1);
        assert_eq!(env[0].0, "KUBE_EDITOR");
        assert!(
            env[0].1.starts_with('/') && env[0].1.ends_with("sh -x"),
            "{env:?}"
        );
        assert!(shell_env(None, &[], false, |_| Some("no-such-editor-k9rs".into())).is_empty());
    }
}
