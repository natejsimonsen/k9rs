//! Networking describers: Service, Endpoints, EndpointSlice, Ingress,
//! IngressClass, ServiceCIDR, IPAddress and NetworkPolicy.

use std::collections::BTreeSet;

use anyhow::Result;
use serde_json::Value;

use super::events::describe_events;
use super::util::{
    format_label_selector, format_labels, intstr, intstr_ptr, join_host_port,
    print_annotations_multiline, print_labels_multiline, Ctx, J,
};
use super::writer::{PrefixWriter, L0, L1, L2, L3};

const SERVICE_NAME_LABEL: &str = "kubernetes.io/service-name";

/// EndpointSlices of service `name` in `ns` from the related objects.
fn slices_for<'a>(ctx: &'a Ctx, ns: &str, name: &str) -> Vec<J<'a>> {
    ctx.rel
        .endpoint_slices
        .as_ref()
        .map(|l| {
            l.iter()
                .map(J)
                .filter(|s| s.s("metadata.namespace") == ns)
                .filter(|s| s.get("metadata.labels").key(SERVICE_NAME_LABEL).as_str() == name)
                .collect()
        })
        .unwrap_or_default()
}

/// formatEndpointSlices: at most 3 ready endpoints for the named ports.
fn format_endpoint_slices(slices: &[J], ports: Option<&BTreeSet<String>>) -> String {
    if slices.is_empty() {
        return "<none>".to_string();
    }
    const MAX: usize = 3;
    let mut list: Vec<String> = Vec::new();
    let mut more = false;
    let mut count = 0usize;
    let ready = |e: &J| e.opt_b("conditions.ready").unwrap_or(true);
    let first_addr = |e: &J| e.strs("addresses").into_iter().next().unwrap_or_default();
    for s in slices {
        let sports = s.arr("ports");
        if sports.is_empty() {
            for e in s.arr("endpoints") {
                if list.len() == MAX {
                    more = true;
                }
                if !ready(&e) {
                    continue;
                }
                if !more {
                    list.push(first_addr(&e));
                }
                count += 1;
            }
        } else {
            for p in sports {
                if ports.is_none_or(|set| set.contains(p.s("name"))) {
                    for e in s.arr("endpoints") {
                        if list.len() == MAX {
                            more = true;
                        }
                        if !ready(&e) {
                            continue;
                        }
                        if !more {
                            list.push(join_host_port(&first_addr(&e), &p.i("port").to_string()));
                        }
                        count += 1;
                    }
                }
            }
        }
    }
    let ret = list.join(",");
    if more {
        return format!("{ret} + {} more...", count - MAX);
    }
    ret
}

fn build_ingress_string(ingress: &[J]) -> String {
    let mut out = String::new();
    for (i, ing) in ingress.iter().enumerate() {
        if i != 0 {
            out.push_str(", ");
        }
        if !ing.s("ip").is_empty() {
            out.push_str(ing.s("ip"));
            if let Some(mode) = ing.opt_s("ipMode") {
                out.push_str(&format!(" ({mode})"));
            }
        } else {
            out.push_str(ing.s("hostname"));
        }
    }
    out
}

