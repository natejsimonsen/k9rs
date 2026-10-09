//! Pod renderer (k9s render/pod.go).

use anyhow::Result;
use chrono::{DateTime, Utc};
use ratatui::style::Color;
use serde_json::{json, Value};

use super::container::container_requests;
use super::field_of;
use crate::config::ViewSetting;
use crate::model::{Align, Header, HeaderColumn, Row, RowEvent};
use crate::render::helpers::*;
use crate::render::{default_colorer, Palette, RenderInput, Renderer};

pub const PHASE_TERMINATING: &str = "Terminating";
pub const PHASE_INITIALIZED: &str = "Initialized";
pub const PHASE_RUNNING: &str = "Running";
pub const PHASE_NOT_READY: &str = "NoReady";
pub const PHASE_COMPLETED: &str = "Completed";
pub const PHASE_CONTAINER_CREATING: &str = "ContainerCreating";
pub const PHASE_POD_INITIALIZING: &str = "PodInitializing";
pub const PHASE_UNKNOWN: &str = "Unknown";
pub const PHASE_PENDING: &str = "Pending";
/// k8s NodeUnreachablePodReason.
pub const NODE_UNREACHABLE_POD_REASON: &str = "NodeLost";

pub(crate) fn header() -> Header {
    let r = |n: &str| HeaderColumn::new(n).align(Align::Right);
    Header::new(vec![
        HeaderColumn::new("NAMESPACE"),
        HeaderColumn::new("NAME"),
        HeaderColumn::new("VS").vs(),
        HeaderColumn::new("PF"),
        HeaderColumn::new("READY"),
        HeaderColumn::new("STATUS"),
        r("RESTARTS"),
        r("LAST RESTART").time().wide(),
        r("CPU").mx(),
        r("CPU/RL").wide(),
        r("%CPU/R").mx(),
        r("%CPU/L").mx(),
        r("MEM").mx(),
        r("MEM/RL").wide(),
        r("%MEM/R").mx(),
        r("%MEM/L").mx(),
        r("GPU/RL").wide(),
        HeaderColumn::new("IP"),
        HeaderColumn::new("NODE"),
        HeaderColumn::new("SERVICE-ACCOUNT").wide(),
        HeaderColumn::new("NOMINATED NODE").wide(),
        HeaderColumn::new("READINESS GATES").wide(),
        HeaderColumn::new("QOS").wide(),
        HeaderColumn::new("LABELS").wide(),
        HeaderColumn::new("VALID").wide(),
        HeaderColumn::new("AGE").time(),
    ])
}

/// k9s render.Pod.
#[derive(Default)]
pub struct Pod {
    base: ViewBase,
}

impl Renderer for Pod {
    fn header(&self, ns: &str) -> Header {
        self.base.header(&header(), ns)
    }

    fn render(&self, input: &RenderInput<'_>, ns: &str, row: &mut Row) -> Result<()> {
        let o = input.obj;
        let empty = json!({});
        let spec = o.data.get("spec").unwrap_or(&empty);
        let st = o.data.get("status").unwrap_or(&empty);
        let dt = deletion_timestamp(o);

        let cs = array_at(st, "containerStatuses");
        let (mut c_ready, _, c_restarts, last_restart) = container_stats(cs);
        let (i_ready, i_terminated, i_restarts) = init_container_stats(
            array_at(spec, "initContainers"),
            array_at(st, "initContainerStatuses"),
        );
        c_ready += i_ready;
        let all_counts = array_at(spec, "containers").len() + i_terminated;
        let (rgr, rgt) = readiness_gate_stats(spec, st);
        let ready = has_pod_ready_condition(st);

        let (pns, name) = (obj_ns(o), obj_name(o));
        let fqn = crate::k8s::fqn(pns, name);
        let usage = input
            .metrics
            .and_then(|m| m.pods.get(&fqn))
            .map(|m| m.total())
            .unwrap_or_default();
        let r = gather_pod_requests(spec);
        let (ccpu, cmem) = (usage.cpu_millis, usage.mem_bytes);
        let phase = phase(dt.is_some(), spec, st);

        row.id = fqn;
        row.fields = vec![
            pns.to_string(),
            name.to_string(),
            NA_VALUE.to_string(),
            "●".to_string(),
            format!("{c_ready}/{all_counts}"),
            phase.clone(),
            (c_restarts + i_restarts).to_string(),
            to_age_opt(last_restart),
            to_mc(ccpu),
            format!("{}:{}", to_mc(r.cpu), to_mc(r.lcpu)),
            to_percentage_str(ccpu, r.cpu),
            to_percentage_str(ccpu, r.lcpu),
            to_mi(cmem),
            format!("{}:{}", to_mi(r.mem), to_mi(r.lmem)),
            to_percentage_str(cmem, r.mem),
            to_percentage_str(cmem, r.lmem),
            format!("{}:{}", to_mc(r.gpu), to_mc(r.lgpu)),
            na(str_at(st, "podIP")),
            na(str_at(spec, "nodeName")),
            na(str_at(spec, "serviceAccountName")),
            missing(str_at(st, "nominatedNodeName")),
            as_readiness_gate(spec, st),
            map_qos(str_at(st, "qosClass")).to_string(),
            labels_str(o),
            as_status(diagnose(&phase, c_ready, all_counts, ready, rgr, rgt)),
            obj_age(o),
        ];
        let raw = obj_to_value(o);
        self.base.finish(Some(&raw), &header(), ns, row)
    }

