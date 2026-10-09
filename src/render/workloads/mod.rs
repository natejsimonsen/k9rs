//! Workload / config / network / storage / RBAC renderers (owned by the
//! `render-workloads` slice): deploy, sts, ds, rs, job, cronjob, svc,
//! configmap, secret, pv, pvc, sa, roles & bindings, netpol, pdb,
//! storageclass, endpoints, endpointslices, crd, workloads, rbac policy,
//! subject and reference views.
//!
//! k9s (v0.50.18) has no dedicated renderer for ingresses,
//! replicationcontrollers, priorityclasses or horizontalpodautoscalers: they
//! use the server-side table, so `renderer_for` returns `None` for them.
//! Matching is on the exact GVR, like the k9s registry.
//!
//! Every renderer builds the full k9s header (NAMESPACE included) and hands
//! it to `ViewBase`, which applies views.yaml custom columns and drops the
//! NAMESPACE column unless `ns` is cluster wide. Like k9s, the pseudo views
//! (workloads, rbac, policy, users/groups, references) ignore custom columns.

use anyhow::Result;
use kube::api::DynamicObject;
use ratatui::style::Color;

use crate::config::ViewSetting;
use crate::k8s::Gvr;
use crate::model::{Header, HeaderColumn, Row, RowEvent};
use crate::render::helpers::{obj_to_value, ViewBase};

use super::{default_colorer, Palette, RenderInput, Renderer};

mod apps;
mod batch;
mod config;
mod crd;
mod network;
mod pseudo_rbac;
mod rbac;
mod storage;
mod util;
mod workload;

#[cfg(test)]
mod testutil;

/// One resource's column set, row builder and colorer.
trait Def: Send + Sync {
    /// Full k9s header, NAMESPACE included.
    fn cols(&self) -> Vec<HeaderColumn>;
    /// Row id and full fields (matching `cols`).
    fn row(&self, o: &DynamicObject) -> Result<(String, Vec<String>)>;
    /// k9s ColorerFunc.
    fn color(&self, ns: &str, h: &Header, re: &RowEvent, p: &Palette) -> Color {
        default_colorer(ns, h, re, p)
    }
    /// True when the k9s renderer applies views.yaml custom columns.
    fn custom_cols(&self) -> bool {
        true
    }
}

/// A `Def` plus the k9s render.Base behavior (custom columns, NAMESPACE).
struct Wrap<D: Def> {
    def: D,
    base: ViewBase,
}

impl<D: Def> Wrap<D> {
    fn default_header(&self) -> Header {
        Header::new(self.def.cols())
    }
}

impl<D: Def> Renderer for Wrap<D> {
    fn header(&self, ns: &str) -> Header {
        self.base.header(&self.default_header(), ns)
    }

    fn render(&self, input: &RenderInput<'_>, ns: &str, row: &mut Row) -> Result<()> {
        let (id, fields) = self.def.row(input.obj)?;
        row.id = id;
        row.fields = fields;
        // JSONPath columns need the object as JSON; skip the copy otherwise.
        let raw = (!self.base.specs().is_empty()).then(|| obj_to_value(input.obj));
        self.base
            .finish(raw.as_ref(), &self.default_header(), ns, row)
    }

    fn color_for(&self, ns: &str, header: &Header, re: &RowEvent, palette: &Palette) -> Color {
        self.def.color(ns, header, re, palette)
    }

    fn set_view_setting(&mut self, vs: Option<ViewSetting>) {
        if self.def.custom_cols() {
            self.base.set_view_setting(vs.as_ref());
        }
    }
}

fn boxed<D: Def + 'static>(def: D) -> Option<Box<dyn Renderer>> {
    Some(Box::new(Wrap {
        def,
        base: ViewBase::default(),
    }))
}

