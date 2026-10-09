//! Pseudo resource views: Rbac (policy rules of a role), Policy (rules bound
//! to a subject), Subject (users/groups) and Reference (k9s rbac.go,
//! policy.go, subject.go, reference.go). Inputs are the pseudo objects from
//! `render::pseudo`.

use anyhow::Result;
use kube::api::DynamicObject;
use ratatui::style::Color;

use crate::model::{Header, HeaderColumn, RowEvent};
use crate::render::pseudo::{str_field, strs_field};
use crate::render::Palette;

use super::Def;
use crate::render::helpers::*;

/// tcell.ColorMediumSpringGreen.
const MEDIUM_SPRING_GREEN: Color = Color::Rgb(0x00, 0xFA, 0x9A);
/// tcell.ColorCadetBlue.
const CADET_BLUE: Color = Color::Rgb(0x5F, 0x9E, 0xA0);

const ALL_VERBS: &str = "*";
const K8S_VERBS: [&str; 8] = [
    "get",
    "list",
    "watch",
    "create",
    "patch",
    "update",
    "delete",
    "deletecollection",
];

fn http_to_k8s_verb(v: &str) -> Option<&'static str> {
    match v {
        "post" => Some("create"),
        "put" => Some("update"),
        _ => None,
    }
}

fn rbac_verb_header() -> Vec<HeaderColumn> {
    vec![
        HeaderColumn::new("GET   "),
        HeaderColumn::new("LIST  "),
        HeaderColumn::new("WATCH "),
        HeaderColumn::new("CREATE"),
        HeaderColumn::new("PATCH "),
        HeaderColumn::new("UPDATE"),
        HeaderColumn::new("DELETE"),
        HeaderColumn::new("DEL-LIST "),
        HeaderColumn::new("EXTRAS").wide(),
    ]
}

fn has_verb(verbs: &[String], verb: &str) -> bool {
    if verbs.len() == 1 && verbs[0] == ALL_VERBS {
        return true;
    }
    verbs
        .iter()
        .any(|v| http_to_k8s_verb(v) == Some(verb) || v == verb)
}

fn to_verb_icon(ok: bool) -> String {
    if ok {
        "[green::b] ✓ [::]".into()
    } else {
        "[orangered::b] × [::]".into()
    }
}

/// k9s asVerbs: one icon per k8s verb plus the truncated extras.
fn as_verbs(verbs: &[String]) -> Vec<String> {
    const UNKNOWN_LEN: usize = 30;
    let mut r: Vec<String> = K8S_VERBS
        .iter()
        .map(|v| to_verb_icon(has_verb(verbs, v)))
        .collect();
    let unknowns: Vec<&str> = verbs
        .iter()
        .map(|v| http_to_k8s_verb(v).unwrap_or(v))
        .filter(|v| !K8S_VERBS.contains(v) && *v != ALL_VERBS)
        .collect();
    r.push(truncate(&unknowns.join(","), UNKNOWN_LEN));
    r
}

/// k9s cleanseResource: drop the leading group from `grp/res[/sub]`.
fn cleanse_resource(r: &str) -> String {
    if r.is_empty() || r.starts_with('/') {
        return r.to_string();
    }
    let tt: Vec<&str> = r.split('/').collect();
    match tt.len() {
        2 | 3 => r
            .strip_prefix(&format!("{}/", tt[0]))
            .unwrap_or(r)
            .to_string(),
        _ => r.to_string(),
    }
}

pub(super) struct Rbac;

impl Def for Rbac {
    // k9s ignores views.yaml columns for this view.
    fn custom_cols(&self) -> bool {
        false
    }

    fn cols(&self) -> Vec<HeaderColumn> {
        let mut h = vec![HeaderColumn::new("NAME"), HeaderColumn::new("API-GROUP")];
        h.extend(rbac_verb_header());
        h.push(HeaderColumn::new("VALID").wide());
        h
    }

