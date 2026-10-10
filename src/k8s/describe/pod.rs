//! Pod describer plus the container and pod template helpers that
//! workload describers share.

use std::collections::{BTreeMap, HashMap};

use anyhow::Result;
use serde_json::Value;

use super::events::describe_events;
use super::gosort::sort_by;
use super::quantity::{canonical, parse_or_zero, Format, Quantity};
use super::util::{
    format_label_selector, go_slice, intstr, join_host_port, print_annotations_multiline,
    print_bool, print_controller, print_labels_multiline, print_tolerations_multiline,
    string_or_none, Ctx, J,
};
use super::volumes::describe_volumes;
use super::writer::{PrefixWriter, L0, L1, L2, L3};

pub(crate) fn describe_pod(ctx: &Ctx, pod: J, events: &[Value]) -> Result<String> {
    let mut w = PrefixWriter::new();
    let (meta, spec, status) = (pod.get("metadata"), pod.get("spec"), pod.get("status"));
    w.write(L0, &format!("Name:\t{}\n", meta.s("name")));
    w.write(L0, &format!("Namespace:\t{}\n", meta.s("namespace")));
    if let Some(p) = spec.opt_i("priority") {
        w.write(L0, &format!("Priority:\t{p}\n"));
    }
    if !spec.s("priorityClassName").is_empty() {
        w.write(
            L0,
            &format!("Priority Class Name:\t{}\n", spec.s("priorityClassName")),
        );
    }
    if !spec.s("runtimeClassName").is_empty() {
        w.write(
            L0,
            &format!("Runtime Class Name:\t{}\n", spec.s("runtimeClassName")),
        );
    }
    if !spec.s("serviceAccountName").is_empty() {
        w.write(
            L0,
            &format!("Service Account:\t{}\n", spec.s("serviceAccountName")),
        );
    }
    if spec.s("nodeName").is_empty() {
        w.write(L0, "Node:\t<none>\n");
    } else {
        w.write(
            L0,
            &format!("Node:\t{}/{}\n", spec.s("nodeName"), status.s("hostIP")),
        );
    }
    if status.exists("startTime") {
        w.write(
            L0,
            &format!("Start Time:\t{}\n", ctx.rfc1123z(status.get("startTime"))),
        );
    }
    print_labels_multiline(&mut w, "Labels", &meta.smap("labels"));
    print_annotations_multiline(&mut w, "Annotations", &meta.smap("annotations"));
    let phase = status.s("phase");
    if meta.exists("deletionTimestamp") && phase != "Failed" && phase != "Succeeded" {
        w.write(
            L0,
            &format!(
                "Status:\tTerminating (lasts {})\n",
                ctx.since(meta.get("deletionTimestamp"))
            ),
        );
        w.write(
            L0,
            &format!(
                "Termination Grace Period:\t{}s\n",
                meta.i("deletionGracePeriodSeconds")
            ),
        );
    } else {
        w.write(L0, &format!("Status:\t{phase}\n"));
    }
    if !status.s("reason").is_empty() {
        w.write(L0, &format!("Reason:\t{}\n", status.s("reason")));
    }
    if !status.s("message").is_empty() {
        w.write(L0, &format!("Message:\t{}\n", status.s("message")));
    }
    let seccomp = spec.get("securityContext.seccompProfile");
    if !seccomp.is_null() {
        w.write(L0, &format!("SeccompProfile:\t{}\n", seccomp.s("type")));
        if seccomp.s("type") == "Localhost" {
            w.write(
                L0,
                &format!("LocalhostProfile:\t{}\n", seccomp.s("localhostProfile")),
            );
        }
    }
    w.write(L0, &format!("IP:\t{}\n", status.s("podIP")));
    let ips = status.arr("podIPs");
    if ips.is_empty() {
        w.write(L0, "IPs:\t<none>\n");
    } else {
        w.write(L0, "IPs:\n");
        for ip in ips {
            w.write(L1, &format!("IP:\t{}\n", ip.s("ip")));
        }
    }
    let controlled_by = print_controller(pod);
    if !controlled_by.is_empty() {
        w.write(L0, &format!("Controlled By:\t{controlled_by}\n"));
    }
    if !status.s("nominatedNodeName").is_empty() {
        w.write(
            L0,
            &format!("NominatedNodeName:\t{}\n", status.s("nominatedNodeName")),
        );
    }
    if spec.exists("resources") {
        w.write(L0, "Resources:\n");
        describe_resources(spec.get("resources"), &mut w, L1);
    }
    let resolver = Some(pod);
    let init = spec.arr("initContainers");
    if !init.is_empty() {
        describe_containers(
            ctx,
            "Init Containers",
            init,
            status.arr("initContainerStatuses"),
            resolver,
            &mut w,
            "",
        );
    }
    describe_containers(
        ctx,
        "Containers",
        spec.arr("containers"),
        status.arr("containerStatuses"),
        resolver,
        &mut w,
        "",
    );
    let eph = spec.arr("ephemeralContainers");
    if !eph.is_empty() {
        describe_containers(
            ctx,
            "Ephemeral Containers",
            eph,
            status.arr("ephemeralContainerStatuses"),
            resolver,
            &mut w,
            "",
        );
    }
    let gates = spec.arr("readinessGates");
    if !gates.is_empty() {
        w.write(L0, "Readiness Gates:\n  Type\tStatus\n");
        for g in gates {
            let ct = g.s("conditionType");
            let st = status
                .arr("conditions")
                .into_iter()
                .find(|c| c.s("type") == ct)
                .map(|c| c.s("status").to_string())
                .unwrap_or_else(|| "<none>".to_string());
            w.write(L1, &format!("{ct} \t{st} \n"));
        }
    }
    let conditions = status.arr("conditions");
    if !conditions.is_empty() {
        w.write(L0, "Conditions:\n  Type\tStatus\n");
        for c in conditions {
            w.write(L1, &format!("{} \t{} \n", c.s("type"), c.s("status")));
        }
    }
    describe_volumes(spec.arr("volumes"), &mut w, "");
    w.write(L0, &format!("QoS Class:\t{}\n", pod_qos(pod)));
    print_labels_multiline(&mut w, "Node-Selectors", &spec.smap("nodeSelector"));
    print_tolerations_multiline(&mut w, "", "Tolerations", spec.arr("tolerations"));
    describe_topology_spread_constraints(spec.arr("topologySpreadConstraints"), &mut w, "");
    describe_workload_reference(spec, &mut w, "");
    describe_events(ctx, &mut w, events);
    Ok(w.finish())
}

