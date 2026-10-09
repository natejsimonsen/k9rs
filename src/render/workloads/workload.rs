//! Workloads view (k9s workload.go + dao/workload.go): pods, services,
//! daemonsets, statefulsets, deployments and replicasets in one table.
//! k9s derives STATUS/READY from each kind's server-side table cells; here
//! the same cells (kubectl printer columns) are computed from the object.

use anyhow::Result;
use kube::api::DynamicObject;
use ratatui::style::Color;
use serde_json::Value;

use crate::model::{Header, HeaderColumn, RowEvent, MISSING_VALUE, NA_VALUE};
use crate::render::{default_colorer, Palette};

use super::util;
use super::Def;
use crate::render::helpers::*;

const STATUS_OK: &str = "OK";
const DEGRADED: &str = "DEGRADED";

pub(super) struct Workload;

/// The k9s GVR string for an object's kind.
fn gvr_of(o: &DynamicObject) -> String {
    let (api, kind) = o
        .types
        .as_ref()
        .map(|t| (t.api_version.as_str(), t.kind.as_str()))
        .unwrap_or(("", ""));
    match kind {
        "Pod" => "v1/pods".into(),
        "Service" => "v1/services".into(),
        "DaemonSet" => "apps/v1/daemonsets".into(),
        "StatefulSet" => "apps/v1/statefulsets".into(),
        "Deployment" => "apps/v1/deployments".into(),
        "ReplicaSet" => "apps/v1/replicasets".into(),
        _ => format!("{api}/{}s", kind.to_lowercase()),
    }
}

/// dao isReady: `r/c` with c == 0 or r == c.
fn is_ready(s: &str) -> bool {
    let tt: Vec<&str> = s.split('/').collect();
    if tt.len() != 2 {
        return false;
    }
    let (Ok(r), Ok(c)) = (tt[0].parse::<i64>(), tt[1].parse::<i64>()) else {
        return false;
    };
    c == 0 || r == c
}

fn has_true_condition(d: &Value, kind: &str) -> bool {
    array_at(d, "status.conditions")
        .iter()
        .any(|c| str_at(c, "type") == kind && str_at(c, "status") == "True")
}

/// kubectl printPod: (STATUS reason, READY ready/total).
pub(super) fn pod_status(o: &DynamicObject) -> (String, String) {
    let d = &o.data;
    let init_specs = array_at(d, "spec.initContainers");
    let restartable = |name: &str| {
        init_specs
            .iter()
            .any(|c| str_at(c, "name") == name && str_at(c, "restartPolicy") == "Always")
    };
    let mut total = array_at(d, "spec.containers").len();
    total += init_specs
        .iter()
        .filter(|c| str_at(c, "restartPolicy") == "Always")
        .count();
    let mut ready = 0;
    let phase = str_at(d, "status.phase");
    let mut reason = phase.to_string();
    let status_reason = str_at(d, "status.reason");
    if !status_reason.is_empty() {
        reason = status_reason.to_string();
    }
    if array_at(d, "status.conditions")
        .iter()
        .any(|c| str_at(c, "type") == "PodScheduled" && str_at(c, "reason") == "SchedulingGated")
    {
        reason = "SchedulingGated".into();
    }

    let mut initializing = false;
    for (idx, c) in array_at(d, "status.initContainerStatuses")
        .iter()
        .enumerate()
    {
        let term = get_path(c, "state.terminated").filter(|v| !v.is_null());
        let wait = get_path(c, "state.waiting").filter(|v| !v.is_null());
        if let Some(t) = term {
            if i64_at(t, "exitCode") == 0 {
                continue;
            }
        }
        if restartable(str_at(c, "name")) && bool_at(c, "started") {
            if bool_at(c, "ready") {
                ready += 1;
            }
            continue;
        }
        if let Some(t) = term {
            let r = str_at(t, "reason");
            reason = if r.is_empty() {
                let sig = i64_at(t, "signal");
                if sig != 0 {
                    format!("Init:Signal:{sig}")
                } else {
                    format!("Init:ExitCode:{}", i64_at(t, "exitCode"))
                }
            } else {
                format!("Init:{r}")
            };
        } else if let Some(w) = wait.filter(|w| {
            let r = str_at(w, "reason");
            !r.is_empty() && r != "PodInitializing"
        }) {
            reason = format!("Init:{}", str_at(w, "reason"));
        } else {
            reason = format!("Init:{idx}/{}", init_specs.len());
        }
        initializing = true;
        break;
    }

    if !initializing || has_true_condition(d, "Initialized") {
        let mut has_running = false;
        for c in array_at(d, "status.containerStatuses").iter().rev() {
            let term = get_path(c, "state.terminated").filter(|v| !v.is_null());
            let wait_reason = util::opt_str_at(c, "state.waiting.reason").unwrap_or("");
            let running = get_path(c, "state.running")
                .filter(|v| !v.is_null())
                .is_some();
            if !wait_reason.is_empty() {
                reason = wait_reason.to_string();
            } else if let Some(t) = term {
                let r = str_at(t, "reason");
                if !r.is_empty() {
                    reason = r.to_string();
                } else {
                    let sig = i64_at(t, "signal");
                    reason = if sig != 0 {
                        format!("Signal:{sig}")
                    } else {
                        format!("ExitCode:{}", i64_at(t, "exitCode"))
                    };
                }
            } else if bool_at(c, "ready") && running {
                has_running = true;
                ready += 1;
            }
        }
        if reason == "Completed" && has_running {
            reason = if has_true_condition(d, "Ready") {
                "Running".into()
            } else {
                "NotReady".into()
            };
        }
    }

    if deletion_timestamp(o).is_some() && status_reason == "NodeLost" {
        reason = "Unknown".into();
    } else if deletion_timestamp(o).is_some() && phase != "Failed" && phase != "Succeeded" {
        reason = "Terminating".into();
    }
    (reason, format!("{ready}/{total}"))
}

