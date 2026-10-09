//! ConfigMap, Secret, ServiceAccount and PodDisruptionBudget (k9s cm.go,
//! secret.go, sa.go, pdb.go).

use anyhow::Result;
use kube::api::DynamicObject;
use serde_json::Value;

use crate::model::{Align, HeaderColumn, NA_VALUE};

use super::util;
use super::Def;
use crate::render::helpers::*;

fn col(n: &str) -> HeaderColumn {
    HeaderColumn::new(n)
}
fn wide(n: &str) -> HeaderColumn {
    HeaderColumn::new(n).wide()
}
fn right(n: &str) -> HeaderColumn {
    HeaderColumn::new(n).align(Align::Right)
}

pub(super) struct ConfigMap;

impl Def for ConfigMap {
    fn cols(&self) -> Vec<HeaderColumn> {
        vec![
            col("NAMESPACE"),
            col("NAME"),
            col("DATA"),
            wide("VALID"),
            HeaderColumn::new("AGE").time(),
        ]
    }

    fn row(&self, o: &DynamicObject) -> Result<(String, Vec<String>)> {
        let d = &o.data;
        Ok((
            crate::k8s::fqn(obj_ns(o), obj_name(o)),
            vec![
                obj_ns(o).to_string(),
                obj_name(o).to_string(),
                util::obj_len_at(d, "data").to_string(),
                String::new(),
                obj_age(o),
            ],
        ))
    }
}

pub(super) struct Secret;

impl Def for Secret {
    fn cols(&self) -> Vec<HeaderColumn> {
        vec![
            col("NAMESPACE"),
            col("NAME"),
            col("TYPE"),
            col("DATA"),
            wide("VALID"),
            HeaderColumn::new("AGE").time(),
        ]
    }

    fn row(&self, o: &DynamicObject) -> Result<(String, Vec<String>)> {
        let d = &o.data;
        Ok((
            crate::k8s::fqn(obj_ns(o), obj_name(o)),
            vec![
                obj_ns(o).to_string(),
                obj_name(o).to_string(),
                str_at(d, "type").to_string(),
                util::obj_len_at(d, "data").to_string(),
                String::new(),
                obj_age(o),
            ],
        ))
    }
}

pub(super) struct ServiceAccount;

impl Def for ServiceAccount {
    fn cols(&self) -> Vec<HeaderColumn> {
        vec![
            col("NAMESPACE"),
            col("NAME"),
            col("SECRET"),
            wide("LABELS"),
            wide("VALID"),
            HeaderColumn::new("AGE").time(),
        ]
    }

    fn row(&self, o: &DynamicObject) -> Result<(String, Vec<String>)> {
        let d = &o.data;
        Ok((
            obj_fqn(o),
            vec![
                obj_ns(o).to_string(),
                obj_name(o).to_string(),
                array_at(d, "secrets").len().to_string(),
                labels_str(o),
                String::new(),
                obj_age(o),
            ],
        ))
    }
}

pub(super) struct PodDisruptionBudget;

/// k9s numbToStr.
fn numb_to_str(v: Option<&Value>) -> String {
    match v.filter(|v| !v.is_null()) {
        None => NA_VALUE.into(),
        Some(v) => go_fmt(v),
    }
}

impl Def for PodDisruptionBudget {
    fn cols(&self) -> Vec<HeaderColumn> {
        vec![
            col("NAMESPACE"),
            col("NAME"),
            right("MIN-AVAILABLE"),
            right("MAX-UNAVAILABLE"),
            right("ALLOWED-DISRUPTIONS"),
            right("CURRENT"),
            right("DESIRED"),
            right("EXPECTED"),
            wide("LABELS"),
            wide("VALID"),
            HeaderColumn::new("AGE").time(),
        ]
    }

