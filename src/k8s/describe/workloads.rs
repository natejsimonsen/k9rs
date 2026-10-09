//! Workload describers: Deployment, ReplicaSet, ReplicationController,
//! StatefulSet, DaemonSet, Job and CronJob.

use std::collections::BTreeMap;

use anyhow::{anyhow, Result};
use serde_json::Value;

use super::events::describe_events;
use super::gosort::sort_by;
use super::pod::describe_pod_template;
use super::util::{
    controller_of, format_label_selector, format_labels, human_duration, intstr_ptr, parse_time,
    print_annotations_multiline, print_bool_ptr, print_controller, print_labels_multiline,
    print_labels_multiline_indent, selector_string, Ctx, J,
};
use super::volumes::{canonical_or_zero, pvc_class};
use super::writer::{PrefixWriter, L0, L1};

/// Whether a metav1.LabelSelector matches `labels` (nil matches nothing,
/// empty matches everything).
pub(crate) fn selector_matches(sel: J, labels: &BTreeMap<String, String>) -> bool {
    if sel.is_null() {
        return false;
    }
    for (k, v) in sel.smap("matchLabels") {
        if labels.get(&k) != Some(&v) {
            return false;
        }
    }
    for e in sel.arr("matchExpressions") {
        let key = e.s("key");
        let values = e.strs("values");
        let ok = match e.s("operator") {
            "In" => labels.get(key).is_some_and(|v| values.contains(v)),
            "NotIn" => labels.get(key).is_none_or(|v| !values.contains(v)),
            "Exists" => labels.contains_key(key),
            "DoesNotExist" => !labels.contains_key(key),
            _ => false,
        };
        if !ok {
            return false;
        }
    }
    true
}

/// labels.SelectorFromSet(set).Matches(labels).
fn set_matches(set: &BTreeMap<String, String>, labels: &BTreeMap<String, String>) -> bool {
    set.iter().all(|(k, v)| labels.get(k) == Some(v))
}

/// getPodStatusForController over the related pods; `None` without pods.
fn pod_status_for_controller(
    ctx: &Ctx,
    obj: J,
    matches: &dyn Fn(&BTreeMap<String, String>) -> bool,
) -> Option<(i64, i64, i64, i64)> {
    let pods = ctx.rel.pods.as_ref()?;
    let ns = obj.s("metadata.namespace");
    let uid = obj.s("metadata.uid");
    let mut counts = (0, 0, 0, 0);
    for p in pods.iter().map(J) {
        if p.s("metadata.namespace") != ns || !matches(&p.smap("metadata.labels")) {
            continue;
        }
        match controller_of(p) {
            Some((_, _, cuid)) if cuid == uid => {}
            _ => continue,
        }
        match p.s("status.phase") {
            "Running" => counts.0 += 1,
            "Pending" => counts.1 += 1,
            "Succeeded" => counts.2 += 1,
            "Failed" => counts.3 += 1,
            _ => {}
        }
    }
    Some(counts)
}

/// Pod counts with the status based fallback used when pods are unknown.
fn pods_status_line(
    ctx: &Ctx,
    obj: J,
    matches: &dyn Fn(&BTreeMap<String, String>) -> bool,
    total: i64,
    ready: i64,
) -> String {
    let (r, wt, s, f) = pod_status_for_controller(ctx, obj, matches).unwrap_or((
        ready,
        (total - ready).max(0),
        0,
        0,
    ));
    format!("{r} Running / {wt} Waiting / {s} Succeeded / {f} Failed")
}

fn conditions_type_status_reason(w: &mut PrefixWriter, conditions: Vec<J>) {
    if conditions.is_empty() {
        return;
    }
    w.write(L0, "Conditions:\n  Type\tStatus\tReason\n");
    w.write(L1, "----\t------\t------\n");
    for c in conditions {
        w.write(
            L1,
            &format!("{} \t{}\t{}\n", c.s("type"), c.s("status"), c.s("reason")),
        );
    }
}

