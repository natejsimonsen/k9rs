//! Volume source printers shared by pods, pod templates and PVs.

use super::quantity::{canonical, parse_or_zero};
use super::util::{go_map, go_slice, print_annotations_multiline, print_labels_multiline, J};
use super::writer::{PrefixWriter, L0, L1, L2, L3};

/// Go `%v` of a `*LocalObjectReference`: `&{name}` or `<nil>`.
fn local_ref(j: J) -> String {
    if j.is_null() {
        "<nil>".to_string()
    } else {
        format!("&{{{}}}", j.s("name"))
    }
}

/// Go `%v` of a `*SecretReference`: `&{name namespace}` or `<nil>`.
fn secret_ref(j: J) -> String {
    if j.is_null() {
        "<nil>".to_string()
    } else {
        format!("&{{{} {}}}", j.s("name"), j.s("namespace"))
    }
}

/// describeVolumes.
pub(crate) fn describe_volumes(volumes: Vec<J>, w: &mut PrefixWriter, space: &str) {
    if volumes.is_empty() {
        w.write(L0, &format!("{space}Volumes:\t<none>\n"));
        return;
    }
    w.write(L0, &format!("{space}Volumes:\n"));
    let name_indent = if space.is_empty() { "" } else { " " };
    for v in volumes {
        w.write(L1, &format!("{name_indent}{}:\n", v.s("name")));
        let has = |k: &str| v.exists(k);
        if has("hostPath") {
            host_path(v.get("hostPath"), w);
        } else if has("emptyDir") {
            empty_dir(v.get("emptyDir"), w);
        } else if has("gcePersistentDisk") {
            gce_pd(v.get("gcePersistentDisk"), w);
        } else if has("awsElasticBlockStore") {
            aws_ebs(v.get("awsElasticBlockStore"), w);
        } else if has("gitRepo") {
            let g = v.get("gitRepo");
            w.write(
                L2,
                &format!(
                    "Type:\tGitRepo (a volume that is pulled from git when the pod is created)\n    Repository:\t{}\n    Revision:\t{}\n",
                    g.s("repository"),
                    g.s("revision")
                ),
            );
        } else if has("secret") {
            let s = v.get("secret");
            w.write(
                L2,
                &format!(
                    "Type:\tSecret (a volume populated by a Secret)\n    SecretName:\t{}\n    Optional:\t{}\n",
                    s.s("secretName"),
                    s.b("optional")
                ),
            );
        } else if has("configMap") {
            let c = v.get("configMap");
            w.write(
                L2,
                &format!(
                    "Type:\tConfigMap (a volume populated by a ConfigMap)\n    Name:\t{}\n    Optional:\t{}\n",
                    c.s("name"),
                    c.b("optional")
                ),
            );
        } else if has("nfs") {
            nfs(v.get("nfs"), w);
        } else if has("iscsi") {
            iscsi(v.get("iscsi"), w, false);
        } else if has("glusterfs") {
            let g = v.get("glusterfs");
            w.write(
                L2,
                &format!(
                    "Type:\tGlusterfs (a Glusterfs mount on the host that shares a pod's lifetime)\n    EndpointsName:\t{}\n    Path:\t{}\n    ReadOnly:\t{}\n",
                    g.s("endpoints"),
                    g.s("path"),
                    g.b("readOnly")
                ),
            );
        } else if has("persistentVolumeClaim") {
            let c = v.get("persistentVolumeClaim");
            w.write(
                L2,
                &format!(
                    "Type:\tPersistentVolumeClaim (a reference to a PersistentVolumeClaim in the same namespace)\n    ClaimName:\t{}\n    ReadOnly:\t{}\n",
                    c.s("claimName"),
                    c.b("readOnly")
                ),
            );
        } else if has("ephemeral") {
            ephemeral(v.get("ephemeral"), w);
        } else if has("rbd") {
            rbd(v.get("rbd"), w, false);
        } else if has("quobyte") {
            quobyte(v.get("quobyte"), w);
        } else if has("downwardAPI") {
            downward_api(v.get("downwardAPI"), w);
        } else if has("azureDisk") {
            azure_disk(v.get("azureDisk"), w);
        } else if has("vsphereVolume") {
            vsphere(v.get("vsphereVolume"), w);
        } else if has("cinder") {
            cinder(v.get("cinder"), w, false);
        } else if has("photonPersistentDisk") {
            photon(v.get("photonPersistentDisk"), w);
        } else if has("portworxVolume") {
            portworx(v.get("portworxVolume"), w);
        } else if has("scaleIO") {
            let s = v.get("scaleIO");
            w.write(
                L2,
                &format!(
                    "Type:\tScaleIO (a persistent volume backed by a block device in ScaleIO)\n    Gateway:\t{}\n    System:\t{}\n    Protection Domain:\t{}\n    Storage Pool:\t{}\n    Storage Mode:\t{}\n    VolumeName:\t{}\n    FSType:\t{}\n    ReadOnly:\t{}\n",
                    s.s("gateway"),
                    s.s("system"),
                    s.s("protectionDomain"),
                    s.s("storagePool"),
                    s.s("storageMode"),
                    s.s("volumeName"),
                    s.s("fsType"),
                    s.b("readOnly")
                ),
            );
        } else if has("cephfs") {
            cephfs(v.get("cephfs"), w, false);
        } else if has("storageos") {
            storageos(v.get("storageos"), w);
        } else if has("fc") {
            fc(v.get("fc"), w);
        } else if has("azureFile") {
            let a = v.get("azureFile");
            w.write(
                L2,
                &format!(
                    "Type:\tAzureFile (an Azure File Service mount on the host and bind mount to the pod)\n    SecretName:\t{}\n    ShareName:\t{}\n    ReadOnly:\t{}\n",
                    a.s("secretName"),
                    a.s("shareName"),
                    a.b("readOnly")
                ),
            );
        } else if has("flexVolume") {
            flex(v.get("flexVolume"), w, false);
        } else if has("flocker") {
            flocker(v.get("flocker"), w);
        } else if has("projected") {
            projected(v.get("projected"), w);
        } else if has("csi") {
            let c = v.get("csi");
            w.write(
                L2,
                &format!(
                    "Type:\tCSI (a Container Storage Interface (CSI) volume source)\n    Driver:\t{}\n    FSType:\t{}\n    ReadOnly:\t{}\n",
                    c.s("driver"),
                    c.s("fsType"),
                    c.opt_b("readOnly").unwrap_or(false)
                ),
            );
            csi_attributes(c, w);
        } else if has("image") {
            let i = v.get("image");
            w.write(
                L2,
                &format!(
                    "Type:\tImage (a container image or OCI artifact)\n    Reference:\t{}\n    PullPolicy:\t{}\n",
                    i.s("reference"),
                    i.s("pullPolicy")
                ),
            );
        } else {
            w.write(L1, "<unknown>\n");
        }
    }
}