/// dao readiness + status for one object.
fn status_ready(gvr: &str, o: &DynamicObject) -> (String, String) {
    let d = &o.data;
    match gvr {
        "v1/pods" => {
            let (st, rd) = pod_status(o);
            let status = if st == "Completed" {
                STATUS_OK
            } else if !is_ready(&rd) || st != "Running" {
                DEGRADED
            } else {
                STATUS_OK
            };
            (status.into(), rd)
        }
        "apps/v1/deployments" | "apps/v1/statefulsets" => {
            let rd = format!(
                "{}/{}",
                i64_at(d, "status.readyReplicas"),
                i64_at(d, "spec.replicas")
            );
            let status = if is_ready(&rd) { STATUS_OK } else { DEGRADED };
            (status.into(), rd)
        }
        "apps/v1/replicasets" | "apps/v1/daemonsets" => {
            let rd = if gvr == "apps/v1/replicasets" {
                format!(
                    "{}/{}",
                    i64_at(d, "status.readyReplicas"),
                    i64_at(d, "spec.replicas")
                )
            } else {
                format!(
                    "{}/{}",
                    i64_at(d, "status.numberReady"),
                    i64_at(d, "status.desiredNumberScheduled")
                )
            };
            let status = if is_ready(&rd) { STATUS_OK } else { DEGRADED };
            (status.into(), rd)
        }
        "v1/services" => (STATUS_OK.into(), String::new()),
        _ => (MISSING_VALUE.into(), NA_VALUE.into()),
    }
}

impl Def for Workload {
    // k9s ignores views.yaml columns for this view.
    fn custom_cols(&self) -> bool {
        false
    }

    fn cols(&self) -> Vec<HeaderColumn> {
        vec![
            HeaderColumn::new("KIND"),
            HeaderColumn::new("NAMESPACE"),
            HeaderColumn::new("NAME"),
            HeaderColumn::new("STATUS"),
            HeaderColumn::new("READY"),
            HeaderColumn::new("VALID").wide(),
            HeaderColumn::new("AGE").time(),
        ]
    }

    fn row(&self, o: &DynamicObject) -> Result<(String, Vec<String>)> {
        let gvr = gvr_of(o);
        let ns = obj_ns(o).to_string();
        let name = obj_name(o).to_string();
        let (status, ready) = status_ready(&gvr, o);
        let valid = if status == DEGRADED {
            status.clone()
        } else {
            String::new()
        };
        Ok((
            format!("{gvr}|{ns}|{name}"),
            vec![gvr, ns, name, status, ready, valid, obj_age(o)],
        ))
    }

