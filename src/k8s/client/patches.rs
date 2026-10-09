//! Patch and object builders (k9s dao/patch.go, dao/cronjob.go, kubectl
//! ObjectRestarter, drain.CordonHelper).

use anyhow::{anyhow, Result};
use rand::Rng;
use serde_json::{json, Map, Value};

use crate::k8s::ImageSpec;

/// k9s cronjob maxJobNameSize.
const MAX_JOB_NAME_SIZE: usize = 42;
/// k8s.io/apimachinery/pkg/util/rand alphanumerics (no vowels, no confusables).
const RAND_ALPHABET: &[u8] = b"bcdfghjklmnpqrstvwxz2456789";
/// Pod template annotation used by `kubectl rollout restart`.
pub(crate) const RESTARTED_AT: &str = "kubectl.kubernetes.io/restartedAt";

fn element(name: &str, image: Option<&str>) -> Value {
    // Element{Image omitempty, Name}: image first like Go's field order.
    let mut m = Map::new();
    if let Some(i) = image.filter(|i| !i.is_empty()) {
        m.insert("image".into(), json!(i));
    }
    m.insert("name".into(), json!(name));
    Value::Object(m)
}

/// getPatchPodSpec: `{"spec": {"$setElementOrder/containers": .., ...}}`.
fn images_pod_spec(images: &[ImageSpec]) -> Value {
    let mut init_orders = vec![];
    let mut inits = vec![];
    let mut orders = vec![];
    let mut elems = vec![];
    for s in images {
        if s.init {
            init_orders.push(element(&s.name, None));
            inits.push(element(&s.name, Some(&s.docker_image)));
        } else {
            orders.push(element(&s.name, None));
            elems.push(element(&s.name, Some(&s.docker_image)));
        }
    }
    let mut spec = Map::new();
    if !orders.is_empty() {
        spec.insert("$setElementOrder/containers".into(), Value::Array(orders));
    }
    if !init_orders.is_empty() {
        spec.insert(
            "$setElementOrder/initContainers".into(),
            Value::Array(init_orders),
        );
    }
    if !elems.is_empty() {
        spec.insert("containers".into(), Value::Array(elems));
    }
    if !inits.is_empty() {
        spec.insert("initContainers".into(), Value::Array(inits));
    }
    json!({ "spec": Value::Object(spec) })
}

/// GetJsonPatch: strategic merge patch for a bare pod.
pub(crate) fn pod_images_patch(images: &[ImageSpec]) -> Value {
    images_pod_spec(images)
}

/// GetTemplateJsonPatch: strategic merge patch for a workload pod template.
pub(crate) fn template_images_patch(images: &[ImageSpec]) -> Value {
    json!({ "spec": { "template": images_pod_spec(images) } })
}

/// ObjectRestarter: refuse paused deployments, then build the two-way merge
/// patch that stamps the restartedAt annotation.
pub(crate) fn restart_patch(obj: &Value, is_deployment: bool, now: &str) -> Result<Value> {
    if is_deployment
        && obj
            .pointer("/spec/paused")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    {
        return Err(anyhow!(
            "can't restart paused deployment (run rollout resume first)"
        ));
    }
    Ok(json!({
        "spec": {"template": {"metadata": {"annotations": {RESTARTED_AT: now}}}}
    }))
}