pub(crate) fn describe_service(ctx: &Ctx, svc: J, events: &[Value]) -> Result<String> {
    let (meta, spec, status) = (svc.get("metadata"), svc.get("spec"), svc.get("status"));
    let slices = slices_for(ctx, meta.s("namespace"), meta.s("name"));
    let mut w = PrefixWriter::new();
    w.write(L0, &format!("Name:\t{}\n", meta.s("name")));
    w.write(L0, &format!("Namespace:\t{}\n", meta.s("namespace")));
    print_labels_multiline(&mut w, "Labels", &meta.smap("labels"));
    print_annotations_multiline(&mut w, "Annotations", &meta.smap("annotations"));
    w.write(
        L0,
        &format!("Selector:\t{}\n", format_labels(&spec.smap("selector"))),
    );
    w.write(L0, &format!("Type:\t{}\n", spec.s("type")));
    if let Some(p) = spec.opt_s("ipFamilyPolicy") {
        w.write(L0, &format!("IP Family Policy:\t{p}\n"));
    }
    let families = spec.strs("ipFamilies");
    if families.is_empty() {
        w.write(L0, "IP Families:\t<none>\n");
    } else {
        w.write(L0, &format!("IP Families:\t{}\n", families.join(",")));
    }
    w.write(L0, &format!("IP:\t{}\n", spec.s("clusterIP")));
    let ips = spec.strs("clusterIPs");
    if ips.is_empty() {
        w.write(L0, "IPs:\t<none>\n");
    } else {
        w.write(L0, &format!("IPs:\t{}\n", ips.join(",")));
    }
    let external = spec.strs("externalIPs");
    if !external.is_empty() {
        w.write(L0, &format!("External IPs:\t{}\n", external.join(",")));
    }
    if !spec.s("loadBalancerIP").is_empty() {
        w.write(
            L0,
            &format!("Desired LoadBalancer IP:\t{}\n", spec.s("loadBalancerIP")),
        );
    }
    if !spec.s("externalName").is_empty() {
        w.write(L0, &format!("External Name:\t{}\n", spec.s("externalName")));
    }
    let lb = status.arr("loadBalancer.ingress");
    if !lb.is_empty() {
        w.write(
            L0,
            &format!("LoadBalancer Ingress:\t{}\n", build_ingress_string(&lb)),
        );
    }
    for sp in spec.arr("ports") {
        let name = if sp.s("name").is_empty() {
            "<unset>"
        } else {
            sp.s("name")
        };
        let proto = sp.s("protocol");
        w.write(L0, &format!("Port:\t{name}\t{}/{proto}\n", sp.i("port")));
        w.write(
            L0,
            &format!("TargetPort:\t{}/{proto}\n", intstr(sp.get("targetPort"))),
        );
        if sp.i("nodePort") != 0 {
            w.write(
                L0,
                &format!("NodePort:\t{name}\t{}/{proto}\n", sp.i("nodePort")),
            );
        }
        let ports: BTreeSet<String> = [sp.s("name").to_string()].into_iter().collect();
        w.write(
            L0,
            &format!(
                "Endpoints:\t{}\n",
                format_endpoint_slices(&slices, Some(&ports))
            ),
        );
    }
    w.write(
        L0,
        &format!("Session Affinity:\t{}\n", spec.s("sessionAffinity")),
    );
    if !spec.s("externalTrafficPolicy").is_empty() {
        w.write(
            L0,
            &format!(
                "External Traffic Policy:\t{}\n",
                spec.s("externalTrafficPolicy")
            ),
        );
    }
    if let Some(p) = spec.opt_s("internalTrafficPolicy") {
        w.write(L0, &format!("Internal Traffic Policy:\t{p}\n"));
    }
    if spec.i("healthCheckNodePort") != 0 {
        w.write(
            L0,
            &format!("HealthCheck NodePort:\t{}\n", spec.i("healthCheckNodePort")),
        );
    }
    let ranges = spec.strs("loadBalancerSourceRanges");
    if !ranges.is_empty() {
        w.write(
            L0,
            &format!("LoadBalancer Source Ranges:\t{}\n", ranges.join(",")),
        );
    }
    if let Some(t) = spec.opt_s("trafficDistribution") {
        w.write(L0, &format!("Traffic Distribution:\t{t}\n"));
    }
    describe_events(ctx, &mut w, events);
    Ok(w.finish())
}