/// describeTopologySpreadConstraints.
fn describe_topology_spread_constraints(tscs: Vec<J>, w: &mut PrefixWriter, space: &str) {
    if tscs.is_empty() {
        return;
    }
    let mut tscs = tscs;
    sort_by(&mut tscs, |a, b| a.s("topologyKey") < b.s("topologyKey"));
    w.write(L0, &format!("{space}Topology Spread Constraints:\t"));
    for (i, t) in tscs.iter().enumerate() {
        if i != 0 {
            w.write(L0, space);
            w.write(L0, "\t");
        }
        w.write(L0, &format!("{}:", t.s("topologyKey")));
        w.write(L0, t.s("whenUnsatisfiable"));
        w.write(
            L0,
            &format!(" when max skew {} is exceeded", t.i("maxSkew")),
        );
        if t.exists("labelSelector") {
            w.write(
                L0,
                &format!(
                    " for selector {}",
                    format_label_selector(t.get("labelSelector"))
                ),
            );
        }
        w.write(L0, "\n");
    }
}

/// describeResources: limits then requests, each sorted by name.
pub(crate) fn describe_resources(res: J, w: &mut PrefixWriter, level: usize) {
    if res.is_null() {
        return;
    }
    for (title, key) in [("Limits", "limits"), ("Requests", "requests")] {
        let keys = res.keys(key);
        if !keys.is_empty() {
            w.write(level, &format!("{title}:\n"));
        }
        for name in keys {
            let q = res.get(key).key(name).as_str();
            w.write(level + 1, &format!("{name}:\t{}\n", canonical(q)));
        }
    }
}

