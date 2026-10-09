//! Cluster access: GVRs, discovery, the `Accessor` trait, a real kube-rs
//! backed implementation and a fake used by tests.

pub mod accessor;
pub mod client;
pub mod describe;
pub mod fake;
pub mod goyaml;
pub mod gvr;
pub mod types;

pub use accessor::*;
pub use gvr::*;
pub use types::*;

/// k9s client.NamespaceAll: the fictional "all namespaces" namespace.
pub const NAMESPACE_ALL: &str = "all";
/// k9s client.BlankNamespace.
pub const BLANK_NAMESPACE: &str = "";
/// k9s client.DefaultNamespace.
pub const DEFAULT_NAMESPACE: &str = "default";
/// k9s client.ClusterScope: resource is not namespaced.
pub const CLUSTER_SCOPE: &str = "-";
/// k9s client.NotNamespaced: non-resource namespace.
pub const NOT_NAMESPACED: &str = "*";

/// True when `ns` means "all namespaces" (client.IsAllNamespaces).
pub fn is_all_namespaces(ns: &str) -> bool {
    ns == NAMESPACE_ALL || ns == BLANK_NAMESPACE
}

/// True for cluster scoped namespaces (client.IsClusterScoped).
pub fn is_cluster_scoped(ns: &str) -> bool {
    ns == CLUSTER_SCOPE
}

/// Build a resource FQN (client.FQN): `ns/name`, or `name` when cluster scoped.
/// client.CleanseNamespace, also dropping the cluster scope marker.
pub fn clean_namespace(ns: &str) -> &str {
    if is_all_namespaces(ns) || is_cluster_scoped(ns) {
        BLANK_NAMESPACE
    } else {
        ns
    }
}

pub fn fqn(ns: &str, name: &str) -> String {
    if ns.is_empty() || is_cluster_scoped(ns) {
        name.to_string()
    } else {
        format!("{ns}/{name}")
    }
}

/// Split an FQN into (namespace, name) (client.Namespaced).
pub fn namespaced(path: &str) -> (&str, &str) {
    match path.split_once('/') {
        Some((ns, n)) => (ns, n),
        None => ("", path),
    }
}