/// Renderer for `gvr` if this module owns it.
pub fn renderer_for(gvr: &Gvr) -> Option<Box<dyn Renderer>> {
    match gvr.to_string().as_str() {
        "apps/v1/deployments" => boxed(apps::Deployment),
        "apps/v1/statefulsets" => boxed(apps::StatefulSet),
        "apps/v1/daemonsets" => boxed(apps::DaemonSet),
        "apps/v1/replicasets" => boxed(apps::ReplicaSet),
        "batch/v1/jobs" => boxed(batch::Job),
        "batch/v1/cronjobs" => boxed(batch::CronJob),
        "v1/services" => boxed(network::Service),
        "v1/endpoints" => boxed(network::Endpoints),
        "discovery.k8s.io/v1/endpointslices" => boxed(network::EndpointSlice),
        "networking.k8s.io/v1/networkpolicies" => boxed(network::NetworkPolicy),
        "v1/configmaps" => boxed(config::ConfigMap),
        "v1/secrets" => boxed(config::Secret),
        "v1/serviceaccounts" => boxed(config::ServiceAccount),
        "policy/v1/poddisruptionbudgets" => boxed(config::PodDisruptionBudget),
        "v1/persistentvolumes" => boxed(storage::PersistentVolume),
        "v1/persistentvolumeclaims" => boxed(storage::PersistentVolumeClaim),
        "storage.k8s.io/v1/storageclasses" => boxed(storage::StorageClass),
        "rbac.authorization.k8s.io/v1/roles" => boxed(rbac::Role),
        "rbac.authorization.k8s.io/v1/clusterroles" => boxed(rbac::ClusterRole),
        "rbac.authorization.k8s.io/v1/rolebindings" => boxed(rbac::RoleBinding),
        "rbac.authorization.k8s.io/v1/clusterrolebindings" => boxed(rbac::ClusterRoleBinding),
        "apiextensions.k8s.io/v1/customresourcedefinitions" => boxed(crd::CustomResourceDefinition),
        "workloads" => boxed(workload::Workload),
        "rbac" => boxed(pseudo_rbac::Rbac),
        "policy" => boxed(pseudo_rbac::Policy),
        "users" | "groups" => boxed(pseudo_rbac::Subject),
        "references" => boxed(pseudo_rbac::Reference),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry() {
        for g in [
            "apps/v1/deployments",
            "apps/v1/statefulsets",
            "apps/v1/daemonsets",
            "apps/v1/replicasets",
            "batch/v1/jobs",
            "batch/v1/cronjobs",
            "v1/services",
            "v1/endpoints",
            "discovery.k8s.io/v1/endpointslices",
            "networking.k8s.io/v1/networkpolicies",
            "v1/configmaps",
            "v1/secrets",
            "v1/serviceaccounts",
            "policy/v1/poddisruptionbudgets",
            "v1/persistentvolumes",
            "v1/persistentvolumeclaims",
            "storage.k8s.io/v1/storageclasses",
            "rbac.authorization.k8s.io/v1/roles",
            "rbac.authorization.k8s.io/v1/clusterroles",
            "rbac.authorization.k8s.io/v1/rolebindings",
            "rbac.authorization.k8s.io/v1/clusterrolebindings",
            "apiextensions.k8s.io/v1/customresourcedefinitions",
            "workloads",
            "rbac",
            "policy",
            "users",
            "groups",
            "references",
        ] {
            let r = renderer_for(&Gvr::parse(g)).unwrap_or_else(|| panic!("missing {g}"));
            assert!(!r.is_generic());
            assert!(!r.header("all").is_empty(), "{g}");
        }
        for g in [
            "v1/pods",
            "networking.k8s.io/v1/ingresses",
            "v1/replicationcontrollers",
            "scheduling.k8s.io/v1/priorityclasses",
            "autoscaling/v2/horizontalpodautoscalers",
            "autoscaling/v1/horizontalpodautoscalers",
            "batch/v1beta1/cronjobs",
            "policy/v1beta1/poddisruptionbudgets",
            "contexts",
        ] {
            assert!(renderer_for(&Gvr::parse(g)).is_none(), "{g}");
        }
    }

    #[test]
    fn namespace_column_hidden_unless_cluster_wide() {
        let r = renderer_for(&Gvr::parse("v1/configmaps")).unwrap();
        for ns in ["all", "", "-"] {
            assert_eq!(r.header(ns).index_of("NAMESPACE"), Some(0), "{ns}");
        }
        let h = r.header("default");
        assert_eq!(h.index_of("NAMESPACE"), None);
        let o = testutil::load("cm");
        let mut row = Row::new(h.len());
        r.render(&RenderInput::new(&o), "default", &mut row)
            .unwrap();
        assert_eq!(row.id, "default/blee");
        assert_eq!(row.fields.len(), h.len());
        assert_eq!(row.fields[..3], ["blee", "2", ""]);
    }

    fn view(cols: &[&str]) -> Option<ViewSetting> {
        Some(ViewSetting {
            columns: cols.iter().map(|c| c.to_string()).collect(),
            sort_column: String::new(),
        })
    }

    fn render_in(r: &dyn Renderer, o: &DynamicObject, ns: &str) -> Row {
        let mut row = Row::new(r.header(ns).len());
        r.render(&RenderInput::new(o), ns, &mut row).unwrap();
        assert_eq!(row.fields.len(), r.header(ns).len(), "{ns}");
        row
    }

    #[test]
    fn deployment_custom_columns() {
        let mut r = renderer_for(&Gvr::parse("apps/v1/deployments")).unwrap();
        r.set_view_setting(view(&["NAME", "READY|W", "REPLICAS:.spec.replicas|R"]));
        let h = r.header("all");
        assert_eq!(
            testutil::names(&h),
            testutil::sv(&[
                "NAME",
                "READY",
                "REPLICAS",
                "NAMESPACE",
                "VS",
                "UP-TO-DATE",
                "AVAILABLE",
                "LABELS",
                "VALID",
                "AGE"
            ])
        );
        // READY keeps the default right alignment and gains wide.
        assert!(h.0[1].wide);
        assert_eq!(h.0[1].align, crate::model::Align::Right);
        assert_eq!(h.0[2].align, crate::model::Align::Right);
        assert!(!h.0[2].wide);
        assert!(h.0[4].vs);
        assert!(h.0[9].time);

        let o = testutil::load("dp");
        let row = render_in(&*r, &o, "all");
        assert_eq!(row.id, "icx/icx-db");
        assert_eq!(
            row.fields,
            testutil::sv(&[
                "icx-db",
                "1/1",
                "1",
                "icx",
                "n/a",
                "1",
                "1",
                "app=icx-db",
                "",
                &testutil::age(&o)
            ])
        );

        // Namespaced view: NAMESPACE is still dropped.
        let h = r.header("icx");
        assert_eq!(h.index_of("NAMESPACE"), None);
        assert_eq!(
            testutil::names(&h)[..3],
            testutil::sv(&["NAME", "READY", "REPLICAS"])
        );
        let row = render_in(&*r, &o, "icx");
        assert_eq!(
            row.fields[..4],
            testutil::sv(&["icx-db", "1/1", "1", "n/a"])
        );

        // The colorer finds READY by name in the custom header.
        let re = RowEvent::new(
            crate::model::ResEvent::Unchanged,
            Row::with(
                "x",
                testutil::sv(&["d", "0/0", "0", "ns", "n/a", "0", "0", "", "", "1d"]),
            ),
        );
        let p = Palette::default();
        assert_eq!(r.color_for("all", &r.header("all"), &re, &p), p.pending);
    }

    #[test]
    fn custom_column_jsonpath_missing_and_invalid_specs() {
        let mut r = renderer_for(&Gvr::parse("v1/configmaps")).unwrap();
        r.set_view_setting(view(&["NAME", "NOPE:.spec.nope"]));
        let o = testutil::load("cm");
        let row = render_in(&*r, &o, "default");
        assert_eq!(row.fields[..2], testutil::sv(&["blee", "<none>"]));

        // k9s rejects a spec without a name (`.spec.replicas|R`) and keeps
        // the previous columns.
        let mut r = renderer_for(&Gvr::parse("apps/v1/deployments")).unwrap();
        let default = r.header("all");
        r.set_view_setting(view(&["NAME", ".spec.replicas|R"]));
        assert_eq!(r.header("all"), default);

        // Clearing the setting restores the default header.
        let mut r = renderer_for(&Gvr::parse("apps/v1/deployments")).unwrap();
        r.set_view_setting(view(&["AGE"]));
        assert_eq!(testutil::names(&r.header("all"))[0], "AGE");
        r.set_view_setting(None);
        assert_eq!(r.header("all"), default);
    }

    #[test]
    fn pseudo_views_ignore_custom_columns() {
        for g in [
            "workloads",
            "rbac",
            "policy",
            "users",
            "groups",
            "references",
        ] {
            let mut r = renderer_for(&Gvr::parse(g)).unwrap();
            let default = r.header("all");
            r.set_view_setting(view(&["VALID", "X:.metadata.name"]));
            assert_eq!(r.header("all"), default, "{g}");
        }
        let mut r = renderer_for(&Gvr::parse("users")).unwrap();
        r.set_view_setting(view(&["KIND"]));
        let o = crate::render::pseudo::subject("fernand", "User", "RoleBinding:a", "RoleBinding");
        let row = render_in(&*r, &o, "all");
        assert_eq!(
            row.fields,
            testutil::sv(&["fernand", "User", "RoleBinding:a", ""])
        );
    }
}
