//! Job and CronJob (k9s job.go, cronjob.go).

use anyhow::Result;
use kube::api::DynamicObject;
use serde_json::Value;

use crate::model::{HeaderColumn, MISSING_VALUE, NA_VALUE};

use super::util;
use super::Def;
use crate::render::helpers::*;

fn col(n: &str) -> HeaderColumn {
    HeaderColumn::new(n)
}
fn wide(n: &str) -> HeaderColumn {
    HeaderColumn::new(n).wide()
}

const MAX_SHOW: usize = 2;

/// k9s toContainers: init + regular containers, at most 2 of each list.
fn to_containers(spec: &Value) -> (String, String) {
    let mut cc = Vec::new();
    let mut ii = Vec::new();
    for p in ["initContainers", "containers"] {
        for c in array_at(spec, p) {
            cc.push(str_at(c, "name").to_string());
            ii.push(str_at(c, "image").to_string());
        }
    }
    let limit = |mut v: Vec<String>| {
        if v.len() > MAX_SHOW {
            let more = v.len() - MAX_SHOW;
            v.truncate(MAX_SHOW);
            v.push(format!("(+{more})..."));
        }
        v.join(",")
    };
    (limit(cc), limit(ii))
}

/// k9s toCompletion.
fn to_completion(spec: &Value, status: &Value) -> String {
    let succeeded = i64_at(status, "succeeded");
    if let Some(c) = opt_i64_at(spec, "completions") {
        return format!("{succeeded}/{c}");
    }
    match opt_i64_at(spec, "parallelism") {
        Some(p) if p > 1 => format!("{succeeded}/1 of {p}"),
        _ => format!("{succeeded}/1"),
    }
}

/// k9s toDuration: completion - start, or `<none>` while running.
fn to_duration(status: &Value) -> String {
    let start = util::opt_str_at(status, "startTime").and_then(parse_time);
    let end = util::opt_str_at(status, "completionTime").and_then(parse_time);
    match (start, end) {
        (Some(s), Some(e)) => human_duration(e - s),
        _ => MISSING_VALUE.into(),
    }
}

/// k9s jobSelector.
fn job_selector(spec: &Value) -> String {
    let Some(sel) = spec.get("selector").filter(|v| !v.is_null()) else {
        return MISSING_VALUE.into();
    };
    if util::obj_len_at(sel, "matchLabels") > 0 {
        return map_to_str(&str_map_at(sel, "matchLabels"));
    }
    let ee = array_at(sel, "matchExpressions");
    ee.iter()
        .map(util::requirement_go_string)
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) struct Job;

impl Def for Job {
    fn cols(&self) -> Vec<HeaderColumn> {
        vec![
            col("NAMESPACE"),
            col("NAME"),
            col("VS").vs(),
            col("COMPLETIONS"),
            col("DURATION"),
            wide("SELECTOR"),
            wide("CONTAINERS"),
            wide("IMAGES"),
            wide("VALID"),
            HeaderColumn::new("AGE").time(),
        ]
    }

    fn row(&self, o: &DynamicObject) -> Result<(String, Vec<String>)> {
        let d = &o.data;
        let spec = d.get("spec").unwrap_or(&Value::Null);
        let status = d.get("status").unwrap_or(&Value::Null);
        let ready = to_completion(spec, status);
        let pod_spec = get_path(spec, "template.spec").unwrap_or(&Value::Null);
        let (cc, ii) = to_containers(pod_spec);
        let failed = i64_at(status, "failed");
        let diag = match ready.split_once('/') {
            Some((a, b)) if a != b && failed > 0 => Err(format!("{failed} pods failed")),
            _ => Ok(()),
        };
        Ok((
            obj_fqn(o),
            vec![
                obj_ns(o).to_string(),
                obj_name(o).to_string(),
                NA_VALUE.into(),
                ready,
                to_duration(status),
                job_selector(spec),
                cc,
                ii,
                as_status(diag),
                obj_age(o),
            ],
        ))
    }
}

pub(super) struct CronJob;

impl Def for CronJob {
    fn cols(&self) -> Vec<HeaderColumn> {
        vec![
            col("NAMESPACE"),
            col("NAME"),
            col("VS").vs(),
            col("SCHEDULE"),
            col("SUSPEND"),
            col("ACTIVE"),
            HeaderColumn::new("LAST_SCHEDULE").time(),
            wide("SELECTOR"),
            wide("CONTAINERS"),
            wide("IMAGES"),
            wide("LABELS"),
            wide("VALID"),
            HeaderColumn::new("AGE").time(),
        ]
    }