    fn color(&self, ns: &str, h: &Header, re: &RowEvent, p: &Palette) -> Color {
        let c = default_colorer(ns, h, re, p);
        match h.index_of("STATUS").and_then(|i| re.row.fields.get(i)) {
            Some(st) if st.trim() == DEGRADED => p.pending,
            _ => c,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::testutil::*;
    use super::*;
    use crate::model::ResEvent;

    #[test]
    fn header() {
        assert_eq!(
            cols(&renderer("workloads").header("all")),
            vec![
                ("KIND".into(), false, false, false),
                ("NAMESPACE".into(), false, false, false),
                ("NAME".into(), false, false, false),
                ("STATUS".into(), false, false, false),
                ("READY".into(), false, false, false),
                ("VALID".into(), true, false, false),
                ("AGE".into(), false, true, false),
            ]
        );
        assert_eq!(
            names(&renderer("workloads").header("default")),
            sv(&["KIND", "NAME", "STATUS", "READY", "VALID", "AGE"])
        );
    }

    #[test]
    fn deployments_and_friends() {
        let o = load("dp");
        let r = render("workloads", &o);
        assert_eq!(r.id, "apps/v1/deployments|icx|icx-db");
        assert_eq!(
            r.fields,
            sv(&[
                "apps/v1/deployments",
                "icx",
                "icx-db",
                "OK",
                "1/1",
                "",
                &age(&o)
            ])
        );
        let r = render("workloads", &load("sts"));
        assert_eq!(
            r.fields[..5],
            sv(&["apps/v1/statefulsets", "default", "nginx-sts", "OK", "4/4"])
        );
        let r = render("workloads", &load("ds"));
        assert_eq!(r.fields[3..5], sv(&["OK", "2/2"]));
        let r = render("workloads", &load("rs"));
        assert_eq!(r.fields[3..5], sv(&["OK", "1/1"]));
        let r = render("workloads", &load("svc"));
        assert_eq!(
            r.fields[..5],
            sv(&["v1/services", "default", "dictionary1", "OK", ""])
        );
        let r = render("workloads", &load("cm"));
        assert_eq!(
            r.fields[..5],
            sv(&["v1/configmaps", "default", "blee", "<none>", "n/a"])
        );

        let o = obj(json!({
            "apiVersion": "apps/v1", "kind": "Deployment",
            "metadata": {"name": "d", "namespace": "ns"},
            "spec": {"replicas": 3}, "status": {"readyReplicas": 1}
        }));
        let r = render("workloads", &o);
        assert_eq!(r.fields[3..6], sv(&["DEGRADED", "1/3", "DEGRADED"]));
        let o = obj(json!({
            "apiVersion": "apps/v1", "kind": "Deployment",
            "metadata": {"name": "d", "namespace": "ns"},
            "spec": {"replicas": 0}
        }));
        assert_eq!(render("workloads", &o).fields[3..5], sv(&["OK", "0/0"]));
    }

    #[test]
    fn pods() {
        let o = load("po");
        let (st, rd) = pod_status(&o);
        let r = render("workloads", &o);
        assert_eq!(r.fields[0], "v1/pods");
        assert_eq!(r.fields[4], rd);
        let ok = st == "Completed" || (st == "Running" && is_ready(&rd));
        assert_eq!(r.fields[3], if ok { "OK" } else { "DEGRADED" });
    }

    fn pod(status: Value, extra_spec: Value) -> DynamicObject {
        let mut spec = json!({"containers": [{"name": "c1"}, {"name": "c2"}]});
        if let (Some(m), Some(e)) = (spec.as_object_mut(), extra_spec.as_object()) {
            for (k, v) in e {
                m.insert(k.clone(), v.clone());
            }
        }
        obj(json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": {"name": "p", "namespace": "ns"},
            "spec": spec, "status": status
        }))
    }

    #[test]
    fn pod_statuses() {
        let running = json!({"running": {}});
        let o = pod(
            json!({"phase": "Running", "containerStatuses": [
                {"name": "c1", "ready": true, "state": running},
                {"name": "c2", "ready": true, "state": running}
            ]}),
            json!({}),
        );
        assert_eq!(pod_status(&o), ("Running".into(), "2/2".into()));
        assert_eq!(render("workloads", &o).fields[3], "OK");

        let o = pod(
            json!({"phase": "Running", "containerStatuses": [
                {"name": "c1", "ready": false, "state": {"waiting": {"reason": "CrashLoopBackOff"}}},
                {"name": "c2", "ready": true, "state": running}
            ]}),
            json!({}),
        );
        assert_eq!(pod_status(&o), ("CrashLoopBackOff".into(), "1/2".into()));
        assert_eq!(
            render("workloads", &o).fields[3..6],
            sv(&["DEGRADED", "1/2", "DEGRADED"])
        );

        let o = pod(
            json!({"phase": "Succeeded", "containerStatuses": [
                {"name": "c1", "state": {"terminated": {"reason": "Completed", "exitCode": 0}}},
                {"name": "c2", "state": {"terminated": {"reason": "Completed", "exitCode": 0}}}
            ]}),
            json!({}),
        );
        assert_eq!(pod_status(&o), ("Completed".into(), "0/2".into()));
        assert_eq!(render("workloads", &o).fields[3], "OK");

        let o = pod(
            json!({"phase": "Running", "containerStatuses": [
                {"name": "c1", "state": {"terminated": {"exitCode": 2}}},
                {"name": "c2", "state": {"terminated": {"signal": 9, "exitCode": 137}}}
            ]}),
            json!({}),
        );
        assert_eq!(pod_status(&o).0, "ExitCode:2");

        let o = pod(
            json!({"phase": "Running", "conditions": [{"type": "Ready", "status": "True"}],
                "containerStatuses": [
                {"name": "c1", "ready": true, "state": running},
                {"name": "c2", "state": {"terminated": {"reason": "Completed", "exitCode": 0}}}
            ]}),
            json!({}),
        );
        assert_eq!(pod_status(&o), ("Running".into(), "1/2".into()));

        let o = pod(json!({"phase": "Pending", "reason": "Evicted"}), json!({}));
        assert_eq!(pod_status(&o).0, "Evicted");

        let o = pod(
            json!({"phase": "Pending", "conditions": [{"type": "PodScheduled", "reason": "SchedulingGated"}]}),
            json!({}),
        );
        assert_eq!(pod_status(&o).0, "SchedulingGated");
    }

