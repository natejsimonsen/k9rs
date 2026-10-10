//! `kubectl describe` text generation. k9s (dao/describe.go) calls kubectl's
//! describers; this module ports them (kubectl v0.35) so the output matches
//! byte for byte: same sections, tabwriter alignment, `<none>`/`<unset>`
//! markers, quantity formatting and event ages.
//!
//! kubectl's describers fetch extra objects from the API server (the pods of
//! a controller, a service's endpoint slices, a node's pods and lease, ...).
//! Callers that have them pass them in [`Related`]; without them the
//! describers fall back as documented on each [`Related`] field.

mod certificates;
mod cluster;
mod config;
mod events;
mod generic;
mod gosort;
mod network;
mod pod;
mod quantity;
mod rbac;
mod storage;
mod util;
mod volumes;
mod workloads;
mod writer;

#[cfg(test)]
mod tests;

use anyhow::{Context, Result};
use jiff::{tz::TimeZone, Timestamp};
use kube::api::DynamicObject;
use serde_json::Value;

use super::Gvr;
use util::{Ctx, J};

/// Events related to an object, as needed by describers.
pub type Events = Vec<DynamicObject>;

/// Objects kubectl's describers look up besides the described object.
/// Every field is optional; `None` means "not fetched".
#[derive(Clone, Debug, Default)]
pub struct Related {
    /// Pods used for controller "Pods Status" counts, a PVC's "Used By" and a
    /// node's "Non-terminated Pods"/"Allocated resources" sections. Pods are
    /// filtered like kubectl does (namespace, selector, controller UID, node).
    /// Without pods, controllers derive counts from their status (ready
    /// replicas as Running, the rest as Waiting), PVCs show `Used By: <none>`
    /// and nodes omit the pod sections.
    pub pods: Option<Vec<DynamicObject>>,
    /// ReplicaSets for a Deployment's OldReplicaSets/NewReplicaSet lines
    /// (omitted when absent, like kubectl when none are found).
    pub replica_sets: Option<Vec<DynamicObject>>,
    /// EndpointSlices for Service `Endpoints:` and Ingress backends
    /// (`<none>` when absent).
    pub endpoint_slices: Option<Vec<DynamicObject>>,
    /// Services for Ingress backends. When absent, backends render as if the
    /// service had no endpoints.
    pub services: Option<Vec<DynamicObject>>,
    /// The node's Lease (kube-node-lease namespace). The `Lease:` section is
    /// omitted when absent.
    pub lease: Option<DynamicObject>,
    /// ResourceQuotas for a Namespace (section omitted when absent).
    pub resource_quotas: Option<Vec<DynamicObject>>,
    /// LimitRanges for a Namespace (section omitted when absent).
    pub limit_ranges: Option<Vec<DynamicObject>>,
}

/// JSON form of [`Related`] used by the describers.
#[derive(Default)]
pub(crate) struct Rel {
    pub(crate) pods: Option<Vec<Value>>,
    pub(crate) replica_sets: Option<Vec<Value>>,
    pub(crate) endpoint_slices: Option<Vec<Value>>,
    pub(crate) services: Option<Vec<Value>>,
    pub(crate) lease: Option<Value>,
    pub(crate) resource_quotas: Option<Vec<Value>>,
    pub(crate) limit_ranges: Option<Vec<Value>>,
}

fn to_value(o: &DynamicObject) -> Result<Value> {
    serde_json::to_value(o).context("serializing object")
}

fn to_values(list: &Option<Vec<DynamicObject>>) -> Result<Option<Vec<Value>>> {
    list.as_ref()
        .map(|l| l.iter().map(to_value).collect::<Result<Vec<_>>>())
        .transpose()
}

impl Rel {
    fn from_related(r: &Related) -> Result<Rel> {
        Ok(Rel {
            pods: to_values(&r.pods)?,
            replica_sets: to_values(&r.replica_sets)?,
            endpoint_slices: to_values(&r.endpoint_slices)?,
            services: to_values(&r.services)?,
            lease: r.lease.as_ref().map(to_value).transpose()?,
            resource_quotas: to_values(&r.resource_quotas)?,
            limit_ranges: to_values(&r.limit_ranges)?,
        })
    }
}

/// Describe `obj` in kubectl's format. `events` are the object's events.
pub fn describe(gvr: &Gvr, obj: &DynamicObject, events: &Events) -> Result<String> {
    describe_at(gvr, obj, events, chrono::Utc::now())
}