    fn row(&self, o: &DynamicObject) -> Result<(String, Vec<String>)> {
        let res = str_field(o, "resource");
        let mut f = vec![cleanse_resource(res), str_field(o, "group").to_string()];
        f.extend(as_verbs(&strs_field(o, "verbs")));
        f.push(String::new());
        Ok((res.to_string(), f))
    }
}

pub(super) struct Policy;

impl Def for Policy {
    // k9s ignores views.yaml columns for this view.
    fn custom_cols(&self) -> bool {
        false
    }

    fn cols(&self) -> Vec<HeaderColumn> {
        let mut h = vec![
            HeaderColumn::new("NAMESPACE"),
            HeaderColumn::new("NAME"),
            HeaderColumn::new("API-GROUP"),
            HeaderColumn::new("BINDING"),
        ];
        h.extend(rbac_verb_header());
        h.push(HeaderColumn::new("VALID").wide());
        h
    }

    fn row(&self, o: &DynamicObject) -> Result<(String, Vec<String>)> {
        let ns = str_field(o, "namespace");
        let res = str_field(o, "resource");
        let mut f = vec![
            ns.to_string(),
            cleanse_resource(res),
            str_field(o, "group").to_string(),
            str_field(o, "binding").to_string(),
        ];
        f.extend(as_verbs(&strs_field(o, "verbs")));
        f.push(String::new());
        Ok((crate::k8s::fqn(ns, res), f))
    }

    fn color(&self, _: &str, _: &Header, _: &RowEvent, _: &Palette) -> Color {
        MEDIUM_SPRING_GREEN
    }
}

pub(super) struct Subject;

impl Def for Subject {
    // k9s ignores views.yaml columns for this view.
    fn custom_cols(&self) -> bool {
        false
    }

    fn cols(&self) -> Vec<HeaderColumn> {
        vec![
            HeaderColumn::new("NAME"),
            HeaderColumn::new("KIND"),
            HeaderColumn::new("FIRST LOCATION"),
            HeaderColumn::new("VALID").wide(),
        ]
    }

    fn row(&self, o: &DynamicObject) -> Result<(String, Vec<String>)> {
        let name = obj_name(o).to_string();
        Ok((
            name.clone(),
            vec![
                name,
                str_field(o, "subjectKind").to_string(),
                str_field(o, "firstLocation").to_string(),
                String::new(),
            ],
        ))
    }

    fn color(&self, _: &str, _: &Header, _: &RowEvent, _: &Palette) -> Color {
        MEDIUM_SPRING_GREEN
    }
}

pub(super) struct Reference;

impl Def for Reference {
    // k9s ignores views.yaml columns for this view.
    fn custom_cols(&self) -> bool {
        false
    }

    fn cols(&self) -> Vec<HeaderColumn> {
        vec![
            HeaderColumn::new("NAMESPACE"),
            HeaderColumn::new("NAME"),
            HeaderColumn::new("GVR"),
        ]
    }

    fn row(&self, o: &DynamicObject) -> Result<(String, Vec<String>)> {
        let ns = obj_ns(o).to_string();
        let name = obj_name(o).to_string();
        Ok((
            crate::k8s::fqn(&ns, &name),
            vec![ns, name, str_field(o, "gvr").to_string()],
        ))
    }

