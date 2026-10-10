//! In-process shell/attach and edit sessions (replacing `kubectl exec -it`
//! and `kubectl edit`): a raw-mode TTY bridge over the API websocket, and
//! kubectl edit's temp-file loop applied with the existing client.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::Value;

use crate::k8s::TtyProcess;

// ---------------------------------------------------------------------------
// TTY bridge

/// Pump the local terminal into the remote TTY until the remote side ends.
/// Returns the remote exit code (0 on success). The terminal must already be
/// in raw mode.
pub(crate) async fn tty_bridge(mut p: TtyProcess) -> i32 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let stop = Arc::new(AtomicBool::new(false));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    let pump = {
        let stop = stop.clone();
        std::thread::spawn(move || stdin_pump(&stop, &tx))
    };
    let mut resize = p.resize.take();
    let send_size = |r: &mut Option<futures::channel::mpsc::Sender<kube::api::TerminalSize>>| {
        if let (Some(r), Ok((width, height))) = (r.as_mut(), crossterm::terminal::size()) {
            let _ = r.try_send(kube::api::TerminalSize { width, height });
        }
    };
    send_size(&mut resize);
    #[cfg(unix)]
    let mut winch =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change()).ok();
    let mut out = tokio::io::stdout();
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        #[cfg(unix)]
        let resized = async {
            match winch.as_mut() {
                Some(w) => {
                    w.recv().await;
                }
                None => std::future::pending::<()>().await,
            }
        };
        #[cfg(not(unix))]
        let resized = std::future::pending::<()>();
        tokio::select! {
            n = p.stdout.read(&mut buf) => match n {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if out.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                    let _ = out.flush().await;
                }
            },
            Some(b) = rx.recv() => {
                if p.stdin.write_all(&b).await.is_err() {
                    break;
                }
                let _ = p.stdin.flush().await;
            }
            _ = resized => send_size(&mut resize),
        }
    }
    stop.store(true, Ordering::SeqCst);
    let _ = pump.join();
    drop(p.stdin);
    exit_code(p.status.await.as_ref())
}

