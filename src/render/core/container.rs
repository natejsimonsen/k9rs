//! Container renderer (k9s render/container.go). Input is a pseudo
//! `Container` object (see `render::pseudo::container`).
//!
//! IDX: the optional `idx` string field wins (k9s form, e.g. `M1`); else it
//! is `I`/`E`/`M` (init/ephemeral/main) + `index + 1`, so `index` should be
//! the position within its kind like k9s. AGE is the pod's age: taken from
//! an optional `podCreationTimestamp` field, else the object's
//! creationTimestamp, else `<unknown>`. Metrics come from
//! `metrics.pods["ns/pod"].containers[name]`.

use anyhow::Result;
use ratatui::style::Color;
use serde_json::Value;

use super::field_of;
use crate::model::{Align, Header, HeaderColumn, Row, RowEvent};
use crate::render::helpers::*;
use crate::render::pseudo::{bool_field, i64_field, str_field};
use crate::render::{default_colorer, Palette, RenderInput, Renderer};

pub(crate) fn header() -> Header {
    let r = |n: &str| HeaderColumn::new(n).align(Align::Right);
    Header::new(vec![
        HeaderColumn::new("IDX"),
        HeaderColumn::new("NAME"),
        HeaderColumn::new("PF"),
        HeaderColumn::new("IMAGE"),
        HeaderColumn::new("READY"),
        HeaderColumn::new("STATE"),
        r("RESTARTS"),
        HeaderColumn::new("PROBES(L:R:S)"),
        r("CPU").mx(),
        r("CPU/RL"),
        r("%CPU/R").mx(),
        r("%CPU/L").mx(),
        r("MEM").mx(),
        r("MEM/RL"),
        r("%MEM/R").mx(),
        r("%MEM/L").mx(),
        r("GPU/RL"),
        HeaderColumn::new("PORTS"),
        HeaderColumn::new("VALID").wide(),
        HeaderColumn::new("AGE").time(),
    ])
}

/// k9s render.Container.
pub struct Container;

impl Renderer for Container {
    fn header(&self, _ns: &str) -> Header {
        header()
    }

    fn render(&self, input: &RenderInput<'_>, _ns: &str, row: &mut Row) -> Result<()> {
        let o = input.obj;
        let null = Value::Null;
        let co = o.data.get("spec").unwrap_or(&null);
        let status = o.data.get("status").filter(|s| !s.is_null());
        let name = str_at(co, "name");
        let pod_fqn = crate::k8s::fqn(obj_ns(o), str_field(o, "pod"));
        let usage = input
            .metrics
            .and_then(|m| m.pods.get(&pod_fqn))
            .and_then(|pm| pm.containers.get(name))
            .copied()
            .unwrap_or_default();

        let rl = container_requests(co);
        let ll = get_path(co, "resources.limits");
        let (rcpu, lcpu) = (res_milli(rl, "cpu"), res_milli(ll, "cpu"));
        let (rmem, lmem) = (res_value(rl, "memory"), res_value(ll, "memory"));
        let (rgpu, lgpu) = (extract_gpu(rl), extract_gpu(ll));
        let (ccpu, cmem) = (usage.cpu_millis, usage.mem_bytes);

        let (ready, state, restarts) = match status {
            Some(s) => (
                bool_to_str(bool_at(s, "ready")),
                container_state(s.get("state")),
                i64_at(s, "restartCount").to_string(),
            ),
            None => (
                "false".to_string(),
                MISSING_VALUE.to_string(),
                "0".to_string(),
            ),
        };
        let probes = format!(
            "{}:{}:{}",
            probe(co.get("livenessProbe")),
            probe(co.get("readinessProbe")),
            probe(co.get("startupProbe"))
        );
        let age = match o.data.get("podCreationTimestamp") {
            Some(v) => to_age_opt(value_time(Some(v))),
            None => obj_age(o),
        };
        let valid = as_status(diagnose(&state, &ready));

        row.id = name.to_string();
        row.fields = vec![
            container_idx(o),
            name.to_string(),
            "●".to_string(),
            str_at(co, "image").to_string(),
            ready,
            state,
            restarts,
            probes,
            to_mc(ccpu),
            format!("{}:{}", to_mc(rcpu), to_mc(lcpu)),
            to_percentage_str(ccpu, rcpu),
            to_percentage_str(ccpu, lcpu),
            to_mi(cmem),
            format!("{}:{}", to_mi(rmem), to_mi(lmem)),
            to_percentage_str(cmem, rmem),
            to_percentage_str(cmem, lmem),
            format!("{}:{}", to_mc(rgpu), to_mc(lgpu)),
            container_ports(array_at(co, "ports")),
            valid,
            age,
        ];
        Ok(())
    }

