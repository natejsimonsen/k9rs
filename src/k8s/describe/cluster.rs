//! Cluster level describers: Node, Namespace, ResourceQuota, LimitRange,
//! PriorityClass, HorizontalPodAutoscaler and PodDisruptionBudget.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use serde_json::Value;

use super::events::describe_events;
use super::gosort::sort_by;
use super::pod::{go_quote, pod_requests_and_limits};
use super::quantity::{canonical, parse_or_zero, Quantity};
use super::util::{
    format_label_selector, format_labels, intstr_ptr, print_annotations_multiline,
    print_labels_multiline, print_taints_multiline, Ctx, J,
};
use super::writer::{PrefixWriter, L0, L1, L2, L3};

const NODE_ROLE_PREFIX: &str = "node-role.kubernetes.io/";
const NODE_ROLE_LABEL: &str = "kubernetes.io/role";

fn node_roles(labels: &BTreeMap<String, String>) -> Vec<String> {
    let mut roles = BTreeSet::new();
    for (k, v) in labels {
        if let Some(role) = k.strip_prefix(NODE_ROLE_PREFIX) {
            if !role.is_empty() {
                roles.insert(role.to_string());
            }
        } else if k == NODE_ROLE_LABEL && !v.is_empty() {
            roles.insert(v.clone());
        }
    }
    roles.into_iter().collect()
}

pub(crate) fn describe_node(ctx: &Ctx, node: J, events: &[Value]) -> Result<String> {
    let (meta, spec, status) = (node.get("metadata"), node.get("spec"), node.get("status"));
    let labels = meta.smap("labels");
    let mut w = PrefixWriter::new();
    w.write(L0, &format!("Name:\t{}\n", meta.s("name")));
    let roles = node_roles(&labels);
    if roles.is_empty() {
        w.write(L0, "Roles:\t<none>\n");
    } else {
        w.write(L0, &format!("Roles:\t{}\n", roles.join(",")));
    }
    print_labels_multiline(&mut w, "Labels", &labels);
    print_annotations_multiline(&mut w, "Annotations", &meta.smap("annotations"));
    w.write(
        L0,
        &format!(
            "CreationTimestamp:\t{}\n",
            ctx.rfc1123z(meta.get("creationTimestamp"))
        ),
    );
    print_taints_multiline(&mut w, "Taints", spec.arr("taints"));
    w.write(
        L0,
        &format!("Unschedulable:\t{}\n", spec.b("unschedulable")),
    );
    if let Some(lease) = &ctx.rel.lease {
        let l = J(lease).get("spec");
        w.write(L0, "Lease:\n");
        w.write(
            L1,
            &format!(
                "HolderIdentity:\t{}\n",
                l.opt_s("holderIdentity").unwrap_or("<unset>")
            ),
        );
        for (title, key) in [("AcquireTime", "acquireTime"), ("RenewTime", "renewTime")] {
            let t = l.get(key);
            let v = if t.is_null() {
                "<unset>".to_string()
            } else {
                ctx.rfc1123z(t)
            };
            w.write(L1, &format!("{title}:\t{v}\n"));
        }
    }
    let conditions = status.arr("conditions");
    if !conditions.is_empty() {
        w.write(
            L0,
            "Conditions:\n  Type\tStatus\tLastHeartbeatTime\tLastTransitionTime\tReason\tMessage\n",
        );
        w.write(
            L1,
            "----\t------\t-----------------\t------------------\t------\t-------\n",
        );
        for c in conditions {
            w.write(
                L1,
                &format!(
                    "{} \t{} \t{} \t{} \t{} \t{}\n",
                    c.s("type"),
                    c.s("status"),
                    ctx.rfc1123z(c.get("lastHeartbeatTime")),
                    ctx.rfc1123z(c.get("lastTransitionTime")),
                    c.s("reason"),
                    c.s("message")
                ),
            );
        }
    }
    w.write(L0, "Addresses:\n");
    for a in status.arr("addresses") {
        w.write(L1, &format!("{}:\t{}\n", a.s("type"), a.s("address")));
    }
    for (title, key) in [("Capacity", "capacity"), ("Allocatable", "allocatable")] {
        let names = status.keys(key);
        if names.is_empty() {
            continue;
        }
        w.write(L0, &format!("{title}:\n"));
        for n in names {
            w.write(
                L0,
                &format!("  {n}:\t{}\n", canonical(status.get(key).key(n).as_str())),
            );
        }
    }
    let info = status.get("nodeInfo");
    w.write(L0, "System Info:\n");
    for (title, key) in [
        ("Machine ID", "machineID"),
        ("System UUID", "systemUUID"),
        ("Boot ID", "bootID"),
        ("Kernel Version", "kernelVersion"),
        ("OS Image", "osImage"),
        ("Operating System", "operatingSystem"),
        ("Architecture", "architecture"),
        ("Container Runtime Version", "containerRuntimeVersion"),
        ("Kubelet Version", "kubeletVersion"),
        ("Kube-Proxy Version", "kubeProxyVersion"),
    ] {
        w.write(L0, &format!("  {title}:\t{}\n", info.s(key)));
    }
    if !spec.s("podCIDR").is_empty() {
        w.write(L0, &format!("PodCIDR:\t{}\n", spec.s("podCIDR")));
    }
    let cidrs = spec.strs("podCIDRs");
    if !cidrs.is_empty() {
        w.write(L0, &format!("PodCIDRs:\t{}\n", cidrs.join(",")));
    }
    if !spec.s("providerID").is_empty() {
        w.write(L0, &format!("ProviderID:\t{}\n", spec.s("providerID")));
    }
    if let Some(pods) = &ctx.rel.pods {
        let name = meta.s("name");
        let pods: Vec<J> = pods
            .iter()
            .map(J)
            .filter(|p| p.s("spec.nodeName") == name)
            .filter(|p| !matches!(p.s("status.phase"), "Succeeded" | "Failed"))
            .collect();
        describe_node_resource(ctx, &pods, node, &mut w);
    }
    describe_events(ctx, &mut w, events);
    Ok(w.finish())
}