    fn color_for(&self, ns: &str, h: &Header, re: &RowEvent, p: &Palette) -> Color {
        pod_colorer(ns, h, re, p)
    }

    fn set_view_setting(&mut self, vs: Option<ViewSetting>) {
        self.base.set_view_setting(vs.as_ref());
    }
}

/// k9s Pod ColorerFunc.
pub(crate) fn pod_colorer(ns: &str, h: &Header, re: &RowEvent, p: &Palette) -> Color {
    let mut c = default_colorer(ns, h, re, p);
    let Some(status) = field_of(h, re, "STATUS") else {
        return c;
    };
    match status {
        PHASE_PENDING | PHASE_CONTAINER_CREATING => c = p.pending,
        PHASE_POD_INITIALIZING => c = p.add,
        PHASE_INITIALIZED => c = p.highlight,
        PHASE_COMPLETED => c = p.completed,
        PHASE_RUNNING => {
            if c != p.err {
                c = p.std;
            }
        }
        PHASE_TERMINATING => c = p.kill,
        _ => {}
    }
    c
}

fn diagnose(
    phase: &str,
    cr: usize,
    ct: usize,
    ready: bool,
    rgr: usize,
    rgt: usize,
) -> Result<(), String> {
    if phase == PHASE_COMPLETED {
        return Ok(());
    }
    if cr != ct || ct == 0 {
        return Err(format!("container ready check failed: {cr} of {ct}"));
    }
    if rgt > 0 && rgr != rgt {
        return Err(format!("readiness gate check failed: {rgr} of {rgt}"));
    }
    if !ready {
        return Err("pod condition ready is false".into());
    }
    if phase == PHASE_TERMINATING {
        return Err("pod is terminating".into());
    }
    Ok(())
}

fn as_readiness_gate(spec: &Value, st: &Value) -> String {
    let gates = array_at(spec, "readinessGates");
    if gates.is_empty() {
        return MISSING_VALUE.into();
    }
    let conds = array_at(st, "conditions");
    let mut true_conditions = 0;
    for g in gates {
        let ct = str_at(g, "conditionType");
        if let Some(c) = conds.iter().find(|c| str_at(c, "type") == ct) {
            if str_at(c, "status") == "True" {
                true_conditions += 1;
            }
        }
    }
    format!("{true_conditions}/{}", gates.len())
}

fn map_qos(class: &str) -> &'static str {
    match class {
        "Guaranteed" => "GA",
        "Burstable" => "BU",
        _ => "BE",
    }
}

/// Container requests/limits of a pod (k9s `metric`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PodRes {
    pub cpu: i64,
    pub mem: i64,
    pub gpu: i64,
    pub lcpu: i64,
    pub lmem: i64,
    pub lgpu: i64,
}

/// k9s gatherPodMX (requests/limits side): sidecar init containers plus
/// regular containers.
pub(crate) fn gather_pod_requests(spec: &Value) -> PodRes {
    let mut r = PodRes::default();
    let sidecars = array_at(spec, "initContainers")
        .iter()
        .filter(|c| is_sidecar(c));
    for co in sidecars.chain(array_at(spec, "containers").iter()) {
        let rl = container_requests(co);
        r.cpu += res_milli(rl, "cpu");
        r.mem += res_value(rl, "memory");
        r.gpu += extract_gpu(rl);
        let lim = get_path(co, "resources.limits")
            .filter(|l| l.as_object().is_some_and(|m| !m.is_empty()));
        r.lcpu += res_milli(lim, "cpu");
        r.lmem += res_value(lim, "memory");
        r.lgpu += extract_gpu(lim);
    }
    r
}

