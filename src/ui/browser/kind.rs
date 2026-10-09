//! Resource kinds with dedicated viewers (k9s view/registrar.go) and the
//! resource metadata the browser needs (dao.MetaAccess / registry.go).

use crate::k8s::{Gvr, ResourceMeta};

/// Which k9s viewer drives a browser.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// Default viewer: ScaleExtender(OwnerExtender(Browser)).
    Generic,
    Pod,
    Container,
    Deploy,
    StatefulSet,
    DaemonSet,
    ReplicaSet,
    Job,
    CronJob,
    Node,
    Namespace,
    Context,
    Service,
    Secret,
    ConfigMap,
    Crd,
    Pvc,
    ServiceAccount,
    PriorityClass,
    Event,
    /// roles, clusterroles and their bindings: plain browser, enter shows rules.
    RbacSource,
    /// `rbac` pseudo resource: rules of a role/binding.
    Rules,
    /// `policy` pseudo resource: rules granted to a subject.
    Policy,
    User,
    Group,
    Alias,
    PortForward,
    ScreenDump,
    Benchmark,
    Dir,
    Helm,
    HelmHistory,
    Workload,
    Reference,
}

/// Resolve the viewer kind for a GVR.
pub(crate) fn kind_for(gvr: &Gvr) -> Kind {
    let s = gvr.to_string();
    match s.as_str() {
        "v1/pods" => Kind::Pod,
        "containers" => Kind::Container,
        "apps/v1/deployments" => Kind::Deploy,
        "apps/v1/statefulsets" => Kind::StatefulSet,
        "apps/v1/daemonsets" => Kind::DaemonSet,
        "apps/v1/replicasets" => Kind::ReplicaSet,
        "batch/v1/jobs" => Kind::Job,
        "batch/v1/cronjobs" => Kind::CronJob,
        "v1/nodes" => Kind::Node,
        "v1/namespaces" => Kind::Namespace,
        "contexts" => Kind::Context,
        "v1/services" => Kind::Service,
        "v1/secrets" => Kind::Secret,
        "v1/configmaps" => Kind::ConfigMap,
        "apiextensions.k8s.io/v1/customresourcedefinitions" => Kind::Crd,
        "v1/persistentvolumeclaims" => Kind::Pvc,
        "v1/serviceaccounts" => Kind::ServiceAccount,
        "scheduling.k8s.io/v1/priorityclasses" => Kind::PriorityClass,
        "v1/events" | "events.k8s.io/v1/events" => Kind::Event,
        "rbac.authorization.k8s.io/v1/roles"
        | "rbac.authorization.k8s.io/v1/clusterroles"
        | "rbac.authorization.k8s.io/v1/rolebindings"
        | "rbac.authorization.k8s.io/v1/clusterrolebindings" => Kind::RbacSource,
        "rbac" => Kind::Rules,
        "policy" => Kind::Policy,
        "users" => Kind::User,
        "groups" => Kind::Group,
        "aliases" => Kind::Alias,
        "portforwards" => Kind::PortForward,
        "screendumps" => Kind::ScreenDump,
        "benchmarks" => Kind::Benchmark,
        "dir" | "dirs" => Kind::Dir,
        "helm" => Kind::Helm,
        "helm-history" => Kind::HelmHistory,
        "workloads" => Kind::Workload,
        "references" => Kind::Reference,
        _ => Kind::Generic,
    }
}

/// Resource metadata (metav1.APIResource subset).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Meta {
    /// Kind, used as the view name.
    pub kind: String,
    pub namespaced: bool,
    /// `None` means unrestricted (k9s nil verbs).
    pub verbs: Option<Vec<String>>,
    /// k9s category (pseudo resource without yaml/describe).
    pub k9s: bool,
    /// helm category.
    pub helm: bool,
    pub scalable: bool,
}

impl Meta {
    fn pseudo(kind: &str, namespaced: bool, verbs: Option<&[&str]>) -> Meta {
        Meta {
            kind: kind.into(),
            namespaced,
            verbs: verbs.map(|v| v.iter().map(|s| s.to_string()).collect()),
            k9s: true,
            helm: false,
            scalable: false,
        }
    }

    /// client.Can.
    pub(crate) fn can(&self, verb: &str) -> bool {
        let Some(verbs) = &self.verbs else {
            return true;
        };
        let candidates: &[&str] = match verb {
            "describe" => &["get"],
            "view" => &["get", "list"],
            "delete" => &["delete"],
            "edit" => &["patch", "update"],
            _ => &[],
        };
        verbs.iter().any(|v| candidates.contains(&v.as_str()))
    }

    /// dao.IsK8sMeta.
    pub(crate) fn is_k8s(&self) -> bool {
        !self.k9s && !self.helm
    }
}

const SCALABLE: &[&str] = &[
    "apps/v1/deployments",
    "apps/v1/statefulsets",
    "apps/v1/replicasets",
    "apps/v1/replicationcontrollers",
    "v1/replicationcontrollers",
];

const CLUSTER_SCOPED: &[&str] = &[
    "nodes",
    "namespaces",
    "persistentvolumes",
    "storageclasses",
    "clusterroles",
    "clusterrolebindings",
    "customresourcedefinitions",
    "priorityclasses",
    "apiservices",
    "mutatingwebhookconfigurations",
    "validatingwebhookconfigurations",
    "csidrivers",
    "csinodes",
    "runtimeclasses",
    "ingressclasses",
    "volumeattachments",
    "certificatesigningrequests",
    "componentstatuses",
    "flowschemas",
    "prioritylevelconfigurations",
];