pub(crate) fn host_path(h: J, w: &mut PrefixWriter) {
    let t = h.opt_s("type").unwrap_or("<none>");
    w.write(
        L2,
        &format!(
            "Type:\tHostPath (bare host directory volume)\n    Path:\t{}\n    HostPathType:\t{}\n",
            h.s("path"),
            t
        ),
    );
}

fn empty_dir(e: J, w: &mut PrefixWriter) {
    let size = match e.opt_s("sizeLimit") {
        Some(raw) if parse_or_zero(raw).cmp(&Default::default()).is_gt() => canonical(raw),
        _ => "<unset>".to_string(),
    };
    w.write(
        L2,
        &format!(
            "Type:\tEmptyDir (a temporary directory that shares a pod's lifetime)\n    Medium:\t{}\n    SizeLimit:\t{}\n",
            e.s("medium"),
            size
        ),
    );
}

pub(crate) fn gce_pd(g: J, w: &mut PrefixWriter) {
    w.write(
        L2,
        &format!(
            "Type:\tGCEPersistentDisk (a Persistent Disk resource in Google Compute Engine)\n    PDName:\t{}\n    FSType:\t{}\n    Partition:\t{}\n    ReadOnly:\t{}\n",
            g.s("pdName"),
            g.s("fsType"),
            g.i("partition"),
            g.b("readOnly")
        ),
    );
}

