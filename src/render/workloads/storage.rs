//! PersistentVolume, PersistentVolumeClaim and StorageClass (k9s pv.go,
//! pvc.go, sc.go).

use anyhow::Result;
use kube::api::DynamicObject;
use ratatui::style::Color;

use crate::model::{Header, HeaderColumn, RowEvent, MISSING_VALUE};
use crate::render::{default_colorer, Palette};

use super::util;
use super::Def;
use crate::render::helpers::*;

const TERMINATING: &str = "Terminating";
const BETA_STORAGE_CLASS: &str = "volume.beta.kubernetes.io/storage-class";

fn col(n: &str) -> HeaderColumn {
    HeaderColumn::new(n)
}
fn wide(n: &str) -> HeaderColumn {
    HeaderColumn::new(n).wide()
}

/// k9s accessMode: deduped short access modes.
fn access_mode(aa: &[String]) -> String {
    let mut seen: Vec<&str> = Vec::new();
    for a in aa {
        if !seen.contains(&a.as_str()) {
            seen.push(a);
        }
    }
    seen.iter()
        .filter_map(|a| match *a {
            "ReadWriteOnce" => Some("RWO"),
            "ReadOnlyMany" => Some("ROX"),
            "ReadWriteMany" => Some("RWX"),
            "ReadWriteOncePod" => Some("RWOP"),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(",")
}

pub(super) struct PersistentVolume;

impl Def for PersistentVolume {
    fn cols(&self) -> Vec<HeaderColumn> {
        vec![
            col("NAME"),
            col("CAPACITY").capacity(),
            col("ACCESS MODES"),
            col("RECLAIM POLICY"),
            col("STATUS"),
            col("CLAIM"),
            col("STORAGECLASS"),
            col("REASON"),
            wide("VOLUMEMODE"),
            wide("LABELS"),
            wide("VALID"),
            HeaderColumn::new("AGE").time(),
        ]
    }

    fn row(&self, o: &DynamicObject) -> Result<(String, Vec<String>)> {
        let d = &o.data;
        let mut phase = str_at(d, "status.phase");
        if deletion_timestamp(o).is_some() {
            phase = TERMINATING;
        }
        let claim = match get_path(d, "spec.claimRef").filter(|v| !v.is_null()) {
            Some(c) => {
                let ns = str_at(c, "namespace");
                let n = str_at(c, "name");
                // path.Join
                match (ns.is_empty(), n.is_empty()) {
                    (true, true) => String::new(),
                    (true, false) => n.to_string(),
                    (false, true) => ns.to_string(),
                    (false, false) => format!("{ns}/{n}"),
                }
            }
            None => String::new(),
        };
        let class = match util::annotation(o, BETA_STORAGE_CLASS) {
            Some(c) => c.to_string(),
            None => str_at(d, "spec.storageClassName").to_string(),
        };
        let size = util::opt_str_at(d, "spec.capacity.storage")
            .map(util::canonical_quantity)
            .unwrap_or_else(|| "0".into());
        let mode = util::opt_str_at(d, "spec.volumeMode")
            .map(String::from)
            .unwrap_or_else(|| MISSING_VALUE.into());
        let diag = if phase == "Failed" {
            Err("failed to delete or recycle".to_string())
        } else {
            Ok(())
        };
        Ok((
            obj_fqn(o),
            vec![
                obj_name(o).to_string(),
                size,
                access_mode(&util::strs_at(d, "spec.accessModes")),
                str_at(d, "spec.persistentVolumeReclaimPolicy").to_string(),
                phase.to_string(),
                claim,
                class,
                str_at(d, "status.reason").to_string(),
                mode,
                labels_str(o),
                as_status(diag),
                obj_age(o),
            ],
        ))
    }

    fn color(&self, ns: &str, h: &Header, re: &RowEvent, p: &Palette) -> Color {
        let c = default_colorer(ns, h, re, p);
        let Some(status) = h.index_of("STATUS").and_then(|i| re.row.fields.get(i)) else {
            return c;
        };
        match status.trim() {
            "Bound" => p.std,
            "Available" => Color::Green,
            "Pending" => p.pending,
            TERMINATING => p.completed,
            _ => c,
        }
    }
}

pub(super) struct PersistentVolumeClaim;

impl Def for PersistentVolumeClaim {
    fn cols(&self) -> Vec<HeaderColumn> {
        vec![
            col("NAMESPACE"),
            col("NAME"),
            col("STATUS"),
            col("VOLUME"),
            col("CAPACITY").capacity(),
            col("ACCESS MODES"),
            col("STORAGECLASS"),
            wide("LABELS"),
            wide("VALID"),
            HeaderColumn::new("AGE").time(),
        ]
    }

    fn row(&self, o: &DynamicObject) -> Result<(String, Vec<String>)> {
        let d = &o.data;
        let mut phase = str_at(d, "status.phase");
        if deletion_timestamp(o).is_some() {
            phase = TERMINATING;
        }
        let volume = str_at(d, "spec.volumeName");
        let (capacity, modes) = if volume.is_empty() {
            (String::new(), String::new())
        } else {
            let cap = util::opt_str_at(d, "status.capacity.storage")
                .map(util::canonical_quantity)
                .unwrap_or_else(|| "0".into());
            (cap, access_mode(&util::strs_at(d, "status.accessModes")))
        };
        let class = match util::annotation(o, BETA_STORAGE_CLASS) {
            Some(c) => c.to_string(),
            None => str_at(d, "spec.storageClassName").to_string(),
        };
        let diag = if phase != "Bound" && phase != "Available" {
            Err(format!("unexpected status {phase}"))
        } else {
            Ok(())
        };
        Ok((
            obj_fqn(o),
            vec![
                obj_ns(o).to_string(),
                obj_name(o).to_string(),
                phase.to_string(),
                volume.to_string(),
                capacity,
                modes,
                class,
                labels_str(o),
                as_status(diag),
                obj_age(o),
            ],
        ))
    }
}

pub(super) struct StorageClass;

const DEFAULT_CLASS: &str = "storageclass.kubernetes.io/is-default-class";
const BETA_DEFAULT_CLASS: &str = "storageclass.beta.kubernetes.io/is-default-class";

impl Def for StorageClass {
    fn cols(&self) -> Vec<HeaderColumn> {
        vec![
            col("NAME"),
            col("PROVISIONER"),
            col("RECLAIMPOLICY"),
            col("VOLUMEBINDINGMODE"),
            col("ALLOWVOLUMEEXPANSION"),
            wide("LABELS"),
            wide("VALID"),
            HeaderColumn::new("AGE").time(),
        ]
    }

    fn row(&self, o: &DynamicObject) -> Result<(String, Vec<String>)> {
        let d = &o.data;
        let is_default = util::annotation(o, DEFAULT_CLASS) == Some("true")
            || util::annotation(o, BETA_DEFAULT_CLASS) == Some("true");
        let mut name = obj_name(o).to_string();
        if is_default {
            name.push_str(" (default)");
        }
        Ok((
            crate::k8s::fqn(crate::k8s::CLUSTER_SCOPE, obj_name(o)),
            vec![
                name,
                str_at(d, "provisioner").to_string(),
                str_at(d, "reclaimPolicy").to_string(),
                str_at(d, "volumeBindingMode").to_string(),
                bool_to_str(bool_at(d, "allowVolumeExpansion")),
                labels_str(o),
                String::new(),
                obj_age(o),
            ],
        ))
    }
}

#[cfg(test)]
mod tests {
    use ratatui::style::Color;
    use serde_json::json;

    use super::super::testutil::*;
    use super::*;
    use crate::model::ResEvent;

    #[test]
    fn pv_header() {
        let h = renderer("v1/persistentvolumes").header("all");
        assert_eq!(
            cols(&h),
            vec![
                ("NAME".into(), false, false, false),
                ("CAPACITY".into(), false, false, false),
                ("ACCESS MODES".into(), false, false, false),
                ("RECLAIM POLICY".into(), false, false, false),
                ("STATUS".into(), false, false, false),
                ("CLAIM".into(), false, false, false),
                ("STORAGECLASS".into(), false, false, false),
                ("REASON".into(), false, false, false),
                ("VOLUMEMODE".into(), true, false, false),
                ("LABELS".into(), true, false, false),
                ("VALID".into(), true, false, false),
                ("AGE".into(), false, true, false),
            ]
        );
        assert!(h.is_capacity_col(1));
        assert_eq!(h, renderer("v1/persistentvolumes").header("default"));
    }

    #[test]
    fn pv_render() {
        let o = load("pv");
        let r = render("v1/persistentvolumes", &o);
        assert_eq!(r.id, "pvc-07aa4e2c-8726-11e9-a8e8-42010a80015b");
        assert_eq!(
            r.fields,
            sv(&[
                "pvc-07aa4e2c-8726-11e9-a8e8-42010a80015b",
                "1Gi",
                "RWO",
                "Delete",
                "Bound",
                "default/www-nginx-sts-1",
                "standard",
                "",
                "<none>",
                "failure-domain.beta.kubernetes.io/region=us-central1,failure-domain.beta.kubernetes.io/zone=us-central1-a",
                "",
                &age(&o),
            ])
        );
    }

    #[test]
    fn pv_terminating() {
        let o = load("pv_terminating");
        let r = render("v1/persistentvolumes", &o);
        assert_eq!(r.id, "pvc-a4d86f51-916c-476b-83af-b551c91a8ac0");
        assert_eq!(
            r.fields[..7],
            sv(&[
                "pvc-a4d86f51-916c-476b-83af-b551c91a8ac0",
                "1Gi",
                "RWO",
                "Delete",
                "Terminating",
                "default/www-nginx-sts-2",
                "standard"
            ])
        );
    }

    #[test]
    fn pv_failed() {
        let o = obj(json!({
            "apiVersion": "v1", "kind": "PersistentVolume",
            "metadata": {"name": "p", "annotations": {"volume.beta.kubernetes.io/storage-class": "beta"}},
            "spec": {"capacity": {"storage": "1.5Gi"}, "volumeMode": "Block",
                     "accessModes": ["ReadWriteMany", "ReadWriteMany", "ReadOnlyMany", "ReadWriteOncePod"],
                     "storageClassName": "x"},
            "status": {"phase": "Failed", "reason": "boom"}
        }));
        let r = render("v1/persistentvolumes", &o);
        assert_eq!(
            r.fields[..11],
            sv(&[
                "p",
                "1536Mi",
                "RWX,ROX,RWOP",
                "",
                "Failed",
                "",
                "beta",
                "boom",
                "Block",
                "",
                "failed to delete or recycle"
            ])
        );
    }

    #[test]
    fn pv_colorer() {
        let r = renderer("v1/persistentvolumes");
        let p = Palette::default();
        let row = |st: &'static str| {
            vec![
                "p", "1Gi", "RWO", "Delete", st, "", "", "", "", "", "", "1d",
            ]
        };
        assert_eq!(color(&*r, "-", ResEvent::Add, &row("Bound")), p.std);
        assert_eq!(
            color(&*r, "-", ResEvent::Unchanged, &row("Available")),
            Color::Green
        );
        assert_eq!(
            color(&*r, "-", ResEvent::Unchanged, &row("Pending")),
            p.pending
        );
        assert_eq!(
            color(&*r, "-", ResEvent::Unchanged, &row("Terminating")),
            p.completed
        );
        assert_eq!(color(&*r, "-", ResEvent::Add, &row("Released")), p.add);
    }

    #[test]
    fn pvc() {
        assert_eq!(
            cols(&renderer("v1/persistentvolumeclaims").header("all")),
            vec![
                ("NAMESPACE".into(), false, false, false),
                ("NAME".into(), false, false, false),
                ("STATUS".into(), false, false, false),
                ("VOLUME".into(), false, false, false),
                ("CAPACITY".into(), false, false, false),
                ("ACCESS MODES".into(), false, false, false),
                ("STORAGECLASS".into(), false, false, false),
                ("LABELS".into(), true, false, false),
                ("VALID".into(), true, false, false),
                ("AGE".into(), false, true, false),
            ]
        );
        let o = load("pvc");
        let r = render("v1/persistentvolumeclaims", &o);
        assert_eq!(r.id, "default/www-nginx-sts-0");
        assert_eq!(
            r.fields,
            sv(&[
                "default",
                "www-nginx-sts-0",
                "Bound",
                "pvc-fbabd470-8725-11e9-a8e8-42010a80015b",
                "1Gi",
                "RWO",
                "standard",
                "app=nginx-sts",
                "",
                &age(&o),
            ])
        );
        let o = obj(json!({
            "apiVersion": "v1", "kind": "PersistentVolumeClaim",
            "metadata": {"name": "c", "namespace": "ns", "deletionTimestamp": "2020-01-01T00:00:00Z"},
            "spec": {"resources": {"requests": {"storage": "1Gi"}}},
            "status": {"phase": "Pending"}
        }));
        let r = render("v1/persistentvolumeclaims", &o);
        assert_eq!(
            r.fields[2..9],
            sv(&[
                "Terminating",
                "",
                "",
                "",
                "",
                "",
                "unexpected status Terminating"
            ])
        );
    }

    #[test]
    fn storage_class() {
        assert_eq!(
            names(&renderer("storage.k8s.io/v1/storageclasses").header("all")),
            sv(&[
                "NAME",
                "PROVISIONER",
                "RECLAIMPOLICY",
                "VOLUMEBINDINGMODE",
                "ALLOWVOLUMEEXPANSION",
                "LABELS",
                "VALID",
                "AGE"
            ])
        );
        let o = load("sc");
        let r = render("storage.k8s.io/v1/storageclasses", &o);
        assert_eq!(r.id, "standard");
        assert_eq!(
            r.fields,
            sv(&[
                "standard (default)",
                "kubernetes.io/gce-pd",
                "Delete",
                "Immediate",
                "true",
                "addonmanager.kubernetes.io/mode=EnsureExists,kubernetes.io/cluster-service=true",
                "",
                &age(&o),
            ])
        );
        let o = obj(
            json!({"apiVersion": "storage.k8s.io/v1", "kind": "StorageClass",
            "metadata": {"name": "x"}, "provisioner": "p"}),
        );
        let r = render("storage.k8s.io/v1/storageclasses", &o);
        assert_eq!(r.fields[..5], sv(&["x", "p", "", "", "false"]));
    }

    #[test]
    fn access_modes() {
        assert_eq!(access_mode(&[]), "");
        assert_eq!(
            access_mode(&sv(&["ReadWriteOnce", "Bogus", "ReadWriteOnce"])),
            "RWO"
        );
    }
}