pub(crate) fn describe_endpoints(ctx: &Ctx, ep: J, events: &[Value]) -> Result<String> {
    let meta = ep.get("metadata");
    let mut w = PrefixWriter::new();
    w.write(L0, &format!("Name:\t{}\n", meta.s("name")));
    w.write(L0, &format!("Namespace:\t{}\n", meta.s("namespace")));
    print_labels_multiline(&mut w, "Labels", &meta.smap("labels"));
    print_annotations_multiline(&mut w, "Annotations", &meta.smap("annotations"));
    w.write(L0, "Subsets:\n");
    for subset in ep.arr("subsets") {
        let join_ips = |key: &str| {
            let ips: Vec<&str> = subset.arr(key).into_iter().map(|a| a.s("ip")).collect();
            let s = ips.join(",");
            if s.is_empty() {
                "<none>".to_string()
            } else {
                s
            }
        };
        w.write(L1, &format!("Addresses:\t{}\n", join_ips("addresses")));
        w.write(
            L1,
            &format!("NotReadyAddresses:\t{}\n", join_ips("notReadyAddresses")),
        );
        let ports = subset.arr("ports");
        if !ports.is_empty() {
            w.write(L1, "Ports:\n");
            w.write(L2, "Name\tPort\tProtocol\n");
            w.write(L2, "----\t----\t--------\n");
            for p in ports {
                let name = if p.s("name").is_empty() {
                    "<unset>"
                } else {
                    p.s("name")
                };
                w.write(
                    L2,
                    &format!("{name}\t{}\t{}\n", p.i("port"), p.s("protocol")),
                );
            }
        }
        w.write(L0, "\n");
    }
    describe_events(ctx, &mut w, events);
    Ok(w.finish())
}

pub(crate) fn describe_endpoint_slice(ctx: &Ctx, eps: J, events: &[Value]) -> Result<String> {
    let meta = eps.get("metadata");
    let mut w = PrefixWriter::new();
    w.write(L0, &format!("Name:\t{}\n", meta.s("name")));
    w.write(L0, &format!("Namespace:\t{}\n", meta.s("namespace")));
    print_labels_multiline(&mut w, "Labels", &meta.smap("labels"));
    print_annotations_multiline(&mut w, "Annotations", &meta.smap("annotations"));
    w.write(L0, &format!("AddressType:\t{}\n", eps.s("addressType")));
    let ports = eps.arr("ports");
    if ports.is_empty() {
        w.write(L0, "Ports: <unset>\n");
    } else {
        w.write(L0, "Ports:\n");
        w.write(L1, "Name\tPort\tProtocol\n");
        w.write(L1, "----\t----\t--------\n");
        for p in ports {
            let name = match p.opt_s("name") {
                Some(n) if !n.is_empty() => n,
                _ => "<unset>",
            };
            let num = p
                .opt_i("port")
                .map(|n| n.to_string())
                .unwrap_or_else(|| "<unset>".to_string());
            w.write(L1, &format!("{name}\t{num}\t{}\n", p.s("protocol")));
        }
    }
    let endpoints = eps.arr("endpoints");
    if endpoints.is_empty() {
        w.write(L0, "Endpoints: <none>\n");
    } else {
        w.write(L0, "Endpoints:\n");
        for e in endpoints {
            let addrs = e.strs("addresses").join(", ");
            let addrs = if addrs.is_empty() {
                "<none>".to_string()
            } else {
                addrs
            };
            w.write(L1, &format!("- Addresses:\t{addrs}\n"));
            w.write(L2, "Conditions:\n");
            let ready = e
                .opt_b("conditions.ready")
                .map(|b| b.to_string())
                .unwrap_or_else(|| "<unset>".to_string());
            w.write(L3, &format!("Ready:\t{ready}\n"));
            w.write(
                L2,
                &format!("Hostname:\t{}\n", e.opt_s("hostname").unwrap_or("<unset>")),
            );
            if e.exists("targetRef") {
                w.write(
                    L2,
                    &format!(
                        "TargetRef:\t{}/{}\n",
                        e.s("targetRef.kind"),
                        e.s("targetRef.name")
                    ),
                );
            }
            w.write(
                L2,
                &format!("NodeName:\t{}\n", e.opt_s("nodeName").unwrap_or("<unset>")),
            );
            w.write(
                L2,
                &format!("Zone:\t{}\n", e.opt_s("zone").unwrap_or("<unset>")),
            );
        }
    }
    describe_events(ctx, &mut w, events);
    Ok(w.finish())
}

/// ingressLoadBalancerStatusStringerV1 (wide): sorted unique IPs/hostnames.
fn ingress_lb_status(status: J) -> String {
    let mut set = BTreeSet::new();
    for ing in status.arr("loadBalancer.ingress") {
        if !ing.s("ip").is_empty() {
            set.insert(ing.s("ip").to_string());
        } else if !ing.s("hostname").is_empty() {
            set.insert(ing.s("hostname").to_string());
        }
    }
    set.into_iter().collect::<Vec<_>>().join(",")
}