pub(crate) fn describe_deployment(ctx: &Ctx, d: J, events: &[Value]) -> Result<String> {
    let (meta, spec, status) = (d.get("metadata"), d.get("spec"), d.get("status"));
    let selector = selector_string(spec.get("selector")).map_err(|e| anyhow!(e))?;
    let mut w = PrefixWriter::new();
    w.write(L0, &format!("Name:\t{}\n", meta.s("name")));
    w.write(L0, &format!("Namespace:\t{}\n", meta.s("namespace")));
    w.write(
        L0,
        &format!(
            "CreationTimestamp:\t{}\n",
            ctx.rfc1123z(meta.get("creationTimestamp"))
        ),
    );
    print_labels_multiline(&mut w, "Labels", &meta.smap("labels"));
    print_annotations_multiline(&mut w, "Annotations", &meta.smap("annotations"));
    w.write(L0, &format!("Selector:\t{selector}\n"));
    w.write(
        L0,
        &format!(
            "Replicas:\t{} desired | {} updated | {} total | {} available | {} unavailable\n",
            spec.i("replicas"),
            status.i("updatedReplicas"),
            status.i("replicas"),
            status.i("availableReplicas"),
            status.i("unavailableReplicas")
        ),
    );
    w.write(L0, &format!("StrategyType:\t{}\n", spec.s("strategy.type")));
    w.write(
        L0,
        &format!("MinReadySeconds:\t{}\n", spec.i("minReadySeconds")),
    );
    let ru = spec.get("strategy.rollingUpdate");
    if !ru.is_null() {
        w.write(
            L0,
            &format!(
                "RollingUpdateStrategy:\t{} max unavailable, {} max surge\n",
                intstr_ptr(ru.get("maxUnavailable")),
                intstr_ptr(ru.get("maxSurge"))
            ),
        );
    }
    describe_pod_template(ctx, spec.get("template"), &mut w);
    conditions_type_status_reason(&mut w, status.arr("conditions"));
    if let Some((old, new)) = deployment_replica_sets(ctx, d) {
        if !old.is_empty() || !new.is_empty() {
            w.write(
                L0,
                &format!("OldReplicaSets:\t{}\n", replica_sets_by_labels(&old)),
            );
            w.write(
                L0,
                &format!("NewReplicaSet:\t{}\n", replica_sets_by_labels(&new)),
            );
        }
    }
    describe_events(ctx, &mut w, events);
    Ok(w.finish())
}

/// deploymentutil.GetAllReplicaSets: (all old RSs, new RS) owned by `d`.
fn deployment_replica_sets<'a>(ctx: &'a Ctx, d: J) -> Option<(Vec<J<'a>>, Vec<J<'a>>)> {
    let list = ctx.rel.replica_sets.as_ref()?;
    let ns = d.s("metadata.namespace");
    let uid = d.s("metadata.uid");
    let sel = d.get("spec.selector");
    let mut owned: Vec<J> = list
        .iter()
        .map(J)
        .filter(|rs| rs.s("metadata.namespace") == ns)
        .filter(|rs| selector_matches(sel, &rs.smap("metadata.labels")))
        .filter(|rs| matches!(controller_of(*rs), Some((_, _, u)) if u == uid))
        .collect();
    sort_by(&mut owned, |a, b| {
        let (ta, tb) = (
            parse_time(a.get("metadata.creationTimestamp")),
            parse_time(b.get("metadata.creationTimestamp")),
        );
        if ta == tb {
            a.s("metadata.name") < b.s("metadata.name")
        } else {
            ta < tb
        }
    });
    let template = normalized_template(d.get("spec.template"));
    let new_rs = owned
        .iter()
        .copied()
        .find(|rs| normalized_template(rs.get("spec.template")) == template);
    let new_uid = new_rs.map(|r| r.s("metadata.uid"));
    let old: Vec<J> = owned
        .into_iter()
        .filter(|rs| Some(rs.s("metadata.uid")) != new_uid)
        .collect();
    Some((old, new_rs.into_iter().collect()))
}

/// Pod template without the pod-template-hash label (equalIgnoreHash).
fn normalized_template(t: J) -> Value {
    let mut v = t.0.clone();
    if let Some(labels) = v
        .get_mut("metadata")
        .and_then(|m| m.get_mut("labels"))
        .and_then(|l| l.as_object_mut())
    {
        labels.remove("pod-template-hash");
    }
    v
}

