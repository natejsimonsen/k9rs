//! Deployment, StatefulSet, DaemonSet and ReplicaSet (k9s dp.go, sts.go,
//! ds.go, rs.go).

use anyhow::Result;
use kube::api::DynamicObject;
use ratatui::style::Color;

use crate::model::{Align, Header, HeaderColumn, RowEvent, NA_VALUE};
use crate::render::{default_colorer, Palette};

use super::util;
use super::Def;
use crate::render::helpers::*;

fn col(n: &str) -> HeaderColumn {
    HeaderColumn::new(n)
}
fn right(n: &str) -> HeaderColumn {
    HeaderColumn::new(n).align(Align::Right)
}
fn wide(n: &str) -> HeaderColumn {
    HeaderColumn::new(n).wide()
}
fn vs() -> HeaderColumn {
    HeaderColumn::new("VS").vs()
}

/// k9s computeVulScore without an image scanner: always `n/a`.
fn vul_score() -> String {
    NA_VALUE.into()
}

pub(super) struct Deployment;

impl Def for Deployment {
    fn cols(&self) -> Vec<HeaderColumn> {
        vec![
            col("NAMESPACE"),
            col("NAME"),
            vs(),
            right("READY"),
            right("UP-TO-DATE"),
            right("AVAILABLE"),
            wide("LABELS"),
            wide("VALID"),
            HeaderColumn::new("AGE").time(),
        ]
    }

    fn row(&self, o: &DynamicObject) -> Result<(String, Vec<String>)> {
        let d = &o.data;
        let desired = i64_at(d, "spec.replicas");
        let avail = i64_at(d, "status.availableReplicas");
        let replicas = i64_at(d, "status.replicas");
        let diag = if replicas != avail {
            Err(format!(
                "desiring {replicas} replicas got {avail} available"
            ))
        } else {
            Ok(())
        };
        Ok((
            obj_fqn(o),
            vec![
                obj_ns(o).to_string(),
                obj_name(o).to_string(),
                vul_score(),
                format!("{avail}/{desired}"),
                i64_at(d, "status.updatedReplicas").to_string(),
                avail.to_string(),
                labels_str(o),
                as_status(diag),
                obj_age(o),
            ],
        ))
    }

    fn color(&self, ns: &str, h: &Header, re: &RowEvent, p: &Palette) -> Color {
        let c = default_colorer(ns, h, re, p);
        let Some(idx) = h.index_of("READY") else {
            return c;
        };
        let Some(ready) = re.row.fields.get(idx) else {
            return c;
        };
        let tt: Vec<&str> = ready.trim().split('/').collect();
        if tt.len() == 2 && tt[1] == "0" {
            return p.pending;
        }
        c
    }
}

pub(super) struct StatefulSet;

impl Def for StatefulSet {
    fn cols(&self) -> Vec<HeaderColumn> {
        vec![
            col("NAMESPACE"),
            col("NAME"),
            vs(),
            col("READY"),
            wide("SELECTOR"),
            col("SERVICE"),
            wide("CONTAINERS"),
            wide("IMAGES"),
            wide("LABELS"),
            wide("VALID"),
            HeaderColumn::new("AGE").time(),
        ]
    }

    fn row(&self, o: &DynamicObject) -> Result<(String, Vec<String>)> {
        let d = &o.data;
        let desired = i64_at(d, "spec.replicas");
        let current = i64_at(d, "status.replicas");
        let ready = i64_at(d, "status.readyReplicas");
        let diag = if current != ready {
            Err(format!("desired {current} replicas got {ready} available"))
        } else if desired != ready {
            Err(format!("want {desired} replicas got {ready} available"))
        } else {
            Ok(())
        };
        let spec = get_path(d, "spec.template.spec").unwrap_or(&serde_json::Value::Null);
        let (cc, ii) = util::pod_names_images(spec, true);
        Ok((
            obj_fqn(o),
            vec![
                obj_ns(o).to_string(),
                obj_name(o).to_string(),
                vul_score(),
                format!("{ready}/{desired}"),
                util::as_selector(get_path(d, "spec.selector")),
                na(str_at(d, "spec.serviceName")),
                cc,
                ii,
                labels_str(o),
                as_status(diag),
                obj_age(o),
            ],
        ))
    }
}