/// time.Now().Format(time.RFC3339) in local time.
pub(crate) fn rfc3339_now() -> String {
    chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// rand.String(n).
pub(crate) fn rand_string(n: usize) -> String {
    let mut rng = rand::rng();
    (0..n)
        .map(|_| RAND_ALPHABET[rng.random_range(0..RAND_ALPHABET.len())] as char)
        .collect()
}

/// Manual job name: cronjob name truncated to 42 chars + `-manual-` + suffix.
pub(crate) fn manual_job_name(cronjob: &str, suffix: &str) -> String {
    let base: String = if cronjob.len() >= MAX_JOB_NAME_SIZE {
        cronjob.chars().take(MAX_JOB_NAME_SIZE).collect()
    } else {
        cronjob.to_string()
    };
    format!("{base}-manual-{suffix}")
}

/// CronJob.Run job: template labels/annotations, controller owner ref and
/// the job template spec.
pub(crate) fn job_from_cronjob(cj: &Value, cj_api_version: &str, suffix: &str) -> Result<Value> {
    let name = cj
        .pointer("/metadata/name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("expecting CronJob resource"))?;
    let ns = cj
        .pointer("/metadata/namespace")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let uid = cj
        .pointer("/metadata/uid")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let tpl = cj
        .pointer("/spec/jobTemplate")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let mut meta = Map::new();
    meta.insert("name".into(), json!(manual_job_name(name, suffix)));
    meta.insert("namespace".into(), json!(ns));
    if let Some(l) = tpl.pointer("/metadata/labels").filter(|v| !v.is_null()) {
        meta.insert("labels".into(), l.clone());
    }
    if let Some(a) = tpl
        .pointer("/metadata/annotations")
        .filter(|v| !v.is_null())
    {
        meta.insert("annotations".into(), a.clone());
    }
    meta.insert(
        "ownerReferences".into(),
        json!([{
            "apiVersion": cj_api_version,
            "kind": "CronJob",
            "name": name,
            "uid": uid,
            "controller": true,
            "blockOwnerDeletion": true,
        }]),
    );
    Ok(json!({
        "apiVersion": "batch/v1",
        "kind": "Job",
        "metadata": Value::Object(meta),
        "spec": tpl.get("spec").cloned().unwrap_or_else(|| json!({})),
    }))
}

/// Flip spec.suspend (nil -> true).
pub(crate) fn toggle_suspend(cj: &mut Value) -> Result<()> {
    let spec = cj
        .get_mut("spec")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| anyhow!("expecting cronjob resource"))?;
    let current = spec.get("suspend").and_then(Value::as_bool);
    let next = match current {
        Some(s) => !s,
        None => true,
    };
    spec.insert("suspend".into(), json!(next));
    Ok(())
}

/// CordonHelper two-way merge patch for spec.unschedulable.
pub(crate) fn cordon_patch(cordon: bool) -> Value {
    if cordon {
        json!({"spec": {"unschedulable": true}})
    } else {
        json!({"spec": {"unschedulable": null}})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(name: &str, image: &str, init: bool) -> ImageSpec {
        ImageSpec {
            name: name.into(),
            docker_image: image.into(),
            init,
        }
    }

    #[test]
    fn template_patch_matches_k9s() {
        let p = template_images_patch(&[
            img("init", "busybox:1", true),
            img("nginx", "nginx:2", false),
        ]);
        assert_eq!(
            serde_json::to_string(&p).unwrap(),
            r#"{"spec":{"template":{"spec":{"$setElementOrder/containers":[{"name":"nginx"}],"$setElementOrder/initContainers":[{"name":"init"}],"containers":[{"image":"nginx:2","name":"nginx"}],"initContainers":[{"image":"busybox:1","name":"init"}]}}}}"#
        );
    }

    #[test]
    fn pod_patch_matches_k9s() {
        // k9s patch_test.go: single regular container.
        let p = pod_images_patch(&[img("nginx", "nginx:latest", false)]);
        assert_eq!(
            serde_json::to_string(&p).unwrap(),
            r#"{"spec":{"$setElementOrder/containers":[{"name":"nginx"}],"containers":[{"image":"nginx:latest","name":"nginx"}]}}"#
        );
        let p = pod_images_patch(&[img("i", "x", true)]);
        assert_eq!(
            serde_json::to_string(&p).unwrap(),
            r#"{"spec":{"$setElementOrder/initContainers":[{"name":"i"}],"initContainers":[{"image":"x","name":"i"}]}}"#
        );
    }

    #[test]
    fn restart() {
        let p = restart_patch(&json!({"spec": {}}), true, "2024-01-01T00:00:00Z").unwrap();
        assert_eq!(
            p,
            json!({"spec":{"template":{"metadata":{"annotations":{"kubectl.kubernetes.io/restartedAt":"2024-01-01T00:00:00Z"}}}}})
        );
        let err = restart_patch(&json!({"spec": {"paused": true}}), true, "x").unwrap_err();
        assert_eq!(
            err.to_string(),
            "can't restart paused deployment (run rollout resume first)"
        );
        assert!(restart_patch(&json!({"spec": {"paused": true}}), false, "x").is_ok());
        let now = rfc3339_now();
        assert!(chrono::DateTime::parse_from_rfc3339(&now).is_ok(), "{now}");
        assert!(!now.contains('.'));
    }

    #[test]
    fn job_names() {
        assert_eq!(manual_job_name("fred", "abc"), "fred-manual-abc");
        let long = "a".repeat(50);
        assert_eq!(
            manual_job_name(&long, "xyz"),
            format!("{}-manual-xyz", "a".repeat(42))
        );
        let exact = "b".repeat(42);
        assert_eq!(manual_job_name(&exact, "q"), format!("{exact}-manual-q"));
        let s = rand_string(3);
        assert_eq!(s.len(), 3);
        assert!(s.bytes().all(|b| RAND_ALPHABET.contains(&b)));
    }

    #[test]
    fn job_from_cj() {
        let cj = json!({
            "apiVersion": "batch/v1", "kind": "CronJob",
            "metadata": {"name": "hello", "namespace": "default", "uid": "u-1"},
            "spec": {"schedule": "* * * * *", "jobTemplate": {
                "metadata": {"labels": {"a": "b"}, "annotations": {"x": "y"}},
                "spec": {"template": {"spec": {"containers": [{"name": "c", "image": "busybox"}]}}}
            }}
        });
        let job = job_from_cronjob(&cj, "batch/v1", "q2z").unwrap();
        assert_eq!(
            job,
            json!({
                "apiVersion": "batch/v1", "kind": "Job",
                "metadata": {
                    "name": "hello-manual-q2z", "namespace": "default",
                    "labels": {"a": "b"}, "annotations": {"x": "y"},
                    "ownerReferences": [{"apiVersion": "batch/v1", "kind": "CronJob", "name": "hello",
                        "uid": "u-1", "controller": true, "blockOwnerDeletion": true}]
                },
                "spec": {"template": {"spec": {"containers": [{"name": "c", "image": "busybox"}]}}}
            })
        );
        assert!(job_from_cronjob(&json!({}), "batch/v1", "x").is_err());
    }

    #[test]
    fn suspend_toggle() {
        let mut cj = json!({"spec": {}});
        toggle_suspend(&mut cj).unwrap();
        assert_eq!(cj["spec"]["suspend"], json!(true));
        toggle_suspend(&mut cj).unwrap();
        assert_eq!(cj["spec"]["suspend"], json!(false));
        assert!(toggle_suspend(&mut json!({})).is_err());
    }

    #[test]
    fn cordon() {
        assert_eq!(cordon_patch(true), json!({"spec":{"unschedulable":true}}));
        assert_eq!(cordon_patch(false), json!({"spec":{"unschedulable":null}}));
    }
}
