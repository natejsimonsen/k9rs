//! JSON schema validation of k9s config files (config/json/validator.go).
//!
//! k9s validates every YAML config against embedded JSON schemas with
//! gojsonschema. This is a small validator for the keyword subset those
//! schemas use (`type`, `properties`, `additionalProperties`, `required`,
//! `items`, `oneOf`) producing gojsonschema's error descriptions, sorted and
//! joined with newlines like k9s.

use serde_json::Value as Json;
use serde_yaml::Value as Yaml;

/// plugins.yaml schema (`plugins:` root).
pub const PLUGINS_SCHEMA: &str = "plugins.json";
/// A single plugin snippet file.
pub const PLUGIN_SCHEMA: &str = "plugin.json";
/// A file of several plugin snippets.
pub const PLUGIN_MULTI_SCHEMA: &str = "plugin-multi.json";
pub const ALIASES_SCHEMA: &str = "aliases.json";
pub const VIEWS_SCHEMA: &str = "views.json";
pub const HOTKEYS_SCHEMA: &str = "hotkeys.json";
pub const K9S_SCHEMA: &str = "k9s.json";
pub const CONTEXT_SCHEMA: &str = "context.json";
pub const SKIN_SCHEMA: &str = "skin.json";

fn schema_src(name: &str) -> Option<&'static str> {
    Some(match name {
        PLUGINS_SCHEMA => include_str!("schemas/plugins.json"),
        PLUGIN_SCHEMA => include_str!("schemas/plugin.json"),
        PLUGIN_MULTI_SCHEMA => include_str!("schemas/plugin-multi.json"),
        ALIASES_SCHEMA => include_str!("schemas/aliases.json"),
        VIEWS_SCHEMA => include_str!("schemas/views.json"),
        HOTKEYS_SCHEMA => include_str!("schemas/hotkeys.json"),
        K9S_SCHEMA => include_str!("schemas/k9s.json"),
        CONTEXT_SCHEMA => include_str!("schemas/context.json"),
        SKIN_SCHEMA => include_str!("schemas/skin.json"),
        _ => return None,
    })
}

/// Validate YAML text against the named schema. The error lists every
/// violation, one per line.
pub fn validate(schema: &str, bb: &str) -> anyhow::Result<()> {
    let doc = super::yaml::parse_value(bb)?;
    validate_value(schema, &doc)
}

/// Validate an already parsed YAML document.
pub fn validate_value(schema: &str, doc: &Yaml) -> anyhow::Result<()> {
    let src =
        schema_src(schema).ok_or_else(|| anyhow::anyhow!("no schema found for: {schema:?}"))?;
    let s: Json = serde_json::from_str(src)?;
    let mut errs = Vec::new();
    check(&s, doc, &mut errs);
    if errs.is_empty() {
        return Ok(());
    }
    errs.sort();
    Err(anyhow::anyhow!(errs.join("\n")))
}

/// Validate plugin files: full, snippet then multi-snippets schema. Returns
/// the first schema that validates.
pub fn validate_plugins(bb: &str) -> anyhow::Result<&'static str> {
    let doc = super::yaml::parse_value(bb)?;
    let mut all = Vec::new();
    for k in [PLUGINS_SCHEMA, PLUGIN_SCHEMA, PLUGIN_MULTI_SCHEMA] {
        match validate_value(k, &doc) {
            Ok(()) => return Ok(k),
            Err(e) => all.push(e.to_string()),
        }
    }
    Err(anyhow::anyhow!(all.join("\n")))
}

fn given(v: &Yaml) -> &'static str {
    match v {
        Yaml::Null => "null",
        Yaml::Bool(_) => "boolean",
        Yaml::Number(n) => {
            if n.is_i64() || n.is_u64() {
                "integer"
            } else {
                let f = n.as_f64().unwrap_or(0.5);
                if f.fract() == 0.0 && f.is_finite() {
                    "integer"
                } else {
                    "number"
                }
            }
        }
        Yaml::String(_) => "string",
        Yaml::Sequence(_) => "array",
        Yaml::Mapping(_) => "object",
        Yaml::Tagged(t) => given(&t.value),
    }
}