/// [`describe`] with a fixed clock for ages ("Events" ages, "Terminating
/// (lasts ...)", node pod ages). Timestamps print in the local time zone.
pub fn describe_at(
    gvr: &Gvr,
    obj: &DynamicObject,
    events: &Events,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<String> {
    describe_related(gvr, obj, events, &Related::default(), now)
}

/// [`describe_at`] with the related objects kubectl would look up.
pub fn describe_related(
    gvr: &Gvr,
    obj: &DynamicObject,
    events: &Events,
    related: &Related,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<String> {
    let obj = to_value(obj)?;
    let events = events.iter().map(to_value).collect::<Result<Vec<_>>>()?;
    let rel = Rel::from_related(related)?;
    let now = chrono_to_jiff(now)?;
    run(gvr, &obj, &events, &rel, now, TimeZone::system())
}

fn chrono_to_jiff(t: chrono::DateTime<chrono::Utc>) -> Result<Timestamp> {
    Timestamp::new(t.timestamp(), t.timestamp_subsec_nanos() as i32).context("converting clock")
}

type DescribeFn = fn(&Ctx, J, &[Value]) -> Result<String>;

/// kubectl's describerMap keyed by group and resource; anything else uses
/// the generic describer.
fn describer_for(gvr: &Gvr) -> DescribeFn {
    match (gvr.g(), gvr.r()) {
        ("", "pods") => pod::describe_pod,
        ("", "replicationcontrollers") => workloads::describe_replication_controller,
        ("", "secrets") => config::describe_secret,
        ("", "services") => network::describe_service,
        ("", "serviceaccounts") => config::describe_service_account,
        ("", "nodes") => cluster::describe_node,
        ("", "limitranges") => cluster::describe_limit_range,
        ("", "resourcequotas") => cluster::describe_resource_quota,
        ("", "persistentvolumes") => storage::describe_persistent_volume,
        ("", "persistentvolumeclaims") => storage::describe_persistent_volume_claim,
        ("", "namespaces") => cluster::describe_namespace,
        ("", "endpoints") => network::describe_endpoints,
        ("", "configmaps") => config::describe_config_map,
        ("discovery.k8s.io", "endpointslices") => network::describe_endpoint_slice,
        ("autoscaling", "horizontalpodautoscalers") if gvr.v() == "v1" => cluster::describe_hpa_v1,
        ("autoscaling", "horizontalpodautoscalers") => cluster::describe_hpa_v2,
        ("networking.k8s.io", "ingresses") => network::describe_ingress,
        ("networking.k8s.io", "ingressclasses") => network::describe_ingress_class,
        ("networking.k8s.io", "servicecidrs") => network::describe_service_cidr,
        ("networking.k8s.io", "ipaddresses") => network::describe_ip_address,
        ("networking.k8s.io", "networkpolicies") => network::describe_network_policy,
        ("batch", "jobs") => workloads::describe_job,
        ("batch", "cronjobs") => workloads::describe_cron_job,
        ("apps", "statefulsets") => workloads::describe_stateful_set,
        ("apps", "deployments") => workloads::describe_deployment,
        ("apps", "daemonsets") => workloads::describe_daemon_set,
        ("apps", "replicasets") => workloads::describe_replica_set,
        ("storage.k8s.io", "storageclasses") => storage::describe_storage_class,
        ("storage.k8s.io", "csinodes") => storage::describe_csi_node,
        ("storage.k8s.io", "volumeattributesclasses") => storage::describe_volume_attributes_class,
        ("policy", "poddisruptionbudgets") => cluster::describe_pdb,
        ("rbac.authorization.k8s.io", "roles") => rbac::describe_role,
        ("rbac.authorization.k8s.io", "clusterroles") => rbac::describe_role,
        ("rbac.authorization.k8s.io", "rolebindings") => rbac::describe_binding,
        ("rbac.authorization.k8s.io", "clusterrolebindings") => rbac::describe_binding,
        ("scheduling.k8s.io", "priorityclasses") => cluster::describe_priority_class,
        ("certificates.k8s.io", "certificatesigningrequests") => certificates::describe_csr,
        _ => generic::describe_generic,
    }
}

fn run(
    gvr: &Gvr,
    obj: &Value,
    events: &[Value],
    rel: &Rel,
    now: Timestamp,
    tz: TimeZone,
) -> Result<String> {
    let ctx = Ctx { now, tz, rel };
    describer_for(gvr)(&ctx, J(obj), events)
}