pub(crate) fn aws_ebs(a: J, w: &mut PrefixWriter) {
    w.write(
        L2,
        &format!(
            "Type:\tAWSElasticBlockStore (a Persistent Disk resource in AWS)\n    VolumeID:\t{}\n    FSType:\t{}\n    Partition:\t{}\n    ReadOnly:\t{}\n",
            a.s("volumeID"),
            a.s("fsType"),
            a.i("partition"),
            a.b("readOnly")
        ),
    );
}

pub(crate) fn nfs(n: J, w: &mut PrefixWriter) {
    w.write(
        L2,
        &format!(
            "Type:\tNFS (an NFS mount that lasts the lifetime of a pod)\n    Server:\t{}\n    Path:\t{}\n    ReadOnly:\t{}\n",
            n.s("server"),
            n.s("path"),
            n.b("readOnly")
        ),
    );
}

pub(crate) fn quobyte(q: J, w: &mut PrefixWriter) {
    w.write(
        L2,
        &format!(
            "Type:\tQuobyte (a Quobyte mount on the host that shares a pod's lifetime)\n    Registry:\t{}\n    Volume:\t{}\n    ReadOnly:\t{}\n",
            q.s("registry"),
            q.s("volume"),
            q.b("readOnly")
        ),
    );
}

pub(crate) fn portworx(p: J, w: &mut PrefixWriter) {
    w.write(
        L2,
        &format!(
            "Type:\tPortworxVolume (a Portworx Volume resource)\n    VolumeID:\t{}\n",
            p.s("volumeID")
        ),
    );
}

/// ISCSI volume; `persistent` selects the PV flavour (SecretReference).
pub(crate) fn iscsi(i: J, w: &mut PrefixWriter, persistent: bool) {
    let initiator = i.opt_s("initiatorName").unwrap_or("<none>");
    let secret = if persistent {
        secret_ref(i.get("secretRef"))
    } else {
        local_ref(i.get("secretRef"))
    };
    w.write(
        L2,
        &format!(
            "Type:\tISCSI (an ISCSI Disk resource that is attached to a kubelet's host machine and then exposed to the pod)\n    TargetPortal:\t{}\n    IQN:\t{}\n    Lun:\t{}\n    ISCSIInterface\t{}\n    FSType:\t{}\n    ReadOnly:\t{}\n    Portals:\t{}\n    DiscoveryCHAPAuth:\t{}\n    SessionCHAPAuth:\t{}\n    SecretRef:\t{}\n    InitiatorName:\t{}\n",
            i.s("targetPortal"),
            i.s("iqn"),
            i.i("lun"),
            i.s("iscsiInterface"),
            i.s("fsType"),
            i.b("readOnly"),
            go_slice(&i.strs("portals")),
            i.b("chapAuthDiscovery"),
            i.b("chapAuthSession"),
            secret,
            initiator
        ),
    );
}

pub(crate) fn glusterfs_persistent(g: J, w: &mut PrefixWriter) {
    let ns = g.opt_s("endpointsNamespace").unwrap_or("<unset>");
    w.write(
        L2,
        &format!(
            "Type:\tGlusterfs (a Glusterfs mount on the host that shares a pod's lifetime)\n    EndpointsName:\t{}\n    EndpointsNamespace:\t{}\n    Path:\t{}\n    ReadOnly:\t{}\n",
            g.s("endpoints"),
            ns,
            g.s("path"),
            g.b("readOnly")
        ),
    );
}

pub(crate) fn rbd(r: J, w: &mut PrefixWriter, persistent: bool) {
    let secret = if persistent {
        secret_ref(r.get("secretRef"))
    } else {
        local_ref(r.get("secretRef"))
    };
    w.write(
        L2,
        &format!(
            "Type:\tRBD (a Rados Block Device mount on the host that shares a pod's lifetime)\n    CephMonitors:\t{}\n    RBDImage:\t{}\n    FSType:\t{}\n    RBDPool:\t{}\n    RadosUser:\t{}\n    Keyring:\t{}\n    SecretRef:\t{}\n    ReadOnly:\t{}\n",
            go_slice(&r.strs("monitors")),
            r.s("image"),
            r.s("fsType"),
            r.s("pool"),
            r.s("user"),
            r.s("keyring"),
            secret,
            r.b("readOnly")
        ),
    );
}

