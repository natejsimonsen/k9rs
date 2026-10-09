//! Node renderer (k9s render/node.go).
//!
//! PODS: k9s counts the pods scheduled on each node. Callers pass that count
//! as an optional integer `podCount` field in the node object's top-level
//! data; when absent (or negative, i.e. counting disabled) PODS is `n/a`.
//! Usage metrics come from `metrics.nodes[name].usage`; allocatable values
//! come from the node's `status.allocatable`, like k9s.

use anyhow::Result;
use ratatui::style::Color;
use serde_json::{json, Value};

use super::field_of;
use crate::config::ViewSetting;
use crate::model::{Align, Header, HeaderColumn, Row, RowEvent};
use crate::render::helpers::*;
use crate::render::{default_colorer, Palette, RenderInput, Renderer};

const LABEL_NODE_ROLE_PREFIX: &str = "node-role.kubernetes.io/";
const LABEL_NODE_ROLE_SUFFIX: &str = "kubernetes.io/role";
const CORDON_ERR: &str = "node is cordoned";
const NOT_READY_ERR: &str = "node is not ready";

pub(crate) fn header() -> Header {
    let r = |n: &str| HeaderColumn::new(n).align(Align::Right);
    Header::new(vec![
        HeaderColumn::new("NAME"),
        HeaderColumn::new("STATUS"),
        HeaderColumn::new("ROLE"),
        HeaderColumn::new("ARCH").wide(),
        HeaderColumn::new("TAINTS"),
        HeaderColumn::new("VERSION"),
        HeaderColumn::new("OS-IMAGE").wide(),
        HeaderColumn::new("KERNEL").wide(),
        HeaderColumn::new("INTERNAL-IP").wide(),
        HeaderColumn::new("EXTERNAL-IP").wide(),
        r("PODS"),
        r("CPU").mx(),
        r("CPU/A").mx(),
        r("%CPU").mx(),
        r("MEM").mx(),
        r("MEM/A").mx(),
        r("%MEM").mx(),
        r("GPU/A").mx(),
        r("GPU/C").mx(),
        r("SH-GPU/A").mx(),
        r("SH-GPU/C").mx(),
        HeaderColumn::new("LABELS").wide(),
        HeaderColumn::new("VALID").wide(),
        HeaderColumn::new("AGE").time(),
    ])
}

/// k9s render.Node.
#[derive(Default)]
pub struct Node {
    base: ViewBase,
}

impl Renderer for Node {
    fn header(&self, ns: &str) -> Header {
        self.base.header(&header(), ns)
    }

    fn render(&self, input: &RenderInput<'_>, ns: &str, row: &mut Row) -> Result<()> {
        let o = input.obj;
        let empty = json!({});
        let spec = o.data.get("spec").unwrap_or(&empty);
        let st = o.data.get("status").unwrap_or(&empty);
        let name = obj_name(o);

        let (iip, eip) = get_ips(array_at(st, "addresses"));
        let usage = input
            .metrics
            .and_then(|m| m.nodes.get(name))
            .map(|m| m.usage)
            .unwrap_or_default();
        let alloc = get_path(st, "allocatable");
        let (acpu, amem) = (res_milli(alloc, "cpu"), res_value(alloc, "memory"));
        let (agpu, agpu_shared) = extract_node_gpu(alloc);
        let (cgpu, cgpu_shared) = extract_node_gpu(get_path(st, "capacity"));

        let mut statuses = status(array_at(st, "conditions"), bool_at(spec, "unschedulable"));
        statuses.sort();
        let mut roles = node_roles(o);
        roles.sort();

        let pod_count = match o.data.get("podCount").and_then(Value::as_i64) {
            Some(n) if n >= 0 => n.to_string(),
            _ => NA_VALUE.to_string(),
        };

        row.id = crate::k8s::fqn("", name);
        row.fields = vec![
            name.to_string(),
            join(&statuses, ","),
            join(&roles, ","),
            str_at(st, "nodeInfo.architecture").to_string(),
            array_at(spec, "taints").len().to_string(),
            str_at(st, "nodeInfo.kubeletVersion").to_string(),
            str_at(st, "nodeInfo.osImage").to_string(),
            str_at(st, "nodeInfo.kernelVersion").to_string(),
            missing(&iip),
            missing(&eip),
            pod_count,
            to_mc(usage.cpu_millis),
            to_mc(acpu),
            to_percentage_str(usage.cpu_millis, acpu),
            to_mi(usage.mem_bytes),
            to_mi(amem),
            to_percentage_str(usage.mem_bytes, amem),
            to_mu(agpu.unwrap_or(0)),
            to_mu(cgpu.unwrap_or(0)),
            to_mu(agpu_shared.unwrap_or(0)),
            to_mu(cgpu_shared.unwrap_or(0)),
            labels_str(o),
            as_status(diagnose(&statuses)),
            obj_age(o),
        ];
        let raw = obj_to_value(o);
        self.base.finish(Some(&raw), &header(), ns, row)
    }