    fn row(&self, o: &DynamicObject) -> Result<(String, Vec<String>)> {
        let d = &o.data;
        let min = get_path(d, "spec.minAvailable");
        let healthy = i64_at(d, "status.currentHealthy");
        // k9s compares IntVal, which is 0 for percentage values.
        let diag = match min.filter(|v| !v.is_null()) {
            Some(v) => {
                let want = v.as_i64().unwrap_or(0);
                if want > healthy {
                    Err(format!("expected {want} but got {healthy}"))
                } else {
                    Ok(())
                }
            }
            None => Ok(()),
        };
        Ok((
            obj_fqn(o),
            vec![
                obj_ns(o).to_string(),
                obj_name(o).to_string(),
                numb_to_str(min),
                numb_to_str(get_path(d, "spec.maxUnavailable")),
                i64_at(d, "status.disruptionsAllowed").to_string(),
                healthy.to_string(),
                i64_at(d, "status.desiredHealthy").to_string(),
                i64_at(d, "status.expectedPods").to_string(),
                labels_str(o),
                as_status(diag),
                obj_age(o),
            ],
        ))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::testutil::*;

    #[test]
    fn configmap() {
        assert_eq!(
            cols(&renderer("v1/configmaps").header("all")),
            vec![
                ("NAMESPACE".into(), false, false, false),
                ("NAME".into(), false, false, false),
                ("DATA".into(), false, false, false),
                ("VALID".into(), true, false, false),
                ("AGE".into(), false, true, false),
            ]
        );
        let o = load("cm");
        let r = render("v1/configmaps", &o);
        assert_eq!(r.id, "default/blee");
        assert_eq!(r.fields, sv(&["default", "blee", "2", "", &age(&o)]));
    }

    #[test]
    fn secret() {
        assert_eq!(
            names(&renderer("v1/secrets").header("all")),
            sv(&["NAMESPACE", "NAME", "TYPE", "DATA", "VALID", "AGE"])
        );
        let o = load("sec");
        let r = render("v1/secrets", &o);
        assert_eq!(r.id, "default/s1");
        assert_eq!(
            r.fields,
            sv(&["default", "s1", "Opaque", "2", "", &age(&o)])
        );
    }

    #[test]
    fn service_account() {
        assert_eq!(
            cols(&renderer("v1/serviceaccounts").header("all")),
            vec![
                ("NAMESPACE".into(), false, false, false),
                ("NAME".into(), false, false, false),
                ("SECRET".into(), false, false, false),
                ("LABELS".into(), true, false, false),
                ("VALID".into(), true, false, false),
                ("AGE".into(), false, true, false),
            ]
        );
        let o = load("sa");
        let r = render("v1/serviceaccounts", &o);
        assert_eq!(r.id, "default/blee");
        assert_eq!(r.fields[..3], sv(&["default", "blee", "2"]));
        assert_eq!(r.fields[4..], sv(&["", &age(&o)]));
    }

    #[test]
    fn pdb() {
        assert_eq!(
            cols(&renderer("policy/v1/poddisruptionbudgets").header("all")),
            vec![
                ("NAMESPACE".into(), false, false, false),
                ("NAME".into(), false, false, false),
                ("MIN-AVAILABLE".into(), false, false, true),
                ("MAX-UNAVAILABLE".into(), false, false, true),
                ("ALLOWED-DISRUPTIONS".into(), false, false, true),
                ("CURRENT".into(), false, false, true),
                ("DESIRED".into(), false, false, true),
                ("EXPECTED".into(), false, false, true),
                ("LABELS".into(), true, false, false),
                ("VALID".into(), true, false, false),
                ("AGE".into(), false, true, false),
            ]
        );
        let o = load("pdb");
        let r = render("policy/v1/poddisruptionbudgets", &o);
        assert_eq!(r.id, "default/fred");
        assert_eq!(
            r.fields,
            sv(&[
                "default",
                "fred",
                "2",
                "n/a",
                "0",
                "0",
                "2",
                "0",
                "",
                "expected 2 but got 0",
                &age(&o)
            ])
        );
        let o = obj(json!({
            "apiVersion": "policy/v1", "kind": "PodDisruptionBudget",
            "metadata": {"name": "p", "namespace": "ns"},
            "spec": {"minAvailable": "50%", "maxUnavailable": 1},
            "status": {"currentHealthy": 0}
        }));
        let r = render("policy/v1/poddisruptionbudgets", &o);
        assert_eq!(r.fields[2..4], sv(&["50%", "1"]));
        assert_eq!(r.fields[9], "");
    }
}
