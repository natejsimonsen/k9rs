//! Group/Version/Resource identifiers (k9s client.GVR), e.g. `v1/pods`,
//! `apps/v1/deployments`, `contexts`, `aliases`.

use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Gvr {
    pub group: String,
    pub version: String,
    pub resource: String,
    pub sub_resource: String,
}

impl Gvr {
    /// Parse a GVR string: `[group/]version/resource[:subresource]` or a bare
    /// non-resource name like `contexts`.
    pub fn parse(s: &str) -> Gvr {
        let (main, sub) = match s.split_once(':') {
            Some((m, sub)) => (m, sub.to_string()),
            None => (s, String::new()),
        };
        let parts: Vec<&str> = main.split('/').collect();
        match parts.as_slice() {
            [g, v, r] => Gvr {
                group: g.to_string(),
                version: v.to_string(),
                resource: r.to_string(),
                sub_resource: sub,
            },
            [v, r] => Gvr {
                group: String::new(),
                version: v.to_string(),
                resource: r.to_string(),
                sub_resource: sub,
            },
            [r] => Gvr {
                group: String::new(),
                version: String::new(),
                resource: r.to_string(),
                sub_resource: sub,
            },
            _ => Gvr {
                resource: main.to_string(),
                sub_resource: sub,
                ..Default::default()
            },
        }
    }

    pub fn new(group: &str, version: &str, resource: &str) -> Gvr {
        Gvr {
            group: group.into(),
            version: version.into(),
            resource: resource.into(),
            sub_resource: String::new(),
        }
    }

    /// Resource name (`pods`).
    pub fn r(&self) -> &str {
        &self.resource
    }
    pub fn g(&self) -> &str {
        &self.group
    }
    pub fn v(&self) -> &str {
        &self.version
    }
    /// `group/version` or just `version` for core resources.
    pub fn gv(&self) -> String {
        if self.group.is_empty() {
            self.version.clone()
        } else {
            format!("{}/{}", self.group, self.version)
        }
    }
    /// Non-k8s pseudo resources (contexts, aliases, portforwards, ...).
    pub fn is_non_resource(&self) -> bool {
        self.version.is_empty()
    }
    /// Pseudo-resource name used for the API path of non-resources.
    pub fn is_core(&self) -> bool {
        self.group.is_empty() && self.version == "v1"
    }
    /// API path prefix: `/api/v1` or `/apis/<group>/<version>`.
    pub fn api_path(&self) -> String {
        if self.group.is_empty() {
            format!("/api/{}", self.version)
        } else {
            format!("/apis/{}/{}", self.group, self.version)
        }
    }
}

impl fmt::Display for Gvr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut s = String::new();
        if !self.group.is_empty() {
            s.push_str(&self.group);
            s.push('/');
        }
        if !self.version.is_empty() {
            s.push_str(&self.version);
            s.push('/');
        }
        s.push_str(&self.resource);
        if !self.sub_resource.is_empty() {
            s.push(':');
            s.push_str(&self.sub_resource);
        }
        f.write_str(&s)
    }
}

impl From<&str> for Gvr {
    fn from(s: &str) -> Self {
        Gvr::parse(s)
    }
}

/// Well-known GVRs (k9s client/gvr.go).
pub mod known {
    use super::Gvr;
    macro_rules! gvr {
        ($name:ident, $s:expr) => {
            pub fn $name() -> Gvr {
                Gvr::parse($s)
            }
        };
    }
    gvr!(pods, "v1/pods");
    gvr!(namespaces, "v1/namespaces");
    gvr!(nodes, "v1/nodes");
    gvr!(services, "v1/services");
    gvr!(configmaps, "v1/configmaps");
    gvr!(secrets, "v1/secrets");
    gvr!(events, "v1/events");
    gvr!(endpoints, "v1/endpoints");
    gvr!(serviceaccounts, "v1/serviceaccounts");
    gvr!(persistentvolumes, "v1/persistentvolumes");
    gvr!(persistentvolumeclaims, "v1/persistentvolumeclaims");
    gvr!(replicationcontrollers, "v1/replicationcontrollers");
    gvr!(deployments, "apps/v1/deployments");
    gvr!(statefulsets, "apps/v1/statefulsets");
    gvr!(daemonsets, "apps/v1/daemonsets");
    gvr!(replicasets, "apps/v1/replicasets");
    gvr!(jobs, "batch/v1/jobs");
    gvr!(cronjobs, "batch/v1/cronjobs");
    gvr!(ingresses, "networking.k8s.io/v1/ingresses");
    gvr!(networkpolicies, "networking.k8s.io/v1/networkpolicies");
    gvr!(storageclasses, "storage.k8s.io/v1/storageclasses");
    gvr!(roles, "rbac.authorization.k8s.io/v1/roles");
    gvr!(rolebindings, "rbac.authorization.k8s.io/v1/rolebindings");
    gvr!(clusterroles, "rbac.authorization.k8s.io/v1/clusterroles");
    gvr!(
        clusterrolebindings,
        "rbac.authorization.k8s.io/v1/clusterrolebindings"
    );
    gvr!(hpas, "autoscaling/v2/horizontalpodautoscalers");
    gvr!(pdbs, "policy/v1/poddisruptionbudgets");
    gvr!(priorityclasses, "scheduling.k8s.io/v1/priorityclasses");
    gvr!(crds, "apiextensions.k8s.io/v1/customresourcedefinitions");
    gvr!(endpointslices, "discovery.k8s.io/v1/endpointslices");
    gvr!(pod_metrics, "metrics.k8s.io/v1beta1/pods");
    gvr!(node_metrics, "metrics.k8s.io/v1beta1/nodes");
    // k9s pseudo resources
    gvr!(contexts, "contexts");
    gvr!(aliases, "aliases");
    gvr!(containers, "containers");
    gvr!(portforwards, "portforwards");
    gvr!(screendumps, "screendumps");
    gvr!(benchmarks, "benchmarks");
    gvr!(xrays, "xrays");
    gvr!(pulses, "pulses");
    gvr!(dirs, "dir");
    gvr!(rbac, "rbac");
    gvr!(policy, "policy");
    gvr!(users, "users");
    gvr!(groups, "groups");
    gvr!(workloads, "workloads");
    gvr!(references, "references");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_roundtrip() {
        for s in ["v1/pods", "apps/v1/deployments", "contexts", "v1/pods:log"] {
            assert_eq!(Gvr::parse(s).to_string(), s);
        }
        let g = Gvr::parse("apps/v1/deployments");
        assert_eq!(g.g(), "apps");
        assert_eq!(g.v(), "v1");
        assert_eq!(g.r(), "deployments");
        assert_eq!(g.api_path(), "/apis/apps/v1");
        assert_eq!(Gvr::parse("v1/pods").api_path(), "/api/v1");
        assert!(Gvr::parse("contexts").is_non_resource());
    }
}