fn pct(num: i64, den: i64) -> i64 {
    (num as f64 / den as f64 * 100.0) as i64
}

fn q_of(list: &BTreeMap<String, Quantity>, name: &str) -> Quantity {
    list.get(name).copied().unwrap_or_default()
}

/// describeNodeResource.
fn describe_node_resource(ctx: &Ctx, pods: &[J], node: J, w: &mut PrefixWriter) {
    w.write(
        L0,
        &format!("Non-terminated Pods:\t({} in total)\n", pods.len()),
    );
    w.write(
        L1,
        "Namespace\tName\t\tCPU Requests\tCPU Limits\tMemory Requests\tMemory Limits\tAge\n",
    );
    w.write(
        L1,
        "---------\t----\t\t------------\t----------\t---------------\t-------------\t---\n",
    );
    let status = node.get("status");
    let alloc_node = if status.len("allocatable") > 0 {
        status.get("allocatable")
    } else {
        status.get("capacity")
    };
    let allocatable: BTreeMap<String, Quantity> = alloc_node
        .keys("")
        .into_iter()
        .map(|k| (k.to_string(), parse_or_zero(alloc_node.key(k).as_str())))
        .collect();
    let alloc_cpu = q_of(&allocatable, "cpu").milli_value();
    let alloc_mem = q_of(&allocatable, "memory").value();
    let alloc_eph = q_of(&allocatable, "ephemeral-storage").value();
    let mut reqs: BTreeMap<String, Quantity> = BTreeMap::new();
    let mut limits: BTreeMap<String, Quantity> = BTreeMap::new();
    for p in pods {
        let (req, lim) = pod_requests_and_limits(*p);
        let (cr, cl, mr, ml) = (
            q_of(&req, "cpu"),
            q_of(&lim, "cpu"),
            q_of(&req, "memory"),
            q_of(&lim, "memory"),
        );
        w.write(
            L1,
            &format!(
                "{}\t{}\t\t{} ({}%)\t{} ({}%)\t{} ({}%)\t{} ({}%)\t{}\n",
                p.s("metadata.namespace"),
                p.s("metadata.name"),
                cr.go_string(),
                pct(cr.milli_value(), alloc_cpu),
                cl.go_string(),
                pct(cl.milli_value(), alloc_cpu),
                mr.go_string(),
                pct(mr.value(), alloc_mem),
                ml.go_string(),
                pct(ml.value(), alloc_mem),
                ctx.since(p.get("metadata.creationTimestamp"))
            ),
        );
        for (src, dst) in [(&req, &mut reqs), (&lim, &mut limits)] {
            for (k, q) in src {
                match dst.get_mut(k) {
                    Some(v) => v.add(q),
                    None => {
                        dst.insert(k.clone(), *q);
                    }
                }
            }
        }
    }
    w.write(
        L0,
        "Allocated resources:\n  (Total limits may be over 100 percent, i.e., overcommitted.)\n",
    );
    w.write(L1, "Resource\tRequests\tLimits\n");
    w.write(L1, "--------\t--------\t------\n");
    let frac = |n: i64, d: i64| if d != 0 { pct(n, d) } else { 0 };
    for (name, alloc, milli) in [
        ("cpu", alloc_cpu, true),
        ("memory", alloc_mem, false),
        ("ephemeral-storage", alloc_eph, false),
    ] {
        let (r, l) = (q_of(&reqs, name), q_of(&limits, name));
        let (rv, lv) = if milli {
            (r.milli_value(), l.milli_value())
        } else {
            (r.value(), l.value())
        };
        w.write(
            L1,
            &format!(
                "{name}\t{} ({}%)\t{} ({}%)\n",
                r.go_string(),
                frac(rv, alloc),
                l.go_string(),
                frac(lv, alloc)
            ),
        );
    }
    let standard = |n: &str| {
        matches!(n, "cpu" | "memory" | "ephemeral-storage") || n.starts_with("hugepages-")
    };
    let huge: Vec<&String> = allocatable
        .keys()
        .filter(|k| k.starts_with("hugepages-"))
        .collect();
    let ext: Vec<&String> = allocatable
        .keys()
        .filter(|k| !standard(k) && k.as_str() != "pods")
        .collect();
    for name in huge {
        let (r, l, a) = (
            q_of(&reqs, name),
            q_of(&limits, name),
            q_of(&allocatable, name),
        );
        w.write(
            L1,
            &format!(
                "{name}\t{} ({}%)\t{} ({}%)\n",
                r.go_string(),
                frac(r.value(), a.value()),
                l.go_string(),
                frac(l.value(), a.value())
            ),
        );
    }
    for name in ext {
        w.write(
            L1,
            &format!(
                "{name}\t{}\t{}\n",
                q_of(&reqs, name).go_string(),
                q_of(&limits, name).go_string()
            ),
        );
    }
}

