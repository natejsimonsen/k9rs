//! Context rename/delete written back like client-go clientcmd.ModifyConfig:
//! only the file a context came from (the first file defining it) is
//! rewritten, re-encoded through the clientcmd v1 types (unknown fields and
//! empty optional fields dropped, named lists sorted) and printed with
//! sigs.k8s.io/yaml.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context as _, Result};
use base64::Engine as _;
use serde_json::{json, Map, Value};

/// One kubeconfig file in clientcmd api form (named maps).
#[derive(Debug, Default)]
struct FileConfig {
    clusters: BTreeMap<String, Value>,
    users: BTreeMap<String, Value>,
    contexts: BTreeMap<String, Value>,
    current: String,
    preferences: Option<Value>,
    extensions: Option<Value>,
}

/// RenameContext: fails if `new` exists anywhere in the merged config.
pub(crate) fn rename_context(paths: &[PathBuf], old: &str, new: &str) -> Result<()> {
    if paths.iter().any(|p| defines(p, new)) {
        return Err(anyhow!("context with name {new} already exists"));
    }
    let Some(p) = origin(paths, old) else {
        return Err(anyhow!("context {old:?} does not exist"));
    };
    let mut fc = load(&p)?;
    if let Some(c) = fc.contexts.remove(old) {
        fc.contexts.insert(new.to_string(), c);
    }
    write(&p, &fc)
}

/// DelContext.
pub(crate) fn delete_context(paths: &[PathBuf], name: &str) -> Result<()> {
    let Some(p) = origin(paths, name) else {
        return Ok(());
    };
    let mut fc = load(&p)?;
    fc.contexts.remove(name);
    write(&p, &fc)
}

/// LocationOfOrigin: the first file (merge order) defining `name`.
fn origin(paths: &[PathBuf], name: &str) -> Option<PathBuf> {
    paths.iter().find(|p| defines(p, name)).cloned()
}

fn defines(p: &Path, name: &str) -> bool {
    load(p).is_ok_and(|fc| fc.contexts.contains_key(name))
}

fn read_value(p: &Path) -> Result<Value> {
    let raw = std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
    let y: serde_yaml::Value = serde_yaml::from_str(&raw).map_err(|e| {
        anyhow!(
            "error loading config file {:?}: {e}",
            p.display().to_string()
        )
    })?;
    Ok(serde_json::to_value(y)?)
}

/// clientcmd.LoadFromFile into named maps (first entry wins on duplicates).
fn load(p: &Path) -> Result<FileConfig> {
    let v = read_value(p)?;
    let mut fc = FileConfig {
        current: str_of(&v, "current-context"),
        ..Default::default()
    };
    for (key, inner, map, norm) in [
        (
            "clusters",
            "cluster",
            &mut fc.clusters,
            cluster as fn(&Value) -> Value,
        ),
        ("users", "user", &mut fc.users, user),
        ("contexts", "context", &mut fc.contexts, context),
    ] {
        for item in v.get(key).and_then(|l| l.as_array()).into_iter().flatten() {
            let name = str_of(item, "name");
            let body = item.get(inner).cloned().unwrap_or(Value::Null);
            map.entry(name).or_insert_with(|| norm(&body));
        }
    }
    if let Some(p) = v.get("preferences") {
        let mut m = Map::new();
        if p.get("colors").and_then(|c| c.as_bool()) == Some(true) {
            m.insert("colors".into(), json!(true));
        }
        put_ext(&mut m, p);
        if !m.is_empty() {
            fc.preferences = Some(Value::Object(m));
        }
    }
    fc.extensions = v
        .get("extensions")
        .filter(|e| e.as_array().is_some_and(|a| !a.is_empty()))
        .cloned();
    Ok(fc)
}