/// describeBackendV1.
fn describe_backend(ctx: &Ctx, ns: &str, backend: J) -> String {
    if backend.exists("service") {
        let svc = backend.get("service");
        let name = svc.s("name");
        let number = svc.i("port.number");
        let port_name = svc.s("port.name");
        let sb = if number != 0 {
            format!("{name}:{number}")
        } else {
            format!("{name}:{port_name}")
        };
        let slices = slices_for(ctx, ns, name);
        let sp_name = match &ctx.rel.services {
            Some(list) => {
                let found = list
                    .iter()
                    .map(J)
                    .find(|s| s.s("metadata.namespace") == ns && s.s("metadata.name") == name);
                let Some(service) = found else {
                    return format!("{sb} (<error: services \"{name}\" not found>)");
                };
                let mut sp_name = String::new();
                for sp in service.arr("spec.ports") {
                    let by_number = number != 0 && number == sp.i("port");
                    let by_name = !port_name.is_empty() && port_name == sp.s("name");
                    if by_number || by_name {
                        sp_name = sp.s("name").to_string();
                    }
                }
                sp_name
            }
            None => port_name.to_string(),
        };
        let ports: BTreeSet<String> = [sp_name].into_iter().collect();
        return format!("{sb} ({})", format_endpoint_slices(&slices, Some(&ports)));
    }
    if backend.exists("resource") {
        let r = backend.get("resource");
        let group = r.opt_s("apiGroup").unwrap_or("<none>");
        return format!(
            "APIGroup: {group}, Kind: {}, Name: {}",
            r.s("kind"),
            r.s("name")
        );
    }
    String::new()
}

pub(crate) fn describe_ingress(ctx: &Ctx, ing: J, events: &[Value]) -> Result<String> {
    let (meta, spec) = (ing.get("metadata"), ing.get("spec"));
    let ns = meta.s("namespace");
    let mut w = PrefixWriter::new();
    w.write(L0, &format!("Name:\t{}\n", meta.s("name")));
    print_labels_multiline(&mut w, "Labels", &meta.smap("labels"));
    w.write(L0, &format!("Namespace:\t{ns}\n"));
    w.write(
        L0,
        &format!("Address:\t{}\n", ingress_lb_status(ing.get("status"))),
    );
    w.write(
        L0,
        &format!(
            "Ingress Class:\t{}\n",
            spec.opt_s("ingressClassName").unwrap_or("<none>")
        ),
    );
    let def = spec.get("defaultBackend");
    let default_backend = if def.is_null() {
        "<default>".to_string()
    } else {
        describe_backend(ctx, ns, def)
    };
    w.write(L0, &format!("Default backend:\t{default_backend}\n"));
    let tls = spec.arr("tls");
    if !tls.is_empty() {
        w.write(L0, "TLS:\n");
        for t in tls {
            let hosts = t.strs("hosts").join(",");
            if t.s("secretName").is_empty() {
                w.write(L1, &format!("SNI routes {hosts}\n"));
            } else {
                w.write(L1, &format!("{} terminates {hosts}\n", t.s("secretName")));
            }
        }
    }
    w.write(L0, "Rules:\n  Host\tPath\tBackends\n");
    w.write(L1, "----\t----\t--------\n");
    let mut count = 0;
    for rule in spec.arr("rules") {
        if !rule.exists("http") {
            continue;
        }
        count += 1;
        let host = if rule.s("host").is_empty() {
            "*"
        } else {
            rule.s("host")
        };
        w.write(L1, &format!("{host}\t\n"));
        for path in rule.arr("http.paths") {
            w.write(
                L2,
                &format!(
                    "\t{} \t{}\n",
                    path.s("path"),
                    describe_backend(ctx, ns, path.get("backend"))
                ),
            );
        }
    }
    if count == 0 {
        w.write(L1, &format!("*\t*\t{default_backend}\n"));
    }
    print_annotations_multiline(&mut w, "Annotations", &meta.smap("annotations"));
    describe_events(ctx, &mut w, events);
    Ok(w.finish())
}