    fn color_for(&self, ns: &str, h: &Header, re: &RowEvent, p: &Palette) -> Color {
        let c = default_colorer(ns, h, re, p);
        let Some(state) = field_of(h, re, "STATE") else {
            return c;
        };
        match state {
            "Pending" => p.pending,
            "ContainerCreating" | "PodInitializing" => p.add,
            "Terminating" | "Initialized" => p.highlight,
            "Completed" => p.completed,
            "Running" => c,
            _ => p.err,
        }
    }
}

fn container_idx(o: &kube::api::DynamicObject) -> String {
    let idx = str_field(o, "idx");
    if !idx.is_empty() {
        return idx.to_string();
    }
    let kind = if bool_field(o, "isInit") {
        "I"
    } else if bool_field(o, "isEphemeral") {
        "E"
    } else {
        "M"
    };
    format!("{kind}{}", i64_field(o, "index") + 1)
}

fn diagnose(state: &str, ready: &str) -> Result<(), String> {
    if state == "Completed" {
        return Ok(());
    }
    if ready == "false" {
        return Err("container is not ready".into());
    }
    Ok(())
}

fn probe(p: Option<&Value>) -> &'static str {
    match p {
        Some(v) if !v.is_null() => "on",
        _ => "off",
    }
}

/// k9s containerRequests: requests when set, else limits, else none.
pub(crate) fn container_requests(co: &Value) -> Option<&Value> {
    fn non_empty(v: Option<&Value>) -> Option<&Value> {
        v.filter(|x| x.as_object().is_some_and(|m| !m.is_empty()))
    }
    non_empty(get_path(co, "resources.requests"))
        .or_else(|| non_empty(get_path(co, "resources.limits")))
}