pub(crate) fn describe_namespace(ctx: &Ctx, ns: J, _events: &[Value]) -> Result<String> {
    let meta = ns.get("metadata");
    let name = meta.s("name");
    let mut w = PrefixWriter::new();
    w.write(L0, &format!("Name:\t{name}\n"));
    print_labels_multiline(&mut w, "Labels", &meta.smap("labels"));
    print_annotations_multiline(&mut w, "Annotations", &meta.smap("annotations"));
    w.write(L0, &format!("Status:\t{}\n", ns.s("status.phase")));
    let conditions = ns.arr("status.conditions");
    if !conditions.is_empty() {
        w.write(L0, "Conditions:\n");
        w.write(L1, "Type\tStatus\tLastTransitionTime\tReason\tMessage\n");
        w.write(L1, "----\t------\t------------------\t------\t-------\n");
        for c in conditions {
            w.write(
                L1,
                &format!(
                    "{}\t{}\t{}\t{}\t{}\n",
                    c.s("type"),
                    c.s("status"),
                    ctx.rfc1123z(c.get("lastTransitionTime")),
                    c.s("reason"),
                    c.s("message")
                ),
            );
        }
    }
    let in_ns = |l: &Vec<Value>| -> Vec<Value> {
        l.iter()
            .filter(|o| J(o).s("metadata.namespace") == name)
            .cloned()
            .collect()
    };
    if let Some(quotas) = &ctx.rel.resource_quotas {
        w.write(L0, "\n");
        describe_resource_quotas(&in_ns(quotas), &mut w);
    }
    if let Some(ranges) = &ctx.rel.limit_ranges {
        w.write(L0, "\n");
        describe_limit_ranges(&in_ns(ranges), &mut w);
    }
    Ok(w.finish())
}

