//! CustomResourceDefinition (k9s crd.go).

use anyhow::Result;
use kube::api::DynamicObject;
use serde_json::Value;

use crate::model::HeaderColumn;

use super::util;
use super::Def;
use crate::render::helpers::*;

pub(super) struct CustomResourceDefinition;

fn diagnose(n: &str, vv: &[Value]) -> Result<(), String> {
    if vv.is_empty() {
        return Err(format!("unable to assert CRD servers versions for {n}"));
    }
    let mut ee = Vec::new();
    let mut served = false;
    for v in vv {
        if bool_at(v, "served") {
            served = true;
        }
        if bool_at(v, "deprecated") {
            match util::opt_str_at(v, "deprecationWarning") {
                Some(w) => ee.push(w.to_string()),
                None => ee.push(format!("{n}[{}] is deprecated", str_at(v, "name"))),
            }
        }
    }
    if !served {
        ee.push(format!("CRD {n} is no longer served by the api server"));
    }
    if ee.is_empty() {
        Ok(())
    } else {
        Err(ee.join(" - "))
    }
}

impl Def for CustomResourceDefinition {
    fn cols(&self) -> Vec<HeaderColumn> {
        vec![
            HeaderColumn::new("NAME"),
            HeaderColumn::new("GROUP"),
            HeaderColumn::new("KIND"),
            HeaderColumn::new("VERSIONS"),
            HeaderColumn::new("SCOPE"),
            HeaderColumn::new("ALIASES").wide(),
            HeaderColumn::new("LABELS").wide(),
            HeaderColumn::new("VALID").wide(),
            HeaderColumn::new("AGE").time(),
        ]
    }

    fn row(&self, o: &DynamicObject) -> Result<(String, Vec<String>)> {
        let d = &o.data;
        let vv = array_at(d, "spec.versions");
        let versions: Vec<String> = vv
            .iter()
            .filter(|v| bool_at(v, "served"))
            .map(|v| {
                let mut n = str_at(v, "name").to_string();
                if bool_at(v, "deprecated") {
                    n.push('!');
                }
                n
            })
            .collect();
        if versions.is_empty() {
            tracing::warn!(fqn = %obj_name(o).to_string(), "Unable to assert CRD versions");
        }
        Ok((
            meta_fqn(o),
            vec![
                str_at(d, "spec.names.plural").to_string(),
                str_at(d, "spec.group").to_string(),
                str_at(d, "spec.names.kind").to_string(),
                na_strings(&versions),
                str_at(d, "spec.scope").to_string(),
                na_strings(&util::strs_at(d, "spec.names.shortNames")),
                // k9s mapToIfc on a map[string]string never matches: always blank.
                String::new(),
                as_status(diagnose(obj_name(o), vv)),
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
    fn header() {
        assert_eq!(
            cols(&renderer("apiextensions.k8s.io/v1/customresourcedefinitions").header("all")),
            vec![
                ("NAME".into(), false, false, false),
                ("GROUP".into(), false, false, false),
                ("KIND".into(), false, false, false),
                ("VERSIONS".into(), false, false, false),
                ("SCOPE".into(), false, false, false),
                ("ALIASES".into(), true, false, false),
                ("LABELS".into(), true, false, false),
                ("VALID".into(), true, false, false),
                ("AGE".into(), false, true, false),
            ]
        );
    }

    #[test]
    fn render_fixture() {
        let o = load("crd");
        let r = render("apiextensions.k8s.io/v1/customresourcedefinitions", &o);
        // k9s render tests: cluster-scoped ids keep the "-/" prefix.
        assert_eq!(r.id, "-/adapters.config.istio.io");
        assert_eq!(
            r.fields,
            sv(&[
                "adapters",
                "config.istio.io",
                "adapter",
                "v1alpha2",
                "Namespaced",
                "n/a",
                "",
                "",
                &age(&o)
            ])
        );
    }

    #[test]
    fn deprecations() {
        let o = obj(json!({
            "apiVersion": "apiextensions.k8s.io/v1", "kind": "CustomResourceDefinition",
            "metadata": {"name": "foos.x.io"},
            "spec": {"group": "x.io", "scope": "Cluster",
                     "names": {"plural": "foos", "kind": "Foo", "shortNames": ["f", "fo"]},
                     "versions": [
                        {"name": "v1", "served": true, "deprecated": true},
                        {"name": "v2", "served": true, "deprecated": true, "deprecationWarning": "use v3"},
                        {"name": "v0", "served": false}
                     ]}
        }));
        let r = render("apiextensions.k8s.io/v1/customresourcedefinitions", &o);
        assert_eq!(r.fields[3], "v1!,v2!");
        assert_eq!(r.fields[5], "f,fo");
        assert_eq!(r.fields[7], "foos.x.io[v1] is deprecated - use v3");
        assert_eq!(
            diagnose("n", json!([{"name": "v1"}]).as_array().unwrap()),
            Err("CRD n is no longer served by the api server".into())
        );
        assert_eq!(
            diagnose("n", &[]),
            Err("unable to assert CRD servers versions for n".into())
        );
        let o = obj(
            json!({"apiVersion": "apiextensions.k8s.io/v1", "kind": "CustomResourceDefinition",
            "metadata": {"name": "b.x.io"}, "spec": {}}),
        );
        let r = render("apiextensions.k8s.io/v1/customresourcedefinitions", &o);
        assert_eq!(r.fields[3], "n/a");
    }
}