/// Read stdin without blocking past `stop`, so no keystroke meant for the
/// TUI is swallowed once the session ends.
fn stdin_pump(stop: &AtomicBool, tx: &tokio::sync::mpsc::UnboundedSender<Vec<u8>>) {
    let mut buf = [0u8; 4096];
    while !stop.load(Ordering::SeqCst) {
        #[cfg(unix)]
        {
            let mut fd = libc::pollfd {
                fd: libc::STDIN_FILENO,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one valid pollfd for the duration of the call.
            let ready = unsafe { libc::poll(&mut fd, 1, 50) };
            if ready <= 0 || stop.load(Ordering::SeqCst) {
                continue;
            }
            // SAFETY: reads into a stack buffer of the given length.
            let n = unsafe { libc::read(libc::STDIN_FILENO, buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                break;
            }
            if tx.send(buf[..n as usize].to_vec()).is_err() {
                break;
            }
        }
        #[cfg(not(unix))]
        {
            let _ = (&mut buf, tx);
            break;
        }
    }
}

/// remotecommand status: Success is 0, else the ExitCode cause, else 1.
pub(crate) fn exit_code(s: Option<&k8s_openapi::apimachinery::pkg::apis::meta::v1::Status>) -> i32 {
    let Some(s) = s else {
        return 0;
    };
    if s.status.as_deref() == Some("Success") {
        return 0;
    }
    s.details
        .as_ref()
        .and_then(|d| d.causes.as_ref())
        .and_then(|cc| {
            cc.iter()
                .find(|c| c.reason.as_deref() == Some("ExitCode"))
                .and_then(|c| c.message.as_deref()?.parse().ok())
        })
        .unwrap_or(1)
}

/// A process status carrying `code` (for the views' on_done hooks).
pub(crate) fn exit_status(code: i32) -> std::process::ExitStatus {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        std::process::ExitStatus::from_raw((code & 0xff) << 8)
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(code as u32)
    }
}

// ---------------------------------------------------------------------------
// kubectl edit

/// editHeader.writeTo (NormalEditMode) plus the retry reasons.
pub(crate) fn edit_header(reasons: &[(String, Vec<String>)]) -> String {
    let mut s = String::from(
        "# Please edit the object below. Lines beginning with a '#' will be ignored,\n\
         # and an empty file will abort the edit. If an error occurs while saving this file will be\n\
         # reopened with the relevant failures.\n#\n",
    );
    let hash = |t: &str| t.replace('\n', "\n# ");
    for (head, other) in reasons {
        if other.is_empty() {
            s.push_str(&format!("# {}\n", hash(head)));
        } else {
            s.push_str(&format!("# {}:\n", hash(head)));
        }
        for o in other {
            s.push_str(&format!("# * {}\n", hash(o)));
        }
        s.push_str("#\n");
    }
    s
}

/// cmdutil.StripComments: drop lines starting (after spaces) with `#`.
pub(crate) fn strip_comments(s: &str) -> String {
    s.split_inclusive('\n')
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect()
}

/// hasLines: any non-blank line that is not a comment.
pub(crate) fn has_lines(s: &str) -> bool {
    s.lines().any(|l| {
        let t = l.trim();
        !t.is_empty() && !t.starts_with('#')
    })
}

/// The object as kubectl edit shows it: managedFields hidden.
pub(crate) fn edit_object(mut v: Value) -> Value {
    if let Some(m) = v.get_mut("metadata").and_then(Value::as_object_mut) {
        m.remove("managedFields");
    }
    v
}

/// jsonpatch.CreateMergePatch (RFC 7386).
pub(crate) fn merge_patch(orig: &Value, edited: &Value) -> Value {
    match (orig, edited) {
        (Value::Object(o), Value::Object(e)) => {
            let mut p = serde_json::Map::new();
            for k in o.keys() {
                if !e.contains_key(k) {
                    p.insert(k.clone(), Value::Null);
                }
            }
            for (k, ev) in e {
                match o.get(k) {
                    Some(ov) if ov == ev => {}
                    Some(ov @ Value::Object(_)) if ev.is_object() => {
                        let sub = merge_patch(ov, ev);
                        if sub.as_object().is_some_and(|m| !m.is_empty()) {
                            p.insert(k.clone(), sub);
                        }
                    }
                    _ => {
                        p.insert(k.clone(), ev.clone());
                    }
                }
            }
            Value::Object(p)
        }
        _ => edited.clone(),
    }
}

/// visitToPatch preconditions: apiVersion, kind and metadata.name unchanged.
pub(crate) fn identity_changed(patch: &Value) -> bool {
    patch.get("apiVersion").is_some()
        || patch.get("kind").is_some()
        || patch.pointer("/metadata/name").is_some()
}

/// kubectl editor.defaultEnvEditor (with k9s' K9S_EDITOR first): the argv
/// to open `file`. Editors with quotes or escapes run through $SHELL -c.
pub(crate) fn editor_argv(env: impl Fn(&str) -> Option<String>, file: &str) -> Vec<String> {
    let editor = ["K9S_EDITOR", "KUBE_EDITOR", "EDITOR"]
        .iter()
        .find_map(|k| env(k).filter(|v| !v.is_empty()))
        .unwrap_or_else(|| "vi".into());
    if !editor.contains(' ') {
        return vec![editor, file.to_string()];
    }
    if !editor.contains(['"', '\'', '\\']) {
        let mut argv: Vec<String> = editor.split(' ').map(String::from).collect();
        argv.push(file.to_string());
        return argv;
    }
    let shell = env("SHELL")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/bin/bash".into());
    vec![
        shell,
        "-c".into(),
        format!("{editor} {}", crate::ui::shell::text::go_quote(file)),
    ]
}

/// One pass of the edit loop, run after the editor exits.
pub(crate) struct EditJob {
    pub gvr: crate::k8s::Gvr,
    pub path: String,
    pub file: std::path::PathBuf,
    pub original_text: String,
    pub original: Value,
    /// The previous (failed) attempt's content.
    pub retry_of: Option<String>,
}

#[derive(Debug, PartialEq)]
pub(crate) enum EditOutcome {
    /// Re-open the editor; the file now holds the error header and `.0`.
    Reopen(String),
    /// Finished; `Some` is the failure to flash.
    Done(Option<String>),
}

/// Fetch the object and write kubectl edit's file (header + yaml) to a
/// private temp file.
pub(crate) async fn edit_prepare(
    acc: &crate::k8s::SharedAccessor,
    gvr: &crate::k8s::Gvr,
    path: &str,
) -> anyhow::Result<(tempfile::TempPath, String, Value)> {
    let o = acc.get(gvr, path).await?;
    let v = edit_object(serde_json::to_value(&o)?);
    let text = format!("{}{}", edit_header(&[]), crate::k8s::goyaml::to_yaml(&v));
    let file = write_temp(&text)?;
    Ok((file, text, v))
}

fn write_temp(text: &str) -> anyhow::Result<tempfile::TempPath> {
    use std::io::Write as _;
    let mut f = tempfile::Builder::new()
        .prefix("k9rs-edit-")
        .suffix(".yaml")
        .tempfile()?;
    f.write_all(text.as_bytes())?;
    f.flush()?;
    Ok(f.into_temp_path())
}

/// EditOptions.Run after LaunchTempFile, with a JSON merge patch.
pub(crate) async fn edit_apply(acc: &crate::k8s::SharedAccessor, job: EditJob) -> EditOutcome {
    let edited = match std::fs::read_to_string(&job.file) {
        Ok(s) => s,
        Err(e) => return EditOutcome::Done(Some(e.to_string())),
    };
    if let Some(prev) = &job.retry_of {
        if strip_comments(prev) == strip_comments(&edited) {
            return EditOutcome::Done(Some("Edit cancelled, no valid changes were saved.".into()));
        }
    }
    // "Edit cancelled, no changes made." / "saved file was empty."
    if strip_comments(&job.original_text) == strip_comments(&edited) || !has_lines(&edited) {
        return EditOutcome::Done(None);
    }
    let reopen = |reasons: Vec<(String, Vec<String>)>| {
        let body = strip_comments(&edited);
        let text = format!("{}{body}", edit_header(&reasons));
        match std::fs::write(&job.file, &text) {
            Ok(()) => EditOutcome::Reopen(text),
            Err(e) => EditOutcome::Done(Some(e.to_string())),
        }
    };
    let updated: Value = match serde_yaml::from_str(&edited) {
        Ok(v) => v,
        Err(e) => {
            return reopen(vec![(
                format!("The edited file had a syntax error: {e}"),
                Vec::new(),
            )])
        }
    };
    let mut patch = merge_patch(&job.original, &updated);
    if patch.as_object().is_some_and(|m| m.is_empty()) {
        return EditOutcome::Done(None);
    }
    if identity_changed(&patch) {
        return EditOutcome::Done(Some(
            "At least one of apiVersion, kind and name was changed".into(),
        ));
    }
    // Optimistic lock: the merge patch carries the fetched resourceVersion so
    // a change made while the editor was open is a 409, not a silent overwrite.
    if let Some(rv) = job.original.pointer("/metadata/resourceVersion").cloned() {
        if patch.pointer("/metadata/resourceVersion").is_none() {
            if let Some(m) = patch.as_object_mut() {
                let meta = m
                    .entry("metadata")
                    .or_insert_with(|| Value::Object(Default::default()));
                if let Some(meta) = meta.as_object_mut() {
                    meta.insert("resourceVersion".into(), rv);
                }
            }
        }
    }
    let (_, name) = crate::k8s::namespaced(&job.path);
    let res = if job.gvr.g().is_empty() {
        job.gvr.r().to_string()
    } else {
        format!("{}.{}", job.gvr.r(), job.gvr.g())
    };
    match acc
        .patch_merge(&job.gvr, &job.path, &patch, "kubectl-edit")
        .await
    {
        Ok(()) => EditOutcome::Done(None),
        Err(e) => match e.downcast_ref::<crate::k8s::ApiFailure>() {
            Some(f) if f.code == 422 => reopen(vec![(
                format!("{res} {name:?} was not valid"),
                f.causes
                    .iter()
                    .map(|(fl, m)| format!("{fl}: {m}"))
                    .collect(),
            )]),
            Some(f) if f.code == 404 => EditOutcome::Done(Some(format!(
                "{res} {name:?} could not be found on the server"
            ))),
            // addError default (incl. 409 Conflict): kubectl exits with the
            // file preserved and a `replace -f` hint; it does not reopen.
            _ => EditOutcome::Done(Some(format!(
                "{res} {name:?} could not be patched: {e}. You can run `kubectl replace -f {}` to try this update again",
                job.file.display()
            ))),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merge_patch_like_rfc7386() {
        let o = json!({"a": 1, "b": {"c": 2, "d": 3}, "l": [1, 2], "gone": true});
        let e = json!({"a": 1, "b": {"c": 5, "d": 3}, "l": [1], "new": "x"});
        assert_eq!(
            merge_patch(&o, &e),
            json!({"b": {"c": 5}, "l": [1], "new": "x", "gone": null})
        );
        assert_eq!(merge_patch(&o, &o), json!({}));
        assert!(identity_changed(&json!({"metadata": {"name": "x"}})));
        assert!(!identity_changed(&json!({"metadata": {"labels": {}}})));
    }

    #[test]
    fn edit_file_helpers() {
        let h = edit_header(&[(
            "deployments.apps \"web\" was not valid".into(),
            vec!["spec.replicas: Invalid value".into()],
        )]);
        assert!(h.starts_with("# Please edit the object below."));
        assert!(h.ends_with(
            "#\n# deployments.apps \"web\" was not valid:\n# * spec.replicas: Invalid value\n#\n"
        ));
        assert_eq!(strip_comments("# x\na: 1\n  # y\nb: 2\n"), "a: 1\nb: 2\n");
        assert!(!has_lines("# only\n\n   \n"));
        assert!(has_lines("# c\na: 1"));
        let v = edit_object(json!({"metadata": {"name": "a", "managedFields": [1]}}));
        assert_eq!(v, json!({"metadata": {"name": "a"}}));
    }

    #[test]
    fn editor_selection() {
        let env = |vars: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                vars.iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert_eq!(editor_argv(env(&[]), "/t/f.yaml"), ["vi", "/t/f.yaml"]);
        assert_eq!(
            editor_argv(env(&[("EDITOR", "nano"), ("KUBE_EDITOR", "code -w")]), "/f"),
            ["code", "-w", "/f"]
        );
        assert_eq!(
            editor_argv(env(&[("K9S_EDITOR", "hx"), ("KUBE_EDITOR", "vim")]), "/f"),
            ["hx", "/f"]
        );
        assert_eq!(
            editor_argv(
                env(&[("EDITOR", "emacs -nw --eval '(x)'"), ("SHELL", "/bin/zsh")]),
                "/f"
            ),
            ["/bin/zsh", "-c", "emacs -nw --eval '(x)' \"/f\""]
        );
    }

    /// kubectl edit semantics on the fake: no-op, patch, syntax error and
    /// invalid retries, unchanged retry.
    #[tokio::test]
    async fn edit_loop() {
        use crate::k8s::fake::{obj, FakeAccessor};
        let f = Arc::new(FakeAccessor::new());
        f.add(
            "v1/configmaps",
            obj(json!({"apiVersion": "v1", "kind": "ConfigMap",
                "metadata": {"name": "cfg", "namespace": "default", "resourceVersion": "7",
                    "managedFields": [{"manager": "x"}]},
                "data": {"a": "1"}})),
        );
        let acc: crate::k8s::SharedAccessor = f.clone();
        let gvr = crate::k8s::Gvr::parse("v1/configmaps");
        let (file, text, original) = edit_prepare(&acc, &gvr, "default/cfg").await.unwrap();
        assert!(text.starts_with("# Please edit the object below."));
        assert!(!text.contains("managedFields"), "{text}");
        let job = |retry_of: Option<String>| EditJob {
            gvr: gvr.clone(),
            path: "default/cfg".into(),
            file: file.to_path_buf(),
            original_text: text.clone(),
            original: original.clone(),
            retry_of,
        };
        // Unchanged: "Edit cancelled, no changes made."
        assert_eq!(edit_apply(&acc, job(None)).await, EditOutcome::Done(None));
        // Empty: cancelled.
        std::fs::write(&file, "# nothing\n").unwrap();
        assert_eq!(edit_apply(&acc, job(None)).await, EditOutcome::Done(None));
        assert!(f.calls().is_empty());
        // A change is merge-patched with kubectl's field manager.
        std::fs::write(&file, text.replace("a: \"1\"", "a: \"2\"")).unwrap();
        assert_eq!(edit_apply(&acc, job(None)).await, EditOutcome::Done(None));
        assert_eq!(
            f.calls(),
            [
                r#"patch_merge v1/configmaps default/cfg {"data":{"a":"2"},"metadata":{"resourceVersion":"7"}} kubectl-edit"#
            ]
        );
        // Syntax error: re-open with the reason.
        std::fs::write(&file, "data: [oops\n").unwrap();
        let EditOutcome::Reopen(t) = edit_apply(&acc, job(None)).await else {
            panic!("expected reopen");
        };
        assert!(t.contains("# The edited file had a syntax error:"), "{t}");
        // Unchanged after an error: give up.
        assert_eq!(
            edit_apply(&acc, job(Some(t))).await,
            EditOutcome::Done(Some("Edit cancelled, no valid changes were saved.".into()))
        );
        // Invalid: re-open with the causes.
        *f.patch_failure.lock().unwrap() = Some(crate::k8s::ApiFailure {
            code: 422,
            reason: "Invalid".into(),
            message: "bad".into(),
            causes: vec![("data.a".into(), "Invalid value".into())],
        });
        std::fs::write(&file, text.replace("a: \"1\"", "a: \"3\"")).unwrap();
        let EditOutcome::Reopen(t) = edit_apply(&acc, job(None)).await else {
            panic!("expected reopen");
        };
        assert!(
            t.contains("# configmaps \"cfg\" was not valid:\n# * data.a: Invalid value\n#\n"),
            "{t}"
        );
        // Changed while the editor was open: 409, the file is kept, no overwrite.
        f.add(
            "v1/configmaps",
            obj(json!({"apiVersion": "v1", "kind": "ConfigMap",
                "metadata": {"name": "cfg", "namespace": "default", "resourceVersion": "8"}})),
        );
        f.objects.lock().unwrap().get_mut(&gvr).unwrap().remove(0);
        std::fs::write(&file, text.replace("a: \"1\"", "a: \"4\"")).unwrap();
        let EditOutcome::Done(Some(e)) = edit_apply(&acc, job(None)).await else {
            panic!("expected a conflict");
        };
        assert!(
            e.starts_with("configmaps \"cfg\" could not be patched: Operation cannot be fulfilled"),
            "{e}"
        );
        assert!(e.contains("kubectl replace -f"), "{e}");
        // Renames are refused.
        std::fs::write(&file, text.replace("name: cfg", "name: other")).unwrap();
        assert_eq!(
            edit_apply(&acc, job(None)).await,
            EditOutcome::Done(Some(
                "At least one of apiVersion, kind and name was changed".into()
            ))
        );
    }

    #[test]
    fn exit_codes() {
        use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Status, StatusCause, StatusDetails};
        assert_eq!(exit_code(None), 0);
        let ok = Status {
            status: Some("Success".into()),
            ..Default::default()
        };
        assert_eq!(exit_code(Some(&ok)), 0);
        let failed = Status {
            status: Some("Failure".into()),
            details: Some(StatusDetails {
                causes: Some(vec![StatusCause {
                    reason: Some("ExitCode".into()),
                    message: Some("3".into()),
                    ..Default::default()
                }]),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(exit_code(Some(&failed)), 3);
        assert_eq!(exit_status(3).code(), Some(3));
    }
}
