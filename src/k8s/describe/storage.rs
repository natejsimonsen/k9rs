//! Storage describers: PersistentVolume, PersistentVolumeClaim,
//! StorageClass, CSINode and VolumeAttributesClass.

use anyhow::Result;
use serde_json::Value;

use super::events::describe_events;
use super::util::{
    format_labels, go_slice, print_annotations_multiline, print_bool_ptr, print_labels_multiline,
    print_pods_multiline, Ctx, J,
};
use super::volumes::{self, access_modes, canonical_or_zero, print_pvc_common, BETA_CLASS};
use super::writer::{PrefixWriter, L0, L1, L2, L3, L4};

fn print_node_selector_terms(w: &mut PrefixWriter, level: usize, title: &str, reqs: Vec<J>) {
    w.write(level, &format!("{title}:\t"));
    if reqs.is_empty() {
        w.write_line("<none>");
        return;
    }
    for (i, r) in reqs.iter().enumerate() {
        if i != 0 {
            w.write(level, "\t");
        }
        let mut expr = format!("{} {}", r.s("key"), r.s("operator").to_lowercase());
        let values = r.strs("values");
        if !values.is_empty() {
            expr = format!("{expr} [{}]", values.join(", "));
        }
        w.write(L0, &format!("{expr}\n"));
    }
}

fn print_volume_node_affinity(w: &mut PrefixWriter, affinity: J) {
    w.write(L0, "Node Affinity:\t");
    if affinity.is_null() || !affinity.exists("required") {
        w.write_line("<none>");
        return;
    }
    w.write_line("");
    w.write(L1, "Required Terms:\t");
    let terms = affinity.arr("required.nodeSelectorTerms");
    if terms.is_empty() {
        w.write_line("<none>");
    } else {
        w.write_line("");
        for (i, t) in terms.into_iter().enumerate() {
            print_node_selector_terms(w, L2, &format!("Term {i}"), t.arr("matchExpressions"));
        }
    }
}

pub(crate) fn describe_persistent_volume(ctx: &Ctx, pv: J, events: &[Value]) -> Result<String> {
    let (meta, spec, status) = (pv.get("metadata"), pv.get("spec"), pv.get("status"));
    let mut w = PrefixWriter::new();
    w.write(L0, &format!("Name:\t{}\n", meta.s("name")));
    print_labels_multiline(&mut w, "Labels", &meta.smap("labels"));
    print_annotations_multiline(&mut w, "Annotations", &meta.smap("annotations"));
    w.write(
        L0,
        &format!("Finalizers:\t{}\n", go_slice(&meta.strs("finalizers"))),
    );
    let class = match meta.get("annotations").key(BETA_CLASS).0.as_str() {
        Some(c) => c.to_string(),
        None => spec.s("storageClassName").to_string(),
    };
    w.write(L0, &format!("StorageClass:\t{class}\n"));
    if meta.exists("deletionTimestamp") {
        w.write(
            L0,
            &format!(
                "Status:\tTerminating (lasts {})\n",
                ctx.since(meta.get("deletionTimestamp"))
            ),
        );
    } else {
        w.write(L0, &format!("Status:\t{}\n", status.s("phase")));
    }
    if spec.exists("claimRef") {
        w.write(
            L0,
            &format!(
                "Claim:\t{}/{}\n",
                spec.s("claimRef.namespace"),
                spec.s("claimRef.name")
            ),
        );
    } else {
        w.write(L0, "Claim:\t\n");
    }
    w.write(
        L0,
        &format!(
            "Reclaim Policy:\t{}\n",
            spec.s("persistentVolumeReclaimPolicy")
        ),
    );
    w.write(
        L0,
        &format!(
            "Access Modes:\t{}\n",
            access_modes(&spec.strs("accessModes"))
        ),
    );
    if let Some(m) = spec.opt_s("volumeMode") {
        w.write(L0, &format!("VolumeMode:\t{m}\n"));
    }
    w.write(
        L0,
        &format!(
            "Capacity:\t{}\n",
            canonical_or_zero(spec.get("capacity").key("storage"))
        ),
    );
    print_volume_node_affinity(&mut w, spec.get("nodeAffinity"));
    w.write(L0, &format!("Message:\t{}\n", status.s("message")));
    w.write(L0, "Source:\n");
    let has = |k: &str| spec.exists(k);
    let w2 = &mut w;
    if has("hostPath") {
        volumes::host_path(spec.get("hostPath"), w2);
    } else if has("gcePersistentDisk") {
        volumes::gce_pd(spec.get("gcePersistentDisk"), w2);
    } else if has("awsElasticBlockStore") {
        volumes::aws_ebs(spec.get("awsElasticBlockStore"), w2);
    } else if has("nfs") {
        volumes::nfs(spec.get("nfs"), w2);
    } else if has("iscsi") {
        volumes::iscsi(spec.get("iscsi"), w2, true);
    } else if has("glusterfs") {
        volumes::glusterfs_persistent(spec.get("glusterfs"), w2);
    } else if has("rbd") {
        volumes::rbd(spec.get("rbd"), w2, true);
    } else if has("quobyte") {
        volumes::quobyte(spec.get("quobyte"), w2);
    } else if has("vsphereVolume") {
        volumes::vsphere(spec.get("vsphereVolume"), w2);
    } else if has("cinder") {
        volumes::cinder(spec.get("cinder"), w2, true);
    } else if has("azureDisk") {
        volumes::azure_disk(spec.get("azureDisk"), w2);
    } else if has("photonPersistentDisk") {
        volumes::photon(spec.get("photonPersistentDisk"), w2);
    } else if has("portworxVolume") {
        volumes::portworx(spec.get("portworxVolume"), w2);
    } else if has("scaleIO") {
        volumes::scaleio_persistent(spec.get("scaleIO"), w2);
    } else if has("local") {
        volumes::local(spec.get("local"), w2);
    } else if has("cephfs") {
        volumes::cephfs(spec.get("cephfs"), w2, true);
    } else if has("storageos") {
        volumes::storageos(spec.get("storageos"), w2);
    } else if has("fc") {
        volumes::fc(spec.get("fc"), w2);
    } else if has("azureFile") {
        volumes::azure_file_persistent(spec.get("azureFile"), w2);
    } else if has("flexVolume") {
        volumes::flex(spec.get("flexVolume"), w2, true);
    } else if has("flocker") {
        volumes::flocker(spec.get("flocker"), w2);
    } else if has("csi") {
        let c = spec.get("csi");
        w2.write(
            L2,
            &format!(
                "Type:\tCSI (a Container Storage Interface (CSI) volume source)\n    Driver:\t{}\n    FSType:\t{}\n    VolumeHandle:\t{}\n    ReadOnly:\t{}\n",
                c.s("driver"),
                c.s("fsType"),
                c.s("volumeHandle"),
                c.b("readOnly")
            ),
        );
        volumes::csi_attributes(c, w2);
    } else {
        w2.write(L1, "<unknown>\n");
    }
    describe_events(ctx, &mut w, events);
    Ok(w.finish())
}