/// describeContainers. `pod` is the env resolver (None for templates).
pub(crate) fn describe_containers(
    ctx: &Ctx,
    label: &str,
    containers: Vec<J>,
    statuses: Vec<J>,
    pod: Option<J>,
    w: &mut PrefixWriter,
    space: &str,
) {
    let statuses: HashMap<&str, J> = statuses.into_iter().map(|s| (s.s("name"), s)).collect();
    let none = if containers.is_empty() { " <none>" } else { "" };
    w.write(L0, &format!("{space}{label}:{none}\n"));
    for c in containers {
        let status = statuses.get(c.s("name")).copied();
        describe_container_basic_info(c, status, space, w);
        describe_container_command(c, w);
        if let Some(st) = status {
            describe_container_state(ctx, st, w);
        }
        describe_resources(c.get("resources"), w, L2);
        describe_container_probes(c, w);
        if !c.arr("envFrom").is_empty() {
            describe_container_env_from(c, w);
        }
        describe_container_env_vars(c, pod, w);
        describe_container_volumes(c, w);
    }
}

fn describe_container_basic_info(c: J, status: Option<J>, space: &str, w: &mut PrefixWriter) {
    let name_indent = if space.is_empty() { "" } else { " " };
    w.write(L1, &format!("{name_indent}{}:\n", c.s("name")));
    if let Some(st) = status {
        w.write(L2, &format!("Container ID:\t{}\n", st.s("containerID")));
    }
    w.write(L2, &format!("Image:\t{}\n", c.s("image")));
    if let Some(st) = status {
        w.write(L2, &format!("Image ID:\t{}\n", st.s("imageID")));
    }
    let ports = c.arr("ports");
    let port_string = container_ports(&ports, "containerPort");
    if port_string.contains(',') {
        w.write(L2, &format!("Ports:\t{port_string}\n"));
    } else {
        w.write(L2, &format!("Port:\t{}\n", string_or_none(&port_string)));
    }
    let host_ports = container_ports(&ports, "hostPort");
    if host_ports.contains(',') {
        w.write(L2, &format!("Host Ports:\t{host_ports}\n"));
    } else {
        w.write(
            L2,
            &format!("Host Port:\t{}\n", string_or_none(&host_ports)),
        );
    }
    let seccomp = c.get("securityContext.seccompProfile");
    if !seccomp.is_null() {
        w.write(L2, &format!("SeccompProfile:\t{}\n", seccomp.s("type")));
        if seccomp.s("type") == "Localhost" {
            w.write(
                L3,
                &format!("LocalhostProfile:\t{}\n", seccomp.s("localhostProfile")),
            );
        }
    }
}