/// clientcmd.WriteToFile (0600 for new files).
fn write(p: &Path, fc: &FileConfig) -> Result<()> {
    let named = |m: &BTreeMap<String, Value>, inner: &str| -> Value {
        if m.is_empty() {
            return Value::Null;
        }
        Value::Array(
            m.iter()
                .map(|(n, b)| json!({ "name": n, inner: b }))
                .collect(),
        )
    };
    let mut doc = Map::new();
    doc.insert("apiVersion".into(), json!("v1"));
    doc.insert("kind".into(), json!("Config"));
    doc.insert("clusters".into(), named(&fc.clusters, "cluster"));
    doc.insert("users".into(), named(&fc.users, "user"));
    doc.insert("contexts".into(), named(&fc.contexts, "context"));
    doc.insert("current-context".into(), json!(fc.current));
    if let Some(pr) = &fc.preferences {
        doc.insert("preferences".into(), pr.clone());
    }
    if let Some(e) = &fc.extensions {
        doc.insert("extensions".into(), e.clone());
    }
    let out = crate::k8s::goyaml::to_yaml(&Value::Object(doc));
    let existed = p.exists();
    std::fs::write(p, out).with_context(|| format!("writing {}", p.display()))?;
    #[cfg(unix)]
    if !existed {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn str_of(v: &Value, k: &str) -> String {
    match v.get(k) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        _ => String::new(),
    }
}

/// Copy a non-empty string field.
fn put_str(m: &mut Map<String, Value>, v: &Value, k: &str) {
    let s = str_of(v, k);
    if !s.is_empty() {
        m.insert(k.into(), json!(s));
    }
}

fn put_true(m: &mut Map<String, Value>, v: &Value, k: &str) {
    if v.get(k).and_then(|b| b.as_bool()) == Some(true) {
        m.insert(k.into(), json!(true));
    }
}

/// []byte fields: base64 decoded on load, re-encoded (std, padded) on write.
fn put_bytes(m: &mut Map<String, Value>, v: &Value, k: &str) {
    let s = str_of(v, k);
    if s.is_empty() {
        return;
    }
    let clean: String = s.chars().filter(|c| *c != '\n' && *c != '\r').collect();
    let std = base64::engine::general_purpose::STANDARD;
    let out = match std.decode(clean.as_bytes()) {
        Ok(b) if b.is_empty() => return,
        Ok(b) => std.encode(b),
        Err(_) => s,
    };
    m.insert(k.into(), json!(out));
}

fn put_ext(m: &mut Map<String, Value>, v: &Value) {
    if let Some(e) = v
        .get("extensions")
        .filter(|e| e.as_array().is_some_and(|a| !a.is_empty()))
    {
        m.insert("extensions".into(), e.clone());
    }
}

fn cluster(v: &Value) -> Value {
    let mut m = Map::new();
    m.insert("server".into(), json!(str_of(v, "server")));
    put_str(&mut m, v, "tls-server-name");
    put_true(&mut m, v, "insecure-skip-tls-verify");
    put_str(&mut m, v, "certificate-authority");
    put_bytes(&mut m, v, "certificate-authority-data");
    put_str(&mut m, v, "proxy-url");
    put_true(&mut m, v, "disable-compression");
    put_ext(&mut m, v);
    Value::Object(m)
}

fn context(v: &Value) -> Value {
    let mut m = Map::new();
    m.insert("cluster".into(), json!(str_of(v, "cluster")));
    m.insert("user".into(), json!(str_of(v, "user")));
    put_str(&mut m, v, "namespace");
    put_ext(&mut m, v);
    Value::Object(m)
}

fn user(v: &Value) -> Value {
    let mut m = Map::new();
    put_str(&mut m, v, "client-certificate");
    put_bytes(&mut m, v, "client-certificate-data");
    put_str(&mut m, v, "client-key");
    put_bytes(&mut m, v, "client-key-data");
    put_str(&mut m, v, "token");
    put_str(&mut m, v, "tokenFile");
    put_str(&mut m, v, "as");
    put_str(&mut m, v, "as-uid");
    if let Some(g) = v
        .get("as-groups")
        .filter(|g| g.as_array().is_some_and(|a| !a.is_empty()))
    {
        m.insert("as-groups".into(), g.clone());
    }
    if let Some(x) = v
        .get("as-user-extra")
        .filter(|x| x.as_object().is_some_and(|o| !o.is_empty()))
    {
        m.insert("as-user-extra".into(), x.clone());
    }
    put_str(&mut m, v, "username");
    put_str(&mut m, v, "password");
    if let Some(ap) = v.get("auth-provider").filter(|a| a.is_object()) {
        let config = ap
            .get("config")
            .filter(|c| c.is_object())
            .cloned()
            .unwrap_or(Value::Null);
        m.insert(
            "auth-provider".into(),
            json!({ "name": str_of(ap, "name"), "config": config }),
        );
    }
    if let Some(ex) = v.get("exec").filter(|e| e.is_object()) {
        m.insert("exec".into(), exec(ex));
    }
    put_ext(&mut m, v);
    Value::Object(m)
}

/// ExecConfig, with SetDefaults_ExecConfig's interactiveMode default.
fn exec(v: &Value) -> Value {
    let mut m = Map::new();
    m.insert("command".into(), json!(str_of(v, "command")));
    let list = |k: &str| {
        v.get(k)
            .filter(|a| a.is_array())
            .cloned()
            .unwrap_or(Value::Null)
    };
    m.insert("args".into(), list("args"));
    let env = match v.get("env").and_then(|e| e.as_array()) {
        Some(a) => Value::Array(
            a.iter()
                .map(|e| json!({ "name": str_of(e, "name"), "value": str_of(e, "value") }))
                .collect(),
        ),
        None => Value::Null,
    };
    m.insert("env".into(), env);
    let api = str_of(v, "apiVersion");
    put_str(&mut m, v, "apiVersion");
    put_str(&mut m, v, "installHint");
    m.insert(
        "provideClusterInfo".into(),
        json!(v
            .get("provideClusterInfo")
            .and_then(|b| b.as_bool())
            .unwrap_or(false)),
    );
    let mut mode = str_of(v, "interactiveMode");
    if mode.is_empty()
        && matches!(
            api.as_str(),
            "client.authentication.k8s.io/v1beta1" | "client.authentication.k8s.io/v1alpha1"
        )
    {
        mode = "IfAvailable".into();
    }
    if !mode.is_empty() {
        m.insert("interactiveMode".into(), json!(mode));
    }
    Value::Object(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    // What k9s v0.50.18 wrote after renaming ctx-a2 -> ctx-x in this file.
    const IN: &str = r#"apiVersion: v1
kind: Config
users:
  - name: fake-user
    user:
        token: not-a-real-token
  - name: exec-user
    user:
        exec:
            apiVersion: client.authentication.k8s.io/v1beta1
            command: /usr/bin/false
            args:
              - token
              - --cluster-id
              - "on"
contexts:
  - name: ctx-a
    context:
        cluster: fake
        user: fake-user
  - name: ctx-a2
    context:
        cluster: fake
        user: exec-user
        namespace: kube-system
clusters:
  - name: fake
    cluster:
        certificate-authority-data: Zm9vYmFy
        proxy-url: ""
        server: https://127.0.0.1:9
        unknown-field: 1
current-context: ctx-a
"#;

    const RENAMED: &str = r#"apiVersion: v1
clusters:
- cluster:
    certificate-authority-data: Zm9vYmFy
    server: https://127.0.0.1:9
  name: fake
contexts:
- context:
    cluster: fake
    user: fake-user
  name: ctx-a
- context:
    cluster: fake
    namespace: kube-system
    user: exec-user
  name: ctx-x
current-context: ctx-a
kind: Config
users:
- name: exec-user
  user:
    exec:
      apiVersion: client.authentication.k8s.io/v1beta1
      args:
      - token
      - --cluster-id
      - "on"
      command: /usr/bin/false
      env: null
      interactiveMode: IfAvailable
      provideClusterInfo: false
- name: fake-user
  user:
    token: not-a-real-token
"#;

    const KC2: &str = r#"apiVersion: v1
clusters:
- cluster:
    server: https://127.0.0.1:9
  name: fake2
contexts:
- context:
    cluster: fake2
    user: fake2-user
  name: ctx-b
current-context: ctx-b
kind: Config
preferences: {}
users:
- name: fake2-user
  user:
    token: also-not-real
"#;

    const KC2_DELETED: &str = r#"apiVersion: v1
clusters:
- cluster:
    server: https://127.0.0.1:9
  name: fake2
contexts: null
current-context: ctx-b
kind: Config
users:
- name: fake2-user
  user:
    token: also-not-real
"#;

    fn files() -> (tempfile::TempDir, Vec<PathBuf>) {
        let d = tempfile::tempdir().unwrap();
        let p1 = d.path().join("kc1");
        let p2 = d.path().join("kc2");
        std::fs::write(&p1, IN).unwrap();
        std::fs::write(&p2, KC2).unwrap();
        (d, vec![p1, p2])
    }

    #[test]
    fn rename_rewrites_origin_file_like_client_go() {
        let (_d, ps) = files();
        rename_context(&ps, "ctx-a2", "ctx-x").unwrap();
        assert_eq!(std::fs::read_to_string(&ps[0]).unwrap(), RENAMED);
        assert_eq!(
            std::fs::read_to_string(&ps[1]).unwrap(),
            KC2,
            "other file untouched"
        );
        // Names are checked across every file.
        assert_eq!(
            rename_context(&ps, "ctx-x", "ctx-b")
                .unwrap_err()
                .to_string(),
            "context with name ctx-b already exists"
        );
    }

    #[test]
    fn delete_writes_only_the_origin_file() {
        let (_d, ps) = files();
        delete_context(&ps, "ctx-b").unwrap();
        assert_eq!(std::fs::read_to_string(&ps[1]).unwrap(), KC2_DELETED);
        assert_eq!(std::fs::read_to_string(&ps[0]).unwrap(), IN);
    }

    #[test]
    fn first_file_wins_for_shared_names() {
        let (_d, ps) = files();
        std::fs::write(&ps[1], KC2.replace("ctx-b", "ctx-a")).unwrap();
        delete_context(&ps, "ctx-a").unwrap();
        // Removed from kc1 only; kc2's copy shows through afterwards.
        assert!(!std::fs::read_to_string(&ps[0])
            .unwrap()
            .contains("name: ctx-a\n"));
        assert!(std::fs::read_to_string(&ps[1])
            .unwrap()
            .contains("name: ctx-a"));
    }
}