fn replica_sets_by_labels(rss: &[J]) -> String {
    let list: Vec<String> = rss
        .iter()
        .map(|rs| {
            format!(
                "{} ({}/{} replicas created)",
                rs.s("metadata.name"),
                rs.i("status.replicas"),
                rs.i("spec.replicas")
            )
        })
        .collect();
    if list.is_empty() {
        "<none>".to_string()
    } else {
        list.join(", ")
    }
}

pub(crate) fn describe_replica_set(ctx: &Ctx, rs: J, events: &[Value]) -> Result<String> {
    let (meta, spec, status) = (rs.get("metadata"), rs.get("spec"), rs.get("status"));
    let sel = spec.get("selector");
    selector_string(sel).map_err(|e| anyhow!(e))?;
    let mut w = PrefixWriter::new();
    w.write(L0, &format!("Name:\t{}\n", meta.s("name")));
    w.write(L0, &format!("Namespace:\t{}\n", meta.s("namespace")));
    w.write(L0, &format!("Selector:\t{}\n", format_label_selector(sel)));
    print_labels_multiline(&mut w, "Labels", &meta.smap("labels"));
    print_annotations_multiline(&mut w, "Annotations", &meta.smap("annotations"));
    let controlled_by = print_controller(rs);
    if !controlled_by.is_empty() {
        w.write(L0, &format!("Controlled By:\t{controlled_by}\n"));
    }
    w.write(
        L0,
        &format!(
            "Replicas:\t{} current / {} desired\n",
            status.i("replicas"),
            spec.i("replicas")
        ),
    );
    w.write(L0, "Pods Status:\t");
    let line = pods_status_line(
        ctx,
        rs,
        &|l| selector_matches(sel, l),
        status.i("replicas"),
        status.i("readyReplicas"),
    );
    w.write(L0, &format!("{line}\n"));
    describe_pod_template(ctx, spec.get("template"), &mut w);
    conditions_type_status_reason(&mut w, status.arr("conditions"));
    describe_events(ctx, &mut w, events);
    Ok(w.finish())
}

pub(crate) fn describe_replication_controller(
    ctx: &Ctx,
    rc: J,
    events: &[Value],
) -> Result<String> {
    let (meta, spec, status) = (rc.get("metadata"), rc.get("spec"), rc.get("status"));
    let selector = spec.smap("selector");
    let mut w = PrefixWriter::new();
    w.write(L0, &format!("Name:\t{}\n", meta.s("name")));
    w.write(L0, &format!("Namespace:\t{}\n", meta.s("namespace")));
    w.write(L0, &format!("Selector:\t{}\n", format_labels(&selector)));
    print_labels_multiline(&mut w, "Labels", &meta.smap("labels"));
    print_annotations_multiline(&mut w, "Annotations", &meta.smap("annotations"));
    w.write(
        L0,
        &format!(
            "Replicas:\t{} current / {} desired\n",
            status.i("replicas"),
            spec.i("replicas")
        ),
    );
    let line = pods_status_line(
        ctx,
        rc,
        &|l| set_matches(&selector, l),
        status.i("replicas"),
        status.i("readyReplicas"),
    );
    w.write(L0, &format!("Pods Status:\t{line}\n"));
    describe_pod_template(ctx, spec.get("template"), &mut w);
    conditions_type_status_reason(&mut w, status.arr("conditions"));
    describe_events(ctx, &mut w, events);
    Ok(w.finish())
}