fn types(s: &Json) -> Vec<String> {
    match s.get("type") {
        Some(Json::String(t)) => vec![t.clone()],
        Some(Json::Array(a)) => a
            .iter()
            .filter_map(|t| t.as_str().map(String::from))
            .collect(),
        _ => vec![],
    }
}

fn type_ok(tt: &[String], g: &str) -> bool {
    tt.iter()
        .any(|t| t == g || (t == "number" && g == "integer"))
}

fn key_str(k: &Yaml) -> String {
    match k {
        Yaml::String(s) => s.clone(),
        Yaml::Bool(b) => b.to_string(),
        Yaml::Number(n) => n.to_string(),
        Yaml::Null => "null".into(),
        other => serde_yaml::to_string(other)
            .unwrap_or_default()
            .trim()
            .to_string(),
    }
}

fn check(s: &Json, v: &Yaml, errs: &mut Vec<String>) {
    let v = match v {
        Yaml::Tagged(t) => &t.value,
        v => v,
    };
    let tt = types(s);
    let g = given(v);
    if !tt.is_empty() && !type_ok(&tt, g) {
        let expected = if tt.len() > 1 {
            format!("[{}]", tt.join(","))
        } else {
            tt[0].clone()
        };
        errs.push(format!("Invalid type. Expected: {expected}, given: {g}"));
        return;
    }
    if let Some(Json::Array(one_of)) = s.get("oneOf") {
        let mut valid = 0;
        let mut best: Option<Vec<String>> = None;
        for sub in one_of {
            let mut e = Vec::new();
            check(sub, v, &mut e);
            if e.is_empty() {
                valid += 1;
            } else if valid == 0 && best.as_ref().is_none_or(|b| e.len() < b.len()) {
                best = Some(e);
            }
        }
        if valid != 1 {
            errs.push("Must validate one and only one schema (oneOf)".into());
            if valid == 0 {
                errs.extend(best.unwrap_or_default());
            }
        }
    }
    match v {
        Yaml::Mapping(m) => {
            let props = s.get("properties").and_then(|p| p.as_object());
            if let Some(Json::Array(req)) = s.get("required") {
                for r in req.iter().filter_map(|r| r.as_str()) {
                    if !m.keys().any(|k| key_str(k) == r) {
                        errs.push(format!("{r} is required"));
                    }
                }
            }
            for (k, val) in m {
                let ks = key_str(k);
                if let Some(ps) = props.and_then(|p| p.get(&ks)) {
                    check(ps, val, errs);
                    continue;
                }
                match s.get("additionalProperties") {
                    Some(Json::Bool(false)) => {
                        errs.push(format!("Additional property {ks} is not allowed"));
                    }
                    Some(sub @ Json::Object(_)) => check(sub, val, errs),
                    _ => {}
                }
            }
        }
        Yaml::Sequence(items) => {
            if let Some(item_schema @ Json::Object(_)) = s.get("items") {
                for it in items {
                    check(item_schema, it, errs);
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn td(p: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/src/config/testdata/json/{p}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    fn err_of(schema: &str, f: &str) -> String {
        match validate(schema, &td(f)) {
            Ok(()) => String::new(),
            Err(e) => e.to_string(),
        }
    }

    #[test]
    fn plugin_snippet() {
        assert!(validate(PLUGIN_SCHEMA, &td("plugins/snippet.yaml")).is_ok());
    }

    #[test]
    fn plugins() {
        assert_eq!(err_of(PLUGINS_SCHEMA, "plugins/cool.yaml"), "");
        assert_eq!(
            err_of(PLUGINS_SCHEMA, "plugins/toast.yaml"),
            "scopes is required\nshortCut is required"
        );
        assert_eq!(err_of(PLUGIN_SCHEMA, "plugins/snippet.yaml"), "");
        assert_eq!(err_of(PLUGIN_MULTI_SCHEMA, "plugins/snippets.yaml"), "");
    }

    #[test]
    fn repo_plugins_and_skins_validate() {
        let dir = format!(
            "{}/src/config/testdata/plugins-repo",
            env!("CARGO_MANIFEST_DIR")
        );
        let mut n = 0;
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            let bb = std::fs::read_to_string(&p).unwrap();
            assert!(
                validate(PLUGINS_SCHEMA, &bb).is_ok(),
                "{p:?}: {:?}",
                validate(PLUGINS_SCHEMA, &bb)
            );
            n += 1;
        }
        assert!(n > 40);
        for (name, src) in crate::config::skin::BUNDLED_SKINS {
            assert!(
                validate(SKIN_SCHEMA, src).is_ok(),
                "{name}: {:?}",
                validate(SKIN_SCHEMA, src)
            );
        }
    }

    #[test]
    fn skin() {
        assert_eq!(err_of(SKIN_SCHEMA, "skins/cool.yaml"), "");
        assert_eq!(
            err_of(SKIN_SCHEMA, "skins/toast.yaml"),
            "Additional property bodys is not allowed"
        );
    }

    #[test]
    fn k9s() {
        assert_eq!(err_of(K9S_SCHEMA, "k9s/cool.yaml"), "");
        assert_eq!(
            err_of(K9S_SCHEMA, "k9s/toast.yaml"),
            "Additional property shellPods is not allowed"
        );
    }

    #[test]
    fn context() {
        assert_eq!(err_of(CONTEXT_SCHEMA, "context/cool.yaml"), "");
        assert_eq!(
            err_of(CONTEXT_SCHEMA, "context/toast.yaml"),
            "Additional property fred is not allowed\nAdditional property namespaces is not allowed"
        );
        assert!(validate(CONTEXT_SCHEMA, "k9s:\n  proxy: null\n").is_ok());
        assert!(validate(CONTEXT_SCHEMA, "k9s:\n  proxy:\n    address: http://x\n").is_ok());
        assert_eq!(
            validate(CONTEXT_SCHEMA, "k9s:\n  proxy: 1\n").unwrap_err().to_string(),
            "Invalid type. Expected: null, given: integer\nMust validate one and only one schema (oneOf)"
        );
    }

    #[test]
    fn aliases() {
        assert_eq!(err_of(ALIASES_SCHEMA, "aliases/cool.yaml"), "");
        assert_eq!(
            err_of(ALIASES_SCHEMA, "aliases/toast.yaml"),
            "Additional property alias is not allowed\naliases is required"
        );
    }

    #[test]
    fn views() {
        assert_eq!(err_of(VIEWS_SCHEMA, "views/cool.yaml"), "");
        assert_eq!(
            err_of(VIEWS_SCHEMA, "views/toast.yaml"),
            "Additional property cols is not allowed\nAdditional property sortCol is not allowed\nInvalid type. Expected: object, given: null\ncolumns is required"
        );
    }

    #[test]
    fn hotkeys() {
        // The upstream fixture uses `hotKey`, which the schema rejects.
        assert_eq!(
            err_of(HOTKEYS_SCHEMA, "hotkeys/cool.yaml"),
            "Additional property hotKey is not allowed\nhotKeys is required"
        );
        assert!(validate(HOTKEYS_SCHEMA, "hotKeys:\n  a:\n    shortCut: Shift-0\n").is_ok());
    }

    #[test]
    fn empty_doc_and_unknown_schema() {
        assert_eq!(
            validate(VIEWS_SCHEMA, "").unwrap_err().to_string(),
            "Invalid type. Expected: object, given: null"
        );
        assert_eq!(
            validate("blee.json", "a: 1").unwrap_err().to_string(),
            "no schema found for: \"blee.json\""
        );
        assert_eq!(
            validate(K9S_SCHEMA, "k9s:\n  refreshRate: fast\n  maxConnRetry: 2.5\n")
                .unwrap_err()
                .to_string(),
            "Invalid type. Expected: integer, given: number\nInvalid type. Expected: number, given: string"
        );
    }
}