/// getPodsForPVC over the related pods.
fn pods_for_pvc(ctx: &Ctx, pvc: J) -> Vec<String> {
    let Some(pods) = ctx.rel.pods.as_ref() else {
        return Vec::new();
    };
    let ns = pvc.s("metadata.namespace");
    let name = pvc.s("metadata.name");
    let ns_pods: Vec<J> = pods
        .iter()
        .map(J)
        .filter(|p| p.s("metadata.namespace") == ns)
        .collect();
    let mut out: Vec<J> = Vec::new();
    for p in &ns_pods {
        for v in p.arr("spec.volumes") {
            if v.exists("persistentVolumeClaim") && v.s("persistentVolumeClaim.claimName") == name {
                out.push(*p);
            }
        }
    }
    for owner in pvc.arr("metadata.ownerReferences") {
        if owner.s("kind") != "Pod" {
            continue;
        }
        let Some(pod) = ns_pods
            .iter()
            .find(|p| p.s("metadata.uid") == owner.s("uid"))
        else {
            continue;
        };
        if out
            .iter()
            .any(|p| p.s("metadata.uid") == pod.s("metadata.uid"))
        {
            continue;
        }
        out.push(*pod);
    }
    out.into_iter()
        .map(|p| p.s("metadata.name").to_string())
        .collect()
}

pub(crate) fn describe_persistent_volume_claim(
    ctx: &Ctx,
    pvc: J,
    events: &[Value],
) -> Result<String> {
    let (meta, spec, status) = (pvc.get("metadata"), pvc.get("spec"), pvc.get("status"));
    let mut w = PrefixWriter::new();
    let status_line = if meta.exists("deletionTimestamp") {
        format!(
            "Terminating (lasts {})",
            ctx.since(meta.get("deletionTimestamp"))
        )
    } else {
        status.s("phase").to_string()
    };
    print_pvc_common(
        &mut w,
        meta,
        spec,
        status,
        Some((
            meta.s("name"),
            meta.s("namespace"),
            status_line,
            go_slice(&meta.strs("finalizers")),
        )),
    );
    print_pods_multiline(&mut w, "Used By", &pods_for_pvc(ctx, pvc));
    let conditions = status.arr("conditions");
    if !conditions.is_empty() {
        w.write(L0, "Conditions:\n");
        w.write(
            L1,
            "Type\tStatus\tLastProbeTime\tLastTransitionTime\tReason\tMessage\n",
        );
        w.write(
            L1,
            "----\t------\t-----------------\t------------------\t------\t-------\n",
        );
        for c in conditions {
            w.write(
                L1,
                &format!(
                    "{} \t{} \t{} \t{} \t{} \t{}\n",
                    c.s("type"),
                    c.s("status"),
                    ctx.rfc1123z(c.get("lastProbeTime")),
                    ctx.rfc1123z(c.get("lastTransitionTime")),
                    c.s("reason"),
                    c.s("message")
                ),
            );
        }
    }
    describe_events(ctx, &mut w, events);
    Ok(w.finish())
}