pub(crate) fn describe_ingress_class(ctx: &Ctx, ic: J, events: &[Value]) -> Result<String> {
    let meta = ic.get("metadata");
    let mut w = PrefixWriter::new();
    w.write(L0, &format!("Name:\t{}\n", meta.s("name")));
    print_labels_multiline(&mut w, "Labels", &meta.smap("labels"));
    print_annotations_multiline(&mut w, "Annotations", &meta.smap("annotations"));
    w.write(L0, &format!("Controller:\t{}\n", ic.s("spec.controller")));
    let p = ic.get("spec.parameters");
    if !p.is_null() {
        w.write(L0, "Parameters:\n");
        if let Some(g) = p.opt_s("apiGroup") {
            w.write(L1, &format!("APIGroup:\t{g}\n"));
        }
        w.write(L1, &format!("Kind:\t{}\n", p.s("kind")));
        w.write(L1, &format!("Name:\t{}\n", p.s("name")));
    }
    describe_events(ctx, &mut w, events);
    Ok(w.finish())
}

pub(crate) fn describe_service_cidr(ctx: &Ctx, sc: J, events: &[Value]) -> Result<String> {
    let meta = sc.get("metadata");
    let mut w = PrefixWriter::new();
    w.write(L0, &format!("Name:\t{}\n", meta.s("name")));
    print_labels_multiline(&mut w, "Labels", &meta.smap("labels"));
    print_annotations_multiline(&mut w, "Annotations", &meta.smap("annotations"));
    w.write(
        L0,
        &format!("CIDRs:\t{}\n", sc.strs("spec.cidrs").join(", ")),
    );
    let conditions = sc.arr("status.conditions");
    if !conditions.is_empty() {
        w.write(L0, "Status:\n");
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
    describe_events(ctx, &mut w, events);
    Ok(w.finish())
}

pub(crate) fn describe_ip_address(ctx: &Ctx, ip: J, events: &[Value]) -> Result<String> {
    let meta = ip.get("metadata");
    let mut w = PrefixWriter::new();
    w.write(L0, &format!("Name:\t{}\n", meta.s("name")));
    print_labels_multiline(&mut w, "Labels", &meta.smap("labels"));
    print_annotations_multiline(&mut w, "Annotations", &meta.smap("annotations"));
    let p = ip.get("spec.parentRef");
    if !p.is_null() {
        w.write(L0, "Parent Reference:\n");
        w.write(L1, &format!("Group:\t{}\n", p.s("group")));
        w.write(L1, &format!("Resource:\t{}\n", p.s("resource")));
        w.write(L1, &format!("Namespace:\t{}\n", p.s("namespace")));
        w.write(L1, &format!("Name:\t{}\n", p.s("name")));
    }
    describe_events(ctx, &mut w, events);
    Ok(w.finish())
}

pub(crate) fn describe_network_policy(ctx: &Ctx, np: J, _events: &[Value]) -> Result<String> {
    let meta = np.get("metadata");
    let mut w = PrefixWriter::new();
    w.write(L0, &format!("Name:\t{}\n", meta.s("name")));
    w.write(L0, &format!("Namespace:\t{}\n", meta.s("namespace")));
    w.write(
        L0,
        &format!(
            "Created on:\t{}\n",
            ctx.go_time_string(meta.get("creationTimestamp"))
        ),
    );
    print_labels_multiline(&mut w, "Labels", &meta.smap("labels"));
    print_annotations_multiline(&mut w, "Annotations", &meta.smap("annotations"));
    let spec = np.get("spec");
    w.write(L0, "Spec:\n");
    w.write(L1, "PodSelector: ");
    let ps = spec.get("podSelector");
    if ps.len("matchLabels") == 0 && ps.len("matchExpressions") == 0 {
        w.write(
            L2,
            "<none> (Allowing the specific traffic to all pods in this namespace)\n",
        );
    } else {
        w.write(L2, &format!("{}\n", format_label_selector(ps)));
    }
    let types = spec.strs("policyTypes");
    let ingress = types.iter().any(|t| t == "Ingress");
    let egress = types.iter().any(|t| t == "Egress");
    let indent = "    ";
    if ingress {
        w.write(L1, "Allowing ingress traffic:\n");
        print_rules(&mut w, spec.arr("ingress"), indent, Direction::Ingress);
    } else {
        w.write(L1, "Not affecting ingress traffic\n");
    }
    if egress {
        w.write(L1, "Allowing egress traffic:\n");
        print_rules(&mut w, spec.arr("egress"), indent, Direction::Egress);
    } else {
        w.write(L1, "Not affecting egress traffic\n");
    }
    let pts = if spec.exists("policyTypes") {
        types.join(", ")
    } else {
        String::new()
    };
    let pts = if pts.is_empty() {
        "<none>".to_string()
    } else {
        pts
    };
    w.write(L1, &format!("Policy Types: {pts}\n"));
    Ok(w.finish())
}

#[derive(Clone, Copy, PartialEq)]
enum Direction {
    Ingress,
    Egress,
}

/// printNetworkPolicySpecIngressFrom / printNetworkPolicySpecEgressTo.
fn print_rules(w: &mut PrefixWriter, rules: Vec<J>, indent: &str, dir: Direction) {
    let (isolated, peers_key, peer_title, any_peer) = match dir {
        Direction::Ingress => (
            "<none> (Selected pods are isolated for ingress connectivity)",
            "from",
            "From:",
            "From: <any> (traffic not restricted by source)",
        ),
        Direction::Egress => (
            "<none> (Selected pods are isolated for egress connectivity)",
            "to",
            "To:",
            "To: <any> (traffic not restricted by destination)",
        ),
    };
    if rules.is_empty() {
        w.write(L0, &format!("{indent}{isolated}\n"));
        return;
    }
    let n = rules.len();
    for (i, rule) in rules.into_iter().enumerate() {
        let ports = rule.arr("ports");
        if ports.is_empty() {
            w.write(
                L0,
                &format!("{indent}To Port: <any> (traffic allowed to all ports)\n"),
            );
        } else {
            for port in ports {
                let proto = port.opt_s("protocol").unwrap_or("TCP");
                let p = intstr_ptr(port.get("port"));
                match port.opt_i("endPort") {
                    None => w.write(L0, &format!("{indent}To Port: {p}/{proto}\n")),
                    Some(end) => {
                        w.write(L0, &format!("{indent}To Port Range: {p}-{end}/{proto}\n"))
                    }
                }
            }
        }
        let peers = rule.arr(peers_key);
        if peers.is_empty() {
            w.write(L0, &format!("{indent}{any_peer}\n"));
        } else {
            for peer in peers {
                w.write(L0, &format!("{indent}{peer_title}\n"));
                let (pod_sel, ns_sel) = (peer.get("podSelector"), peer.get("namespaceSelector"));
                if !pod_sel.is_null() && !ns_sel.is_null() {
                    w.write(
                        L1,
                        &format!(
                            "{indent}NamespaceSelector: {}\n",
                            format_label_selector(ns_sel)
                        ),
                    );
                    w.write(
                        L1,
                        &format!("{indent}PodSelector: {}\n", format_label_selector(pod_sel)),
                    );
                } else if !pod_sel.is_null() {
                    w.write(
                        L1,
                        &format!("{indent}PodSelector: {}\n", format_label_selector(pod_sel)),
                    );
                } else if !ns_sel.is_null() {
                    w.write(
                        L1,
                        &format!(
                            "{indent}NamespaceSelector: {}\n",
                            format_label_selector(ns_sel)
                        ),
                    );
                } else if peer.exists("ipBlock") {
                    let b = peer.get("ipBlock");
                    w.write(L1, &format!("{indent}IPBlock:\n"));
                    w.write(L2, &format!("{indent}CIDR: {}\n", b.s("cidr")));
                    w.write(
                        L2,
                        &format!("{indent}Except: {}\n", b.strs("except").join(", ")),
                    );
                }
            }
        }
        if i != n - 1 {
            w.write(L0, &format!("{indent}----------\n"));
        }
    }
}