/// k9s isSideCarContainer: restartPolicy == Always.
pub(crate) fn is_sidecar(co: &Value) -> bool {
    str_at(co, "restartPolicy") == "Always"
}

fn has_pod_ready_condition(st: &Value) -> bool {
    array_at(st, "conditions")
        .iter()
        .any(|c| str_at(c, "type") == "Ready" && str_at(c, "status") == "True")
}

/// k9s ContainerStats: (ready, terminated, restarts, latest last-termination time).
pub(crate) fn container_stats(cc: &[Value]) -> (usize, usize, i64, Option<DateTime<Utc>>) {
    let (mut ready, mut terminated, mut restarts) = (0, 0, 0);
    let mut latest: Option<DateTime<Utc>> = None;
    for c in cc {
        if get_path(c, "state.terminated").is_some_and(|v| !v.is_null()) {
            terminated += 1;
        }
        if bool_at(c, "ready") {
            ready += 1;
        }
        restarts += i64_at(c, "restartCount");
        if let Some(t) = get_path(c, "lastState.terminated").filter(|v| !v.is_null()) {
            if let Some(ts) = value_time(t.get("finishedAt")) {
                if latest.is_none_or(|l| ts > l) {
                    latest = Some(ts);
                }
            }
        }
    }
    (ready, terminated, restarts, latest)
}

fn init_container_stats(cc: &[Value], cos: &[Value]) -> (usize, usize, i64) {
    let (mut ready, mut total, mut restart) = (0, 0, 0);
    for (i, cs) in cos.iter().enumerate() {
        if !cc.get(i).is_some_and(is_sidecar) {
            continue;
        }
        total += 1;
        if bool_at(cs, "ready") {
            ready += 1;
        }
        restart += i64_at(cs, "restartCount");
    }
    (ready, total, restart)
}

fn readiness_gate_stats(spec: &Value, st: &Value) -> (usize, usize) {
    let gates = array_at(spec, "readinessGates");
    let conds = array_at(st, "conditions");
    let mut ready = 0;
    for g in gates {
        let ct = str_at(g, "conditionType");
        ready += conds
            .iter()
            .filter(|c| str_at(c, "type") == ct && str_at(c, "status") == "True")
            .count();
    }
    (ready, gates.len())
}

fn state_obj<'a>(cs: &'a Value, which: &str) -> Option<&'a Value> {
    get_path(cs, "state")
        .and_then(|s| s.get(which))
        .filter(|v| !v.is_null())
}

/// k9s Pod.Phase: the STATUS column for a pod given whether it is being
/// deleted, its spec and its status JSON.
pub(crate) fn phase(deleting: bool, spec: &Value, st: &Value) -> String {
    let mut status = str_at(st, "phase").to_string();
    let reason = str_at(st, "reason");
    if !reason.is_empty() {
        if deleting && reason == NODE_UNREACHABLE_POD_REASON {
            return "Unknown".into();
        }
        status = reason.to_string();
    }
    if let Some(s) = init_container_phase(spec, st) {
        return s;
    }
    let (s, running) = container_phase(st, status);
    status = s;
    if running && status == PHASE_COMPLETED {
        status = PHASE_RUNNING.into();
    }
    if !deleting {
        return status;
    }
    PHASE_TERMINATING.into()
}

/// k9s Pod.Phase on a pod object.
pub fn pod_phase(o: &kube::api::DynamicObject) -> String {
    let empty = json!({});
    phase(
        deletion_timestamp(o).is_some(),
        o.data.get("spec").unwrap_or(&empty),
        o.data.get("status").unwrap_or(&empty),
    )
}

fn container_phase(st: &Value, mut status: String) -> (String, bool) {
    let mut running = false;
    for cs in array_at(st, "containerStatuses").iter().rev() {
        let waiting = state_obj(cs, "waiting");
        let term = state_obj(cs, "terminated");
        if let Some(r) = waiting
            .map(|w| str_at(w, "reason"))
            .filter(|r| !r.is_empty())
        {
            status = r.to_string();
        } else if let Some(r) = term.map(|t| str_at(t, "reason")).filter(|r| !r.is_empty()) {
            status = r.to_string();
        } else if let Some(t) = term {
            let sig = i64_at(t, "signal");
            status = if sig != 0 {
                format!("Signal:{sig}")
            } else {
                format!("ExitCode:{}", i64_at(t, "exitCode"))
            };
        } else if bool_at(cs, "ready") && state_obj(cs, "running").is_some() {
            running = true;
        }
    }
    (status, running)
}