/// k9s ToContainerPorts: `name:port` (`╱PROTO` when not TCP), comma joined.
pub fn container_ports(pp: &[Value]) -> String {
    pp.iter()
        .map(|p| {
            let mut s = String::new();
            let name = str_at(p, "name");
            if !name.is_empty() {
                s.push_str(name);
                s.push(':');
            }
            s.push_str(&i64_at(p, "containerPort").to_string());
            let proto = str_at(p, "protocol");
            if proto != "TCP" {
                s.push('╱');
                s.push_str(proto);
            }
            s
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// k9s ToContainerState for a ContainerState JSON value.
pub fn container_state(s: Option<&Value>) -> String {
    let get = |k: &str| s.and_then(|s| s.get(k)).filter(|v| !v.is_null());
    if let Some(w) = get("waiting") {
        let r = str_at(w, "reason");
        return if r.is_empty() { "Waiting" } else { r }.to_string();
    }
    if let Some(t) = get("terminated") {
        let r = str_at(t, "reason");
        return if r.is_empty() { "Terminating" } else { r }.to_string();
    }
    if get("running").is_some() {
        return "Running".into();
    }
    MISSING_VALUE.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::k8s::{Metrics, MetricsSnapshot, PodMetrics};
    use crate::model::ResEvent;
    use crate::render::core::testutil::*;
    use serde_json::json;

    fn pod() -> kube::api::DynamicObject {
        obj(json!({
            "apiVersion": "v1", "kind": "Pod",
            "metadata": {"name": "p1", "namespace": "blee", "creationTimestamp": "2018-12-14T10:36:43.326972-07:00"},
            "status": {"phase": "Running"}
        }))
    }

    fn container() -> Value {
        json!({
            "name": "fred", "image": "img",
            "resources": {"limits": {"cpu": "20m", "memory": "100Mi"}},
            "env": [{"name": "fred", "value": "1", "valueFrom": {"configMapKeyRef": {"key": "blee"}}}]
        })
    }

    #[test]
    fn render_container() {
        let p = pod();
        let st = json!({"name": "fred", "state": {"running": {}}, "restartCount": 0});
        let mut o = crate::render::pseudo::container(&p, &container(), Some(&st), false, false, 0);
        o.data["podCreationTimestamp"] = json!("2018-12-14T10:36:43.326972-07:00");
        let mut m = MetricsSnapshot::default();
        let mut pm = PodMetrics::default();
        pm.containers.insert(
            "fred".into(),
            Metrics {
                cpu_millis: 10,
                mem_bytes: 20 * MEGA_BYTE,
            },
        );
        m.pods.insert("blee/p1".into(), pm);
        let mut r = Row::default();
        let n = parse_time("2018-12-14T17:37:43.326972Z").unwrap();
        with_now(n, || {
            Container.render(&RenderInput::with_metrics(&o, &m), "blee", &mut r)
        })
        .unwrap();
        assert_eq!(r.id, "fred");
        assert_eq!(
            r.fields,
            [
                "M1",
                "fred",
                "●",
                "img",
                "false",
                "Running",
                "0",
                "off:off:off",
                "10",
                "20:20",
                "50",
                "50",
                "20",
                "100:100",
                "20",
                "20",
                "0:0",
                "",
                "container is not ready",
                "60s"
            ]
        );
        assert_eq!(r.fields.len(), Container.header("").len());
    }

    #[test]
    fn render_without_status() {
        let p = pod();
        let mut co = container();
        co["ports"] = json!([{"name": "http", "containerPort": 80, "protocol": "TCP"}, {"containerPort": 53, "protocol": "UDP"}]);
        co["livenessProbe"] = json!({"httpGet": {}});
        let o = crate::render::pseudo::container(&p, &co, None, true, false, 1);
        let mut r = Row::default();
        Container
            .render(&RenderInput::new(&o), "blee", &mut r)
            .unwrap();
        assert_eq!(r.fields[0], "I2");
        assert_eq!(r.fields[4..8], ["false", "<none>", "0", "on:off:off"]);
        assert_eq!(r.fields[8], "0");
        assert_eq!(r.fields[17], "http:80,53╱UDP");
        // AGE is the pod's age (pseudo::container copies the pod creation timestamp).
        let created = p
            .metadata
            .creation_timestamp
            .as_ref()
            .unwrap()
            .0
            .to_string();
        let created = crate::render::helpers::parse_time(&created).unwrap();
        assert_eq!(
            r.fields[19],
            crate::render::helpers::to_age(created, crate::render::helpers::now())
        );
        // Without a pod creation timestamp, AGE is <unknown>.
        let mut bare = o.clone();
        bare.data["podCreationTimestamp"] = serde_json::Value::Null;
        let mut r2 = Row::default();
        Container
            .render(&RenderInput::new(&bare), "blee", &mut r2)
            .unwrap();
        assert_eq!(r2.fields[19], "<unknown>");
        let mut o = crate::render::pseudo::container(&p, &co, None, false, true, 0);
        o.data["idx"] = json!("X9");
        Container
            .render(&RenderInput::new(&o), "blee", &mut r)
            .unwrap();
        assert_eq!(r.fields[0], "X9");
        let o = crate::render::pseudo::container(&p, &co, None, false, true, 0);
        Container
            .render(&RenderInput::new(&o), "blee", &mut r)
            .unwrap();
        assert_eq!(r.fields[0], "E1");
    }

    #[test]
    fn states() {
        assert_eq!(container_state(Some(&json!({"waiting": {}}))), "Waiting");
        assert_eq!(
            container_state(Some(&json!({"waiting": {"reason": "CrashLoopBackOff"}}))),
            "CrashLoopBackOff"
        );
        assert_eq!(
            container_state(Some(&json!({"terminated": {}}))),
            "Terminating"
        );
        assert_eq!(
            container_state(Some(&json!({"terminated": {"reason": "Completed"}}))),
            "Completed"
        );
        assert_eq!(container_state(Some(&json!({"running": {}}))), "Running");
        assert_eq!(container_state(Some(&json!({}))), "<none>");
        assert_eq!(container_state(None), "<none>");
    }

    #[test]
    fn requests() {
        assert_eq!(container_requests(&json!({})), None);
        let c = json!({"resources": {"requests": {}, "limits": {"cpu": "1"}}});
        assert_eq!(container_requests(&c), Some(&json!({"cpu": "1"})));
        let c = json!({"resources": {"requests": {"cpu": "2"}, "limits": {"cpu": "1"}}});
        assert_eq!(container_requests(&c), Some(&json!({"cpu": "2"})));
    }

    #[test]
    fn colorer() {
        let h = Header::new(vec![HeaderColumn::new("NAME"), HeaderColumn::new("STATE")]);
        let p = palette();
        let c = |s: &str| {
            Container.color_for(
                "",
                &h,
                &RowEvent::new(
                    ResEvent::Unchanged,
                    Row::with("a", vec!["a".into(), s.into()]),
                ),
                &p,
            )
        };
        assert_eq!(c("Pending"), p.pending);
        assert_eq!(c("ContainerCreating"), p.add);
        assert_eq!(c("PodInitializing"), p.add);
        assert_eq!(c("Terminating"), p.highlight);
        assert_eq!(c("Initialized"), p.highlight);
        assert_eq!(c("Completed"), p.completed);
        assert_eq!(c("Running"), p.std);
        assert_eq!(c("CrashLoopBackOff"), p.err);
        let h2 = Header::new(vec![HeaderColumn::new("NAME")]);
        assert_eq!(
            Container.color_for(
                "",
                &h2,
                &RowEvent::new(ResEvent::Add, Row::with("a", vec!["a".into()])),
                &p
            ),
            p.add
        );
    }
}