fn downward_api(d: J, w: &mut PrefixWriter) {
    w.write(
        L2,
        "Type:\tDownwardAPI (a volume populated by information about the pod)\n    Items:\n",
    );
    for item in d.arr("items") {
        if item.exists("fieldRef") {
            w.write(
                L3,
                &format!("{} -> {}\n", item.s("fieldRef.fieldPath"), item.s("path")),
            );
        }
        if item.exists("resourceFieldRef") {
            w.write(
                L3,
                &format!(
                    "{} -> {}\n",
                    item.s("resourceFieldRef.resource"),
                    item.s("path")
                ),
            );
        }
    }
}

pub(crate) fn azure_disk(d: J, w: &mut PrefixWriter) {
    w.write(
        L2,
        &format!(
            "Type:\tAzureDisk (an Azure Data Disk mount on the host and bind mount to the pod)\n    DiskName:\t{}\n    DiskURI:\t{}\n    Kind: \t{}\n    FSType:\t{}\n    CachingMode:\t{}\n    ReadOnly:\t{}\n",
            d.s("diskName"),
            d.s("diskURI"),
            d.s("kind"),
            d.s("fsType"),
            d.s("cachingMode"),
            d.b("readOnly")
        ),
    );
}

pub(crate) fn vsphere(v: J, w: &mut PrefixWriter) {
    w.write(
        L2,
        &format!(
            "Type:\tvSphereVolume (a Persistent Disk resource in vSphere)\n    VolumePath:\t{}\n    FSType:\t{}\n    StoragePolicyName:\t{}\n",
            v.s("volumePath"),
            v.s("fsType"),
            v.s("storagePolicyName")
        ),
    );
}

pub(crate) fn photon(p: J, w: &mut PrefixWriter) {
    w.write(
        L2,
        &format!(
            "Type:\tPhotonPersistentDisk (a Persistent Disk resource in photon platform)\n    PdID:\t{}\n    FSType:\t{}\n",
            p.s("pdID"),
            p.s("fsType")
        ),
    );
}

pub(crate) fn cinder(c: J, w: &mut PrefixWriter, persistent: bool) {
    let secret = if persistent {
        secret_ref(c.get("secretRef"))
    } else {
        local_ref(c.get("secretRef"))
    };
    w.write(
        L2,
        &format!(
            "Type:\tCinder (a Persistent Disk resource in OpenStack)\n    VolumeID:\t{}\n    FSType:\t{}\n    ReadOnly:\t{}\n    SecretRef:\t{}\n",
            c.s("volumeID"),
            c.s("fsType"),
            c.b("readOnly"),
            secret
        ),
    );
}

pub(crate) fn scaleio_persistent(s: J, w: &mut PrefixWriter) {
    w.write(
        L2,
        &format!(
            "Type:\tScaleIO (a persistent volume backed by a block device in ScaleIO)\n    Gateway:\t{}\n    System:\t{}\n    Protection Domain:\t{}\n    Storage Pool:\t{}\n    Storage Mode:\t{}\n    VolumeName:\t{}\n    SecretName:\t{}\n    SecretNamespace:\t{}\n    FSType:\t{}\n    ReadOnly:\t{}\n",
            s.s("gateway"),
            s.s("system"),
            s.s("protectionDomain"),
            s.s("storagePool"),
            s.s("storageMode"),
            s.s("volumeName"),
            s.s("secretRef.name"),
            s.s("secretRef.namespace"),
            s.s("fsType"),
            s.b("readOnly")
        ),
    );
}

pub(crate) fn local(l: J, w: &mut PrefixWriter) {
    w.write(
        L2,
        &format!(
            "Type:\tLocalVolume (a persistent volume backed by local storage on a node)\n    Path:\t{}\n",
            l.s("path")
        ),
    );
}

pub(crate) fn cephfs(c: J, w: &mut PrefixWriter, persistent: bool) {
    let secret = if persistent {
        secret_ref(c.get("secretRef"))
    } else {
        local_ref(c.get("secretRef"))
    };
    w.write(
        L2,
        &format!(
            "Type:\tCephFS (a CephFS mount on the host that shares a pod's lifetime)\n    Monitors:\t{}\n    Path:\t{}\n    User:\t{}\n    SecretFile:\t{}\n    SecretRef:\t{}\n    ReadOnly:\t{}\n",
            go_slice(&c.strs("monitors")),
            c.s("path"),
            c.s("user"),
            c.s("secretFile"),
            secret,
            c.b("readOnly")
        ),
    );
}