fn guess_kind(resource: &str) -> String {
    let known = [
        ("pods", "Pod"),
        ("services", "Service"),
        ("nodes", "Node"),
        ("namespaces", "Namespace"),
        ("configmaps", "ConfigMap"),
        ("secrets", "Secret"),
        ("events", "Event"),
        ("serviceaccounts", "ServiceAccount"),
        ("persistentvolumes", "PersistentVolume"),
        ("persistentvolumeclaims", "PersistentVolumeClaim"),
        ("deployments", "Deployment"),
        ("statefulsets", "StatefulSet"),
        ("daemonsets", "DaemonSet"),
        ("replicasets", "ReplicaSet"),
        ("jobs", "Job"),
        ("cronjobs", "CronJob"),
        ("ingresses", "Ingress"),
        ("roles", "Role"),
        ("rolebindings", "RoleBinding"),
        ("clusterroles", "ClusterRole"),
        ("clusterrolebindings", "ClusterRoleBinding"),
        ("customresourcedefinitions", "CustomResourceDefinition"),
        ("priorityclasses", "PriorityClass"),
        ("endpoints", "Endpoints"),
        ("storageclasses", "StorageClass"),
        ("networkpolicies", "NetworkPolicy"),
        ("horizontalpodautoscalers", "HorizontalPodAutoscaler"),
        ("poddisruptionbudgets", "PodDisruptionBudget"),
        ("endpointslices", "EndpointSlice"),
    ];
    if let Some((_, k)) = known.iter().find(|(r, _)| *r == resource) {
        return k.to_string();
    }
    let single = resource.strip_suffix('s').unwrap_or(resource);
    let mut c = single.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// Metadata for `gvr` from discovery, the k9s pseudo resource table, or a
/// best guess when discovery has not completed yet.
pub(crate) fn meta_for(kind: Kind, gvr: &Gvr, resources: &[ResourceMeta]) -> Option<Meta> {
    let m = match kind {
        Kind::Workload => Meta::pseudo("Workload", true, None),
        Kind::Dir => Meta::pseudo("Dir", false, None),
        Kind::Reference => Meta::pseudo("References", false, Some(&[])),
        Kind::Alias => Meta::pseudo("Aliases", false, Some(&[])),
        Kind::Context => Meta::pseudo("Contexts", false, Some(&[])),
        Kind::ScreenDump => Meta::pseudo("ScreenDumps", false, Some(&["delete"])),
        Kind::Benchmark => Meta::pseudo("Benchmarks", false, Some(&["delete"])),
        Kind::PortForward => Meta::pseudo("PortForwards", true, Some(&["delete"])),
        Kind::Container => Meta::pseudo("Containers", false, Some(&[])),
        Kind::Rules => Meta::pseudo("Rules", false, None),
        Kind::Policy => Meta::pseudo("Rules", true, None),
        Kind::User => Meta::pseudo("User", false, None),
        Kind::Group => Meta::pseudo("Group", false, None),
        Kind::Helm | Kind::HelmHistory => Meta {
            kind: if kind == Kind::Helm {
                "Helm"
            } else {
                "History"
            }
            .into(),
            namespaced: true,
            verbs: Some(vec!["delete".into()]),
            k9s: false,
            helm: true,
            scalable: false,
        },
        _ => {
            let scalable = SCALABLE.contains(&gvr.to_string().as_str());
            if let Some(rm) = resources.iter().find(|m| &m.gvr == gvr) {
                return Some(Meta {
                    kind: rm.kind.clone(),
                    namespaced: rm.namespaced,
                    verbs: Some(rm.verbs.clone()),
                    k9s: false,
                    helm: false,
                    scalable,
                });
            }
            if !resources.is_empty() || gvr.is_non_resource() {
                return None;
            }
            Meta {
                kind: guess_kind(gvr.r()),
                namespaced: !CLUSTER_SCOPED.contains(&gvr.r()),
                verbs: None,
                k9s: false,
                helm: false,
                scalable,
            }
        }
    };
    Some(m)
}

/// Metadata guess for resources missing from discovery.
pub(crate) fn fallback_meta(gvr: &Gvr) -> Meta {
    Meta {
        kind: guess_kind(gvr.r()),
        namespaced: !CLUSTER_SCOPED.contains(&gvr.r()),
        verbs: None,
        k9s: gvr.is_non_resource(),
        helm: false,
        scalable: SCALABLE.contains(&gvr.to_string().as_str()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_and_metas() {
        assert_eq!(kind_for(&Gvr::parse("v1/pods")), Kind::Pod);
        assert_eq!(kind_for(&Gvr::parse("dir")), Kind::Dir);
        assert_eq!(kind_for(&Gvr::parse("foo.io/v1/bars")), Kind::Generic);
        let res = crate::k8s::fake::default_resources();
        let m = meta_for(Kind::Deploy, &Gvr::parse("apps/v1/deployments"), &res).unwrap();
        assert!(m.scalable && m.namespaced && m.can("edit") && m.can("delete"));
        assert!(meta_for(Kind::Generic, &Gvr::parse("foo.io/v1/bars"), &res).is_none());
        let guess = meta_for(Kind::Node, &Gvr::parse("v1/nodes"), &[]).unwrap();
        assert!(!guess.namespaced);
        assert_eq!(guess.kind, "Node");
        let ctx = meta_for(Kind::Context, &Gvr::parse("contexts"), &res).unwrap();
        assert!(!ctx.can("delete") && ctx.k9s);
        let rules = meta_for(Kind::Rules, &Gvr::parse("rbac"), &res).unwrap();
        assert!(rules.can("edit"));
        assert_eq!(guess_kind("widgets"), "Widget");
    }
}