fn scope_help(scope: &str) -> &'static str {
    match scope {
        "Terminating" => "Matches all pods that have an active deadline. These pods have a limited lifespan on a node before being actively terminated by the system.",
        "NotTerminating" => "Matches all pods that do not have an active deadline. These pods usually include long running pods whose container command is not expected to terminate.",
        "BestEffort" => "Matches all pods that do not have resource requirements set. These pods have a best effort quality of service.",
        "NotBestEffort" => "Matches all pods that have at least one resource requirement set. These pods have a burstable or guaranteed quality of service.",
        _ => "",
    }
}

/// DescribeResourceQuotas (namespace describer).
fn describe_resource_quotas(quotas: &[Value], w: &mut PrefixWriter) {
    if quotas.is_empty() {
        w.write(L0, "No resource quota.\n");
        return;
    }
    let mut quotas: Vec<J> = quotas.iter().map(J).collect();
    sort_by(&mut quotas, |a, b| {
        a.s("metadata.name") < b.s("metadata.name")
    });
    w.write(L0, "Resource Quotas\n");
    for q in quotas {
        w.write(L1, &format!("Name:\t{}\n", q.s("metadata.name")));
        let mut scopes = q.strs("spec.scopes");
        if !scopes.is_empty() {
            scopes.sort();
            w.write(L1, &format!("Scopes:\t{}\n", scopes.join(", ")));
            for s in &scopes {
                let help = scope_help(s);
                if !help.is_empty() {
                    w.write(L1, &format!("* {help}\n"));
                }
            }
        }
        w.write(L1, "Resource\tUsed\tHard\n");
        w.write(L1, "--------\t---\t---\n");
        for r in q.keys("status.hard") {
            let hard = canonical(q.get("status.hard").key(r).as_str());
            let used = match q.get("status.used").key(r).0.as_str() {
                Some(u) => canonical(u),
                None => "0".to_string(),
            };
            w.write(L1, &format!("{r}\t{used}\t{hard}\n"));
        }
    }
}

/// describeLimitRangeSpec. kubectl iterates the resource set as a Go map
/// (random order); resources are printed sorted here.
fn describe_limit_range_spec(spec: J, prefix: &str, w: &mut PrefixWriter) {
    for item in spec.arr("limits") {
        let fields = [
            "max",
            "min",
            "default",
            "defaultRequest",
            "maxLimitRequestRatio",
        ];
        let mut set = BTreeSet::new();
        for f in fields {
            for k in item.keys(f) {
                set.insert(k);
            }
        }
        for k in set {
            let v = |f: &str| match item.get(f).key(k).0.as_str() {
                Some(q) => canonical(q),
                None => "-".to_string(),
            };
            w.write(
                L0,
                &format!(
                    "{prefix}{}\t{k}\t{}\t{}\t{}\t{}\t{}\n",
                    item.s("type"),
                    v("min"),
                    v("max"),
                    v("defaultRequest"),
                    v("default"),
                    v("maxLimitRequestRatio")
                ),
            );
        }
    }
}

/// DescribeLimitRanges (namespace describer).
fn describe_limit_ranges(ranges: &[Value], w: &mut PrefixWriter) {
    if ranges.is_empty() {
        w.write(L0, "No LimitRange resource.\n");
        return;
    }
    w.write(
        L0,
        "Resource Limits\n Type\tResource\tMin\tMax\tDefault Request\tDefault Limit\tMax Limit/Request Ratio\n",
    );
    w.write(
        L0,
        " ----\t--------\t---\t---\t---------------\t-------------\t-----------------------\n",
    );
    for r in ranges {
        describe_limit_range_spec(J(r).get("spec"), " ", w);
    }
}

pub(crate) fn describe_limit_range(_ctx: &Ctx, lr: J, _events: &[Value]) -> Result<String> {
    let mut w = PrefixWriter::new();
    w.write(L0, &format!("Name:\t{}\n", lr.s("metadata.name")));
    w.write(L0, &format!("Namespace:\t{}\n", lr.s("metadata.namespace")));
    w.write(
        L0,
        "Type\tResource\tMin\tMax\tDefault Request\tDefault Limit\tMax Limit/Request Ratio\n",
    );
    w.write(
        L0,
        "----\t--------\t---\t---\t---------------\t-------------\t-----------------------\n",
    );
    describe_limit_range_spec(lr.get("spec"), "", &mut w);
    Ok(w.finish())
}