pub(crate) fn storageos(s: J, w: &mut PrefixWriter) {
    w.write(
        L2,
        &format!(
            "Type:\tStorageOS (a StorageOS Persistent Disk resource)\n    VolumeName:\t{}\n    VolumeNamespace:\t{}\n    FSType:\t{}\n    ReadOnly:\t{}\n",
            s.s("volumeName"),
            s.s("volumeNamespace"),
            s.s("fsType"),
            s.b("readOnly")
        ),
    );
}

pub(crate) fn fc(f: J, w: &mut PrefixWriter) {
    let lun = f
        .opt_i("lun")
        .map(|l| l.to_string())
        .unwrap_or_else(|| "<none>".to_string());
    w.write(
        L2,
        &format!(
            "Type:\tFC (a Fibre Channel disk)\n    TargetWWNs:\t{}\n    LUN:\t{}\n    FSType:\t{}\n    ReadOnly:\t{}\n",
            f.strs("targetWWNs").join(", "),
            lun,
            f.s("fsType"),
            f.b("readOnly")
        ),
    );
}

pub(crate) fn azure_file_persistent(a: J, w: &mut PrefixWriter) {
    w.write(
        L2,
        &format!(
            "Type:\tAzureFile (an Azure File Service mount on the host and bind mount to the pod)\n    SecretName:\t{}\n    SecretNamespace:\t{}\n    ShareName:\t{}\n    ReadOnly:\t{}\n",
            a.s("secretName"),
            a.s("secretNamespace"),
            a.s("shareName"),
            a.b("readOnly")
        ),
    );
}

pub(crate) fn flex(f: J, w: &mut PrefixWriter, persistent: bool) {
    let secret = if persistent {
        secret_ref(f.get("secretRef"))
    } else {
        local_ref(f.get("secretRef"))
    };
    w.write(
        L2,
        &format!(
            "Type:\tFlexVolume (a generic volume resource that is provisioned/attached using an exec based plugin)\n    Driver:\t{}\n    FSType:\t{}\n    SecretRef:\t{}\n    ReadOnly:\t{}\n    Options:\t{}\n",
            f.s("driver"),
            f.s("fsType"),
            secret,
            f.b("readOnly"),
            go_map(&f.smap("options"))
        ),
    );
}

pub(crate) fn flocker(f: J, w: &mut PrefixWriter) {
    w.write(
        L2,
        &format!(
            "Type:\tFlocker (a Flocker volume mounted by the Flocker agent)\n    DatasetName:\t{}\n    DatasetUUID:\t{}\n",
            f.s("datasetName"),
            f.s("datasetUUID")
        ),
    );
}

fn projected(p: J, w: &mut PrefixWriter) {
    w.write(
        L2,
        "Type:\tProjected (a volume that contains injected data from multiple sources)\n",
    );
    for src in p.arr("sources") {
        if src.exists("secret") {
            w.write(
                L2,
                &format!(
                    "SecretName:\t{}\n    Optional:\t{}\n",
                    src.s("secret.name"),
                    src.b("secret.optional")
                ),
            );
        } else if src.exists("downwardAPI") {
            w.write(L2, "DownwardAPI:\ttrue\n");
        } else if src.exists("configMap") {
            w.write(
                L2,
                &format!(
                    "ConfigMapName:\t{}\n    Optional:\t{}\n",
                    src.s("configMap.name"),
                    src.b("configMap.optional")
                ),
            );
        } else if src.exists("serviceAccountToken") {
            w.write(
                L2,
                &format!(
                    "TokenExpirationSeconds:\t{}\n",
                    src.i("serviceAccountToken.expirationSeconds")
                ),
            );
        }
    }
}

const MAX_ANNOTATION_LEN: usize = 140;

/// printCSIPersistentVolumeAttributesMultiline (every write is at LEVEL_2).
pub(crate) fn csi_attributes(c: J, w: &mut PrefixWriter) {
    let attrs = c.smap("volumeAttributes");
    w.write(L2, "VolumeAttributes:\t");
    if attrs.is_empty() {
        w.write_line("<none>");
        return;
    }
    for (i, (k, v)) in attrs.iter().enumerate() {
        if i != 0 {
            w.write(L2, "");
            w.write(L2, "\t");
        }
        let line = format!("{k}={v}");
        if line.len() > MAX_ANNOTATION_LEN {
            let cut = String::from_utf8_lossy(&line.as_bytes()[..MAX_ANNOTATION_LEN]).to_string();
            w.write(L2, &format!("{cut}...\n"));
        } else {
            w.write(L2, &format!("{line}\n"));
        }
    }
}