    fn color(&self, _: &str, _: &Header, _: &RowEvent, _: &Palette) -> Color {
        CADET_BLUE
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::*;
    use super::*;
    use crate::model::ResEvent;
    use crate::render::pseudo;

    const Y: &str = "[green::b] ✓ [::]";
    const N: &str = "[orangered::b] × [::]";

    fn rule(ns: &str, res: &str, grp: &str, verbs: &[&str]) -> DynamicObject {
        pseudo::policy_rule(ns, grp, res, &[], &[], &sv(verbs), "fred", "Role")
    }

    #[test]
    fn rbac_header() {
        let h = renderer("rbac").header("all");
        assert_eq!(
            names(&h),
            sv(&[
                "NAME",
                "API-GROUP",
                "GET   ",
                "LIST  ",
                "WATCH ",
                "CREATE",
                "PATCH ",
                "UPDATE",
                "DELETE",
                "DEL-LIST ",
                "EXTRAS",
                "VALID"
            ])
        );
        assert!(h.0[10].wide && h.0[11].wide);
        assert!(!h.0[9].wide);
    }

    #[test]
    fn rbac_render() {
        let o = rule(
            "",
            "apps/deployments",
            "apps",
            &["get", "post", "put", "bind"],
        );
        let r = render("rbac", &o);
        assert_eq!(r.id, "apps/deployments");
        assert_eq!(
            r.fields,
            sv(&["deployments", "apps", Y, N, N, Y, N, Y, N, N, "bind", ""])
        );
        let r = render("rbac", &rule("", "pods", "", &["*"]));
        assert_eq!(r.fields[2..10], sv(&[Y, Y, Y, Y, Y, Y, Y, Y]));
        assert_eq!(r.fields[10], "");
    }

    #[test]
    fn policy_render() {
        let o = rule("blee", "res", "grp", &["get", "list", "watch"]);
        let r = render("policy", &o);
        assert_eq!(r.id, "blee/res");
        assert_eq!(
            r.fields,
            sv(&["blee", "res", "grp", "fred", Y, Y, Y, N, N, N, N, N, "", ""])
        );
        let h = renderer("policy").header("blee");
        assert_eq!(h.len(), 13);
        assert_eq!(h.index_of("NAMESPACE"), None);
    }

    #[test]
    fn colors() {
        for (g, c) in [
            ("policy", MEDIUM_SPRING_GREEN),
            ("users", MEDIUM_SPRING_GREEN),
            ("groups", MEDIUM_SPRING_GREEN),
            ("references", CADET_BLUE),
        ] {
            let r = renderer(g);
            assert_eq!(color(&*r, "all", ResEvent::Add, &["x"]), c, "{g}");
        }
        let r = renderer("rbac");
        assert_eq!(
            color(&*r, "all", ResEvent::Add, &["x"]),
            Palette::default().add
        );
    }

    #[test]
    fn extras_truncated() {
        let long = ["aaaaaaaaaa", "bbbbbbbbbb", "cccccccccc", "dddd"];
        let v = as_verbs(&sv(&long));
        assert_eq!(v[8], "aaaaaaaaaa,bbbbbbbbbb,ccccccc…");
        assert_eq!(v[8].chars().count(), 30);
    }

    #[test]
    fn cleanse() {
        for (r, e) in [
            ("", ""),
            ("fred", "fred"),
            ("fred/blee", "blee"),
            ("fred/blee/bob", "blee/bob"),
            ("/healthz", "/healthz"),
            ("a/b/c/d", "a/b/c/d"),
        ] {
            assert_eq!(cleanse_resource(r), e, "{r}");
        }
    }

    #[test]
    fn subject() {
        assert_eq!(
            names(&renderer("users").header("all")),
            sv(&["NAME", "KIND", "FIRST LOCATION", "VALID"])
        );
        let o = pseudo::subject(
            "fernand",
            "User",
            "ClusterRoleBinding:blee",
            "ClusterRoleBinding",
        );
        let r = render("users", &o);
        assert_eq!(r.id, "fernand");
        assert_eq!(
            r.fields,
            sv(&["fernand", "User", "ClusterRoleBinding:blee", ""])
        );
    }

    #[test]
    fn reference() {
        assert_eq!(
            names(&renderer("references").header("all")),
            sv(&["NAMESPACE", "NAME", "GVR"])
        );
        let o = pseudo::reference("ns1", "blee", "v1/secrets");
        let r = render("references", &o);
        assert_eq!(r.id, "ns1/blee");
        assert_eq!(r.fields, sv(&["ns1", "blee", "v1/secrets"]));
    }
}
