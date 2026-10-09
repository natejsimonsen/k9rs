//! Service, Endpoints, EndpointSlice and NetworkPolicy (k9s svc.go, ep.go,
//! eps.go, np.go).

use anyhow::Result;
use kube::api::DynamicObject;
use serde_json::Value;

use crate::model::{HeaderColumn, NA_VALUE};

use super::util;
use super::Def;
use crate::render::helpers::*;

fn col(n: &str) -> HeaderColumn {
    HeaderColumn::new(n)
}
fn wide(n: &str) -> HeaderColumn {
    HeaderColumn::new(n).wide()
}

// Service --------------------------------------------------------------------

pub(super) struct Service;

fn to_ip(ip: &str) -> String {
    if ip.is_empty() || ip == "None" {
        String::new()
    } else {
        ip.to_string()
    }
}

fn lb_ingress_ip(d: &Value) -> String {
    array_at(d, "status.loadBalancer.ingress")
        .iter()
        .filter_map(|ing| {
            let ip = str_at(ing, "ip");
            let host = str_at(ing, "hostname");
            if !ip.is_empty() {
                Some(ip.to_string())
            } else if !host.is_empty() {
                Some(host.to_string())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// k9s getSvcExtIPS.
fn svc_ext_ips(d: &Value) -> Vec<String> {
    let ext = util::strs_at(d, "spec.externalIPs");
    match str_at(d, "spec.type") {
        "NodePort" | "ClusterIP" => ext,
        "LoadBalancer" => {
            let lb = lb_ingress_ip(d);
            let mut out = Vec::new();
            if !lb.is_empty() {
                out.push(lb);
            }
            out.extend(ext);
            out
        }
        "ExternalName" => vec![str_at(d, "spec.externalName").to_string()],
        _ => Vec::new(),
    }
}

/// k9s toIPs.
fn to_ips(svc_type: &str, mut ips: Vec<String>) -> String {
    if ips.is_empty() {
        if svc_type == "LoadBalancer" {
            return "<pending>".into();
        }
        return String::new();
    }
    ips.sort();
    ips.join(",")
}

/// k9s ToPorts.
fn to_ports(pp: &[Value]) -> String {
    pp.iter()
        .map(|p| {
            let mut out = String::new();
            let name = str_at(p, "name");
            if !name.is_empty() {
                out.push_str(name);
                out.push(':');
            }
            out.push_str(&format!("{}►{}", i64_at(p, "port"), i64_at(p, "nodePort")));
            let proto = str_at(p, "protocol");
            if proto != "TCP" {
                out.push('╱');
                out.push_str(proto);
            }
            out
        })
        .collect::<Vec<_>>()
        .join(" ")
}

impl Def for Service {
    fn cols(&self) -> Vec<HeaderColumn> {
        vec![
            col("NAMESPACE"),
            col("NAME"),
            col("TYPE"),
            col("CLUSTER-IP"),
            col("EXTERNAL-IP"),
            wide("SELECTOR"),
            col("PORTS"),
            wide("LABELS"),
            wide("VALID"),
            HeaderColumn::new("AGE").time(),
        ]
    }

    fn row(&self, o: &DynamicObject) -> Result<(String, Vec<String>)> {
        let d = &o.data;
        let t = str_at(d, "spec.type");
        Ok((
            obj_fqn(o),
            vec![
                obj_ns(o).to_string(),
                obj_name(o).to_string(),
                t.to_string(),
                to_ip(str_at(d, "spec.clusterIP")),
                to_ips(t, svc_ext_ips(d)),
                map_to_str(&str_map_at(d, "spec.selector")),
                to_ports(array_at(d, "spec.ports")),
                labels_str(o),
                String::new(),
                obj_age(o),
            ],
        ))
    }
}

// Endpoints ------------------------------------------------------------------

pub(super) struct Endpoints;

/// k9s toEPs. Addresses without an IP leave blank slots, like k9s.
fn to_eps(ss: &[Value]) -> String {
    const MAX_IPS: usize = 3;
    ss.iter()
        .map(|sub| {
            let pp: Vec<String> = array_at(sub, "ports")
                .iter()
                .map(|p| i64_at(p, "port").to_string())
                .collect();
            let addrs = array_at(sub, "addresses");
            let mut aa = vec![String::new(); addrs.len()];
            let mut n = 0;
            for a in addrs {
                let ip = str_at(a, "ip");
                if ip.is_empty() {
                    continue;
                }
                aa[n] = if pp.is_empty() {
                    ip.to_string()
                } else if pp.len() > MAX_IPS {
                    format!("{ip}:{}...", pp[..MAX_IPS].join(","))
                } else {
                    format!("{ip}:{}", pp.join(","))
                };
                n += 1;
            }
            aa.join(",")
        })
        .collect::<Vec<_>>()
        .join(",")
}

impl Def for Endpoints {
    fn cols(&self) -> Vec<HeaderColumn> {
        vec![
            col("NAMESPACE"),
            col("NAME"),
            col("ENDPOINTS"),
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
                missing(&to_eps(array_at(d, "subsets"))),
                obj_age(o),
            ],
        ))
    }
}

// EndpointSlice --------------------------------------------------------------

pub(super) struct EndpointSlice;

impl Def for EndpointSlice {
    fn cols(&self) -> Vec<HeaderColumn> {
        vec![
            col("NAMESPACE"),
            col("NAME"),
            col("ADDRESSTYPE"),
            col("PORTS"),
            col("ENDPOINTS"),
            HeaderColumn::new("AGE").time(),
        ]
    }

    fn row(&self, o: &DynamicObject) -> Result<(String, Vec<String>)> {
        let d = &o.data;
        let ports = array_at(d, "ports");
        let ports = if ports.is_empty() {
            UNSET_VALUE.to_string()
        } else {
            ports
                .iter()
                .filter_map(|p| opt_i64_at(p, "port").map(|n| n.to_string()))
                .collect::<Vec<_>>()
                .join(",")
        };
        let eps = array_at(d, "endpoints");
        let eps = if eps.is_empty() {
            UNSET_VALUE.to_string()
        } else {
            eps.iter()
                .flat_map(|e| util::strs_at(e, "addresses"))
                .collect::<Vec<_>>()
                .join(",")
        };
        Ok((
            obj_fqn(o),
            vec![
                obj_ns(o).to_string(),
                obj_name(o).to_string(),
                str_at(d, "addressType").to_string(),
                ports,
                eps,
                obj_age(o),
            ],
        ))
    }
}

// NetworkPolicy --------------------------------------------------------------

pub(super) struct NetworkPolicy;

fn lab_to_str(e: &Value) -> String {
    format!(
        "{}-{}{}",
        str_at(e, "key"),
        str_at(e, "operator"),
        util::strs_at(e, "values").join(",")
    )
}

fn exp_to_str(ee: &[Value]) -> String {
    ee.iter().map(lab_to_str).collect::<Vec<_>>().join(",")
}

fn render_peer(p: &Value) -> String {
    let mut out = String::new();
    for (key, prefix) in [("podSelector", "po:"), ("namespaceSelector", "ns:")] {
        let Some(sel) = p.get(key).filter(|v| !v.is_null()) else {
            continue;
        };
        let m = map_to_str(&str_map_at(sel, "matchLabels"));
        if !m.is_empty() {
            out.push_str(prefix);
            out.push_str(&m);
        }
        let e = exp_to_str(array_at(sel, "matchExpressions"));
        if !e.is_empty() {
            out.push_str("--");
            out.push_str(&e);
        }
    }
    out
}

fn render_block(b: &Value) -> String {
    let cidr = str_at(b, "cidr");
    let except = util::strs_at(b, "except");
    if except.is_empty() {
        return cidr.to_string();
    }
    if except.len() > 2 {
        return format!("{cidr}[{}...]", except[..2].join(","));
    }
    format!("{cidr}[{}]", except.join(","))
}

fn peers_to_str(pp: &[Value]) -> (String, String) {
    let mut sels = Vec::new();
    let mut ips = Vec::new();
    for p in pp {
        let peer = render_peer(p);
        if !peer.is_empty() {
            sels.push(peer);
        }
        let Some(block) = p.get("ipBlock").filter(|v| !v.is_null()) else {
            continue;
        };
        let b = render_block(block);
        if !b.is_empty() {
            ips.push(b);
        }
    }
    (sels.join(","), ips.join(","))
}

fn ports_to_str(pp: &[Value]) -> String {
    pp.iter()
        .map(|p| {
            let proto = util::opt_str_at(p, "protocol").unwrap_or(NA_VALUE);
            let port = match p.get("port").filter(|v| !v.is_null()) {
                Some(v) => go_fmt(v),
                None => NA_VALUE.to_string(),
            };
            format!("{proto}:{port}")
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// (ports, selectors, blocks) for ingress (`from`) or egress (`to`) rules.
fn rules(rr: &[Value], peers: &str) -> (String, String, String) {
    let mut ports = Vec::new();
    let mut sels = Vec::new();
    let mut blocks = Vec::new();
    for r in rr {
        let p = ports_to_str(array_at(r, "ports"));
        if !p.is_empty() {
            ports.push(p);
        }
        let (ll, pp) = peers_to_str(array_at(r, peers));
        if !ll.is_empty() {
            sels.push(ll);
        }
        if !pp.is_empty() {
            blocks.push(pp);
        }
    }
    (ports.join(","), sels.join(","), blocks.join(","))
}

impl Def for NetworkPolicy {
    fn cols(&self) -> Vec<HeaderColumn> {
        vec![
            col("NAMESPACE"),
            col("NAME"),
            col("POD-SELECTOR"),
            wide("ING-SELECTOR"),
            wide("ING-PORTS"),
            wide("ING-BLOCK"),
            wide("EGR-SELECTOR"),
            wide("EGR-PORTS"),
            wide("EGR-BLOCK"),
            wide("LABELS"),
            wide("VALID"),
            HeaderColumn::new("AGE").time(),
        ]
    }

    fn row(&self, o: &DynamicObject) -> Result<(String, Vec<String>)> {
        let d = &o.data;
        let (ip, is, ib) = rules(array_at(d, "spec.ingress"), "from");
        let (ep, es, eb) = rules(array_at(d, "spec.egress"), "to");
        let mut pod_sel = String::new();
        if util::obj_len_at(d, "spec.podSelector.matchLabels") > 0 {
            pod_sel = map_to_str(&str_map_at(d, "spec.podSelector.matchLabels"));
        }
        let exps = array_at(d, "spec.podSelector.matchExpressions");
        if !exps.is_empty() {
            pod_sel.push_str("::");
            pod_sel.push_str(&exp_to_str(exps));
        }
        Ok((
            obj_fqn(o),
            vec![
                obj_ns(o).to_string(),
                obj_name(o).to_string(),
                pod_sel,
                is,
                ip,
                ib,
                es,
                ep,
                eb,
                labels_str(o),
                String::new(),
                obj_age(o),
            ],
        ))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::testutil::*;
    use super::*;

    #[test]
    fn service_header() {
        assert_eq!(
            cols(&renderer("v1/services").header("all")),
            vec![
                ("NAMESPACE".into(), false, false, false),
                ("NAME".into(), false, false, false),
                ("TYPE".into(), false, false, false),
                ("CLUSTER-IP".into(), false, false, false),
                ("EXTERNAL-IP".into(), false, false, false),
                ("SELECTOR".into(), true, false, false),
                ("PORTS".into(), false, false, false),
                ("LABELS".into(), true, false, false),
                ("VALID".into(), true, false, false),
                ("AGE".into(), false, true, false),
            ]
        );
    }

    #[test]
    fn service_render() {
        let o = load("svc");
        let r = render("v1/services", &o);
        assert_eq!(r.id, "default/dictionary1");
        assert_eq!(
            r.fields,
            sv(&[
                "default",
                "dictionary1",
                "ClusterIP",
                "10.47.248.116",
                "",
                "app=dictionary1",
                "http:4001►0",
                "",
                "",
                &age(&o),
            ])
        );
    }

    fn svc(spec: Value, status: Value) -> Value {
        json!({"spec": spec, "status": status})
    }

    #[test]
    fn service_ext_ips() {
        let d = svc(json!({"type": "LoadBalancer"}), json!({}));
        assert_eq!(to_ips("LoadBalancer", svc_ext_ips(&d)), "<pending>");
        let d = svc(
            json!({"type": "LoadBalancer", "externalIPs": ["9.9.9.9", "1.1.1.1"]}),
            json!({"loadBalancer": {"ingress": [{"ip": "5.5.5.5"}, {"hostname": "lb.io"}]}}),
        );
        assert_eq!(
            to_ips("LoadBalancer", svc_ext_ips(&d)),
            "1.1.1.1,5.5.5.5,lb.io,9.9.9.9"
        );
        let d = svc(
            json!({"type": "ExternalName", "externalName": "x.io"}),
            json!({}),
        );
        assert_eq!(to_ips("ExternalName", svc_ext_ips(&d)), "x.io");
        let d = svc(
            json!({"type": "NodePort", "externalIPs": ["2.2.2.2"]}),
            json!({}),
        );
        assert_eq!(to_ips("NodePort", svc_ext_ips(&d)), "2.2.2.2");
        assert_eq!(to_ip("None"), "");
    }

    #[test]
    fn service_ports() {
        let pp = json!([
            {"name": "http", "port": 80, "nodePort": 30080, "protocol": "TCP"},
            {"port": 53, "protocol": "UDP"}
        ]);
        assert_eq!(to_ports(pp.as_array().unwrap()), "http:80►30080 53►0╱UDP");
    }

    #[test]
    fn endpoints() {
        assert_eq!(
            names(&renderer("v1/endpoints").header("all")),
            sv(&["NAMESPACE", "NAME", "ENDPOINTS", "AGE"])
        );
        let o = load("ep");
        let r = render("v1/endpoints", &o);
        assert_eq!(r.id, "ns-1/blee");
        assert_eq!(r.fields, sv(&["ns-1", "blee", "10.0.0.67:8080", &age(&o)]));

        let ss = json!([
            {"addresses": [{"ip": "1.1.1.1"}, {"ip": ""}, {"ip": "2.2.2.2"}],
             "ports": [{"port": 1}, {"port": 2}, {"port": 3}, {"port": 4}]},
            {"addresses": [{"ip": "3.3.3.3"}]}
        ]);
        assert_eq!(
            to_eps(ss.as_array().unwrap()),
            "1.1.1.1:1,2,3...,2.2.2.2:1,2,3...,,3.3.3.3"
        );
        let o = obj(json!({"apiVersion": "v1", "kind": "Endpoints",
            "metadata": {"name": "e", "namespace": "ns"}}));
        assert_eq!(render("v1/endpoints", &o).fields[2], "<none>");
    }

    #[test]
    fn endpoint_slices() {
        assert_eq!(
            names(&renderer("discovery.k8s.io/v1/endpointslices").header("all")),
            sv(&[
                "NAMESPACE",
                "NAME",
                "ADDRESSTYPE",
                "PORTS",
                "ENDPOINTS",
                "AGE"
            ])
        );
        let o = load("eps");
        let r = render("discovery.k8s.io/v1/endpointslices", &o);
        assert_eq!(r.id, "blee/fred");
        assert_eq!(
            r.fields,
            sv(&[
                "blee",
                "fred",
                "IPv4",
                "4244",
                "172.20.0.2,172.20.0.3",
                &age(&o)
            ])
        );
        let o = obj(
            json!({"apiVersion": "discovery.k8s.io/v1", "kind": "EndpointSlice",
            "metadata": {"name": "e", "namespace": "ns"}, "addressType": "IPv6"}),
        );
        let r = render("discovery.k8s.io/v1/endpointslices", &o);
        assert_eq!(r.fields[2..5], sv(&["IPv6", "<unset>", "<unset>"]));
    }

    #[test]
    fn network_policy() {
        assert_eq!(
            cols(&renderer("networking.k8s.io/v1/networkpolicies").header("all")),
            vec![
                ("NAMESPACE".into(), false, false, false),
                ("NAME".into(), false, false, false),
                ("POD-SELECTOR".into(), false, false, false),
                ("ING-SELECTOR".into(), true, false, false),
                ("ING-PORTS".into(), true, false, false),
                ("ING-BLOCK".into(), true, false, false),
                ("EGR-SELECTOR".into(), true, false, false),
                ("EGR-PORTS".into(), true, false, false),
                ("EGR-BLOCK".into(), true, false, false),
                ("LABELS".into(), true, false, false),
                ("VALID".into(), true, false, false),
                ("AGE".into(), false, true, false),
            ]
        );
        let o = load("np");
        let r = render("networking.k8s.io/v1/networkpolicies", &o);
        assert_eq!(r.id, "default/fred");
        assert_eq!(
            r.fields,
            sv(&[
                "default",
                "fred",
                "app=nginx",
                "ns:app=blee,po:app=fred",
                "TCP:6379",
                "172.17.0.0/16[172.17.1.0/24,172.17.3.0/24...]",
                "",
                "TCP:5978",
                "10.0.0.0/24",
                "",
                "",
                &age(&o),
            ])
        );
    }

    #[test]
    fn network_policy_selectors() {
        let o = obj(json!({
            "apiVersion": "networking.k8s.io/v1", "kind": "NetworkPolicy",
            "metadata": {"name": "n", "namespace": "ns"},
            "spec": {
                "podSelector": {"matchExpressions": [{"key": "k", "operator": "In", "values": ["a", "b"]}]},
                "egress": [{
                    "ports": [{"port": "http"}, {"protocol": "UDP"}],
                    "to": [{
                        "podSelector": {"matchLabels": {"a": "1"}, "matchExpressions": [{"key": "x", "operator": "Exists"}]},
                        "ipBlock": {"cidr": "10.0.0.0/8", "except": ["10.1.0.0/16"]}
                    }]
                }]
            }
        }));
        let r = render("networking.k8s.io/v1/networkpolicies", &o);
        assert_eq!(r.fields[2], "::k-Ina,b");
        assert_eq!(r.fields[6], "po:a=1--x-Exists");
        assert_eq!(r.fields[7], "n/a:http,UDP:n/a");
        assert_eq!(r.fields[8], "10.0.0.0/8[10.1.0.0/16]");
    }
}