pub(crate) fn describe_resource_quota(_ctx: &Ctx, rq: J, _events: &[Value]) -> Result<String> {
    let mut w = PrefixWriter::new();
    w.write(L0, &format!("Name:\t{}\n", rq.s("metadata.name")));
    w.write(L0, &format!("Namespace:\t{}\n", rq.s("metadata.namespace")));
    let mut scopes = rq.strs("spec.scopes");
    if !scopes.is_empty() {
        scopes.sort();
        w.write(L0, &format!("Scopes:\t{}\n", scopes.join(", ")));
        for s in &scopes {
            let help = scope_help(s);
            if !help.is_empty() {
                w.write(L0, &format!(" * {help}\n"));
            }
        }
    }
    w.write(L0, "Resource\tUsed\tHard\n");
    w.write(L0, "--------\t----\t----\n");
    for r in rq.keys("status.hard") {
        let hard = parse_or_zero(rq.get("status.hard").key(r).as_str());
        let mut used = parse_or_zero(rq.get("status.used").key(r).as_str());
        if hard.format != used.format {
            used = Quantity::new(used.value(), hard.format);
        }
        w.write(
            L0,
            &format!("{r}\t{}\t{}\n", used.go_string(), hard.go_string()),
        );
    }
    Ok(w.finish())
}

pub(crate) fn describe_priority_class(ctx: &Ctx, pc: J, events: &[Value]) -> Result<String> {
    let mut w = PrefixWriter::new();
    w.write(L0, &format!("Name:\t{}\n", pc.s("metadata.name")));
    w.write(L0, &format!("Value:\t{}\n", pc.i("value")));
    w.write(L0, &format!("GlobalDefault:\t{}\n", pc.b("globalDefault")));
    w.write(
        L0,
        &format!("PreemptionPolicy:\t{}\n", pc.s("preemptionPolicy")),
    );
    w.write(L0, &format!("Description:\t{}\n", pc.s("description")));
    w.write(
        L0,
        &format!(
            "Annotations:\t{}\n",
            format_labels(&pc.smap("metadata.annotations"))
        ),
    );
    describe_events(ctx, &mut w, events);
    Ok(w.finish())
}

pub(crate) fn describe_pdb(ctx: &Ctx, pdb: J, events: &[Value]) -> Result<String> {
    let (spec, status) = (pdb.get("spec"), pdb.get("status"));
    let mut w = PrefixWriter::new();
    w.write(L0, &format!("Name:\t{}\n", pdb.s("metadata.name")));
    w.write(
        L0,
        &format!("Namespace:\t{}\n", pdb.s("metadata.namespace")),
    );
    if spec.exists("minAvailable") {
        w.write(
            L0,
            &format!("Min available:\t{}\n", intstr_ptr(spec.get("minAvailable"))),
        );
    } else if spec.exists("maxUnavailable") {
        w.write(
            L0,
            &format!(
                "Max unavailable:\t{}\n",
                intstr_ptr(spec.get("maxUnavailable"))
            ),
        );
    }
    if spec.exists("selector") {
        w.write(
            L0,
            &format!(
                "Selector:\t{}\n",
                format_label_selector(spec.get("selector"))
            ),
        );
    } else {
        w.write(L0, "Selector:\t<unset>\n");
    }
    w.write(L0, "Status:\n");
    w.write(
        L2,
        &format!("Allowed disruptions:\t{}\n", status.i("disruptionsAllowed")),
    );
    w.write(L2, &format!("Current:\t{}\n", status.i("currentHealthy")));
    w.write(L2, &format!("Desired:\t{}\n", status.i("desiredHealthy")));
    w.write(L2, &format!("Total:\t{}\n", status.i("expectedPods")));
    describe_events(ctx, &mut w, events);
    Ok(w.finish())
}

/// (*Quantity).String(): `<nil>` for a nil pointer.
fn qptr(j: J) -> String {
    match j.0.as_str() {
        Some(raw) => canonical(raw),
        None => "<nil>".to_string(),
    }
}

