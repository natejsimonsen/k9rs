//! Role, ClusterRole, RoleBinding and ClusterRoleBinding (k9s ro.go, cr.go,
//! rob.go, crb.go).

use anyhow::Result;
use kube::api::DynamicObject;
use serde_json::Value;

use crate::model::{HeaderColumn, NA_VALUE};

use super::Def;
use crate::render::helpers::*;

fn col(n: &str) -> HeaderColumn {
    HeaderColumn::new(n)
}
fn wide(n: &str) -> HeaderColumn {
    HeaderColumn::new(n).wide()
}

/// k9s toSubjectAlias.
fn to_subject_alias(s: &str) -> String {
    match s {
        "" => String::new(),
        "User" => "User".into(),
        "Group" => "Group".into(),
        "ServiceAccount" => "SvcAcct".into(),
        _ => s.to_uppercase(),
    }
}

/// k9s renderSubjects: (last subject kind, names).
fn render_subjects(ss: &[Value]) -> (String, String) {
    if ss.is_empty() {
        return (NA_VALUE.into(), String::new());
    }
    let mut kind = String::new();
    let mut names = Vec::with_capacity(ss.len());
    for sub in ss {
        kind = to_subject_alias(str_at(sub, "kind"));
        names.push(str_at(sub, "name").to_string());
    }
    (kind, names.join(","))
}

pub(super) struct Role;

impl Def for Role {
    fn cols(&self) -> Vec<HeaderColumn> {
        vec![
            col("NAMESPACE"),
            col("NAME"),
            wide("LABELS"),
            wide("VALID"),
            HeaderColumn::new("AGE").time(),
        ]
    }

    fn row(&self, o: &DynamicObject) -> Result<(String, Vec<String>)> {
        Ok((
            obj_fqn(o),
            vec![
                obj_ns(o).to_string(),
                obj_name(o).to_string(),
                labels_str(o),
                String::new(),
                obj_age(o),
            ],
        ))
    }
}

pub(super) struct ClusterRole;

impl Def for ClusterRole {
    fn cols(&self) -> Vec<HeaderColumn> {
        vec![col("NAME"), wide("LABELS"), HeaderColumn::new("AGE").time()]
    }

    fn row(&self, o: &DynamicObject) -> Result<(String, Vec<String>)> {
        Ok((
            crate::k8s::fqn(crate::k8s::CLUSTER_SCOPE, obj_name(o)),
            vec![obj_name(o).to_string(), labels_str(o), obj_age(o)],
        ))
    }
}

pub(super) struct RoleBinding;

impl Def for RoleBinding {
    fn cols(&self) -> Vec<HeaderColumn> {
        vec![
            col("NAMESPACE"),
            col("NAME"),
            col("ROLE"),
            col("KIND"),
            col("SUBJECTS"),
            wide("LABELS"),
            wide("VALID"),
            HeaderColumn::new("AGE").time(),
        ]
    }

    fn row(&self, o: &DynamicObject) -> Result<(String, Vec<String>)> {
        let d = &o.data;
        let (kind, ss) = render_subjects(array_at(d, "subjects"));
        Ok((
            obj_fqn(o),
            vec![
                obj_ns(o).to_string(),
                obj_name(o).to_string(),
                str_at(d, "roleRef.name").to_string(),
                kind,
                ss,
                labels_str(o),
                String::new(),
                obj_age(o),
            ],
        ))
    }
}

pub(super) struct ClusterRoleBinding;

impl Def for ClusterRoleBinding {
    fn cols(&self) -> Vec<HeaderColumn> {
        vec![
            col("NAME"),
            col("CLUSTERROLE"),
            col("SUBJECT-KIND"),
            col("SUBJECTS"),
            wide("LABELS"),
            HeaderColumn::new("AGE").time(),
        ]
    }

    fn row(&self, o: &DynamicObject) -> Result<(String, Vec<String>)> {
        let d = &o.data;
        let (kind, ss) = render_subjects(array_at(d, "subjects"));
        Ok((
            crate::k8s::fqn(crate::k8s::CLUSTER_SCOPE, obj_name(o)),
            vec![
                obj_name(o).to_string(),
                str_at(d, "roleRef.name").to_string(),
                kind,
                ss,
                labels_str(o),
                obj_age(o),
            ],
        ))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::testutil::*;
    use super::*;

    #[test]
    fn role() {
        assert_eq!(
            cols(&renderer("rbac.authorization.k8s.io/v1/roles").header("all")),
            vec![
                ("NAMESPACE".into(), false, false, false),
                ("NAME".into(), false, false, false),
                ("LABELS".into(), true, false, false),
                ("VALID".into(), true, false, false),
                ("AGE".into(), false, true, false),
            ]
        );
        let o = load("ro");
        let r = render("rbac.authorization.k8s.io/v1/roles", &o);
        assert_eq!(r.id, "default/blee");
        assert_eq!(r.fields[..2], sv(&["default", "blee"]));
        assert_eq!(r.fields[3..], sv(&["", &age(&o)]));
    }

    #[test]
    fn cluster_role() {
        assert_eq!(
            cols(&renderer("rbac.authorization.k8s.io/v1/clusterroles").header("all")),
            vec![
                ("NAME".into(), false, false, false),
                ("LABELS".into(), true, false, false),
                ("AGE".into(), false, true, false),
            ]
        );
        let o = load("cr");
        let r = render("rbac.authorization.k8s.io/v1/clusterroles", &o);
        assert_eq!(r.id, "blee");
        assert_eq!(r.fields[0], "blee");
        assert_eq!(r.fields[2], age(&o));
    }

    #[test]
    fn role_binding() {
        assert_eq!(
            names(&renderer("rbac.authorization.k8s.io/v1/rolebindings").header("all")),
            sv(&[
                "NAMESPACE",
                "NAME",
                "ROLE",
                "KIND",
                "SUBJECTS",
                "LABELS",
                "VALID",
                "AGE"
            ])
        );
        let o = load("rb");
        let r = render("rbac.authorization.k8s.io/v1/rolebindings", &o);
        assert_eq!(r.id, "default/blee");
        assert_eq!(
            r.fields,
            sv(&[
                "default",
                "blee",
                "blee",
                "SvcAcct",
                "fernand",
                "",
                "",
                &age(&o)
            ])
        );
    }

    #[test]
    fn cluster_role_binding() {
        assert_eq!(
            names(&renderer("rbac.authorization.k8s.io/v1/clusterrolebindings").header("all")),
            sv(&[
                "NAME",
                "CLUSTERROLE",
                "SUBJECT-KIND",
                "SUBJECTS",
                "LABELS",
                "AGE"
            ])
        );
        let o = load("crb");
        let r = render("rbac.authorization.k8s.io/v1/clusterrolebindings", &o);
        assert_eq!(r.id, "blee");
        assert_eq!(
            r.fields,
            sv(&["blee", "blee", "User", "fernand", "", &age(&o)])
        );
    }

    #[test]
    fn subjects() {
        assert_eq!(render_subjects(&[]), ("n/a".into(), "".into()));
        let ss = json!([
            {"kind": "User", "name": "a"},
            {"kind": "Group", "name": "b"},
            {"kind": "custom", "name": "c"}
        ]);
        assert_eq!(
            render_subjects(ss.as_array().unwrap()),
            ("CUSTOM".into(), "a,b,c".into())
        );
        assert_eq!(to_subject_alias(""), "");
        assert_eq!(to_subject_alias("Group"), "Group");
    }
}