    fn color_for(&self, ns: &str, h: &Header, re: &RowEvent, p: &Palette) -> Color {
        let c = default_colorer(ns, h, re, p);
        match field_of(h, re, "VALID") {
            Some(CORDON_ERR) => p.pending,
            _ => c,
        }
    }

    fn set_view_setting(&mut self, vs: Option<ViewSetting>) {
        self.base.set_view_setting(vs.as_ref());
    }
}

fn diagnose(ss: &[String]) -> Result<(), &'static str> {
    if ss.is_empty() {
        return Ok(());
    }
    let mut ready = false;
    for s in ss {
        if s.is_empty() {
            continue;
        }
        if s == "SchedulingDisabled" {
            return Err(CORDON_ERR);
        }
        if s == "Ready" {
            ready = true;
        }
    }
    if !ready {
        return Err(NOT_READY_ERR);
    }
    Ok(())
}

/// k9s `status`: Ready/NotReady from the Ready condition, plus
/// SchedulingDisabled when unschedulable (10 slots, blanks for unused).
fn status(conds: &[Value], exempt: bool) -> Vec<String> {
    let mut res = vec![String::new(); 10];
    let mut index = 0;
    if let Some(c) = conds.iter().rev().find(|c| str_at(c, "type") == "Ready") {
        let neg = if str_at(c, "status") != "True" {
            "Not"
        } else {
            ""
        };
        res[index] = format!("{neg}Ready");
        index += 1;
    }
    if exempt {
        res[index] = "SchedulingDisabled".into();
    }
    res
}

/// k9s `nodeRoles` (10 slots, `<none>` when no role labels).
fn node_roles(o: &kube::api::DynamicObject) -> Vec<String> {
    let mut res = vec![String::new(); 10];
    let mut index = 0;
    if let Some(labels) = &o.metadata.labels {
        for (k, v) in labels {
            if let Some(role) = k.strip_prefix(LABEL_NODE_ROLE_PREFIX) {
                if !role.is_empty() {
                    res[index] = role.to_string();
                    index += 1;
                }
            } else if k.ends_with(LABEL_NODE_ROLE_SUFFIX) && !v.is_empty() {
                res[index] = v.clone();
                index += 1;
            }
            if index >= res.len() {
                break;
            }
        }
    }
    if blank(&res) {
        res[index] = MISSING_VALUE.into();
    }
    res
}