pub(crate) fn describe_stateful_set(ctx: &Ctx, ps: J, events: &[Value]) -> Result<String> {
    let (meta, spec, status) = (ps.get("metadata"), ps.get("spec"), ps.get("status"));
    let sel = spec.get("selector");
    let selector = selector_string(sel).map_err(|e| anyhow!(e))?;
    let mut w = PrefixWriter::new();
    w.write(L0, &format!("Name:\t{}\n", meta.s("name")));
    w.write(L0, &format!("Namespace:\t{}\n", meta.s("namespace")));
    w.write(
        L0,
        &format!(
            "CreationTimestamp:\t{}\n",
            ctx.rfc1123z(meta.get("creationTimestamp"))
        ),
    );
    w.write(L0, &format!("Selector:\t{selector}\n"));
    print_labels_multiline(&mut w, "Labels", &meta.smap("labels"));
    print_annotations_multiline(&mut w, "Annotations", &meta.smap("annotations"));
    w.write(
        L0,
        &format!(
            "Replicas:\t{} desired | {} total\n",
            spec.i("replicas"),
            status.i("replicas")
        ),
    );
    w.write(
        L0,
        &format!("Update Strategy:\t{}\n", spec.s("updateStrategy.type")),
    );
    let ru = spec.get("updateStrategy.rollingUpdate");
    if !ru.is_null() {
        if let Some(p) = ru.opt_i("partition") {
            w.write(L1, &format!("Partition:\t{p}\n"));
            if ru.exists("maxUnavailable") {
                w.write(
                    L1,
                    &format!(
                        "MaxUnavailable:\t{}\n",
                        intstr_ptr(ru.get("maxUnavailable"))
                    ),
                );
            }
        }
    }
    let line = pods_status_line(
        ctx,
        ps,
        &|l| selector_matches(sel, l),
        status.i("replicas"),
        status.i("readyReplicas"),
    );
    w.write(L0, &format!("Pods Status:\t{line}\n"));
    describe_pod_template(ctx, spec.get("template"), &mut w);
    describe_volume_claim_templates(spec.arr("volumeClaimTemplates"), &mut w);
    describe_events(ctx, &mut w, events);
    Ok(w.finish())
}

fn describe_volume_claim_templates(templates: Vec<J>, w: &mut PrefixWriter) {
    if templates.is_empty() {
        w.write(L0, "Volume Claims:\t<none>\n");
        return;
    }
    w.write(L0, "Volume Claims:\n");
    for pvc in templates {
        let (meta, spec) = (pvc.get("metadata"), pvc.get("spec"));
        w.write(L1, &format!("Name:\t{}\n", meta.s("name")));
        w.write(L1, &format!("StorageClass:\t{}\n", pvc_class(meta, spec)));
        print_labels_multiline_indent(w, "  ", "Labels", "\t", &meta.smap("labels"));
        print_labels_multiline_indent(w, "  ", "Annotations", "\t", &meta.smap("annotations"));
        let storage = spec.get("resources.requests").key("storage");
        if storage.is_null() {
            w.write(L1, "Capacity:\t<default>\n");
        } else {
            w.write(L1, &format!("Capacity:\t{}\n", canonical_or_zero(storage)));
        }
        w.write(
            L1,
            &format!(
                "Access Modes:\t{}\n",
                super::util::go_slice(&spec.strs("accessModes"))
            ),
        );
    }
}

pub(crate) fn describe_daemon_set(ctx: &Ctx, ds: J, events: &[Value]) -> Result<String> {
    let (meta, spec, status) = (ds.get("metadata"), ds.get("spec"), ds.get("status"));
    let sel = spec.get("selector");
    let selector = selector_string(sel).map_err(|e| anyhow!(e))?;
    let mut w = PrefixWriter::new();
    w.write(L0, &format!("Name:\t{}\n", meta.s("name")));
    w.write(L0, &format!("Namespace:\t{}\n", meta.s("namespace")));
    w.write(L0, &format!("Selector:\t{selector}\n"));
    w.write(
        L0,
        &format!(
            "Node-Selector:\t{}\n",
            format_labels(&spec.smap("template.spec.nodeSelector"))
        ),
    );
    print_labels_multiline(&mut w, "Labels", &meta.smap("labels"));
    print_annotations_multiline(&mut w, "Annotations", &meta.smap("annotations"));
    w.write(
        L0,
        &format!(
            "Desired Number of Nodes Scheduled: {}\n",
            status.i("desiredNumberScheduled")
        ),
    );
    w.write(
        L0,
        &format!(
            "Current Number of Nodes Scheduled: {}\n",
            status.i("currentNumberScheduled")
        ),
    );
    w.write(
        L0,
        &format!(
            "Number of Nodes Scheduled with Up-to-date Pods: {}\n",
            status.i("updatedNumberScheduled")
        ),
    );
    w.write(
        L0,
        &format!(
            "Number of Nodes Scheduled with Available Pods: {}\n",
            status.i("numberAvailable")
        ),
    );
    w.write(
        L0,
        &format!(
            "Number of Nodes Misscheduled: {}\n",
            status.i("numberMisscheduled")
        ),
    );
    let line = pods_status_line(
        ctx,
        ds,
        &|l| selector_matches(sel, l),
        status.i("currentNumberScheduled"),
        status.i("numberReady"),
    );
    w.write(L0, &format!("Pods Status:\t{line}\n"));
    describe_pod_template(ctx, spec.get("template"), &mut w);
    describe_events(ctx, &mut w, events);
    Ok(w.finish())
}