fn hpa_header(ctx: &Ctx, hpa: J, w: &mut PrefixWriter) {
    let meta = hpa.get("metadata");
    w.write(L0, &format!("Name:\t{}\n", meta.s("name")));
    w.write(L0, &format!("Namespace:\t{}\n", meta.s("namespace")));
    print_labels_multiline(w, "Labels", &meta.smap("labels"));
    print_annotations_multiline(w, "Annotations", &meta.smap("annotations"));
    w.write(
        L0,
        &format!(
            "CreationTimestamp:\t{}\n",
            ctx.rfc1123z(meta.get("creationTimestamp"))
        ),
    );
    w.write(
        L0,
        &format!(
            "Reference:\t{}/{}\n",
            hpa.s("spec.scaleTargetRef.kind"),
            hpa.s("spec.scaleTargetRef.name")
        ),
    );
}

fn hpa_replicas(hpa: J, w: &mut PrefixWriter, behavior: bool) {
    let spec = hpa.get("spec");
    let min = spec
        .opt_i("minReplicas")
        .map(|m| m.to_string())
        .unwrap_or_else(|| "<unset>".to_string());
    w.write(L0, &format!("Min replicas:\t{min}\n"));
    w.write(L0, &format!("Max replicas:\t{}\n", spec.i("maxReplicas")));
    if behavior && spec.exists("behavior") {
        w.write(L0, "Behavior:\n");
        print_direction_behavior(w, "Scale Up", spec.get("behavior.scaleUp"));
        print_direction_behavior(w, "Scale Down", spec.get("behavior.scaleDown"));
    }
    w.write(L0, &format!("{} pods:\t", spec.s("scaleTargetRef.kind")));
    w.write(
        L0,
        &format!(
            "{} current / {} desired\n",
            hpa.i("status.currentReplicas"),
            hpa.i("status.desiredReplicas")
        ),
    );
}

fn print_direction_behavior(w: &mut PrefixWriter, direction: &str, rules: J) {
    if rules.is_null() {
        return;
    }
    w.write(L1, &format!("{direction}:\n"));
    if let Some(s) = rules.opt_i("stabilizationWindowSeconds") {
        w.write(L2, &format!("Stabilization Window: {s} seconds\n"));
    }
    let policies = rules.arr("policies");
    if !policies.is_empty() {
        let select = rules.opt_s("selectPolicy").unwrap_or("Max");
        w.write(L2, &format!("Select Policy: {select}\n"));
        w.write(L2, "Policies:\n");
        for p in policies {
            w.write(
                L3,
                &format!(
                    "- Type: {}\tValue: {}\tPeriod: {} seconds\n",
                    p.s("type"),
                    p.i("value"),
                    p.i("periodSeconds")
                ),
            );
        }
    }
}