pub(crate) fn describe_storage_class(ctx: &Ctx, sc: J, events: &[Value]) -> Result<String> {
    let meta = sc.get("metadata");
    let annotations = meta.smap("annotations");
    let is_default = annotations
        .get("storageclass.kubernetes.io/is-default-class")
        .map(String::as_str)
        == Some("true")
        || annotations
            .get("storageclass.beta.kubernetes.io/is-default-class")
            .map(String::as_str)
            == Some("true");
    let mut w = PrefixWriter::new();
    w.write(L0, &format!("Name:\t{}\n", meta.s("name")));
    w.write(
        L0,
        &format!(
            "IsDefaultClass:\t{}\n",
            if is_default { "Yes" } else { "No" }
        ),
    );
    w.write(
        L0,
        &format!("Annotations:\t{}\n", format_labels(&annotations)),
    );
    w.write(L0, &format!("Provisioner:\t{}\n", sc.s("provisioner")));
    w.write(
        L0,
        &format!("Parameters:\t{}\n", format_labels(&sc.smap("parameters"))),
    );
    w.write(
        L0,
        &format!(
            "AllowVolumeExpansion:\t{}\n",
            print_bool_ptr(sc.opt_b("allowVolumeExpansion"))
        ),
    );
    let opts = sc.strs("mountOptions");
    if opts.is_empty() {
        w.write(L0, "MountOptions:\t<none>\n");
    } else {
        w.write(L0, "MountOptions:\n");
        for o in opts {
            w.write(L1, &format!("{o}\n"));
        }
    }
    if let Some(p) = sc.opt_s("reclaimPolicy") {
        w.write(L0, &format!("ReclaimPolicy:\t{p}\n"));
    }
    if let Some(m) = sc.opt_s("volumeBindingMode") {
        w.write(L0, &format!("VolumeBindingMode:\t{m}\n"));
    }
    if sc.exists("allowedTopologies") {
        w.write(L0, "AllowedTopologies:\t");
        let terms = sc.arr("allowedTopologies");
        if terms.is_empty() {
            w.write_line("<none>");
        } else {
            w.write_line("");
            for (i, term) in terms.into_iter().enumerate() {
                w.write(L1, &format!("Term {i}:\t"));
                let reqs = term.arr("matchLabelExpressions");
                if reqs.is_empty() {
                    w.write_line("<none>");
                    continue;
                }
                for (j, r) in reqs.iter().enumerate() {
                    if j != 0 {
                        w.write(L1, "\t");
                    }
                    let mut expr = format!("{} in", r.s("key"));
                    let values = r.strs("values");
                    if !values.is_empty() {
                        expr = format!("{expr} [{}]", values.join(", "));
                    }
                    w.write(L0, &format!("{expr}\n"));
                }
            }
        }
    }
    describe_events(ctx, &mut w, events);
    Ok(w.finish())
}

pub(crate) fn describe_csi_node(ctx: &Ctx, csi: J, events: &[Value]) -> Result<String> {
    let meta = csi.get("metadata");
    let mut w = PrefixWriter::new();
    w.write(L0, &format!("Name:\t{}\n", meta.s("name")));
    print_labels_multiline(&mut w, "Labels", &meta.smap("labels"));
    print_annotations_multiline(&mut w, "Annotations", &meta.smap("annotations"));
    w.write(
        L0,
        &format!(
            "CreationTimestamp:\t{}\n",
            ctx.rfc1123z(meta.get("creationTimestamp"))
        ),
    );
    w.write(L0, "Spec:\n");
    if csi.exists("spec.drivers") {
        w.write(L1, "Drivers:\n");
        for d in csi.arr("spec.drivers") {
            w.write(L2, &format!("{}:\n", d.s("name")));
            w.write(L3, &format!("Node ID:\t{}\n", d.s("nodeID")));
            if let Some(count) = d.opt_i("allocatable.count") {
                w.write(L3, "Allocatables:\n");
                w.write(L4, &format!("Count:\t{count}\n"));
            }
            if d.exists("topologyKeys") {
                w.write(
                    L3,
                    &format!("Topology Keys:\t{}\n", go_slice(&d.strs("topologyKeys"))),
                );
            }
        }
    }
    describe_events(ctx, &mut w, events);
    Ok(w.finish())
}

pub(crate) fn describe_volume_attributes_class(
    ctx: &Ctx,
    vac: J,
    events: &[Value],
) -> Result<String> {
    let mut w = PrefixWriter::new();
    w.write(L0, &format!("Name:\t{}\n", vac.s("metadata.name")));
    w.write(
        L0,
        &format!(
            "Annotations:\t{}\n",
            format_labels(&vac.smap("metadata.annotations"))
        ),
    );
    w.write(L0, &format!("DriverName:\t{}\n", vac.s("driverName")));
    w.write(
        L0,
        &format!("Parameters:\t{}\n", format_labels(&vac.smap("parameters"))),
    );
    describe_events(ctx, &mut w, events);
    Ok(w.finish())
}