fn container_ports(ports: &[J], field: &str) -> String {
    ports
        .iter()
        .map(|p| {
            let mut s = format!("{}/{}", p.i(field), p.s("protocol"));
            if !p.s("name").is_empty() {
                s = format!("{s} ({})", p.s("name"));
            }
            s
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn describe_container_command(c: J, w: &mut PrefixWriter) {
    for (title, key) in [("Command", "command"), ("Args", "args")] {
        let items = c.strs(key);
        if items.is_empty() {
            continue;
        }
        w.write(L2, &format!("{title}:\n"));
        for item in items {
            for s in item.split('\n') {
                w.write(L3, &format!("{s}\n"));
            }
        }
    }
}

fn describe_container_state(ctx: &Ctx, st: J, w: &mut PrefixWriter) {
    describe_status(ctx, "State", st.get("state"), w);
    if st.exists("lastState.terminated") {
        describe_status(ctx, "Last State", st.get("lastState"), w);
    }
    w.write(L2, &format!("Ready:\t{}\n", print_bool(st.b("ready"))));
    w.write(L2, &format!("Restart Count:\t{}\n", st.i("restartCount")));
}

fn describe_status(ctx: &Ctx, name: &str, state: J, w: &mut PrefixWriter) {
    if state.exists("running") {
        w.write(L2, &format!("{name}:\tRunning\n"));
        w.write(
            L3,
            &format!(
                "Started:\t{}\n",
                ctx.rfc1123z(state.get("running.startedAt"))
            ),
        );
    } else if state.exists("waiting") {
        w.write(L2, &format!("{name}:\tWaiting\n"));
        let reason = state.s("waiting.reason");
        if !reason.is_empty() {
            w.write(L3, &format!("Reason:\t{reason}\n"));
        }
    } else if state.exists("terminated") {
        let t = state.get("terminated");
        w.write(L2, &format!("{name}:\tTerminated\n"));
        if !t.s("reason").is_empty() {
            w.write(L3, &format!("Reason:\t{}\n", t.s("reason")));
        }
        if !t.s("message").is_empty() {
            w.write(L3, &format!("Message:\t{}\n", t.s("message")));
        }
        w.write(L3, &format!("Exit Code:\t{}\n", t.i("exitCode")));
        if t.i("signal") > 0 {
            w.write(L3, &format!("Signal:\t{}\n", t.i("signal")));
        }
        w.write(
            L3,
            &format!("Started:\t{}\n", ctx.rfc1123z(t.get("startedAt"))),
        );
        w.write(
            L3,
            &format!("Finished:\t{}\n", ctx.rfc1123z(t.get("finishedAt"))),
        );
    } else {
        w.write(L2, &format!("{name}:\tWaiting\n"));
    }
}

fn describe_container_probes(c: J, w: &mut PrefixWriter) {
    for (title, key) in [
        ("Liveness", "livenessProbe"),
        ("Readiness", "readinessProbe"),
        ("Startup", "startupProbe"),
    ] {
        if c.exists(key) {
            w.write(L2, &format!("{title}:\t{}\n", describe_probe(c.get(key))));
        }
    }
}

/// DescribeProbe.
pub(crate) fn describe_probe(p: J) -> String {
    let attrs = format!(
        "delay={}s timeout={}s period={}s #success={} #failure={}",
        p.i("initialDelaySeconds"),
        p.i("timeoutSeconds"),
        p.i("periodSeconds"),
        p.i("successThreshold"),
        p.i("failureThreshold")
    );
    if p.exists("exec") {
        return format!("exec {} {attrs}", go_slice(&p.strs("exec.command")));
    }
    if p.exists("httpGet") {
        let h = p.get("httpGet");
        let scheme = h.s("scheme").to_lowercase();
        let port = intstr(h.get("port"));
        let host = if port.is_empty() {
            h.s("host").to_string()
        } else {
            join_host_port(h.s("host"), &port)
        };
        return format!(
            "http-get {} {attrs}",
            url_string(&scheme, &host, h.s("path"))
        );
    }
    if p.exists("tcpSocket") {
        let t = p.get("tcpSocket");
        return format!(
            "tcp-socket {}:{} {attrs}",
            t.s("host"),
            intstr(t.get("port"))
        );
    }
    if p.exists("grpc") {
        let g = p.get("grpc");
        return format!("grpc <pod>:{} {} {attrs}", g.i("port"), g.s("service"));
    }
    format!("unknown {attrs}")
}

/// url.URL{Scheme, Host, Path}.String().
fn url_string(scheme: &str, host: &str, path: &str) -> String {
    let mut s = String::new();
    if !scheme.is_empty() {
        s.push_str(scheme);
        s.push(':');
    }
    if !scheme.is_empty() || !host.is_empty() {
        s.push_str("//");
        s.push_str(host);
    }
    let escaped = escape_path(path);
    if !escaped.is_empty() && !escaped.starts_with('/') && !host.is_empty() {
        s.push('/');
    }
    s.push_str(&escaped);
    s
}

/// url.PathEscape-like escaping used by URL.EscapedPath (encodePath mode).
fn escape_path(path: &str) -> String {
    let mut out = String::new();
    for b in path.bytes() {
        let keep = b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'-' | b'_'
                    | b'.'
                    | b'~'
                    | b'$'
                    | b'&'
                    | b'+'
                    | b','
                    | b'/'
                    | b':'
                    | b';'
                    | b'='
                    | b'@'
            );
        if keep {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn describe_container_env_from(c: J, w: &mut PrefixWriter) {
    w.write(L2, "Environment Variables from:\n");
    for e in c.arr("envFrom") {
        let (from, name, optional) = if e.exists("configMapRef") {
            (
                "ConfigMap",
                e.s("configMapRef.name"),
                e.b("configMapRef.optional"),
            )
        } else if e.exists("secretRef") {
            ("Secret", e.s("secretRef.name"), e.b("secretRef.optional"))
        } else {
            ("", "", false)
        };
        let prefix = e.s("prefix");
        if prefix.is_empty() {
            w.write(L3, &format!("{name}\t{from}\tOptional: {optional}\n"));
        } else {
            w.write(
                L3,
                &format!("{name}\t{from} with prefix '{prefix}'\tOptional: {optional}\n"),
            );
        }
    }
}

fn describe_container_env_vars(c: J, pod: Option<J>, w: &mut PrefixWriter) {
    let env = c.arr("env");
    let none = if env.is_empty() { "\t<none>" } else { "" };
    w.write(L2, &format!("Environment:{none}\n"));
    for e in env {
        let name = e.s("name");
        let vf = e.get("valueFrom");
        if vf.is_null() {
            for (i, s) in e.s("value").split('\n').enumerate() {
                if i == 0 {
                    w.write(L3, &format!("{name}:\t{s}\n"));
                } else {
                    w.write(L3, &format!("\t{s}\n"));
                }
            }
            continue;
        }
        if vf.exists("fieldRef") {
            let f = vf.get("fieldRef");
            let value = pod
                .map(|p| resolve_field_ref(p, f.s("apiVersion"), f.s("fieldPath")))
                .unwrap_or_default();
            w.write(
                L3,
                &format!(
                    "{name}:\t{value} ({}:{})\n",
                    f.s("apiVersion"),
                    f.s("fieldPath")
                ),
            );
        } else if vf.exists("resourceFieldRef") {
            let r = vf.get("resourceFieldRef");
            let resource = r.s("resource");
            let mut value = container_resource_value(r, c).unwrap_or_default();
            if value == "0" && (resource == "limits.cpu" || resource == "limits.memory") {
                value = "node allocatable".to_string();
            }
            w.write(L3, &format!("{name}:\t{value} ({resource})\n"));
        } else if vf.exists("secretKeyRef") {
            let s = vf.get("secretKeyRef");
            w.write(
                L3,
                &format!(
                    "{name}:\t<set to the key '{}' in secret '{}'>\tOptional: {}\n",
                    s.s("key"),
                    s.s("name"),
                    s.b("optional")
                ),
            );
        } else if vf.exists("configMapKeyRef") {
            let s = vf.get("configMapKeyRef");
            w.write(
                L3,
                &format!(
                    "{name}:\t<set to the key '{}' of config map '{}'>\tOptional: {}\n",
                    s.s("key"),
                    s.s("name"),
                    s.b("optional")
                ),
            );
        }
    }
}

/// EnvValueRetriever: only metadata field paths resolve (others are "").
fn resolve_field_ref(pod: J, api_version: &str, field_path: &str) -> String {
    let (path, subscript) = split_subscripted(field_path);
    let supported = if api_version == "v1" {
        matches!(
            path,
            "metadata.name"
                | "metadata.namespace"
                | "metadata.uid"
                | "metadata.labels"
                | "metadata.annotations"
                | "spec.nodeName"
                | "spec.restartPolicy"
                | "spec.schedulerName"
                | "spec.serviceAccountName"
                | "spec.hostNetwork"
                | "status.phase"
                | "status.podIP"
                | "status.podIPs"
                | "status.nominatedNodeName"
                | "status.hostIP"
                | "status.hostIPs"
                | "spec.host"
        )
    } else {
        subscript.is_none() && matches!(path, "metadata.name" | "metadata.namespace")
    };
    if !supported {
        return String::new();
    }
    let meta = pod.get("metadata");
    if let Some(sub) = subscript {
        return match path {
            "metadata.annotations" => meta.get("annotations").key(sub).as_str().to_string(),
            "metadata.labels" => meta.get("labels").key(sub).as_str().to_string(),
            _ => String::new(),
        };
    }
    match path {
        "metadata.annotations" => format_map(&meta.smap("annotations")),
        "metadata.labels" => format_map(&meta.smap("labels")),
        "metadata.name" => meta.s("name").to_string(),
        "metadata.namespace" => meta.s("namespace").to_string(),
        "metadata.uid" => meta.s("uid").to_string(),
        _ => String::new(),
    }
}

/// fieldpath.SplitMaybeSubscriptedPath.
fn split_subscripted(fp: &str) -> (&str, Option<&str>) {
    if let Some(s) = fp.strip_suffix("']") {
        if let Some((path, sub)) = s.split_once("['") {
            if !path.is_empty() {
                return (path, Some(sub));
            }
        }
    }
    (fp, None)
}

/// fieldpath.FormatMap: `k="v"` lines.
fn format_map(m: &BTreeMap<String, String>) -> String {
    m.iter()
        .map(|(k, v)| format!("{k}={}", go_quote(v)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// strconv.Quote.
pub(crate) fn go_quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\x07' => out.push_str("\\a"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            '\x0b' => out.push_str("\\v"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// resourcehelper.ExtractContainerResourceValue against `c` (kubectl uses the
/// container being described, not `containerName`).
fn container_resource_value(r: J, c: J) -> Option<String> {
    let divisor_raw = r.s("divisor");
    let mut divisor = parse_or_zero(divisor_raw);
    if divisor.is_zero() {
        divisor = Quantity::new(1, Format::DecimalSI);
    }
    let res = c.get("resources");
    let get = |kind: &str, name: &str| parse_or_zero(res.get(kind).key(name).as_str());
    let ceil_div =
        |a: i64, b: i64| -> String { (((a as f64) / (b as f64)).ceil() as i64).to_string() };
    let resource = r.s("resource");
    let v = match resource {
        "limits.cpu" => ceil_div(get("limits", "cpu").milli_value(), divisor.milli_value()),
        "requests.cpu" => ceil_div(get("requests", "cpu").milli_value(), divisor.milli_value()),
        "limits.memory" => ceil_div(get("limits", "memory").value(), divisor.value()),
        "requests.memory" => ceil_div(get("requests", "memory").value(), divisor.value()),
        "limits.ephemeral-storage" => {
            ceil_div(get("limits", "ephemeral-storage").value(), divisor.value())
        }
        "requests.ephemeral-storage" => ceil_div(
            get("requests", "ephemeral-storage").value(),
            divisor.value(),
        ),
        other => {
            if let Some(n) = other.strip_prefix("requests.") {
                if n.starts_with("hugepages-") {
                    return Some(ceil_div(get("requests", n).value(), divisor.value()));
                }
            }
            if let Some(n) = other.strip_prefix("limits.") {
                if n.starts_with("hugepages-") {
                    return Some(ceil_div(get("limits", n).value(), divisor.value()));
                }
            }
            return None;
        }
    };
    Some(v)
}

fn describe_container_volumes(c: J, w: &mut PrefixWriter) {
    let mut mounts = c.arr("volumeMounts");
    let none = if mounts.is_empty() { "\t<none>" } else { "" };
    w.write(L2, &format!("Mounts:{none}\n"));
    sort_by(&mut mounts, |a, b| a.s("mountPath") < b.s("mountPath"));
    for m in mounts {
        let mut flags = vec![if m.b("readOnly") { "ro" } else { "rw" }.to_string()];
        if !m.s("subPath").is_empty() {
            flags.push(format!("path={}", go_quote(m.s("subPath"))));
        }
        w.write(
            L3,
            &format!(
                "{} from {} ({})\n",
                m.s("mountPath"),
                m.s("name"),
                flags.join(",")
            ),
        );
    }
    let mut devices = c.arr("volumeDevices");
    if !devices.is_empty() {
        w.write(L2, &format!("Devices:{none}\n"));
        sort_by(&mut devices, |a, b| a.s("devicePath") < b.s("devicePath"));
        for d in devices {
            w.write(L3, &format!("{} from {}\n", d.s("devicePath"), d.s("name")));
        }
    }
}

/// DescribePodTemplate.
pub(crate) fn describe_pod_template(ctx: &Ctx, template: J, w: &mut PrefixWriter) {
    w.write(L0, "Pod Template:\n");
    if template.is_null() {
        w.write(L1, "<unset>");
        return;
    }
    let (meta, spec) = (template.get("metadata"), template.get("spec"));
    print_labels_multiline(w, "  Labels", &meta.smap("labels"));
    let annotations = meta.smap("annotations");
    if !annotations.is_empty() {
        print_annotations_multiline(w, "  Annotations", &annotations);
    }
    if !spec.s("serviceAccountName").is_empty() {
        w.write(
            L1,
            &format!("Service Account:\t{}\n", spec.s("serviceAccountName")),
        );
    }
    let init = spec.arr("initContainers");
    if !init.is_empty() {
        describe_containers(ctx, "Init Containers", init, Vec::new(), None, w, "  ");
    }
    describe_containers(
        ctx,
        "Containers",
        spec.arr("containers"),
        Vec::new(),
        None,
        w,
        "  ",
    );
    describe_volumes(spec.arr("volumes"), w, "  ");
    describe_topology_spread_constraints(spec.arr("topologySpreadConstraints"), w, "  ");
    if !spec.s("priorityClassName").is_empty() {
        w.write(
            L1,
            &format!("Priority Class Name:\t{}\n", spec.s("priorityClassName")),
        );
    }
    print_labels_multiline(w, "  Node-Selectors", &spec.smap("nodeSelector"));
    print_tolerations_multiline(w, "  ", "Tolerations", spec.arr("tolerations"));
    describe_workload_reference(spec, w, "  ");
}

/// describeWorkloadReference (kubectl v0.35).
fn describe_workload_reference(spec: J, w: &mut PrefixWriter, space: &str) {
    if !spec.exists("workloadRef") {
        return;
    }
    let r = spec.get("workloadRef");
    w.write(L0, &format!("{space}WorkloadRef:\n"));
    w.write(L1, &format!("Name:\t{}\n", r.s("name")));
    w.write(L1, &format!("PodGroup:\t{}\n", r.s("podGroup")));
    if !r.s("podGroupReplicaKey").is_empty() {
        w.write(
            L1,
            &format!("PodGroupReplicaKey:\t{}\n", r.s("podGroupReplicaKey")),
        );
    }
}

/// qos.GetPodQOS.
pub(crate) fn pod_qos(pod: J) -> String {
    let q = pod.s("status.qosClass");
    if !q.is_empty() {
        return q.to_string();
    }
    compute_pod_qos(pod).to_string()
}

fn compute_pod_qos(pod: J) -> &'static str {
    let zero = Quantity::new(0, Format::DecimalSI);
    let supported = |n: &str| n == "cpu" || n == "memory";
    let mut requests: BTreeMap<String, Quantity> = BTreeMap::new();
    let mut limits: BTreeMap<String, Quantity> = BTreeMap::new();
    let mut guaranteed = true;
    let process = |list: &mut BTreeMap<String, Quantity>, src: J| {
        for name in src.keys("") {
            if !supported(name) {
                continue;
            }
            let q = parse_or_zero(src.key(name).as_str());
            if q.cmp(&zero).is_gt() {
                list.entry(name.to_string())
                    .and_modify(|e| {
                        let mut d = q;
                        d.add(e);
                        *e = d;
                    })
                    .or_insert(q);
            }
        }
    };
    let qos_limits_complete = |src: J| {
        let has = |n: &str| parse_or_zero(src.key(n).as_str()).cmp(&zero).is_gt();
        has("memory") && has("cpu")
    };
    let spec = pod.get("spec");
    if spec.exists("resources") {
        let r = spec.get("resources");
        if r.exists("requests") {
            process(&mut requests, r.get("requests"));
        }
        if r.exists("limits") {
            process(&mut limits, r.get("limits"));
            if !qos_limits_complete(r.get("limits")) {
                guaranteed = false;
            }
        }
    } else {
        let mut all = spec.arr("containers");
        all.extend(spec.arr("initContainers"));
        for c in all {
            process(&mut requests, c.get("resources.requests"));
            process(&mut limits, c.get("resources.limits"));
            if !qos_limits_complete(c.get("resources.limits")) {
                guaranteed = false;
            }
        }
    }
    if requests.is_empty() && limits.is_empty() {
        return "BestEffort";
    }
    if guaranteed {
        for (name, req) in &requests {
            match limits.get(name) {
                Some(l) if l.cmp(req).is_eq() => {}
                _ => {
                    guaranteed = false;
                    break;
                }
            }
        }
    }
    if guaranteed && requests.len() == limits.len() {
        return "Guaranteed";
    }
    "Burstable"
}

/// resourcehelper.PodRequestsAndLimits.
pub(crate) fn pod_requests_and_limits(
    pod: J,
) -> (BTreeMap<String, Quantity>, BTreeMap<String, Quantity>) {
    (pod_requests(pod), pod_limits(pod))
}

fn add_list(list: &mut BTreeMap<String, Quantity>, new: &BTreeMap<String, Quantity>) {
    for (k, q) in new {
        match list.get_mut(k) {
            Some(v) => v.add(q),
            None => {
                list.insert(k.clone(), *q);
            }
        }
    }
}

fn max_list(list: &mut BTreeMap<String, Quantity>, new: &BTreeMap<String, Quantity>) {
    for (k, q) in new {
        match list.get(k) {
            Some(v) if !q.cmp(v).is_gt() => {}
            _ => {
                list.insert(k.clone(), *q);
            }
        }
    }
}

fn qlist(j: J) -> BTreeMap<String, Quantity> {
    j.keys("")
        .into_iter()
        .map(|k| (k.to_string(), parse_or_zero(j.key(k).as_str())))
        .collect()
}

fn pod_requests(pod: J) -> BTreeMap<String, Quantity> {
    let spec = pod.get("spec");
    let status = pod.get("status");
    let mut statuses: HashMap<&str, J> = HashMap::new();
    for s in status.arr("containerStatuses") {
        statuses.insert(s.s("name"), s);
    }
    for s in status.arr("initContainerStatuses") {
        statuses.insert(s.s("name"), s);
    }
    let container_reqs = |c: J| -> BTreeMap<String, Quantity> {
        let base = qlist(c.get("resources.requests"));
        match statuses.get(c.s("name")) {
            Some(cs) if cs.exists("resources") => {
                // determineContainerReqs: max(spec, status, allocated); when the
                // resize is infeasible the spec requests are ignored.
                let infeasible = pod_resize_infeasible(pod);
                let mut out = if infeasible {
                    qlist(cs.get("resources.requests"))
                } else {
                    let mut m = base.clone();
                    max_list(&mut m, &qlist(cs.get("resources.requests")));
                    m
                };
                max_list(&mut out, &qlist(cs.get("allocatedResources")));
                out
            }
            _ => base,
        }
    };
    let mut reqs = BTreeMap::new();
    for c in spec.arr("containers") {
        add_list(&mut reqs, &container_reqs(c));
    }
    let mut restartable = BTreeMap::new();
    let mut init_reqs = BTreeMap::new();
    for c in spec.arr("initContainers") {
        let mut creqs = qlist(c.get("resources.requests"));
        if c.s("restartPolicy") == "Always" {
            creqs = container_reqs(c);
            add_list(&mut reqs, &creqs);
            add_list(&mut restartable, &creqs);
            creqs = restartable.clone();
        } else {
            let mut tmp = BTreeMap::new();
            add_list(&mut tmp, &creqs);
            add_list(&mut tmp, &restartable);
            creqs = tmp;
        }
        max_list(&mut init_reqs, &creqs);
    }
    max_list(&mut reqs, &init_reqs);
    if spec.exists("overhead") {
        add_list(&mut reqs, &qlist(spec.get("overhead")));
    }
    reqs
}

/// helpers.IsPodResizeInfeasible: the PodResizePending condition with reason Infeasible.
fn pod_resize_infeasible(pod: J) -> bool {
    pod.arr("status.conditions")
        .into_iter()
        .any(|c| c.s("type") == "PodResizePending" && c.s("reason") == "Infeasible")
}

fn pod_limits(pod: J) -> BTreeMap<String, Quantity> {
    let spec = pod.get("spec");
    let mut limits = BTreeMap::new();
    for c in spec.arr("containers") {
        add_list(&mut limits, &qlist(c.get("resources.limits")));
    }
    let mut restartable = BTreeMap::new();
    let mut init_limits = BTreeMap::new();
    for c in spec.arr("initContainers") {
        let mut cl = qlist(c.get("resources.limits"));
        if c.s("restartPolicy") == "Always" {
            add_list(&mut limits, &cl);
            add_list(&mut restartable, &cl);
            cl = restartable.clone();
        } else {
            let mut tmp = BTreeMap::new();
            add_list(&mut tmp, &cl);
            add_list(&mut tmp, &restartable);
            cl = tmp;
        }
        max_list(&mut init_limits, &cl);
    }
    max_list(&mut limits, &init_limits);
    if spec.exists("overhead") {
        for (name, q) in qlist(spec.get("overhead")) {
            if let Some(v) = limits.get_mut(&name) {
                if !v.is_zero() {
                    v.add(&q);
                }
            }
        }
    }
    limits
}