pub(crate) fn describe_job(ctx: &Ctx, job: J, events: &[Value]) -> Result<String> {
    let (meta, spec, status) = (job.get("metadata"), job.get("spec"), job.get("status"));
    let mut w = PrefixWriter::new();
    w.write(L0, &format!("Name:\t{}\n", meta.s("name")));
    w.write(L0, &format!("Namespace:\t{}\n", meta.s("namespace")));
    match selector_string(spec.get("selector")) {
        Ok(s) => w.write(L0, &format!("Selector:\t{s}\n")),
        Err(e) => w.write(L0, &format!("Selector:\tFailed to get selector: {e}\n")),
    }
    print_labels_multiline(&mut w, "Labels", &meta.smap("labels"));
    print_annotations_multiline(&mut w, "Annotations", &meta.smap("annotations"));
    let controlled_by = print_controller(job);
    if !controlled_by.is_empty() {
        w.write(L0, &format!("Controlled By:\t{controlled_by}\n"));
    }
    if let Some(p) = spec.opt_i("parallelism") {
        w.write(L0, &format!("Parallelism:\t{p}\n"));
    }
    match spec.opt_i("completions") {
        Some(c) => w.write(L0, &format!("Completions:\t{c}\n")),
        None => w.write(L0, "Completions:\t<unset>\n"),
    }
    if let Some(m) = spec.opt_s("completionMode") {
        w.write(L0, &format!("Completion Mode:\t{m}\n"));
    }
    if let Some(s) = spec.opt_b("suspend") {
        w.write(L0, &format!("Suspend:\t{s}\n"));
    }
    if let Some(b) = spec.opt_i("backoffLimit") {
        w.write(L0, &format!("Backoff Limit:\t{b}\n"));
    }
    if let Some(t) = spec.opt_i("ttlSecondsAfterFinished") {
        w.write(L0, &format!("TTL Seconds After Finished:\t{t}\n"));
    }
    let (start, done) = (status.get("startTime"), status.get("completionTime"));
    if !start.is_null() {
        w.write(L0, &format!("Start Time:\t{}\n", ctx.rfc1123z(start)));
    }
    if !done.is_null() {
        w.write(L0, &format!("Completed At:\t{}\n", ctx.rfc1123z(done)));
    }
    if let (Some(s), Some(d)) = (parse_time(start), parse_time(done)) {
        w.write(
            L0,
            &format!("Duration:\t{}\n", human_duration(ctx.nanos_between(s, d))),
        );
    }
    if let Some(a) = spec.opt_i("activeDeadlineSeconds") {
        w.write(L0, &format!("Active Deadline Seconds:\t{a}s\n"));
    }
    match status.opt_i("ready") {
        None => w.write(
            L0,
            &format!(
                "Pods Statuses:\t{} Active / {} Succeeded / {} Failed\n",
                status.i("active"),
                status.i("succeeded"),
                status.i("failed")
            ),
        ),
        Some(r) => w.write(
            L0,
            &format!(
                "Pods Statuses:\t{} Active ({r} Ready) / {} Succeeded / {} Failed\n",
                status.i("active"),
                status.i("succeeded"),
                status.i("failed")
            ),
        ),
    }
    if spec.s("completionMode") == "Indexed" {
        w.write(
            L0,
            &format!(
                "Completed Indexes:\t{}\n",
                cap_indexes(status.s("completedIndexes"), 50)
            ),
        );
    }
    describe_pod_template(ctx, spec.get("template"), &mut w);
    describe_events(ctx, &mut w, events);
    Ok(w.finish())
}

