//! `k9s __complete`: prints shell completion choices (cobra completeCmd)
//! with k9s's kubeconfig/namespace flag completion (cmd/root.go
//! initK8sFlagCompletion).

use std::io::Write;

use super::flags::{directive, CompleteReq, FlagComp};
use super::Cli;

/// k9s filterFlagCompletions.
fn filter(names: Vec<String>, prefix: &str) -> Vec<String> {
    names
        .into_iter()
        .filter(|n| n.starts_with(prefix))
        .collect()
}

/// Run the flag completion function, if any, then print like cobra.
pub async fn run(req: CompleteReq, cli: &Cli, out: &mut dyn Write, err: &mut dyn Write) {
    let mut req = req;
    if let Some((fc, prefix)) = req.dynamic.take() {
        let (comps, d) = flag_values(fc, &prefix, cli).await;
        req.comps.extend(comps);
        req.directive = d;
    }
    print(&req, describe(req.no_desc), out, err);
}

/// cobra: descriptions unless `__completeNoDesc` or
/// K9S_COMPLETION_DESCRIPTIONS / COBRA_COMPLETION_DESCRIPTIONS is false.
fn describe(no_desc: bool) -> bool {
    if no_desc {
        return false;
    }
    let v = std::env::var("K9RS_COMPLETION_DESCRIPTIONS")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var("COBRA_COMPLETION_DESCRIPTIONS").ok())
        .unwrap_or_default();
    !matches!(v.as_str(), "0" | "f" | "F" | "FALSE" | "false" | "False")
}

async fn flag_values(fc: FlagComp, prefix: &str, cli: &Cli) -> (Vec<String>, u32) {
    let Ok(opts) = cli.connect_options(std::time::Duration::from_secs(120)) else {
        return (vec![], directive::ERROR);
    };
    if fc == FlagComp::Namespace {
        let Ok(acc) = crate::k8s::client::connect(opts).await else {
            return (vec![], directive::ERROR);
        };
        // client.InitConnection fails when the API is unreachable.
        if !acc.has_metrics().await && acc.check_connectivity().await.is_err() {
            return (vec![], directive::ERROR);
        }
        return match acc.namespaces().await {
            Ok(nss) => (filter(nss, prefix), directive::NO_FILE_COMP),
            Err(_) => (vec![], directive::ERROR),
        };
    }
    let names = match crate::k8s::client::kubeconfig_names(opts) {
        Ok([ctxs, clusters, users]) => match fc {
            FlagComp::Context => ctxs,
            FlagComp::Cluster => clusters,
            _ => users,
        },
        Err(e) => {
            tracing::error!(error = %e, "K8s raw config getter failed");
            vec![]
        }
    };
    (filter(names, prefix), directive::NO_FILE_COMP)
}

/// cobra completeCmd Run output.
fn print(req: &CompleteReq, desc: bool, out: &mut dyn Write, err: &mut dyn Write) {
    if let Some(e) = &req.err {
        let _ = writeln!(err, "[Debug] [Error] {e}");
    }
    for c in &req.comps {
        let c = if desc {
            c.as_str()
        } else {
            c.split('\t').next().unwrap_or("")
        };
        let c = c.split('\n').next().unwrap_or("").trim();
        let _ = writeln!(out, "{c}");
    }
    let _ = writeln!(out, ":{}", req.directive);
    let _ = writeln!(
        err,
        "Completion ended with directive: {}",
        directive::name(req.directive)
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(comps: &[&str], d: u32, err: Option<&str>) -> CompleteReq {
        CompleteReq {
            comps: comps.iter().map(|s| s.to_string()).collect(),
            directive: d,
            err: err.map(String::from),
            dynamic: None,
            no_desc: false,
        }
    }

    fn printed(r: &CompleteReq, desc: bool) -> (String, String) {
        let (mut o, mut e) = (Vec::new(), Vec::new());
        print(r, desc, &mut o, &mut e);
        (String::from_utf8(o).unwrap(), String::from_utf8(e).unwrap())
    }

    #[test]
    fn output() {
        let r = req(&["info\tList K9s configurations info", "x\t"], 4, None);
        assert_eq!(
            printed(&r, true),
            (
                "info\tList K9s configurations info\nx\n:4\n".into(),
                "Completion ended with directive: ShellCompDirectiveNoFileComp\n".into()
            )
        );
        assert_eq!(printed(&r, false).0, "info\nx\n:4\n");
        let r = req(
            &[],
            0,
            Some("unable to find a command for arguments: [zorg]"),
        );
        assert_eq!(
            printed(&r, true),
            (
                ":0\n".into(),
                "[Debug] [Error] unable to find a command for arguments: [zorg]\nCompletion ended with directive: ShellCompDirectiveDefault\n".into()
            )
        );
        assert_eq!(
            directive::name(3),
            "ShellCompDirectiveError, ShellCompDirectiveNoSpace"
        );
        assert_eq!(
            directive::name(64),
            "ERROR: unexpected ShellCompDirective value: 64"
        );
    }

    #[tokio::test]
    async fn kubeconfig_flags() {
        let tmp = tempfile::tempdir().unwrap();
        let kc = tmp.path().join("kc.yaml");
        std::fs::write(
            &kc,
            "apiVersion: v1\nkind: Config\ncurrent-context: ctx-b\nclusters:\n- name: cl-a\n  cluster: {server: \"https://127.0.0.1:1\"}\ncontexts:\n- name: ctx-a\n  context: {cluster: cl-a, user: u-a}\n- name: ctx-b\n  context: {cluster: cl-a, user: u-a}\n- name: other\n  context: {cluster: cl-a, user: u-a}\nusers:\n- name: u-a\n  user: {token: \"x\"}\n",
        )
        .unwrap();
        let cli = Cli {
            kubeconfig: Some(kc.to_string_lossy().into()),
            ..Cli::default()
        };
        let (c, d) = flag_values(FlagComp::Context, "ctx", &cli).await;
        assert_eq!((c, d), (vec!["ctx-a".into(), "ctx-b".into()], 4));
        let (c, _) = flag_values(FlagComp::Cluster, "", &cli).await;
        assert_eq!(c, vec!["cl-a"]);
        let (c, _) = flag_values(FlagComp::User, "u", &cli).await;
        assert_eq!(c, vec!["u-a"]);
        // An unreachable cluster: k9s returns ShellCompDirectiveError.
        let r = flag_values(FlagComp::Namespace, "", &cli).await;
        assert_eq!(r, (vec![], directive::ERROR));
        let missing = Cli {
            kubeconfig: Some(tmp.path().join("nope").to_string_lossy().into()),
            ..Cli::default()
        };
        assert_eq!(
            flag_values(FlagComp::Context, "", &missing).await,
            (vec![], 4)
        );
    }
}