pub(super) struct DaemonSet;

impl Def for DaemonSet {
    fn cols(&self) -> Vec<HeaderColumn> {
        vec![
            col("NAMESPACE"),
            col("NAME"),
            vs(),
            right("DESIRED"),
            right("CURRENT"),
            right("READY"),
            right("UP-TO-DATE"),
            right("AVAILABLE"),
            wide("LABELS"),
            wide("VALID"),
            HeaderColumn::new("AGE").time(),
        ]
    }

    fn row(&self, o: &DynamicObject) -> Result<(String, Vec<String>)> {
        let d = &o.data;
        let desired = i64_at(d, "status.desiredNumberScheduled");
        let ready = i64_at(d, "status.numberReady");
        let diag = if desired != ready {
            Err(format!("desiring {desired} replicas but {ready} ready"))
        } else {
            Ok(())
        };
        Ok((
            obj_fqn(o),
            vec![
                obj_ns(o).to_string(),
                obj_name(o).to_string(),
                vul_score(),
                desired.to_string(),
                i64_at(d, "status.currentNumberScheduled").to_string(),
                ready.to_string(),
                i64_at(d, "status.updatedNumberScheduled").to_string(),
                i64_at(d, "status.numberAvailable").to_string(),
                labels_str(o),
                as_status(diag),
                obj_age(o),
            ],
        ))
    }
}

pub(super) struct ReplicaSet;

impl Def for ReplicaSet {
    fn cols(&self) -> Vec<HeaderColumn> {
        vec![
            col("NAMESPACE"),
            col("NAME"),
            vs(),
            right("DESIRED"),
            right("CURRENT"),
            right("READY"),
            wide("CONTAINERS"),
            wide("IMAGES"),
            wide("SELECTOR"),
            wide("VALID"),
            HeaderColumn::new("AGE").time(),
        ]
    }