    #[test]
    fn pod_init_statuses() {
        let init = json!({"initContainers": [{"name": "i1"}, {"name": "i2"}]});
        let o = pod(
            json!({"phase": "Pending", "initContainerStatuses": [
                {"name": "i1", "state": {"terminated": {"exitCode": 0}}},
                {"name": "i2", "state": {"running": {}}}
            ]}),
            init.clone(),
        );
        assert_eq!(pod_status(&o), ("Init:1/2".into(), "0/2".into()));
        let o = pod(
            json!({"phase": "Pending", "initContainerStatuses": [
                {"name": "i1", "state": {"waiting": {"reason": "ImagePullBackOff"}}}
            ]}),
            init.clone(),
        );
        assert_eq!(pod_status(&o).0, "Init:ImagePullBackOff");
        let o = pod(
            json!({"phase": "Pending", "initContainerStatuses": [
                {"name": "i1", "state": {"terminated": {"exitCode": 3}}}
            ]}),
            init.clone(),
        );
        assert_eq!(pod_status(&o).0, "Init:ExitCode:3");
        let o = pod(
            json!({"phase": "Pending", "initContainerStatuses": [
                {"name": "i1", "state": {"terminated": {"exitCode": 1, "signal": 9}}}
            ]}),
            init.clone(),
        );
        assert_eq!(pod_status(&o).0, "Init:Signal:9");
        let o = pod(
            json!({"phase": "Pending", "initContainerStatuses": [
                {"name": "i1", "state": {"terminated": {"exitCode": 1, "reason": "Error"}}}
            ]}),
            init,
        );
        assert_eq!(pod_status(&o).0, "Init:Error");

        // Sidecar (restartable init container) counts toward totals.
        let o = pod(
            json!({"phase": "Running", "initContainerStatuses": [
                {"name": "side", "started": true, "ready": true, "state": {"running": {}}}
            ], "containerStatuses": [
                {"name": "c1", "ready": true, "state": {"running": {}}},
                {"name": "c2", "ready": true, "state": {"running": {}}}
            ]}),
            json!({"initContainers": [{"name": "side", "restartPolicy": "Always"}]}),
        );
        assert_eq!(pod_status(&o), ("Running".into(), "3/3".into()));
    }

    #[test]
    fn pod_terminating() {
        let mut o = pod(json!({"phase": "Running"}), json!({}));
        o.metadata.deletion_timestamp = o
            .metadata
            .creation_timestamp
            .clone()
            .or_else(|| serde_json::from_value(json!("2020-01-01T00:00:00Z")).ok());
        assert_eq!(pod_status(&o).0, "Terminating");
        let mut o = pod(json!({"phase": "Running", "reason": "NodeLost"}), json!({}));
        o.metadata.deletion_timestamp = serde_json::from_value(json!("2020-01-01T00:00:00Z")).ok();
        assert_eq!(pod_status(&o).0, "Unknown");
        let mut o = pod(json!({"phase": "Succeeded"}), json!({}));
        o.metadata.deletion_timestamp = serde_json::from_value(json!("2020-01-01T00:00:00Z")).ok();
        assert_eq!(pod_status(&o).0, "Succeeded");
    }

    #[test]
    fn readiness() {
        assert!(is_ready("1/1"));
        assert!(is_ready("0/0"));
        assert!(is_ready("3/0"));
        assert!(!is_ready("1/2"));
        assert!(!is_ready("x/1"));
        assert!(!is_ready(""));
    }

    #[test]
    fn colorer() {
        let r = renderer("workloads");
        let p = Palette::default();
        let row = ["v1/pods", "ns", "p", "DEGRADED", "0/1", "DEGRADED", "1d"];
        assert_eq!(color(&*r, "all", ResEvent::Unchanged, &row), p.pending);
        let row = ["v1/pods", "ns", "p", "OK", "1/1", "", "1d"];
        assert_eq!(color(&*r, "all", ResEvent::Unchanged, &row), p.std);
        assert_eq!(color(&*r, "all", ResEvent::Add, &row), p.add);
    }
}