/// capIndexesListOrNone.
fn cap_indexes(indexes: &str, soft_limit: usize) -> String {
    if indexes.is_empty() {
        return "<none>".to_string();
    }
    let b = indexes.as_bytes();
    let mut ix = soft_limit;
    while ix < b.len() {
        if b[ix] == b',' {
            break;
        }
        ix += 1;
    }
    if ix >= b.len() {
        return indexes.to_string();
    }
    format!("{}...", &indexes[..=ix])
}

pub(crate) fn describe_cron_job(ctx: &Ctx, cj: J, events: &[Value]) -> Result<String> {
    let (meta, spec, status) = (cj.get("metadata"), cj.get("spec"), cj.get("status"));
    let mut w = PrefixWriter::new();
    w.write(L0, &format!("Name:\t{}\n", meta.s("name")));
    w.write(L0, &format!("Namespace:\t{}\n", meta.s("namespace")));
    print_labels_multiline(&mut w, "Labels", &meta.smap("labels"));
    print_annotations_multiline(&mut w, "Annotations", &meta.smap("annotations"));
    w.write(L0, &format!("Schedule:\t{}\n", spec.s("schedule")));
    w.write(
        L0,
        &format!("Concurrency Policy:\t{}\n", spec.s("concurrencyPolicy")),
    );
    w.write(
        L0,
        &format!("Suspend:\t{}\n", print_bool_ptr(spec.opt_b("suspend"))),
    );
    let opt_line = |w: &mut PrefixWriter, title: &str, v: Option<i64>, suffix: &str| match v {
        Some(n) => w.write(L0, &format!("{title}:\t{n}{suffix}\n")),
        None => w.write(L0, &format!("{title}:\t<unset>\n")),
    };
    opt_line(
        &mut w,
        "Successful Job History Limit",
        spec.opt_i("successfulJobsHistoryLimit"),
        "",
    );
    opt_line(
        &mut w,
        "Failed Job History Limit",
        spec.opt_i("failedJobsHistoryLimit"),
        "",
    );
    opt_line(
        &mut w,
        "Starting Deadline Seconds",
        spec.opt_i("startingDeadlineSeconds"),
        "s",
    );
    let jt = spec.get("jobTemplate.spec");
    if jt.exists("selector") {
        match selector_string(jt.get("selector")) {
            Ok(s) => w.write(L0, &format!("Selector:\t{s}\n")),
            Err(e) => w.write(L0, &format!("Selector:\tFailed to get selector: {e}\n")),
        }
    } else {
        w.write(L0, "Selector:\t<unset>\n");
    }
    opt_line(&mut w, "Parallelism", jt.opt_i("parallelism"), "");
    opt_line(&mut w, "Completions", jt.opt_i("completions"), "");
    if let Some(a) = jt.opt_i("activeDeadlineSeconds") {
        w.write(L0, &format!("Active Deadline Seconds:\t{a}s\n"));
    }
    describe_pod_template(ctx, jt.get("template"), &mut w);
    let last = status.get("lastScheduleTime");
    if last.is_null() {
        w.write(L0, "Last Schedule Time:\t<unset>\n");
    } else {
        w.write(
            L0,
            &format!("Last Schedule Time:\t{}\n", ctx.rfc1123z(last)),
        );
    }
    w.write(L0, "Active Jobs:\t");
    let active = status.arr("active");
    if active.is_empty() {
        w.write_line("<none>");
    } else {
        for (i, j) in active.iter().enumerate() {
            if i != 0 {
                w.write(L0, ", ");
            }
            w.write(L0, j.s("name"));
        }
        w.write_line("");
    }
    describe_events(ctx, &mut w, events);
    Ok(w.finish())
}