    fn row(&self, o: &DynamicObject) -> Result<(String, Vec<String>)> {
        let d = &o.data;
        let replicas = i64_at(d, "status.replicas");
        let ready = i64_at(d, "status.readyReplicas");
        let diag = if replicas != ready {
            if replicas == 0 {
                Err(format!(
                    "did not phase down correctly expecting 0 replicas but got {ready}"
                ))
            } else {
                Err(format!("mismatch desired({replicas}) vs ready({ready})"))
            }
        } else {
            Ok(())
        };
        let spec = get_path(d, "spec.template.spec").unwrap_or(&serde_json::Value::Null);
        let (cc, ii) = util::pod_names_images(spec, false);
        // k9s fills the SELECTOR column with the object labels.
        Ok((
            obj_fqn(o),
            vec![
                obj_ns(o).to_string(),
                obj_name(o).to_string(),
                vul_score(),
                opt_i64_at(d, "spec.replicas").unwrap_or(0).to_string(),
                replicas.to_string(),
                ready.to_string(),
                cc,
                ii,
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
    use crate::model::ResEvent;
    use crate::render::Palette;

    #[test]
    fn deployment_header() {
        let h = renderer("apps/v1/deployments").header("all");
        assert_eq!(
            cols(&h),
            vec![
                ("NAMESPACE".into(), false, false, false),
                ("NAME".into(), false, false, false),
                ("VS".into(), false, false, false),
                ("READY".into(), false, false, true),
                ("UP-TO-DATE".into(), false, false, true),
                ("AVAILABLE".into(), false, false, true),
                ("LABELS".into(), true, false, false),
                ("VALID".into(), true, false, false),
                ("AGE".into(), false, true, false),
            ]
        );
        assert!(h.0[2].vs);
        assert_eq!(
            names(&renderer("apps/v1/deployments").header("default"))[0],
            "NAME"
        );
    }

    #[test]
    fn deployment_render() {
        let o = load("dp");
        let r = render("apps/v1/deployments", &o);
        assert_eq!(r.id, "icx/icx-db");
        assert_eq!(
            r.fields,
            sv(&[
                "icx",
                "icx-db",
                "n/a",
                "1/1",
                "1",
                "1",
                "app=icx-db",
                "",
                &age(&o)
            ])
        );
    }

    #[test]
    fn deployment_invalid() {
        let o = obj(json!({
            "apiVersion": "apps/v1", "kind": "Deployment",
            "metadata": {"name": "d", "namespace": "ns"},
            "spec": {"replicas": 3},
            "status": {"replicas": 3, "availableReplicas": 1, "updatedReplicas": 2}
        }));
        let r = render("apps/v1/deployments", &o);
        assert_eq!(r.fields[3..6], sv(&["1/3", "2", "1"]));
        assert_eq!(r.fields[7], "desiring 3 replicas got 1 available");
        assert_eq!(r.fields[8], "<unknown>");
    }

    #[test]
    fn deployment_colorer() {
        let r = renderer("apps/v1/deployments");
        let p = Palette::default();
        let row = ["ns", "d", "n/a", "0/0", "0", "0", "", "", "1d"];
        assert_eq!(color(&*r, "all", ResEvent::Unchanged, &row), p.pending);
        assert_eq!(color(&*r, "all", ResEvent::Add, &row), p.pending);
        let row = ["ns", "d", "n/a", "1/1", "1", "1", "", "", "1d"];
        assert_eq!(color(&*r, "all", ResEvent::Unchanged, &row), p.std);
        assert_eq!(color(&*r, "all", ResEvent::Add, &row), p.add);
        assert_eq!(color(&*r, "all", ResEvent::Update, &row), p.modify);
        assert_eq!(color(&*r, "all", ResEvent::Delete, &row), p.kill);
        let row = ["ns", "d", "n/a", "1/2", "1", "1", "", "boom", "1d"];
        assert_eq!(color(&*r, "all", ResEvent::Unchanged, &row), p.err);
        // Namespaced view: READY shifts left.
        let row = ["d", "n/a", "0/0", "0", "0", "", "", "1d"];
        assert_eq!(color(&*r, "default", ResEvent::Unchanged, &row), p.pending);
    }

    #[test]
    fn statefulset() {
        let r = renderer("apps/v1/statefulsets");
        assert_eq!(
            cols(&r.header("all")),
            vec![
                ("NAMESPACE".into(), false, false, false),
                ("NAME".into(), false, false, false),
                ("VS".into(), false, false, false),
                ("READY".into(), false, false, false),
                ("SELECTOR".into(), true, false, false),
                ("SERVICE".into(), false, false, false),
                ("CONTAINERS".into(), true, false, false),
                ("IMAGES".into(), true, false, false),
                ("LABELS".into(), true, false, false),
                ("VALID".into(), true, false, false),
                ("AGE".into(), false, true, false),
            ]
        );
        let o = load("sts");
        let row = render("apps/v1/statefulsets", &o);
        assert_eq!(row.id, "default/nginx-sts");
        assert_eq!(
            row.fields,
            sv(&[
                "default",
                "nginx-sts",
                "n/a",
                "4/4",
                "app=nginx-sts",
                "nginx-sts",
                "nginx",
                "k8s.gcr.io/nginx-slim:0.8",
                "app=nginx-sts",
                "",
                &age(&o),
            ])
        );
    }

    #[test]
    fn statefulset_diagnose() {
        let mk = |want: i64, cur: i64, ready: i64| {
            obj(json!({
                "apiVersion": "apps/v1", "kind": "StatefulSet",
                "metadata": {"name": "s", "namespace": "ns"},
                "spec": {"replicas": want, "template": {"spec": {
                    "initContainers": [{"name": "i1", "image": "img0"}],
                    "containers": [{"name": "c1", "image": "img1"}, {"name": "c2", "image": "img2"}]
                }}},
                "status": {"replicas": cur, "readyReplicas": ready}
            }))
        };
        let r = render("apps/v1/statefulsets", &mk(3, 3, 1));
        assert_eq!(r.fields[3], "1/3");
        assert_eq!(r.fields[4], "");
        assert_eq!(r.fields[5], "n/a");
        assert_eq!(r.fields[6], "i1,c1,c2");
        assert_eq!(r.fields[7], "img0,img1,img2");
        assert_eq!(r.fields[9], "desired 3 replicas got 1 available");
        let r = render("apps/v1/statefulsets", &mk(3, 1, 1));
        assert_eq!(r.fields[9], "want 3 replicas got 1 available");
        let r = render("apps/v1/statefulsets", &mk(1, 1, 1));
        assert_eq!(r.fields[9], "");
    }

    #[test]
    fn daemonset() {
        let r = renderer("apps/v1/daemonsets");
        assert_eq!(
            cols(&r.header("all")),
            vec![
                ("NAMESPACE".into(), false, false, false),
                ("NAME".into(), false, false, false),
                ("VS".into(), false, false, false),
                ("DESIRED".into(), false, false, true),
                ("CURRENT".into(), false, false, true),
                ("READY".into(), false, false, true),
                ("UP-TO-DATE".into(), false, false, true),
                ("AVAILABLE".into(), false, false, true),
                ("LABELS".into(), true, false, false),
                ("VALID".into(), true, false, false),
                ("AGE".into(), false, true, false),
            ]
        );
        let o = load("ds");
        let row = render("apps/v1/daemonsets", &o);
        assert_eq!(row.id, "kube-system/fluentd-gcp-v3.2.0");
        assert_eq!(
            row.fields[..8],
            sv(&[
                "kube-system",
                "fluentd-gcp-v3.2.0",
                "n/a",
                "2",
                "2",
                "2",
                "2",
                "2"
            ])
        );
        assert_eq!(row.fields[9], "");
        assert_eq!(row.fields[10], age(&o));
        let bad = obj(json!({
            "apiVersion": "apps/v1", "kind": "DaemonSet",
            "metadata": {"name": "d", "namespace": "ns"},
            "status": {"desiredNumberScheduled": 3, "numberReady": 1}
        }));
        assert_eq!(
            render("apps/v1/daemonsets", &bad).fields[9],
            "desiring 3 replicas but 1 ready"
        );
    }

    #[test]
    fn replicaset() {
        let r = renderer("apps/v1/replicasets");
        assert_eq!(
            cols(&r.header("all")),
            vec![
                ("NAMESPACE".into(), false, false, false),
                ("NAME".into(), false, false, false),
                ("VS".into(), false, false, false),
                ("DESIRED".into(), false, false, true),
                ("CURRENT".into(), false, false, true),
                ("READY".into(), false, false, true),
                ("CONTAINERS".into(), true, false, false),
                ("IMAGES".into(), true, false, false),
                ("SELECTOR".into(), true, false, false),
                ("VALID".into(), true, false, false),
                ("AGE".into(), false, true, false),
            ]
        );
        let o = load("rs");
        let row = render("apps/v1/replicasets", &o);
        assert_eq!(row.id, "icx/icx-db-7d4b578979");
        assert_eq!(
            row.fields[..6],
            sv(&["icx", "icx-db-7d4b578979", "n/a", "1", "1", "1"])
        );
        assert_eq!(row.fields[9], "");
        let mk = |cur: i64, ready: i64| {
            obj(json!({
                "apiVersion": "apps/v1", "kind": "ReplicaSet",
                "metadata": {"name": "r", "namespace": "ns", "labels": {"b": "2", "a": "1"}},
                "spec": {"replicas": 2, "template": {"spec": {
                    "initContainers": [{"name": "i1", "image": "img0"}],
                    "containers": [{"name": "c1", "image": "img1"}]
                }}},
                "status": {"replicas": cur, "readyReplicas": ready}
            }))
        };
        let row = render("apps/v1/replicasets", &mk(2, 1));
        assert_eq!(
            row.fields[3..10],
            sv(&[
                "2",
                "2",
                "1",
                "c1",
                "img1",
                "a=1,b=2",
                "mismatch desired(2) vs ready(1)"
            ])
        );
        let row = render("apps/v1/replicasets", &mk(0, 1));
        assert_eq!(
            row.fields[9],
            "did not phase down correctly expecting 0 replicas but got 1"
        );
    }
}