fn ephemeral(e: J, w: &mut PrefixWriter) {
    w.write(
        L2,
        "Type:\tEphemeralVolume (an inline specification for a volume that gets created and deleted with the pod)\n",
    );
    let tpl = e.get("volumeClaimTemplate");
    if !tpl.is_null() {
        w.nested(L2, |w| {
            print_pvc_common(
                w,
                tpl.get("metadata"),
                tpl.get("spec"),
                J(&serde_json::Value::Null),
                None,
            );
        });
    }
}

/// printPersistentVolumeClaim for the parts shared by full PVCs and claim
/// templates. `full` carries (name, namespace, status line, finalizers) for
/// full PVCs.
pub(crate) fn print_pvc_common(
    w: &mut PrefixWriter,
    meta: J,
    spec: J,
    status: J,
    full: Option<(&str, &str, String, String)>,
) {
    if let Some((name, ns, _, _)) = &full {
        w.write(L0, &format!("Name:\t{name}\n"));
        w.write(L0, &format!("Namespace:\t{ns}\n"));
    }
    w.write(L0, &format!("StorageClass:\t{}\n", pvc_class(meta, spec)));
    if let Some((_, _, status_line, _)) = &full {
        w.write(L0, &format!("Status:\t{status_line}\n"));
    }
    w.write(L0, &format!("Volume:\t{}\n", spec.s("volumeName")));
    print_labels_multiline(w, "Labels", &meta.smap("labels"));
    print_annotations_multiline(w, "Annotations", &meta.smap("annotations"));
    if let Some((_, _, _, finalizers)) = &full {
        w.write(L0, &format!("Finalizers:\t{finalizers}\n"));
    }
    let mut capacity = String::new();
    let mut access = String::new();
    if !spec.s("volumeName").is_empty() {
        access = access_modes(&status.strs("accessModes"));
        capacity = canonical_or_zero(status.get("capacity").key("storage"));
    }
    w.write(L0, &format!("Capacity:\t{capacity}\n"));
    w.write(L0, &format!("Access Modes:\t{access}\n"));
    if let Some(mode) = spec.opt_s("volumeMode") {
        w.write(L0, &format!("VolumeMode:\t{mode}\n"));
    }
    if spec.exists("dataSource") {
        let ds = spec.get("dataSource");
        w.write(L0, "DataSource:\n");
        if let Some(g) = ds.opt_s("apiGroup") {
            w.write(L1, &format!("APIGroup:\t{g}\n"));
        }
        w.write(L1, &format!("Kind:\t{}\n", ds.s("kind")));
        w.write(L1, &format!("Name:\t{}\n", ds.s("name")));
    }
}

/// Quantity.String() of an optional quantity node; missing is zero ("0").
pub(crate) fn canonical_or_zero(j: J) -> String {
    match j.0.as_str() {
        Some(raw) => canonical(raw),
        None => "0".to_string(),
    }
}

/// storageutil.GetPersistentVolumeClaimClass.
pub(crate) fn pvc_class(meta: J, spec: J) -> String {
    if let Some(c) = meta.get("annotations").key(BETA_CLASS).0.as_str() {
        return c.to_string();
    }
    spec.s("storageClassName").to_string()
}

pub(crate) const BETA_CLASS: &str = "volume.beta.kubernetes.io/storage-class";

/// storageutil.GetAccessModesAsString.
pub(crate) fn access_modes(modes: &[String]) -> String {
    let has = |m: &str| modes.iter().any(|x| x == m);
    let mut out = Vec::new();
    if has("ReadWriteOnce") {
        out.push("RWO");
    }
    if has("ReadOnlyMany") {
        out.push("ROX");
    }
    if has("ReadWriteMany") {
        out.push("RWX");
    }
    if has("ReadWriteOncePod") {
        out.push("RWOP");
    }
    out.join(",")
}