fn get_ips(addrs: &[Value]) -> (String, String) {
    let (mut iip, mut eip) = (String::new(), String::new());
    for a in addrs {
        match str_at(a, "type") {
            "ExternalIP" => eip = str_at(a, "address").to_string(),
            "InternalIP" => iip = str_at(a, "address").to_string(),
            _ => {}
        }
    }
    (iip, eip)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::k8s::{Metrics, MetricsSnapshot, NodeMetrics};
    use crate::model::ResEvent;
    use crate::render::core::testutil::*;

    fn mx() -> MetricsSnapshot {
        let mut m = MetricsSnapshot::default();
        m.nodes.insert(
            "minikube".into(),
            NodeMetrics {
                usage: Metrics {
                    cpu_millis: 10,
                    mem_bytes: 20 * MEGA_BYTE,
                },
                allocatable: Metrics::default(),
            },
        );
        m
    }

    #[test]
    fn node_render() {
        let mut o = load("no");
        o.data["podCount"] = json!(0);
        let m = mx();
        let mut r = Row::default();
        Node::default()
            .render(&RenderInput::with_metrics(&o, &m), "", &mut r)
            .unwrap();
        assert_eq!(r.id, "minikube");
        assert_eq!(
            r.fields[..19],
            [
                "minikube",
                "Ready",
                "master",
                "amd64",
                "0",
                "v1.15.2",
                "Buildroot 2018.05.3",
                "4.15.0",
                "192.168.64.107",
                "<none>",
                "0",
                "10",
                "4000",
                "0",
                "20",
                "7874",
                "0",
                "n/a",
                "n/a"
            ]
        );
        assert_eq!(r.fields[22], "");
        assert_eq!(r.fields.len(), header().len());
    }

    #[test]
    fn node_without_count_cordoned() {
        let mut o = load("no");
        o.data["spec"] = json!({"unschedulable": true, "taints": [{"key": "a"}, {"key": "b"}]});
        let mut r = Row::default();
        Node::default()
            .render(&RenderInput::new(&o), "-", &mut r)
            .unwrap();
        assert_eq!(r.fields[1], "Ready,SchedulingDisabled");
        assert_eq!(r.fields[4], "2");
        assert_eq!(r.fields[10], "n/a");
        assert_eq!(r.fields[11], "0");
        assert_eq!(r.fields[22], "node is cordoned");
        let h = header();
        let p = palette();
        let re = RowEvent::new(ResEvent::Unchanged, r.clone());
        assert_eq!(Node::default().color_for("", &h, &re, &p), p.pending);
    }

    #[test]
    fn statuses_roles() {
        let s = status(&[json!({"type": "Ready", "status": "False"})], false);
        assert_eq!(join(&s, ","), "NotReady");
        assert_eq!(diagnose(&s), Err(NOT_READY_ERR));
        let s = status(&[], false);
        assert_eq!(join(&s, ","), "");
        assert_eq!(diagnose(&s), Err(NOT_READY_ERR));
        let s = status(&[json!({"type": "Ready", "status": "True"})], false);
        assert_eq!(diagnose(&s), Ok(()));
        let o = obj(
            json!({"apiVersion": "v1", "kind": "Node", "metadata": {"name": "n", "labels": {
            "node-role.kubernetes.io/worker": "", "node-role.kubernetes.io/": "", "kubernetes.io/role": "infra"}}}),
        );
        let mut r = node_roles(&o);
        r.sort();
        assert_eq!(join(&r, ","), "infra,worker");
        let o = obj(json!({"apiVersion": "v1", "kind": "Node", "metadata": {"name": "n"}}));
        assert_eq!(join(&node_roles(&o), ","), "<none>");
        assert_eq!(
            get_ips(&[
                json!({"type": "ExternalIP", "address": "1.1.1.1"}),
                json!({"type": "InternalIP", "address": "10.0.0.1"})
            ]),
            ("10.0.0.1".to_string(), "1.1.1.1".to_string())
        );
    }

    #[test]
    fn gpus() {
        let mut o = load("no");
        o.data["status"]["allocatable"]["nvidia.com/gpu"] = json!("2");
        o.data["status"]["capacity"]["nvidia.com/gpu"] = json!("4");
        o.data["status"]["capacity"]["nvidia.com/gpu.shared"] = json!("8");
        let mut r = Row::default();
        Node::default()
            .render(&RenderInput::new(&o), "", &mut r)
            .unwrap();
        assert_eq!(r.fields[17..21], ["2", "4", "n/a", "8"]);
    }

    #[test]
    fn colorer_default() {
        let h = header();
        let p = palette();
        let mut r = Row::new(h.len());
        r.fields[22] = "node is not ready".into();
        let re = RowEvent::new(ResEvent::Unchanged, r);
        assert_eq!(Node::default().color_for("", &h, &re, &p), p.err);
    }
}