fn init_container_phase(spec: &Value, st: &Value) -> Option<String> {
    let ics = array_at(spec, "initContainers");
    let count = ics.len();
    let sidecars: Vec<&str> = ics
        .iter()
        .filter(|c| is_sidecar(c))
        .map(|c| str_at(c, "name"))
        .collect();
    for (i, cs) in array_at(st, "initContainerStatuses").iter().enumerate() {
        let restartable = sidecars.contains(&str_at(cs, "name"));
        let s = check_init_container_status(cs, i, count, restartable);
        if !s.is_empty() {
            return Some(s);
        }
    }
    None
}

/// k9s checkInitContainerStatus.
pub(crate) fn check_init_container_status(
    cs: &Value,
    count: usize,
    init_count: usize,
    restartable: bool,
) -> String {
    if let Some(t) = state_obj(cs, "terminated") {
        let code = i64_at(t, "exitCode");
        if code == 0 {
            return String::new();
        }
        let reason = str_at(t, "reason");
        if !reason.is_empty() {
            return format!("Init:{reason}");
        }
        let sig = i64_at(t, "signal");
        if sig != 0 {
            return format!("Init:Signal:{sig}");
        }
        return format!("Init:ExitCode:{code}");
    }
    if restartable && bool_at(cs, "started") {
        if bool_at(cs, "ready") {
            return String::new();
        }
    } else if let Some(r) = state_obj(cs, "waiting")
        .map(|w| str_at(w, "reason"))
        .filter(|r| !r.is_empty() && *r != PHASE_POD_INITIALIZING)
    {
        return format!("Init:{r}");
    }
    format!("Init:{count}/{init_count}")
}

/// k9s render.PodStatus (kubectl-style pod status) on a pod object.
pub fn pod_status(o: &kube::api::DynamicObject) -> String {
    let empty = json!({});
    let spec = o.data.get("spec").unwrap_or(&empty);
    let st = o.data.get("status").unwrap_or(&empty);
    pod_status_of(deletion_timestamp(o).is_some(), spec, st)
}