    fn row(&self, o: &DynamicObject) -> Result<(String, Vec<String>)> {
        let d = &o.data;
        let job_spec = get_path(d, "spec.jobTemplate.spec").unwrap_or(&Value::Null);
        let pod_spec = get_path(job_spec, "template.spec").unwrap_or(&Value::Null);
        let last = match util::opt_str_at(d, "status.lastScheduleTime") {
            Some(t) => to_age_opt(parse_time(t)),
            None => MISSING_VALUE.into(),
        };
        let (cc, ii) = util::pod_names_images(pod_spec, true);
        Ok((
            obj_fqn(o),
            vec![
                obj_ns(o).to_string(),
                obj_name(o).to_string(),
                NA_VALUE.into(),
                str_at(d, "spec.schedule").to_string(),
                bool_to_str(util::opt_bool_at(d, "spec.suspend").unwrap_or(false)),
                array_at(d, "status.active").len().to_string(),
                last,
                job_selector(job_spec),
                cc,
                ii,
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
    fn job_header() {
        assert_eq!(
            cols(&renderer("batch/v1/jobs").header("all")),
            vec![
                ("NAMESPACE".into(), false, false, false),
                ("NAME".into(), false, false, false),
                ("VS".into(), false, false, false),
                ("COMPLETIONS".into(), false, false, false),
                ("DURATION".into(), false, false, false),
                ("SELECTOR".into(), true, false, false),
                ("CONTAINERS".into(), true, false, false),
                ("IMAGES".into(), true, false, false),
                ("VALID".into(), true, false, false),
                ("AGE".into(), false, true, false),
            ]
        );
    }

    #[test]
    fn job_render() {
        let o = load("job");
        let r = render("batch/v1/jobs", &o);
        assert_eq!(r.id, "default/hello-1567179180");
        assert_eq!(
            r.fields,
            sv(&[
                "default",
                "hello-1567179180",
                "n/a",
                "1/1",
                "8s",
                "controller-uid=7473e6d0-cb3b-11e9-990f-42010a800218",
                "c1",
                "blang/busybox-bash",
                "",
                &age(&o),
            ])
        );
    }

    #[test]
    fn job_failed() {
        let o = obj(json!({
            "apiVersion": "batch/v1", "kind": "Job",
            "metadata": {"name": "j", "namespace": "ns"},
            "spec": {"completions": 3, "template": {"spec": {
                "initContainers": [{"name": "i1", "image": "a"}],
                "containers": [{"name": "c1", "image": "b"}, {"name": "c2", "image": "c"}]
            }}},
            "status": {"succeeded": 1, "failed": 2, "startTime": "2020-01-01T00:00:00Z"}
        }));
        let r = render("batch/v1/jobs", &o);
        assert_eq!(
            r.fields[3..9],
            sv(&[
                "1/3",
                "<none>",
                "<none>",
                "i1,c1,(+1)...",
                "a,b,(+1)...",
                "2 pods failed"
            ])
        );
    }

    #[test]
    fn completions() {
        let st = json!({"succeeded": 2});
        assert_eq!(to_completion(&json!({"completions": 5}), &st), "2/5");
        assert_eq!(to_completion(&json!({}), &st), "2/1");
        assert_eq!(to_completion(&json!({"parallelism": 1}), &st), "2/1");
        assert_eq!(to_completion(&json!({"parallelism": 4}), &st), "2/1 of 4");
    }

    #[test]
    fn selectors() {
        assert_eq!(job_selector(&json!({})), "<none>");
        assert_eq!(job_selector(&json!({"selector": {}})), "");
        assert_eq!(
            job_selector(&json!({"selector": {"matchExpressions": [
                {"key": "a", "operator": "In", "values": ["x"]},
                {"key": "b", "operator": "Exists"}
            ]}})),
            "&LabelSelectorRequirement{Key:a,Operator:In,Values:[x],} &LabelSelectorRequirement{Key:b,Operator:Exists,Values:[],}"
        );
    }

    #[test]
    fn cronjob() {
        assert_eq!(
            cols(&renderer("batch/v1/cronjobs").header("all")),
            vec![
                ("NAMESPACE".into(), false, false, false),
                ("NAME".into(), false, false, false),
                ("VS".into(), false, false, false),
                ("SCHEDULE".into(), false, false, false),
                ("SUSPEND".into(), false, false, false),
                ("ACTIVE".into(), false, false, false),
                ("LAST_SCHEDULE".into(), false, true, false),
                ("SELECTOR".into(), true, false, false),
                ("CONTAINERS".into(), true, false, false),
                ("IMAGES".into(), true, false, false),
                ("LABELS".into(), true, false, false),
                ("VALID".into(), true, false, false),
                ("AGE".into(), false, true, false),
            ]
        );
        let o = load("cj");
        let r = render("batch/v1/cronjobs", &o);
        assert_eq!(r.id, "default/hello");
        let last = crate::render::helpers::to_age_opt(crate::render::helpers::parse_time(
            "2019-08-30T17:01:00Z",
        ));
        assert_eq!(
            r.fields,
            sv(&[
                "default",
                "hello",
                "n/a",
                "*/1 * * * *",
                "false",
                "0",
                &last,
                "<none>",
                "c1",
                "blang/busybox-bash",
                "",
                "",
                &age(&o),
            ])
        );
        let o = obj(json!({
            "apiVersion": "batch/v1", "kind": "CronJob",
            "metadata": {"name": "c", "namespace": "ns"},
            "spec": {"schedule": "@daily", "suspend": true, "jobTemplate": {"spec": {}}},
            "status": {"active": [{"name": "a"}, {"name": "b"}]}
        }));
        let r = render("batch/v1/cronjobs", &o);
        assert_eq!(r.fields[3..7], sv(&["@daily", "true", "2", "<none>"]));
    }
}