pub(crate) fn describe_hpa_v2(ctx: &Ctx, hpa: J, events: &[Value]) -> Result<String> {
    let mut w = PrefixWriter::new();
    hpa_header(ctx, hpa, &mut w);
    w.write(L0, "Metrics:\t( current / target )\n");
    let current = hpa.arr("status.currentMetrics");
    let cur = |i: usize, key: &str| -> Option<J> {
        current.get(i).map(|c| c.get(key)).filter(|c| !c.is_null())
    };
    for (i, m) in hpa.arr("spec.metrics").into_iter().enumerate() {
        match m.s("type") {
            "External" => {
                let ext = m.get("external");
                let name = go_quote(ext.s("metric.name"));
                if ext.exists("target.averageValue") {
                    let c = cur(i, "external")
                        .filter(|c| c.exists("current.averageValue"))
                        .map(|c| qptr(c.get("current.averageValue")))
                        .unwrap_or_else(|| "<unknown>".to_string());
                    w.write(
                        L1,
                        &format!(
                            "{name} (target average value):\t{c} / {}\n",
                            qptr(ext.get("target.averageValue"))
                        ),
                    );
                } else {
                    let c = cur(i, "external")
                        .map(|c| qptr(c.get("current.value")))
                        .unwrap_or_else(|| "<unknown>".to_string());
                    w.write(
                        L1,
                        &format!(
                            "{name} (target value):\t{c} / {}\n",
                            qptr(ext.get("target.value"))
                        ),
                    );
                }
            }
            "Pods" => {
                let p = m.get("pods");
                let c = cur(i, "pods")
                    .map(|c| qptr(c.get("current.averageValue")))
                    .unwrap_or_else(|| "<unknown>".to_string());
                w.write(
                    L1,
                    &format!(
                        "{} on pods:\t{c} / {}\n",
                        go_quote(p.s("metric.name")),
                        qptr(p.get("target.averageValue"))
                    ),
                );
            }
            "Object" => {
                let o = m.get("object");
                w.write(
                    L1,
                    &format!(
                        "\"{}\" on {}/{} ",
                        o.s("metric.name"),
                        o.s("describedObject.kind"),
                        o.s("describedObject.name")
                    ),
                );
                if o.s("target.type") == "AverageValue" {
                    let c = cur(i, "object")
                        .map(|c| qptr(c.get("current.averageValue")))
                        .unwrap_or_else(|| "<unknown>".to_string());
                    w.write(
                        L0,
                        &format!(
                            "(target average value):\t{c} / {}\n",
                            qptr(o.get("target.averageValue"))
                        ),
                    );
                } else {
                    let c = cur(i, "object")
                        .map(|c| qptr(c.get("current.value")))
                        .unwrap_or_else(|| "<unknown>".to_string());
                    w.write(
                        L0,
                        &format!("(target value):\t{c} / {}\n", qptr(o.get("target.value"))),
                    );
                }
            }
            "Resource" | "ContainerResource" => {
                let (key, src) = if m.s("type") == "Resource" {
                    ("resource", m.get("resource"))
                } else {
                    ("containerResource", m.get("containerResource"))
                };
                if key == "resource" {
                    w.write(L1, &format!("resource {} on pods", src.s("name")));
                } else {
                    w.write(
                        L1,
                        &format!(
                            "resource {} of container \"{}\" on pods",
                            src.s("name"),
                            src.s("container")
                        ),
                    );
                }
                if src.exists("target.averageValue") {
                    let c = cur(i, key)
                        .map(|c| qptr(c.get("current.averageValue")))
                        .unwrap_or_else(|| "<unknown>".to_string());
                    w.write(
                        L0,
                        &format!(":\t{c} / {}\n", qptr(src.get("target.averageValue"))),
                    );
                } else {
                    let c = cur(i, key)
                        .filter(|c| c.exists("current.averageUtilization"))
                        .map(|c| {
                            format!(
                                "{}% ({})",
                                c.i("current.averageUtilization"),
                                qptr(c.get("current.averageValue"))
                            )
                        })
                        .unwrap_or_else(|| "<unknown>".to_string());
                    let target = src
                        .opt_i("target.averageUtilization")
                        .map(|t| format!("{t}%"))
                        .unwrap_or_else(|| "<auto>".to_string());
                    w.write(
                        L1,
                        &format!("(as a percentage of request):\t{c} / {target}\n"),
                    );
                }
            }
            other => w.write(L1, &format!("<unknown metric type {}>\n", go_quote(other))),
        }
    }
    hpa_replicas(hpa, &mut w, true);
    let conditions = hpa.arr("status.conditions");
    if !conditions.is_empty() {
        w.write(L0, "Conditions:\n");
        w.write(L1, "Type\tStatus\tReason\tMessage\n");
        w.write(L1, "----\t------\t------\t-------\n");
        for c in conditions {
            w.write(
                L1,
                &format!(
                    "{}\t{}\t{}\t{}\n",
                    c.s("type"),
                    c.s("status"),
                    c.s("reason"),
                    c.s("message")
                ),
            );
        }
    }
    describe_events(ctx, &mut w, events);
    Ok(w.finish())
}

pub(crate) fn describe_hpa_v1(ctx: &Ctx, hpa: J, events: &[Value]) -> Result<String> {
    let mut w = PrefixWriter::new();
    hpa_header(ctx, hpa, &mut w);
    if let Some(t) = hpa.opt_i("spec.targetCPUUtilizationPercentage") {
        w.write(L0, &format!("Target CPU utilization:\t{t}%\n"));
        let current = hpa
            .opt_i("status.currentCPUUtilizationPercentage")
            .map(|c| c.to_string())
            .unwrap_or_else(|| "<unknown>".to_string());
        w.write(L0, &format!("Current CPU utilization:\t{current}%\n"));
    }
    hpa_replicas(hpa, &mut w, false);
    describe_events(ctx, &mut w, events);
    Ok(w.finish())
}