pub(crate) fn pod_status_of(deleting: bool, spec: &Value, st: &Value) -> String {
    let mut reason = str_at(st, "phase").to_string();
    if !str_at(st, "reason").is_empty() {
        reason = str_at(st, "reason").to_string();
    }
    for c in array_at(st, "conditions") {
        if str_at(c, "type") == "PodScheduled" && str_at(c, "reason") == "SchedulingGated" {
            reason = "SchedulingGated".into();
        }
    }
    let mut initializing = false;
    let n_init = array_at(spec, "initContainers").len();
    for (i, c) in array_at(st, "initContainerStatuses").iter().enumerate() {
        let term = state_obj(c, "terminated");
        let waiting = state_obj(c, "waiting");
        if term.is_some_and(|t| i64_at(t, "exitCode") == 0) {
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
        } else if let Some(r) = waiting
            .map(|w| str_at(w, "reason"))
            .filter(|r| !r.is_empty() && *r != PHASE_POD_INITIALIZING)
        {
            reason = format!("Init:{r}");
        } else {
            reason = format!("Init:{i}/{n_init}");
        }
        initializing = true;
        break;
    }
    if !initializing {
        let mut has_running = false;
        for c in array_at(st, "containerStatuses").iter().rev() {
            let waiting = state_obj(c, "waiting");
            let term = state_obj(c, "terminated");
            if let Some(r) = waiting
                .map(|w| str_at(w, "reason"))
                .filter(|r| !r.is_empty())
            {
                reason = r.to_string();
            } else if let Some(r) = term.map(|t| str_at(t, "reason")).filter(|r| !r.is_empty()) {
                reason = r.to_string();
            } else if let Some(t) = term {
                let sig = i64_at(t, "signal");
                reason = if sig != 0 {
                    format!("Signal:{sig}")
                } else {
                    format!("ExitCode:{}", i64_at(t, "exitCode"))
                };
            } else if bool_at(c, "ready") && state_obj(c, "running").is_some() {
                has_running = true;
            }
        }
        if reason == PHASE_COMPLETED && has_running {
            reason = if has_pod_ready_condition(st) {
                PHASE_RUNNING.into()
            } else {
                PHASE_NOT_READY.into()
            };
        }
    }
    if deleting && str_at(st, "reason") == NODE_UNREACHABLE_POD_REASON {
        reason = PHASE_UNKNOWN.into();
    } else if deleting {
        reason = PHASE_TERMINATING.into();
    }
    reason
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::k8s::{Metrics, MetricsSnapshot, PodMetrics};
    use crate::model::ResEvent;
    use crate::render::core::testutil::*;

    fn mx(fqn: &str, co: &str, cpu: i64, mem_mi: i64) -> MetricsSnapshot {
        let mut m = MetricsSnapshot::default();
        let mut pm = PodMetrics::default();
        pm.containers.insert(
            co.into(),
            Metrics {
                cpu_millis: cpu,
                mem_bytes: mem_mi * MEGA_BYTE,
            },
        );
        m.pods.insert(fqn.into(), pm);
        m
    }

    fn render(fixture: &str, m: &MetricsSnapshot, ns: &str) -> Row {
        let o = load(fixture);
        let mut row = Row::default();
        Pod::default()
            .render(&RenderInput::with_metrics(&o, m), ns, &mut row)
            .unwrap();
        row
    }

    #[test]
    fn pod_render() {
        let r = render("po", &mx("default/nginx", "nginx", 100, 50), "");
        assert_eq!(r.id, "default/nginx");
        assert_eq!(
            r.fields[..21],
            [
                "default",
                "nginx",
                "n/a",
                "●",
                "1/1",
                "Running",
                "0",
                "<unknown>",
                "100",
                "100:0",
                "100",
                "n/a",
                "50",
                "70:170",
                "71",
                "29",
                "0:0",
                "172.17.0.6",
                "minikube",
                "default",
                "<none>"
            ]
        );
        assert_eq!(r.fields.len(), 26);
        assert_eq!(r.fields.len(), Pod::default().header("").len());
    }

    #[test]
    fn pod_init_render() {
        let r = render("po_init", &mx("default/nginx", "nginx", 10, 10), "");
        assert_eq!(
            r.fields[..21],
            [
                "default",
                "nginx",
                "n/a",
                "●",
                "1/1",
                "Init:0/1",
                "0",
                "<unknown>",
                "10",
                "100:0",
                "10",
                "n/a",
                "10",
                "70:170",
                "14",
                "5",
                "0:0",
                "172.17.0.6",
                "minikube",
                "default",
                "<none>"
            ]
        );
    }

    #[test]
    fn pod_sidecar_render() {
        let r = render("po_sidecar", &mx("default/sleep", "sleep", 100, 40), "");
        assert_eq!(r.id, "default/sleep");
        assert_eq!(
            r.fields[..21],
            [
                "default",
                "sleep",
                "n/a",
                "●",
                "2/2",
                "Running",
                "0",
                "<unknown>",
                "100",
                "50:250",
                "200",
                "40",
                "40",
                "50:80",
                "80",
                "50",
                "0:0",
                "10.244.0.8",
                "kind-control-plane",
                "default",
                "<none>"
            ]
        );
    }

    #[test]
    fn pod_namespace_column_and_age() {
        let o = load("p1");
        let mut row = Row::default();
        let p = Pod::default();
        crate::render::helpers::with_now(t0(), || {
            p.render(&RenderInput::new(&o), "default", &mut row)
                .unwrap()
        });
        assert_eq!(p.header("default").len(), 25);
        assert_eq!(row.fields.len(), 25);
        assert_eq!(row.fields[0], "nginx-7fb78fb6d8-2w75j");
        let h = p.header("default");
        let age = h.index_of("AGE").unwrap();
        assert_eq!(row.fields[age], "4h32m");
        // no metrics: zero usage
        assert_eq!(row.fields[h.index_of("CPU").unwrap()], "0");
        let lbl = h.index_of("LABELS").unwrap();
        assert!(!row.fields[lbl].is_empty());
    }

    #[test]
    fn pod_custom_columns() {
        let o = load("po");
        let mut p = Pod::default();
        p.set_view_setting(Some(ViewSetting {
            columns: vec![
                "NAME".into(),
                "IMG:.spec.containers[0].image".into(),
                "STATUS".into(),
            ],
            sort_column: String::new(),
        }));
        let h = p.header("default");
        assert_eq!(names(&h)[..3], ["NAME", "IMG", "STATUS"]);
        let mut row = Row::default();
        p.render(&RenderInput::new(&o), "default", &mut row)
            .unwrap();
        assert_eq!(row.fields.len(), h.len());
        assert_eq!(row.fields[..3], ["nginx", "nginx:alpine", "Running"]);
    }

    #[test]
    fn pod_header_attrs() {
        let h = header();
        assert_eq!(h.len(), 26);
        let c = &h.0[h.index_of("LAST RESTART").unwrap()];
        assert!(c.time && c.wide && c.align == Align::Right);
        assert!(h.0[h.index_of("CPU").unwrap()].mx);
        assert!(!h.0[h.index_of("CPU/RL").unwrap()].mx);
        assert!(h.0[h.index_of("VS").unwrap()].vs);
        assert!(h.0[h.index_of("AGE").unwrap()].time);
    }

    #[test]
    fn colorer() {
        let h = Header::new(
            ["NAMESPACE", "NAME", "READY", "RESTARTS", "STATUS", "VALID"]
                .iter()
                .map(|n| HeaderColumn::new(*n))
                .collect(),
        );
        let p = palette();
        let re = |kind, status: &str, valid: &str| {
            RowEvent::new(
                kind,
                Row::with(
                    "",
                    vec![
                        "blee".into(),
                        "fred".into(),
                        "1/1".into(),
                        "0".into(),
                        status.into(),
                        valid.into(),
                    ],
                ),
            )
        };
        let pod = Pod::default();
        let cases = [
            (re(ResEvent::Add, "Running", ""), p.std),
            (re(ResEvent::Add, "PodInitializing", ""), p.add),
            (re(ResEvent::Add, "PodInitializing", "blah"), p.add),
            (re(ResEvent::Add, "Initialized", "blah"), p.highlight),
            (re(ResEvent::Add, "Completed", "blah"), p.completed),
            (re(ResEvent::Add, "Terminating", "blah"), p.kill),
            (re(ResEvent::Add, "Running", "blah"), p.err),
            (re(ResEvent::Add, "blee", ""), p.add),
            (re(ResEvent::Add, "blee", "doh"), p.err),
            (re(ResEvent::Add, "Pending", ""), p.pending),
            (re(ResEvent::Add, "ContainerCreating", ""), p.pending),
        ];
        for (i, (e, c)) in cases.iter().enumerate() {
            assert_eq!(pod.color_for("", &h, e, &p), *c, "case {i}");
        }
        let h3 = Header::new(h.0[..3].to_vec());
        assert_eq!(
            pod.color_for("", &h3, &re(ResEvent::Delete, "blee", ""), &p),
            p.kill
        );
    }

    fn st(v: Value) -> String {
        pod_status_of(false, &json!({}), &v)
    }

    #[test]
    fn check_pod_status() {
        assert_eq!(st(json!({"phase": "Unknown"})), "Unknown");
        assert_eq!(
            st(json!({"phase": "Running", "initContainerStatuses": [],
                "containerStatuses": [{"name": "c1", "state": {"running": {}}}]})),
            "Running"
        );
        assert_eq!(
            st(json!({"phase": "Running",
                "conditions": [{"type": "PodScheduled", "reason": "SchedulingGated"}],
                "containerStatuses": [{"name": "c1", "state": {"running": {}}}]})),
            "SchedulingGated"
        );
        assert_eq!(
            st(json!({"phase": "Running",
                "containerStatuses": [{"name": "c1", "state": {"waiting": {"reason": "ImagePullBackOff"}}}]})),
            "ImagePullBackOff"
        );
        let co_backoff =
            json!([{"name": "c1", "state": {"waiting": {"reason": "ImagePullBackOff"}}}]);
        assert_eq!(
            st(json!({"phase": "Running",
                "initContainerStatuses": [{"name": "ic1", "state": {"waiting": {"reason": "ImagePullBackOff"}}}],
                "containerStatuses": co_backoff})),
            "Init:ImagePullBackOff"
        );
        assert_eq!(
            st(
                json!({"phase": "Running", "initContainerStatuses": [{"name": "ic1", "state": {}}],
                "containerStatuses": co_backoff})
            ),
            "Init:0/0"
        );
        assert_eq!(
            st(json!({"phase": "Running",
                "initContainerStatuses": [{"name": "ic1", "state": {"terminated": {"exitCode": 1, "reason": "blah"}}}],
                "containerStatuses": co_backoff})),
            "Init:blah"
        );
        assert_eq!(
            st(json!({"phase": "Running",
                "initContainerStatuses": [{"name": "ic1", "state": {"terminated": {"exitCode": 2, "signal": 9}}}],
                "containerStatuses": co_backoff})),
            "Init:Signal:9"
        );
        assert_eq!(
            st(json!({"phase": "Running",
                "initContainerStatuses": [{"name": "ic1", "state": {"terminated": {"exitCode": 2}}}],
                "containerStatuses": co_backoff})),
            "Init:ExitCode:2"
        );
        assert_eq!(
            st(json!({"phase": "Running",
                "containerStatuses": [{"name": "c1", "state": {"terminated": {"reason": "blah"}}}]})),
            "blah"
        );
        assert_eq!(
            st(json!({"phase": "Running",
                "containerStatuses": [{"name": "c1", "ready": true, "state": {"running": {}}}]})),
            "Running"
        );
        assert_eq!(
            st(
                json!({"phase": "Completed", "conditions": [{"type": "Ready", "status": "True"}],
                "containerStatuses": [{"name": "c1", "ready": true, "state": {"running": {}}}]})
            ),
            "Running"
        );
        assert_eq!(
            st(json!({"phase": "Completed",
                "containerStatuses": [{"name": "c1", "ready": true, "state": {"running": {}}}]})),
            "NoReady"
        );
        assert_eq!(
            st(json!({"phase": "Running",
                "containerStatuses": [{"name": "c1", "state": {"terminated": {"exitCode": 2, "signal": 9}}}]})),
            "Signal:9"
        );
        assert_eq!(
            st(json!({"phase": "Running",
                "containerStatuses": [{"name": "c1", "state": {"terminated": {"exitCode": 2}}}]})),
            "ExitCode:2"
        );
        assert_eq!(
            pod_status_of(
                true,
                &json!({}),
                &json!({"phase": "Running", "reason": "NodeLost"})
            ),
            "Unknown"
        );
        assert_eq!(
            pod_status_of(true, &json!({}), &json!({"phase": "Running"})),
            "Terminating"
        );
    }

    #[test]
    fn check_phase() {
        assert_eq!(
            phase(false, &json!({}), &json!({"phase": "Unknown"})),
            "Unknown"
        );
        assert_eq!(
            phase(
                true,
                &json!({}),
                &json!({"phase": "Unknown", "reason": "bla"})
            ),
            "Terminating"
        );
        assert_eq!(
            phase(
                true,
                &json!({}),
                &json!({"phase": "Unknown", "reason": "NodeLost"})
            ),
            "Unknown"
        );
        let spec = json!({"initContainers": [{"name": "ic1", "restartPolicy": "Always"}]});
        assert_eq!(
            phase(
                true,
                &spec,
                &json!({"phase": "Unknown", "reason": "bla", "initContainerStatuses": [{"name": "ic1"}]})
            ),
            "Init:0/1"
        );
        let spec = json!({"initContainers": [{"name": "ic1", "restartPolicy": "Always"}],
            "containers": [{"name": "c1"}]});
        assert_eq!(
            phase(
                true,
                &spec,
                &json!({"phase": "Unknown", "reason": "bla",
                    "initContainerStatuses": [{"name": "ic1", "state": {"running": {}}}],
                    "containerStatuses": [{"name": "c1", "state": {"waiting": {"reason": "bla"}}}]})
            ),
            "Init:0/1"
        );
        // started+ready sidecar is skipped, main container decides.
        assert_eq!(
            phase(
                false,
                &spec,
                &json!({"phase": "Running",
                    "initContainerStatuses": [{"name": "ic1", "started": true, "ready": true, "state": {"running": {}}}],
                    "containerStatuses": [{"name": "c1", "ready": true, "state": {"running": {}}}]})
            ),
            "Running"
        );
        // completed + running container → Running
        assert_eq!(
            phase(
                false,
                &json!({}),
                &json!({"phase": "Completed",
                    "containerStatuses": [{"name": "c1", "ready": true, "state": {"running": {}}}]})
            ),
            "Running"
        );
    }

    #[test]
    fn init_container_status_cases() {
        let c = |v: Value, restartable: bool| check_init_container_status(&v, 0, 0, restartable);
        assert_eq!(c(json!({}), false), "Init:0/0");
        assert_eq!(
            c(json!({"name": "ic1", "started": true, "state": {}}), true),
            "Init:0/0"
        );
        assert_eq!(
            c(
                json!({"name": "ic1", "state": {"terminated": {"exitCode": 1, "reason": "blah"}}}),
                false
            ),
            "Init:blah"
        );
        assert_eq!(
            c(
                json!({"state": {"terminated": {"exitCode": 1, "signal": 9}}}),
                false
            ),
            "Init:Signal:9"
        );
        assert_eq!(
            c(json!({"state": {"terminated": {"exitCode": 1}}}), false),
            "Init:ExitCode:1"
        );
        assert_eq!(
            c(json!({"state": {"terminated": {"exitCode": 0}}}), false),
            ""
        );
        assert_eq!(
            c(json!({"state": {"waiting": {"reason": "blah"}}}), false),
            "Init:blah"
        );
        assert_eq!(
            c(
                json!({"state": {"waiting": {"reason": "PodInitializing"}}}),
                false
            ),
            "Init:0/0"
        );
        assert_eq!(c(json!({"started": true, "ready": true}), true), "");
    }

    #[test]
    fn container_phase_cases() {
        let p = |v: Value| container_phase(&v, "Unknown".into());
        assert_eq!(
            p(json!({"containerStatuses": [{"name": "c1", "state": {"waiting": {"reason": "waiting"}}}]})).0,
            "waiting"
        );
        assert_eq!(
            p(json!({"containerStatuses": [{"name": "c1", "state": {"terminated": {"reason": "done"}}}]})).0,
            "done"
        );
        assert_eq!(
            p(json!({"containerStatuses": [{"name": "c1", "state": {"terminated": {"signal": 9}}}]})).0,
            "Signal:9"
        );
        assert_eq!(
            p(json!({"containerStatuses": [{"name": "c1", "state": {"terminated": {"exitCode": 2}}}]})).0,
            "ExitCode:2"
        );
        assert_eq!(
            p(
                json!({"containerStatuses": [{"name": "c1", "ready": true, "state": {"running": {}}}]})
            ),
            ("Unknown".to_string(), true)
        );
    }

    #[test]
    fn sidecars_and_stats() {
        assert!(is_sidecar(&json!({"restartPolicy": "Always"})));
        assert!(!is_sidecar(&json!({"restartPolicy": "Never"})));
        assert!(!is_sidecar(&json!({})));
        let ts = parse_time("2018-12-14T10:36:43.326972-07:00").unwrap();
        let (_, _, _, l) = container_stats(&[json!({"name": "c1", "lastState": {}})]);
        assert_eq!(l, None);
        let (r, t, n, l) = container_stats(&[
            json!({"name": "c1", "ready": true, "restartCount": 2,
                   "lastState": {"terminated": {"finishedAt": "2018-12-14T10:36:43.326972-07:00"}}}),
            json!({"name": "c2", "restartCount": 1, "state": {"terminated": {}},
                   "lastState": {"terminated": {"finishedAt": "2018-12-14T10:30:00Z"}}}),
        ]);
        assert_eq!((r, t, n, l), (1, 1, 3, Some(ts)));
    }

    #[test]
    fn diagnose_cases() {
        assert!(diagnose("Completed", 0, 1, false, 0, 0).is_ok());
        assert_eq!(
            diagnose("Running", 0, 1, true, 0, 0).unwrap_err(),
            "container ready check failed: 0 of 1"
        );
        assert_eq!(
            diagnose("Running", 0, 0, true, 0, 0).unwrap_err(),
            "container ready check failed: 0 of 0"
        );
        assert_eq!(
            diagnose("Running", 1, 1, true, 0, 1).unwrap_err(),
            "readiness gate check failed: 0 of 1"
        );
        assert_eq!(
            diagnose("Running", 1, 1, false, 0, 0).unwrap_err(),
            "pod condition ready is false"
        );
        assert_eq!(
            diagnose("Terminating", 1, 1, true, 0, 0).unwrap_err(),
            "pod is terminating"
        );
        assert!(diagnose("Running", 1, 1, true, 1, 1).is_ok());
    }

    #[test]
    fn readiness_gates_and_qos() {
        let spec = json!({"readinessGates": [{"conditionType": "a"}, {"conditionType": "b"}]});
        let st = json!({"conditions": [{"type": "a", "status": "True"}, {"type": "b", "status": "False"}]});
        assert_eq!(as_readiness_gate(&spec, &st), "1/2");
        assert_eq!(readiness_gate_stats(&spec, &st), (1, 2));
        assert_eq!(as_readiness_gate(&json!({}), &st), "<none>");
        assert_eq!(map_qos("Guaranteed"), "GA");
        assert_eq!(map_qos("Burstable"), "BU");
        assert_eq!(map_qos(""), "BE");
    }

    #[test]
    fn terminating_pod() {
        let mut v: Value = serde_json::to_value(load("po")).unwrap();
        v["metadata"]["deletionTimestamp"] = json!("2019-08-09T05:12:19Z");
        let o = obj(v);
        assert_eq!(pod_phase(&o), "Terminating");
        assert_eq!(pod_status(&o), "Terminating");
        let mut row = Row::default();
        Pod::default()
            .render(&RenderInput::new(&o), "", &mut row)
            .unwrap();
        assert_eq!(row.fields[5], "Terminating");
        assert_eq!(row.fields[24], "pod is terminating");
    }
}
