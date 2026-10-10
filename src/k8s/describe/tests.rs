//! Golden tests: realistic objects described exactly as kubectl v0.34 would
//! (expectations derived from kubectl's describe.go and, where it has them,
//! kubectl's own describe_test.go goldens).

use jiff::{tz::TimeZone, Timestamp};
use pretty_assertions::assert_eq;
use serde_json::{json, Value};

use super::{run, Rel};
use crate::k8s::Gvr;

fn now() -> Timestamp {
    "2024-01-01T12:00:00Z".parse().expect("time")
}

fn desc(gvr: &str, obj: Value, events: Vec<Value>) -> String {
    desc_rel(gvr, obj, events, Rel::default())
}

fn desc_rel(gvr: &str, obj: Value, events: Vec<Value>, rel: Rel) -> String {
    run(&Gvr::parse(gvr), &obj, &events, &rel, now(), TimeZone::UTC).expect("describe")
}

/// Join lines with newlines (keeps trailing spaces visible in the source).
fn lines(ls: &[&str]) -> String {
    let mut s = ls.join("\n");
    s.push('\n');
    s
}

fn pod_fixture() -> Value {
    json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {
            "name": "nginx-7c5ddbdf54-abcde",
            "namespace": "default",
            "uid": "pod-uid-1",
            "creationTimestamp": "2024-01-01T10:00:00Z",
            "labels": {"app": "nginx", "pod-template-hash": "7c5ddbdf54"},
            "annotations": {"kubectl.kubernetes.io/restartedAt": "2024-01-01T09:59:00Z"},
            "ownerReferences": [{
                "apiVersion": "apps/v1", "kind": "ReplicaSet", "name": "nginx-7c5ddbdf54",
                "uid": "rs-uid", "controller": true, "blockOwnerDeletion": true
            }]
        },
        "spec": {
            "nodeName": "node-1",
            "serviceAccountName": "default",
            "priority": 0,
            "containers": [{
                "name": "nginx",
                "image": "nginx:1.25",
                "ports": [{"containerPort": 80, "protocol": "TCP", "name": "http"}],
                "args": ["--foo", "--bar"],
                "resources": {
                    "limits": {"memory": "128Mi", "cpu": "500m"},
                    "requests": {"cpu": "0.1", "memory": "64Mi"}
                },
                "livenessProbe": {
                    "httpGet": {"path": "/healthz", "port": 8080, "scheme": "HTTP"},
                    "initialDelaySeconds": 3, "timeoutSeconds": 1, "periodSeconds": 10,
                    "successThreshold": 1, "failureThreshold": 3
                },
                "readinessProbe": {
                    "tcpSocket": {"port": "http"},
                    "timeoutSeconds": 1, "periodSeconds": 5, "successThreshold": 1, "failureThreshold": 3
                },
                "env": [
                    {"name": "PLAIN", "value": "hello"},
                    {"name": "POD_NAME", "valueFrom": {"fieldRef": {"apiVersion": "v1", "fieldPath": "metadata.name"}}},
                    {"name": "NODE", "valueFrom": {"fieldRef": {"apiVersion": "v1", "fieldPath": "spec.nodeName"}}},
                    {"name": "MEM", "valueFrom": {"resourceFieldRef": {"resource": "limits.memory", "divisor": "1Mi"}}},
                    {"name": "CPU_REQ", "valueFrom": {"resourceFieldRef": {"resource": "requests.cpu", "divisor": "1m"}}},
                    {"name": "PASSWORD", "valueFrom": {"secretKeyRef": {"name": "db", "key": "password"}}},
                    {"name": "MODE", "valueFrom": {"configMapKeyRef": {"name": "cfg", "key": "mode", "optional": true}}}
                ],
                "envFrom": [
                    {"configMapRef": {"name": "app-config"}},
                    {"secretRef": {"name": "app-secret", "optional": true}, "prefix": "SEC_"}
                ],
                "volumeMounts": [
                    {"name": "kube-api-access-xyz", "mountPath": "/var/run/secrets/kubernetes.io/serviceaccount", "readOnly": true},
                    {"name": "data", "mountPath": "/data", "subPath": "nginx"}
                ]
            }],
            "volumes": [
                {"name": "data", "persistentVolumeClaim": {"claimName": "data-pvc"}},
                {"name": "cache", "emptyDir": {"sizeLimit": "1Gi"}},
                {"name": "cfg", "configMap": {"name": "nginx-conf", "defaultMode": 420}},
                {"name": "kube-api-access-xyz", "projected": {"sources": [
                    {"serviceAccountToken": {"expirationSeconds": 3607, "path": "token"}},
                    {"configMap": {"name": "kube-root-ca.crt", "items": [{"key": "ca.crt", "path": "ca.crt"}]}},
                    {"downwardAPI": {"items": [{"path": "namespace", "fieldRef": {"apiVersion": "v1", "fieldPath": "metadata.namespace"}}]}}
                ]}}
            ],
            "tolerations": [
                {"key": "node.kubernetes.io/unreachable", "operator": "Exists", "effect": "NoExecute", "tolerationSeconds": 300},
                {"key": "node.kubernetes.io/not-ready", "operator": "Exists", "effect": "NoExecute", "tolerationSeconds": 300}
            ],
            "nodeSelector": {"kubernetes.io/os": "linux"}
        },
        "status": {
            "phase": "Running",
            "hostIP": "10.0.0.5",
            "podIP": "10.244.1.7",
            "podIPs": [{"ip": "10.244.1.7"}],
            "startTime": "2024-01-01T10:00:00Z",
            "qosClass": "Burstable",
            "conditions": [
                {"type": "PodReadyToStartContainers", "status": "True"},
                {"type": "Initialized", "status": "True"},
                {"type": "Ready", "status": "True"},
                {"type": "ContainersReady", "status": "True"},
                {"type": "PodScheduled", "status": "True"}
            ],
            "containerStatuses": [{
                "name": "nginx",
                "containerID": "containerd://abc123",
                "image": "nginx:1.25",
                "imageID": "docker.io/library/nginx@sha256:deadbeef",
                "ready": true,
                "restartCount": 2,
                "state": {"running": {"startedAt": "2024-01-01T10:05:00Z"}},
                "lastState": {"terminated": {
                    "exitCode": 137, "reason": "OOMKilled",
                    "startedAt": "2024-01-01T10:00:05Z", "finishedAt": "2024-01-01T10:04:59Z"
                }}
            }]
        }
    })
}

fn pod_events() -> Vec<Value> {
    vec![
        json!({
            "metadata": {"name": "e2", "namespace": "default"},
            "type": "Normal", "reason": "Pulled",
            "source": {"component": "kubelet", "host": "node-1"},
            "firstTimestamp": "2024-01-01T10:00:02Z", "lastTimestamp": "2024-01-01T10:05:00Z",
            "count": 3,
            "message": "Container image \"nginx:1.25\" already present on machine"
        }),
        json!({
            "metadata": {"name": "e1", "namespace": "default"},
            "type": "Normal", "reason": "Scheduled",
            "reportingComponent": "default-scheduler",
            "eventTime": "2024-01-01T10:00:00.000000Z",
            "firstTimestamp": null, "lastTimestamp": null,
            "message": "Successfully assigned default/nginx to node-1"
        }),
        json!({
            "metadata": {"name": "e3", "namespace": "default"},
            "involvedObject": {"kind": "Pod", "name": "nginx", "fieldPath": "spec.containers{nginx}"},
            "type": "Warning", "reason": "BackOff",
            "source": {"component": "kubelet"},
            "firstTimestamp": "2024-01-01T10:04:00Z", "lastTimestamp": "2024-01-01T10:04:30Z",
            "count": 1,
            "message": "Back-off restarting failed container\n"
        }),
    ]
}

#[test]
fn pod_full() {
    let got = desc("v1/pods", pod_fixture(), pod_events());
    let want = lines(&[
        "Name:             nginx-7c5ddbdf54-abcde",
        "Namespace:        default",
        "Priority:         0",
        "Service Account:  default",
        "Node:             node-1/10.0.0.5",
        "Start Time:       Mon, 01 Jan 2024 10:00:00 +0000",
        "Labels:           app=nginx",
        "                  pod-template-hash=7c5ddbdf54",
        "Annotations:      kubectl.kubernetes.io/restartedAt: 2024-01-01T09:59:00Z",
        "Status:           Running",
        "IP:               10.244.1.7",
        "IPs:",
        "  IP:           10.244.1.7",
        "Controlled By:  ReplicaSet/nginx-7c5ddbdf54",
        "Containers:",
        "  nginx:",
        "    Container ID:  containerd://abc123",
        "    Image:         nginx:1.25",
        "    Image ID:      docker.io/library/nginx@sha256:deadbeef",
        "    Port:          80/TCP (http)",
        "    Host Port:     0/TCP (http)",
        "    Args:",
        "      --foo",
        "      --bar",
        "    State:          Running",
        "      Started:      Mon, 01 Jan 2024 10:05:00 +0000",
        "    Last State:     Terminated",
        "      Reason:       OOMKilled",
        "      Exit Code:    137",
        "      Started:      Mon, 01 Jan 2024 10:00:05 +0000",
        "      Finished:     Mon, 01 Jan 2024 10:04:59 +0000",
        "    Ready:          True",
        "    Restart Count:  2",
        "    Limits:",
        "      cpu:     500m",
        "      memory:  128Mi",
        "    Requests:",
        "      cpu:      100m",
        "      memory:   64Mi",
        "    Liveness:   http-get http://:8080/healthz delay=3s timeout=1s period=10s #success=1 #failure=3",
        "    Readiness:  tcp-socket :http delay=0s timeout=1s period=5s #success=1 #failure=3",
        "    Environment Variables from:",
        "      app-config  ConfigMap                  Optional: false",
        "      app-secret  Secret with prefix 'SEC_'  Optional: true",
        "    Environment:",
        "      PLAIN:     hello",
        "      POD_NAME:  nginx-7c5ddbdf54-abcde (v1:metadata.name)",
        "      NODE:       (v1:spec.nodeName)",
        "      MEM:       128 (limits.memory)",
        "      CPU_REQ:   100 (requests.cpu)",
        "      PASSWORD:  <set to the key 'password' in secret 'db'>   Optional: false",
        "      MODE:      <set to the key 'mode' of config map 'cfg'>  Optional: true",
        "    Mounts:",
        "      /data from data (rw,path=\"nginx\")",
        "      /var/run/secrets/kubernetes.io/serviceaccount from kube-api-access-xyz (ro)",
        "Conditions:",
        "  Type                        Status",
        "  PodReadyToStartContainers   True ",
        "  Initialized                 True ",
        "  Ready                       True ",
        "  ContainersReady             True ",
        "  PodScheduled                True ",
        "Volumes:",
        "  data:",
        "    Type:       PersistentVolumeClaim (a reference to a PersistentVolumeClaim in the same namespace)",
        "    ClaimName:  data-pvc",
        "    ReadOnly:   false",
        "  cache:",
        "    Type:       EmptyDir (a temporary directory that shares a pod's lifetime)",
        "    Medium:     ",
        "    SizeLimit:  1Gi",
        "  cfg:",
        "    Type:      ConfigMap (a volume populated by a ConfigMap)",
        "    Name:      nginx-conf",
        "    Optional:  false",
        "  kube-api-access-xyz:",
        "    Type:                    Projected (a volume that contains injected data from multiple sources)",
        "    TokenExpirationSeconds:  3607",
        "    ConfigMapName:           kube-root-ca.crt",
        "    Optional:                false",
        "    DownwardAPI:             true",
        "QoS Class:                   Burstable",
        "Node-Selectors:              kubernetes.io/os=linux",
        "Tolerations:                 node.kubernetes.io/not-ready:NoExecute op=Exists for 300s",
        "                             node.kubernetes.io/unreachable:NoExecute op=Exists for 300s",
        "Events:",
        "  Type     Reason     Age                  From               Message",
        "  ----     ------     ----                 ----               -------",
        "  Normal   Scheduled  120m                 default-scheduler  Successfully assigned default/nginx to node-1",
        // kubectl v0.35 prefixes involvedObject.fieldPath.
        "  Warning  BackOff    116m                 kubelet            spec.containers{nginx}: Back-off restarting failed container",
        "  Normal   Pulled     115m (x3 over 119m)  kubelet            Container image \"nginx:1.25\" already present on machine",
    ]);
    assert_eq!(got, want);
}

/// `a` left-aligned in a column of width `w`, followed by `b`.
fn p(w: usize, a: &str, b: &str) -> String {
    format!("{a:<w$}{b}")
}

fn s(ls: &[String]) -> String {
    let mut out = ls.join("\n");
    out.push('\n');
    out
}

fn pod_template_tail(lbl: &str, container: &str, image: &str, w: usize) -> Vec<String> {
    vec![
        p(11, "  Labels:", lbl),
        "  Containers:".into(),
        format!("   {container}:"),
        p(w, "    Image:", image),
        p(w, "    Port:", "<none>"),
        p(w, "    Host Port:", "<none>"),
        p(w, "    Environment:", "<none>"),
        p(w, "    Mounts:", "<none>"),
        p(w, "  Volumes:", "<none>"),
        p(w, "  Node-Selectors:", "<none>"),
        p(w, "  Tolerations:", "<none>"),
    ]
}

#[test]
fn pod_terminating_minimal() {
    let pod = json!({
        "metadata": {"name": "p", "namespace": "d", "deletionTimestamp": "2024-01-01T11:59:00Z", "deletionGracePeriodSeconds": 30},
        "spec": {
            "initContainers": [{"name": "init", "image": "busybox"}],
            "containers": [{"name": "app", "image": "app"}]
        },
        "status": {
            "phase": "Pending",
            "initContainerStatuses": [{"name": "init", "ready": false, "restartCount": 0, "state": {"waiting": {"reason": "PodInitializing"}}}],
            "containerStatuses": [{"name": "app", "ready": false, "restartCount": 0, "state": {"waiting": {"reason": "PodInitializing"}}}]
        }
    });
    let got = desc("v1/pods", pod, vec![]);
    let c = |name: &str, image: &str| -> Vec<String> {
        vec![
            format!("  {name}:"),
            p(20, "    Container ID:", ""),
            p(20, "    Image:", image),
            p(20, "    Image ID:", ""),
            p(20, "    Port:", "<none>"),
            p(20, "    Host Port:", "<none>"),
            p(20, "    State:", "Waiting"),
            p(20, "      Reason:", "PodInitializing"),
            p(20, "    Ready:", "False"),
            p(20, "    Restart Count:", "0"),
            p(20, "    Environment:", "<none>"),
            p(20, "    Mounts:", "<none>"),
        ]
    };
    let mut want = vec![
        p(27, "Name:", "p"),
        p(27, "Namespace:", "d"),
        p(27, "Node:", "<none>"),
        p(27, "Labels:", "<none>"),
        p(27, "Annotations:", "<none>"),
        p(27, "Status:", "Terminating (lasts 60s)"),
        p(27, "Termination Grace Period:", "30s"),
        p(27, "IP:", ""),
        p(27, "IPs:", "<none>"),
        "Init Containers:".into(),
    ];
    want.extend(c("init", "busybox"));
    want.push("Containers:".into());
    want.extend(c("app", "app"));
    want.extend([
        p(20, "Volumes:", "<none>"),
        p(20, "QoS Class:", "BestEffort"),
        p(20, "Node-Selectors:", "<none>"),
        p(20, "Tolerations:", "<none>"),
        p(20, "Events:", "<none>"),
    ]);
    assert_eq!(got, s(&want));
}

fn deployment_fixture() -> Value {
    json!({
        "apiVersion": "apps/v1", "kind": "Deployment",
        "metadata": {"name": "web", "namespace": "prod", "uid": "dep-uid", "creationTimestamp": "2024-01-01T08:00:00Z",
            "labels": {"app": "web"}, "annotations": {"deployment.kubernetes.io/revision": "2"}},
        "spec": {
            "replicas": 3,
            "selector": {"matchLabels": {"app": "web"}},
            "strategy": {"type": "RollingUpdate", "rollingUpdate": {"maxUnavailable": "25%", "maxSurge": "25%"}},
            "minReadySeconds": 0,
            "template": {"metadata": {"labels": {"app": "web"}},
                "spec": {"containers": [{"name": "web", "image": "web:2", "ports": [{"containerPort": 8080, "protocol": "TCP"}], "resources": {}}]}}
        },
        "status": {"replicas": 3, "updatedReplicas": 3, "availableReplicas": 3, "readyReplicas": 3,
            "conditions": [
                {"type": "Available", "status": "True", "reason": "MinimumReplicasAvailable"},
                {"type": "Progressing", "status": "True", "reason": "NewReplicaSetAvailable"}
            ]}
    })
}

fn rs(name: &str, uid: &str, owner: &str, created: &str, image: &str, replicas: i64) -> Value {
    json!({
        "metadata": {"name": name, "namespace": "prod", "uid": uid, "creationTimestamp": created,
            "labels": {"app": "web", "pod-template-hash": name},
            "ownerReferences": [{"kind": "Deployment", "name": "web", "uid": owner, "controller": true}]},
        "spec": {"replicas": replicas, "selector": {"matchLabels": {"app": "web", "pod-template-hash": name}},
            "template": {"metadata": {"labels": {"app": "web", "pod-template-hash": name}},
                "spec": {"containers": [{"name": "web", "image": image, "ports": [{"containerPort": 8080, "protocol": "TCP"}], "resources": {}}]}}},
        "status": {"replicas": replicas}
    })
}

#[test]
fn deployment_with_replica_sets() {
    let rel = Rel {
        replica_sets: Some(vec![
            rs(
                "web-new",
                "rs-new",
                "dep-uid",
                "2024-01-01T09:00:00Z",
                "web:2",
                3,
            ),
            rs(
                "web-old",
                "rs-old",
                "dep-uid",
                "2024-01-01T08:00:00Z",
                "web:1",
                0,
            ),
            rs(
                "web-other",
                "rs-x",
                "other-uid",
                "2024-01-01T08:00:00Z",
                "web:2",
                1,
            ),
        ]),
        ..Default::default()
    };
    let got = desc_rel("apps/v1/deployments", deployment_fixture(), vec![], rel);
    let want = s(&[
        p(24, "Name:", "web"),
        p(24, "Namespace:", "prod"),
        p(24, "CreationTimestamp:", "Mon, 01 Jan 2024 08:00:00 +0000"),
        p(24, "Labels:", "app=web"),
        p(24, "Annotations:", "deployment.kubernetes.io/revision: 2"),
        p(24, "Selector:", "app=web"),
        p(
            24,
            "Replicas:",
            "3 desired | 3 updated | 3 total | 3 available | 0 unavailable",
        ),
        p(24, "StrategyType:", "RollingUpdate"),
        p(24, "MinReadySeconds:", "0"),
        p(
            24,
            "RollingUpdateStrategy:",
            "25% max unavailable, 25% max surge",
        ),
        "Pod Template:".into(),
        p(11, "  Labels:", "app=web"),
        "  Containers:".into(),
        "   web:".into(),
        p(19, "    Image:", "web:2"),
        p(19, "    Port:", "8080/TCP"),
        p(19, "    Host Port:", "0/TCP"),
        p(19, "    Environment:", "<none>"),
        p(19, "    Mounts:", "<none>"),
        p(19, "  Volumes:", "<none>"),
        p(19, "  Node-Selectors:", "<none>"),
        p(19, "  Tolerations:", "<none>"),
        "Conditions:".into(),
        p(17, "  Type", &p(8, "Status", "Reason")),
        p(17, "  ----", &p(8, "------", "------")),
        p(
            17,
            "  Available ",
            &p(8, "True", "MinimumReplicasAvailable"),
        ),
        p(
            17,
            "  Progressing ",
            &p(8, "True", "NewReplicaSetAvailable"),
        ),
        p(17, "OldReplicaSets:", "web-old (0/0 replicas created)"),
        p(17, "NewReplicaSet:", "web-new (3/3 replicas created)"),
        p(17, "Events:", "<none>"),
    ]);
    assert_eq!(got, want);
}

#[test]
fn deployment_without_replica_sets_omits_rs_lines() {
    let got = desc("apps/v1/deployments", deployment_fixture(), vec![]);
    assert!(!got.contains("OldReplicaSets"));
    assert!(got.ends_with(&format!("{}\n", p(16, "Events:", "<none>"))));
}

fn pod_owned(name: &str, ns: &str, owner: &str, phase: &str) -> Value {
    json!({
        "metadata": {"name": name, "namespace": ns, "uid": name,
            "labels": {"app": "web", "pod-template-hash": "abc"},
            "ownerReferences": [{"kind": "ReplicaSet", "name": "web-abc", "uid": owner, "controller": true}]},
        "status": {"phase": phase}
    })
}

fn replica_set_fixture() -> Value {
    json!({
        "metadata": {"name": "web-abc", "namespace": "prod", "uid": "rs-uid",
            "labels": {"app": "web", "pod-template-hash": "abc"},
            "ownerReferences": [{"kind": "Deployment", "name": "web", "uid": "dep-uid", "controller": true}]},
        "spec": {"replicas": 3, "selector": {"matchLabels": {"app": "web", "pod-template-hash": "abc"}},
            "template": {"metadata": {"labels": {"app": "web", "pod-template-hash": "abc"}},
                "spec": {"containers": [{"name": "web", "image": "web:2"}]}}},
        "status": {"replicas": 3, "readyReplicas": 2}
    })
}

#[test]
fn replica_set_counts_pods() {
    let rel = Rel {
        pods: Some(vec![
            pod_owned("a", "prod", "rs-uid", "Running"),
            pod_owned("b", "prod", "rs-uid", "Running"),
            pod_owned("c", "prod", "rs-uid", "Pending"),
            pod_owned("d", "prod", "other", "Running"),
            pod_owned("e", "dev", "rs-uid", "Failed"),
        ]),
        ..Default::default()
    };
    let got = desc_rel("apps/v1/replicasets", replica_set_fixture(), vec![], rel);
    let mut want = vec![
        p(16, "Name:", "web-abc"),
        p(16, "Namespace:", "prod"),
        p(16, "Selector:", "app=web,pod-template-hash=abc"),
        p(16, "Labels:", "app=web"),
        p(16, "", "pod-template-hash=abc"),
        p(16, "Annotations:", "<none>"),
        p(16, "Controlled By:", "Deployment/web"),
        p(16, "Replicas:", "3 current / 3 desired"),
        p(
            16,
            "Pods Status:",
            "2 Running / 1 Waiting / 0 Succeeded / 0 Failed",
        ),
        "Pod Template:".into(),
        p(11, "  Labels:", "app=web"),
        p(11, "", "pod-template-hash=abc"),
    ];
    let mut tail = pod_template_tail("", "web", "web:2", 19);
    tail.remove(0);
    want.extend(tail);
    want.push(p(19, "Events:", "<none>"));
    assert_eq!(got, s(&want));
}

#[test]
fn replica_set_without_pods_uses_status() {
    let got = desc("apps/v1/replicasets", replica_set_fixture(), vec![]);
    assert!(got.contains("Pods Status:    2 Running / 1 Waiting / 0 Succeeded / 0 Failed\n"));
}

#[test]
fn stateful_set_with_claim_templates() {
    let sts = json!({
        "metadata": {"name": "db", "namespace": "prod", "uid": "sts", "creationTimestamp": "2024-01-01T08:00:00Z"},
        "spec": {
            "replicas": 2,
            "selector": {"matchLabels": {"app": "db"}},
            "updateStrategy": {"type": "RollingUpdate", "rollingUpdate": {"partition": 0}},
            "template": {"metadata": {"labels": {"app": "db"}}, "spec": {"containers": [{"name": "db", "image": "postgres:16"}]}},
            "volumeClaimTemplates": [{"metadata": {"name": "data"}, "spec": {
                "accessModes": ["ReadWriteOnce"], "storageClassName": "gp3",
                "resources": {"requests": {"storage": "10Gi"}}}}]
        },
        "status": {"replicas": 2, "readyReplicas": 2}
    });
    let got = desc("apps/v1/statefulsets", sts, vec![]);
    let mut want = vec![
        p(20, "Name:", "db"),
        p(20, "Namespace:", "prod"),
        p(20, "CreationTimestamp:", "Mon, 01 Jan 2024 08:00:00 +0000"),
        p(20, "Selector:", "app=db"),
        p(20, "Labels:", "<none>"),
        p(20, "Annotations:", "<none>"),
        p(20, "Replicas:", "2 desired | 2 total"),
        p(20, "Update Strategy:", "RollingUpdate"),
        p(20, "  Partition:", "0"),
        p(
            20,
            "Pods Status:",
            "2 Running / 0 Waiting / 0 Succeeded / 0 Failed",
        ),
        "Pod Template:".into(),
    ];
    want.extend(pod_template_tail("app=db", "db", "postgres:16", 19));
    want.extend([
        "Volume Claims:".into(),
        p(17, "  Name:", "data"),
        p(17, "  StorageClass:", "gp3"),
        p(17, "  Labels:", "<none>"),
        p(17, "  Annotations:", "<none>"),
        p(17, "  Capacity:", "10Gi"),
        p(17, "  Access Modes:", "[ReadWriteOnce]"),
        p(17, "Events:", "<none>"),
    ]);
    assert_eq!(got, s(&want));
}

#[test]
fn daemon_set() {
    let ds = json!({
        "metadata": {"name": "fluentd", "namespace": "kube-system", "uid": "ds"},
        "spec": {"selector": {"matchLabels": {"name": "fluentd"}},
            "template": {"metadata": {"labels": {"name": "fluentd"}},
                "spec": {"nodeSelector": {"kubernetes.io/os": "linux"}, "containers": [{"name": "fluentd", "image": "fluentd:v1"}]}}},
        "status": {"desiredNumberScheduled": 3, "currentNumberScheduled": 3, "updatedNumberScheduled": 3,
            "numberAvailable": 3, "numberMisscheduled": 0, "numberReady": 3}
    });
    let got = desc("apps/v1/daemonsets", ds, vec![]);
    let mut want = vec![
        p(16, "Name:", "fluentd"),
        p(16, "Namespace:", "kube-system"),
        p(16, "Selector:", "name=fluentd"),
        p(16, "Node-Selector:", "kubernetes.io/os=linux"),
        p(16, "Labels:", "<none>"),
        p(16, "Annotations:", "<none>"),
        "Desired Number of Nodes Scheduled: 3".into(),
        "Current Number of Nodes Scheduled: 3".into(),
        "Number of Nodes Scheduled with Up-to-date Pods: 3".into(),
        "Number of Nodes Scheduled with Available Pods: 3".into(),
        "Number of Nodes Misscheduled: 0".into(),
        p(
            14,
            "Pods Status:",
            "3 Running / 0 Waiting / 0 Succeeded / 0 Failed",
        ),
        "Pod Template:".into(),
    ];
    let mut tail = pod_template_tail("name=fluentd", "fluentd", "fluentd:v1", 19);
    tail[9] = p(19, "  Node-Selectors:", "kubernetes.io/os=linux");
    want.extend(tail);
    want.push(p(19, "Events:", "<none>"));
    assert_eq!(got, s(&want));
}

#[test]
fn job_completed() {
    let job = json!({
        "metadata": {"name": "pi", "namespace": "default", "uid": "job-uid", "labels": {"batch.kubernetes.io/job-name": "pi"}},
        "spec": {"parallelism": 1, "completions": 1, "completionMode": "NonIndexed", "suspend": false, "backoffLimit": 6,
            "selector": {"matchLabels": {"batch.kubernetes.io/controller-uid": "job-uid"}},
            "template": {"metadata": {"labels": {"batch.kubernetes.io/job-name": "pi"}},
                "spec": {"restartPolicy": "Never", "containers": [{"name": "pi", "image": "perl",
                    "command": ["perl", "-Mbignum=bpi", "-wle", "print bpi(2000)"]}]}}},
        "status": {"startTime": "2024-01-01T10:00:00Z", "completionTime": "2024-01-01T10:00:42Z", "succeeded": 1, "ready": 0}
    });
    let got = desc("batch/v1/jobs", job, vec![]);
    let want = s(&[
        p(18, "Name:", "pi"),
        p(18, "Namespace:", "default"),
        p(
            18,
            "Selector:",
            "batch.kubernetes.io/controller-uid=job-uid",
        ),
        p(18, "Labels:", "batch.kubernetes.io/job-name=pi"),
        p(18, "Annotations:", "<none>"),
        p(18, "Parallelism:", "1"),
        p(18, "Completions:", "1"),
        p(18, "Completion Mode:", "NonIndexed"),
        p(18, "Suspend:", "false"),
        p(18, "Backoff Limit:", "6"),
        p(18, "Start Time:", "Mon, 01 Jan 2024 10:00:00 +0000"),
        p(18, "Completed At:", "Mon, 01 Jan 2024 10:00:42 +0000"),
        p(18, "Duration:", "42s"),
        p(
            18,
            "Pods Statuses:",
            "0 Active (0 Ready) / 1 Succeeded / 0 Failed",
        ),
        "Pod Template:".into(),
        p(11, "  Labels:", "batch.kubernetes.io/job-name=pi"),
        "  Containers:".into(),
        "   pi:".into(),
        p(16, "    Image:", "perl"),
        p(16, "    Port:", "<none>"),
        p(16, "    Host Port:", "<none>"),
        "    Command:".into(),
        "      perl".into(),
        "      -Mbignum=bpi".into(),
        "      -wle".into(),
        "      print bpi(2000)".into(),
        p(19, "    Environment:", "<none>"),
        p(19, "    Mounts:", "<none>"),
        p(19, "  Volumes:", "<none>"),
        p(19, "  Node-Selectors:", "<none>"),
        p(19, "  Tolerations:", "<none>"),
        p(19, "Events:", "<none>"),
    ]);
    assert_eq!(got, want);
}

#[test]
fn cron_job() {
    let cj = json!({
        "metadata": {"name": "hello", "namespace": "default"},
        "spec": {"schedule": "*/5 * * * *", "concurrencyPolicy": "Allow", "suspend": false,
            "successfulJobsHistoryLimit": 3, "failedJobsHistoryLimit": 1,
            "jobTemplate": {"spec": {"template": {"spec": {"containers": [{"name": "hello", "image": "busybox"}]}}}}},
        "status": {"lastScheduleTime": "2024-01-01T11:55:00Z", "active": [{"kind": "Job", "name": "hello-123"}]}
    });
    let got = desc("batch/v1/cronjobs", cj, vec![]);
    let mut want = vec![
        p(31, "Name:", "hello"),
        p(31, "Namespace:", "default"),
        p(31, "Labels:", "<none>"),
        p(31, "Annotations:", "<none>"),
        p(31, "Schedule:", "*/5 * * * *"),
        p(31, "Concurrency Policy:", "Allow"),
        p(31, "Suspend:", "False"),
        p(31, "Successful Job History Limit:", "3"),
        p(31, "Failed Job History Limit:", "1"),
        p(31, "Starting Deadline Seconds:", "<unset>"),
        p(31, "Selector:", "<unset>"),
        p(31, "Parallelism:", "<unset>"),
        p(31, "Completions:", "<unset>"),
        "Pod Template:".into(),
    ];
    want.extend(pod_template_tail("<none>", "hello", "busybox", 21));
    want.extend([
        p(21, "Last Schedule Time:", "Mon, 01 Jan 2024 11:55:00 +0000"),
        p(21, "Active Jobs:", "hello-123"),
        p(21, "Events:", "<none>"),
    ]);
    assert_eq!(got, s(&want));
}

#[test]
fn replication_controller_falls_back_to_status() {
    let rc = json!({
        "metadata": {"name": "rc1", "namespace": "default", "uid": "rc"},
        "spec": {"replicas": 2, "selector": {"app": "rc"},
            "template": {"metadata": {"labels": {"app": "rc"}}, "spec": {"containers": [{"name": "nginx", "image": "nginx"}]}}},
        "status": {"replicas": 2, "readyReplicas": 2}
    });
    let got = desc("v1/replicationcontrollers", rc, vec![]);
    let mut want = vec![
        p(14, "Name:", "rc1"),
        p(14, "Namespace:", "default"),
        p(14, "Selector:", "app=rc"),
        p(14, "Labels:", "<none>"),
        p(14, "Annotations:", "<none>"),
        p(14, "Replicas:", "2 current / 2 desired"),
        p(
            14,
            "Pods Status:",
            "2 Running / 0 Waiting / 0 Succeeded / 0 Failed",
        ),
        "Pod Template:".into(),
    ];
    want.extend(pod_template_tail("app=rc", "nginx", "nginx", 19));
    want.push(p(19, "Events:", "<none>"));
    assert_eq!(got, s(&want));
}

/// kubectl describe_test.go TestDescribeService "test1".
#[test]
fn service_kubectl_golden() {
    let svc = json!({
        "metadata": {"name": "bar", "namespace": "foo"},
        "spec": {
            "type": "LoadBalancer",
            "ports": [{"name": "port-tcp", "port": 8080, "protocol": "TCP", "targetPort": 9527, "nodePort": 31111}],
            "selector": {"blah": "heh"},
            "clusterIP": "1.2.3.4",
            "ipFamilies": ["IPv4"],
            "loadBalancerIP": "5.6.7.8",
            "sessionAffinity": "None",
            "externalTrafficPolicy": "Local",
            "internalTrafficPolicy": "Cluster",
            "healthCheckNodePort": 32222
        },
        "status": {"loadBalancer": {"ingress": [{"ip": "5.6.7.8", "ipMode": "VIP"}]}}
    });
    let slice = json!({
        "metadata": {"name": "bar-abcde", "namespace": "foo", "labels": {"kubernetes.io/service-name": "bar"}},
        "endpoints": [
            {"addresses": ["10.244.0.1"], "conditions": {"ready": true}},
            {"addresses": ["10.244.0.2"], "conditions": {"ready": true}},
            {"addresses": ["10.244.0.3"], "conditions": {"ready": true}}
        ],
        "ports": [{"name": "port-tcp", "port": 9527, "protocol": "TCP"}]
    });
    let rel = Rel {
        endpoint_slices: Some(vec![slice]),
        ..Default::default()
    };
    let got = desc_rel("v1/services", svc, vec![], rel);
    let want = "Name:                     bar
Namespace:                foo
Labels:                   <none>
Annotations:              <none>
Selector:                 blah=heh
Type:                     LoadBalancer
IP Families:              IPv4
IP:                       1.2.3.4
IPs:                      <none>
Desired LoadBalancer IP:  5.6.7.8
LoadBalancer Ingress:     5.6.7.8 (VIP)
Port:                     port-tcp  8080/TCP
TargetPort:               9527/TCP
NodePort:                 port-tcp  31111/TCP
Endpoints:                10.244.0.1:9527,10.244.0.2:9527,10.244.0.3:9527
Session Affinity:         None
External Traffic Policy:  Local
Internal Traffic Policy:  Cluster
HealthCheck NodePort:     32222
Events:                   <none>
";
    assert_eq!(got, want);
}

#[test]
fn service_endpoints_cap_and_missing_slices() {
    let svc = json!({
        "metadata": {"name": "web", "namespace": "d"},
        "spec": {"type": "ClusterIP", "clusterIP": "10.0.0.1", "clusterIPs": ["10.0.0.1"],
            "ports": [{"port": 80, "protocol": "TCP", "targetPort": "http"}], "sessionAffinity": "None"}
    });
    let got = desc("v1/services", svc.clone(), vec![]);
    assert!(got.contains("Port:              <unset>  80/TCP\n"));
    assert!(got.contains("TargetPort:        http/TCP\n"));
    assert!(got.contains("Endpoints:         <none>\n"));
    let eps: Vec<Value> = (1..=5)
        .map(|i| json!({"addresses": [format!("10.1.0.{i}")], "conditions": {"ready": i != 2}}))
        .collect();
    let slice = json!({"metadata": {"name": "web-1", "namespace": "d", "labels": {"kubernetes.io/service-name": "web"}},
        "endpoints": eps, "ports": [{"name": "", "port": 8080, "protocol": "TCP"}]});
    let rel = Rel {
        endpoint_slices: Some(vec![slice]),
        ..Default::default()
    };
    let got = desc_rel("v1/services", svc, vec![], rel);
    assert!(
        got.contains("Endpoints:         10.1.0.1:8080,10.1.0.3:8080,10.1.0.4:8080 + 1 more...\n")
    );
}

#[test]
fn endpoints() {
    let ep = json!({
        "metadata": {"name": "svc", "namespace": "default"},
        "subsets": [{"addresses": [{"ip": "10.0.0.1"}, {"ip": "10.0.0.2"}],
            "ports": [{"name": "http", "port": 8080, "protocol": "TCP"}, {"port": 9090, "protocol": "TCP"}]}]
    });
    let got = desc("v1/endpoints", ep, vec![]);
    let want = s(&[
        p(14, "Name:", "svc"),
        p(14, "Namespace:", "default"),
        p(14, "Labels:", "<none>"),
        p(14, "Annotations:", "<none>"),
        "Subsets:".into(),
        p(22, "  Addresses:", "10.0.0.1,10.0.0.2"),
        p(22, "  NotReadyAddresses:", "<none>"),
        "  Ports:".into(),
        p(13, "    Name", &p(6, "Port", "Protocol")),
        p(13, "    ----", &p(6, "----", "--------")),
        p(13, "    http", &p(6, "8080", "TCP")),
        p(13, "    <unset>", &p(6, "9090", "TCP")),
        String::new(),
        p(9, "Events:", "<none>"),
    ]);
    assert_eq!(got, want);
}

/// kubectl describe_test.go TestDescribeEndpointSlice "EndpointSlices v1".
#[test]
fn endpoint_slice_kubectl_golden() {
    let eps = json!({
        "metadata": {"name": "foo.123", "namespace": "bar"},
        "addressType": "IPv4",
        "endpoints": [
            {"addresses": ["1.2.3.4", "1.2.3.5"], "conditions": {"ready": true},
             "targetRef": {"kind": "Pod", "name": "test-123"}, "zone": "us-central1-a", "nodeName": "node-1"},
            {"addresses": ["1.2.3.6", "1.2.3.7"], "conditions": {"ready": true},
             "targetRef": {"kind": "Pod", "name": "test-124"}, "nodeName": "node-2"}
        ],
        "ports": [{"protocol": "TCP", "port": 80}]
    });
    let got = desc("discovery.k8s.io/v1/endpointslices", eps, vec![]);
    let want = "Name:         foo.123
Namespace:    bar
Labels:       <none>
Annotations:  <none>
AddressType:  IPv4
Ports:
  Name     Port  Protocol
  ----     ----  --------
  <unset>  80    TCP
Endpoints:
  - Addresses:  1.2.3.4, 1.2.3.5
    Conditions:
      Ready:    true
    Hostname:   <unset>
    TargetRef:  Pod/test-123
    NodeName:   node-1
    Zone:       us-central1-a
  - Addresses:  1.2.3.6, 1.2.3.7
    Conditions:
      Ready:    true
    Hostname:   <unset>
    TargetRef:  Pod/test-124
    NodeName:   node-2
    Zone:       <unset>
Events:         <none>
";
    assert_eq!(got, want);
}

#[test]
fn ingress_with_backends() {
    let ing = json!({
        "metadata": {"name": "web", "namespace": "default",
            "annotations": {"nginx.ingress.kubernetes.io/rewrite-target": "/"}},
        "spec": {
            "ingressClassName": "nginx",
            "tls": [{"hosts": ["a.example.com"], "secretName": "tls-a"}],
            "rules": [{"host": "a.example.com", "http": {"paths": [
                {"path": "/", "pathType": "Prefix", "backend": {"service": {"name": "web", "port": {"number": 80}}}},
                {"path": "/api", "pathType": "Prefix", "backend": {"service": {"name": "api", "port": {"name": "http"}}}}
            ]}}]
        },
        "status": {"loadBalancer": {"ingress": [{"hostname": "lb.example.com"}, {"ip": "1.2.3.4"}]}}
    });
    let rel = Rel {
        services: Some(vec![
            json!({"metadata": {"name": "web", "namespace": "default"},
            "spec": {"ports": [{"port": 80, "protocol": "TCP"}]}}),
        ]),
        endpoint_slices: Some(vec![json!({
            "metadata": {"name": "web-x", "namespace": "default", "labels": {"kubernetes.io/service-name": "web"}},
            "endpoints": [{"addresses": ["10.1.0.1"], "conditions": {"ready": true}}, {"addresses": ["10.1.0.2"]}],
            "ports": [{"name": "", "port": 8080, "protocol": "TCP"}]
        })]),
        ..Default::default()
    };
    let got = desc_rel("networking.k8s.io/v1/ingresses", ing, vec![], rel);
    let want = s(&[
        p(18, "Name:", "web"),
        p(18, "Labels:", "<none>"),
        p(18, "Namespace:", "default"),
        p(18, "Address:", "1.2.3.4,lb.example.com"),
        p(18, "Ingress Class:", "nginx"),
        p(18, "Default backend:", "<default>"),
        "TLS:".into(),
        "  tls-a terminates a.example.com".into(),
        "Rules:".into(),
        p(17, "  Host", &p(6, "Path", "Backends")),
        p(17, "  ----", &p(6, "----", "--------")),
        p(17, "  a.example.com", ""),
        p(
            17,
            "    ",
            &p(7, "/ ", "web:80 (10.1.0.1:8080,10.1.0.2:8080)"),
        ),
        p(
            17,
            "    ",
            &p(7, "/api ", "api:http (<error: services \"api\" not found>)"),
        ),
        p(
            17,
            "Annotations:",
            "nginx.ingress.kubernetes.io/rewrite-target: /",
        ),
        p(17, "Events:", "<none>"),
    ]);
    assert_eq!(got, want);
}

#[test]
fn ingress_without_rules_uses_default_backend() {
    let ing = json!({"metadata": {"name": "x", "namespace": "d"},
        "spec": {"defaultBackend": {"resource": {"apiGroup": "k8s.example.com", "kind": "StorageBucket", "name": "static"}}}});
    let got = desc("networking.k8s.io/v1/ingresses", ing, vec![]);
    assert!(got.contains(
        "Default backend:  APIGroup: k8s.example.com, Kind: StorageBucket, Name: static\n"
    ));
    assert!(got.contains(
        "  *           *     APIGroup: k8s.example.com, Kind: StorageBucket, Name: static\n"
    ));
}

/// kubectl describe_test.go TestDescribeIngressClass.
#[test]
fn ingress_class_kubectl_golden() {
    let ic = json!({"metadata": {"name": "example-class"},
        "spec": {"controller": "example.com/controller",
            "parameters": {"apiGroup": "v1", "kind": "ConfigMap", "name": "example-parameters"}}});
    let got = desc("networking.k8s.io/v1/ingressclasses", ic, vec![]);
    let want = "Name:         example-class
Labels:       <none>
Annotations:  <none>
Controller:   example.com/controller
Parameters:
  APIGroup:  v1
  Kind:      ConfigMap
  Name:      example-parameters
Events:      <none>
";
    assert_eq!(got, want);
}

#[test]
fn service_cidr_and_ip_address() {
    let sc = json!({"metadata": {"name": "kubernetes"}, "spec": {"cidrs": ["10.96.0.0/12", "fd00::/108"]},
        "status": {"conditions": [{"type": "Ready", "status": "True", "lastTransitionTime": "2024-01-01T00:00:00Z", "reason": "", "message": "Kubernetes Service CIDR is ready"}]}});
    let got = desc("networking.k8s.io/v1/servicecidrs", sc, vec![]);
    let want = s(&[
        p(14, "Name:", "kubernetes"),
        p(14, "Labels:", "<none>"),
        p(14, "Annotations:", "<none>"),
        p(14, "CIDRs:", "10.96.0.0/12, fd00::/108"),
        "Status:".into(),
        "Conditions:".into(),
        format!(
            "{}{}{}{}{}",
            p(9, "  Type", ""),
            p(8, "Status", ""),
            p(33, "LastTransitionTime", ""),
            p(8, "Reason", ""),
            "Message"
        ),
        format!(
            "{}{}{}{}{}",
            p(9, "  ----", ""),
            p(8, "------", ""),
            p(33, "------------------", ""),
            p(8, "------", ""),
            "-------"
        ),
        format!(
            "{}{}{}{}{}",
            p(9, "  Ready", ""),
            p(8, "True", ""),
            p(33, "Mon, 01 Jan 2024 00:00:00 +0000", ""),
            p(8, "", ""),
            "Kubernetes Service CIDR is ready"
        ),
        p(9, "Events:", "<none>"),
    ]);
    assert_eq!(got, want);
    let ip = json!({"metadata": {"name": "10.96.0.1"},
        "spec": {"parentRef": {"group": "", "resource": "services", "namespace": "default", "name": "kubernetes"}}});
    let got = desc("networking.k8s.io/v1/ipaddresses", ip, vec![]);
    let want = s(&[
        p(14, "Name:", "10.96.0.1"),
        p(14, "Labels:", "<none>"),
        p(14, "Annotations:", "<none>"),
        "Parent Reference:".into(),
        p(14, "  Group:", ""),
        p(14, "  Resource:", "services"),
        p(14, "  Namespace:", "default"),
        p(14, "  Name:", "kubernetes"),
        p(14, "Events:", "<none>"),
    ]);
    assert_eq!(got, want);
}

/// kubectl describe_test.go TestDescribeNetworkPolicies (time shown in UTC).
#[test]
fn network_policy_kubectl_golden() {
    let sel = |a: &str, b: &str| json!({"matchLabels": {"id": a, "id2": b}});
    let peers = json!([
        {"podSelector": sel("pod1", "pod2"), "namespaceSelector": sel("ns1", "ns2")},
        {"podSelector": sel("app2", "app3")},
        {"namespaceSelector": sel("app2", "app3")},
        {"namespaceSelector": {"matchLabels": {"id": "app2", "id2": "app3"},
            "matchExpressions": [{"key": "foo", "operator": "In", "values": ["bar1", "bar2"]}]}},
        {"ipBlock": {"cidr": "192.168.0.0/16", "except": ["192.168.3.0/24", "192.168.4.0/24"]}}
    ]);
    let ports = json!([{"port": 80}, {"port": 82, "protocol": "TCP"}]);
    let np = json!({
        "metadata": {"name": "network-policy-1", "namespace": "default", "creationTimestamp": "2017-06-05T04:45:56Z"},
        "spec": {
            "podSelector": {"matchLabels": {"id1": "app1", "id2": "app2"},
                "matchExpressions": [
                    {"key": "foo", "operator": "In", "values": ["bar1", "bar2"]},
                    {"key": "foo2", "operator": "NotIn", "values": ["bar1", "bar2"]}
                ]},
            "ingress": [{"ports": ports, "from": peers}, {}],
            "egress": [{"ports": ports, "to": peers}, {}],
            "policyTypes": ["Ingress", "Egress"]
        }
    });
    let got = desc("networking.k8s.io/v1/networkpolicies", np, vec![]);
    let want = "Name:         network-policy-1
Namespace:    default
Created on:   2017-06-05 04:45:56 +0000 UTC
Labels:       <none>
Annotations:  <none>
Spec:
  PodSelector:     foo in (bar1,bar2),foo2 notin (bar1,bar2),id1=app1,id2=app2
  Allowing ingress traffic:
    To Port: 80/TCP
    To Port: 82/TCP
    From:
      NamespaceSelector: id=ns1,id2=ns2
      PodSelector: id=pod1,id2=pod2
    From:
      PodSelector: id=app2,id2=app3
    From:
      NamespaceSelector: id=app2,id2=app3
    From:
      NamespaceSelector: foo in (bar1,bar2),id=app2,id2=app3
    From:
      IPBlock:
        CIDR: 192.168.0.0/16
        Except: 192.168.3.0/24, 192.168.4.0/24
    ----------
    To Port: <any> (traffic allowed to all ports)
    From: <any> (traffic not restricted by source)
  Allowing egress traffic:
    To Port: 80/TCP
    To Port: 82/TCP
    To:
      NamespaceSelector: id=ns1,id2=ns2
      PodSelector: id=pod1,id2=pod2
    To:
      PodSelector: id=app2,id2=app3
    To:
      NamespaceSelector: id=app2,id2=app3
    To:
      NamespaceSelector: foo in (bar1,bar2),id=app2,id2=app3
    To:
      IPBlock:
        CIDR: 192.168.0.0/16
        Except: 192.168.3.0/24, 192.168.4.0/24
    ----------
    To Port: <any> (traffic allowed to all ports)
    To: <any> (traffic not restricted by destination)
  Policy Types: Ingress, Egress
";
    assert_eq!(got, want);
}

#[test]
fn network_policy_isolated_and_port_range() {
    let np = json!({"metadata": {"name": "deny", "namespace": "d", "creationTimestamp": "2017-06-05T04:45:56Z"},
        "spec": {"podSelector": {}, "policyTypes": ["Egress"],
            "egress": [{"ports": [{"port": 32000, "endPort": 32768, "protocol": "UDP"}]}]}});
    let got = desc("networking.k8s.io/v1/networkpolicies", np, vec![]);
    assert!(got.contains(
        "  PodSelector:     <none> (Allowing the specific traffic to all pods in this namespace)\n  Not affecting ingress traffic\n  Allowing egress traffic:\n    To Port Range: 32000-32768/UDP\n    To: <any> (traffic not restricted by destination)\n  Policy Types: Egress\n"
    ));
}

/// kubectl describe_test.go TestDescribeSecret (values never shown).
#[test]
fn secret_kubectl_golden() {
    let secret = json!({"metadata": {"name": "bar", "namespace": "foo"},
        "data": {"username": "YWRtaW4=", "password": "MWYyZDFlMmU2N2Rm"}});
    let got = desc("v1/secrets", secret, vec![]);
    let want = "Name:         bar
Namespace:    foo
Labels:       <none>
Annotations:  <none>

Type:  

Data
====
password:  12 bytes
username:  5 bytes
";
    assert_eq!(got, want);
    let token = json!({"metadata": {"name": "t", "namespace": "foo"}, "type": "kubernetes.io/service-account-token",
        "data": {"token": "c2VjcmV0LXRva2Vu"}});
    let got = desc("v1/secrets", token, vec![]);
    assert!(got.contains("token:  12 bytes\n"));
    assert!(!got.contains("secret-token"));
}

#[test]
fn config_map() {
    let cm = json!({"metadata": {"name": "app-config", "namespace": "default"},
        "data": {"mode": "prod", "app.properties": "a=1\nb=2"},
        "binaryData": {"blob": "AAECAw=="}});
    let got = desc("v1/configmaps", cm, vec![]);
    let want = "Name:         app-config
Namespace:    default
Labels:       <none>
Annotations:  <none>

Data
====
app.properties:
----
a=1
b=2

mode:
----
prod


BinaryData
====
blob: 4 bytes

Events:  <none>
";
    assert_eq!(got, want);
}

/// kubectl v0.35 describe_test.go TestDescribeServiceAccount (v0.35 dropped
/// the `Mountable secrets:`/`Tokens:` sections and `(not found)` markers).
#[test]
fn service_account_kubectl_golden() {
    let sa = json!({"metadata": {"name": "bar", "namespace": "foo"},
        "secrets": [{"name": "test-objectref"}], "imagePullSecrets": [{"name": "test-local-ref"}]});
    let got = desc_rel("v1/serviceaccounts", sa, vec![], Rel::default());
    let want = "Name:                bar
Namespace:           foo
Labels:              <none>
Annotations:         <none>
Image pull secrets:  test-local-ref
Events:              <none>
";
    assert_eq!(got, want);
}

#[test]
fn service_account_multiple_pull_secrets() {
    let sa = json!({"metadata": {"name": "bot", "namespace": "ns"},
        "imagePullSecrets": [{"name": "a"}, {"name": "b"}]});
    let got = desc_rel("v1/serviceaccounts", sa, vec![], Rel::default());
    assert!(
        got.contains("Image pull secrets:  a\n                     b\nEvents:"),
        "{got}"
    );
}

#[test]
fn persistent_volume_csi() {
    let pv = json!({
        "metadata": {"name": "pv1", "annotations": {"pv.kubernetes.io/provisioned-by": "ebs.csi.aws.com"},
            "finalizers": ["kubernetes.io/pv-protection"]},
        "spec": {
            "storageClassName": "gp3", "capacity": {"storage": "10Gi"}, "accessModes": ["ReadWriteOnce"],
            "persistentVolumeReclaimPolicy": "Delete", "volumeMode": "Filesystem",
            "claimRef": {"namespace": "default", "name": "data-pvc"},
            "nodeAffinity": {"required": {"nodeSelectorTerms": [{"matchExpressions": [
                {"key": "topology.ebs.csi.aws.com/zone", "operator": "In", "values": ["us-east-1a"]}]}]}},
            "csi": {"driver": "ebs.csi.aws.com", "fsType": "ext4", "volumeHandle": "vol-123",
                "volumeAttributes": {"storage.kubernetes.io/csiProvisionerIdentity": "123-ebs"}}
        },
        "status": {"phase": "Bound"}
    });
    let got = desc("v1/persistentvolumes", pv, vec![]);
    let want = s(&[
        p(19, "Name:", "pv1"),
        p(19, "Labels:", "<none>"),
        p(
            19,
            "Annotations:",
            "pv.kubernetes.io/provisioned-by: ebs.csi.aws.com",
        ),
        p(19, "Finalizers:", "[kubernetes.io/pv-protection]"),
        p(19, "StorageClass:", "gp3"),
        p(19, "Status:", "Bound"),
        p(19, "Claim:", "default/data-pvc"),
        p(19, "Reclaim Policy:", "Delete"),
        p(19, "Access Modes:", "RWO"),
        p(19, "VolumeMode:", "Filesystem"),
        p(19, "Capacity:", "10Gi"),
        p(19, "Node Affinity:", ""),
        p(19, "  Required Terms:", ""),
        p(
            19,
            "    Term 0:",
            "topology.ebs.csi.aws.com/zone in [us-east-1a]",
        ),
        p(19, "Message:", ""),
        "Source:".into(),
        p(
            23,
            "    Type:",
            "CSI (a Container Storage Interface (CSI) volume source)",
        ),
        p(23, "    Driver:", "ebs.csi.aws.com"),
        p(23, "    FSType:", "ext4"),
        p(23, "    VolumeHandle:", "vol-123"),
        p(23, "    ReadOnly:", "false"),
        p(
            23,
            "    VolumeAttributes:",
            "    storage.kubernetes.io/csiProvisionerIdentity=123-ebs",
        ),
        p(23, "Events:", "<none>"),
    ]);
    assert_eq!(got, want);
}

#[test]
fn persistent_volume_host_path_terminating() {
    let pv = json!({"metadata": {"name": "local", "deletionTimestamp": "2024-01-01T11:00:00Z"},
        "spec": {"hostPath": {"path": "/mnt/data", "type": "DirectoryOrCreate"}, "capacity": {"storage": "1000Mi"}}});
    let got = desc("v1/persistentvolumes", pv, vec![]);
    assert!(got.contains("Status:          Terminating (lasts 60m)\n"));
    assert!(got.contains("Claim:           \n"));
    assert!(got.contains("Capacity:        1000Mi\nNode Affinity:   <none>\n"));
    assert!(got.contains("Source:\n    Type:          HostPath (bare host directory volume)\n    Path:          /mnt/data\n    HostPathType:  DirectoryOrCreate\n"));
}

#[test]
fn persistent_volume_claim_used_by() {
    let pvc = json!({
        "metadata": {"name": "data-pvc", "namespace": "default",
            "annotations": {"pv.kubernetes.io/bind-completed": "yes"}, "finalizers": ["kubernetes.io/pvc-protection"]},
        "spec": {"accessModes": ["ReadWriteOnce"], "resources": {"requests": {"storage": "10Gi"}},
            "storageClassName": "gp3", "volumeName": "pv1", "volumeMode": "Filesystem"},
        "status": {"phase": "Bound", "accessModes": ["ReadWriteOnce"], "capacity": {"storage": "10Gi"}}
    });
    let user = |name: &str, ns: &str| {
        json!({"metadata": {"name": name, "namespace": ns, "uid": name},
        "spec": {"volumes": [{"name": "d", "persistentVolumeClaim": {"claimName": "data-pvc"}}]}})
    };
    let rel = Rel {
        pods: Some(vec![
            user("p1", "default"),
            user("p0", "default"),
            user("px", "other"),
        ]),
        ..Default::default()
    };
    let got = desc_rel("v1/persistentvolumeclaims", pvc, vec![], rel);
    let want = s(&[
        p(15, "Name:", "data-pvc"),
        p(15, "Namespace:", "default"),
        p(15, "StorageClass:", "gp3"),
        p(15, "Status:", "Bound"),
        p(15, "Volume:", "pv1"),
        p(15, "Labels:", "<none>"),
        p(15, "Annotations:", "pv.kubernetes.io/bind-completed: yes"),
        p(15, "Finalizers:", "[kubernetes.io/pvc-protection]"),
        p(15, "Capacity:", "10Gi"),
        p(15, "Access Modes:", "RWO"),
        p(15, "VolumeMode:", "Filesystem"),
        p(15, "Used By:", "p0"),
        p(15, "", "p1"),
        p(15, "Events:", "<none>"),
    ]);
    assert_eq!(got, want);
}

#[test]
fn storage_class() {
    let sc = json!({
        "metadata": {"name": "gp3", "annotations": {"storageclass.kubernetes.io/is-default-class": "true"}},
        "provisioner": "ebs.csi.aws.com",
        "parameters": {"type": "gp3", "encrypted": "true"},
        "allowVolumeExpansion": true, "reclaimPolicy": "Delete", "volumeBindingMode": "WaitForFirstConsumer",
        "allowedTopologies": [{"matchLabelExpressions": [{"key": "topology.ebs.csi.aws.com/zone", "values": ["us-east-1a", "us-east-1b"]}]}]
    });
    let got = desc("storage.k8s.io/v1/storageclasses", sc, vec![]);
    let want = s(&[
        p(23, "Name:", "gp3"),
        p(23, "IsDefaultClass:", "Yes"),
        p(
            23,
            "Annotations:",
            "storageclass.kubernetes.io/is-default-class=true",
        ),
        p(23, "Provisioner:", "ebs.csi.aws.com"),
        p(23, "Parameters:", "encrypted=true,type=gp3"),
        p(23, "AllowVolumeExpansion:", "True"),
        p(23, "MountOptions:", "<none>"),
        p(23, "ReclaimPolicy:", "Delete"),
        p(23, "VolumeBindingMode:", "WaitForFirstConsumer"),
        p(23, "AllowedTopologies:", ""),
        p(
            23,
            "  Term 0:",
            "topology.ebs.csi.aws.com/zone in [us-east-1a, us-east-1b]",
        ),
        p(23, "Events:", "<none>"),
    ]);
    assert_eq!(got, want);
}

#[test]
fn csi_node_and_volume_attributes_class() {
    let csi = json!({"metadata": {"name": "node-1", "creationTimestamp": "2024-01-01T00:00:00Z"},
        "spec": {"drivers": [{"name": "ebs.csi.aws.com", "nodeID": "i-123", "allocatable": {"count": 25},
            "topologyKeys": ["topology.ebs.csi.aws.com/zone"]}]}});
    let got = desc("storage.k8s.io/v1/csinodes", csi, vec![]);
    let want = s(&[
        p(20, "Name:", "node-1"),
        p(20, "Labels:", "<none>"),
        p(20, "Annotations:", "<none>"),
        p(20, "CreationTimestamp:", "Mon, 01 Jan 2024 00:00:00 +0000"),
        "Spec:".into(),
        "  Drivers:".into(),
        "    ebs.csi.aws.com:".into(),
        p(16, "      Node ID:", "i-123"),
        "      Allocatables:".into(),
        p(22, "        Count:", "25"),
        p(
            22,
            "      Topology Keys:",
            "[topology.ebs.csi.aws.com/zone]",
        ),
        p(22, "Events:", "<none>"),
    ]);
    assert_eq!(got, want);
    let vac = json!({"metadata": {"name": "fast"}, "driverName": "ebs.csi.aws.com", "parameters": {"iops": "4000"}});
    let got = desc("storage.k8s.io/v1/volumeattributesclasses", vac, vec![]);
    let want = s(&[
        p(14, "Name:", "fast"),
        p(14, "Annotations:", "<none>"),
        p(14, "DriverName:", "ebs.csi.aws.com"),
        p(14, "Parameters:", "iops=4000"),
        p(14, "Events:", "<none>"),
    ]);
    assert_eq!(got, want);
}

#[test]
fn cluster_role_rules_are_compacted_and_sorted() {
    let cr = json!({"metadata": {"name": "reader"}, "rules": [
        {"apiGroups": [""], "resources": ["pods", "pods/log"], "verbs": ["get", "list"]},
        {"apiGroups": [""], "resources": ["pods"], "verbs": ["watch"]},
        {"apiGroups": ["apps"], "resources": ["deployments"], "resourceNames": ["web"], "verbs": ["get"]},
        {"nonResourceURLs": ["/healthz"], "verbs": ["get"]}
    ]});
    let got = desc("rbac.authorization.k8s.io/v1/clusterroles", cr, vec![]);
    let row = |a: &str, b: &str, c: &str, d: &str| p(20, a, &p(19, b, &p(16, c, d)));
    let want = s(&[
        p(14, "Name:", "reader"),
        p(14, "Labels:", "<none>"),
        p(14, "Annotations:", "<none>"),
        "PolicyRule:".into(),
        row(
            "  Resources",
            "Non-Resource URLs",
            "Resource Names",
            "Verbs",
        ),
        row(
            "  ---------",
            "-----------------",
            "--------------",
            "-----",
        ),
        row("  pods", "[]", "[]", "[get list watch]"),
        row("  pods/log", "[]", "[]", "[get list]"),
        row("  ", "[/healthz]", "[]", "[get]"),
        row("  deployments.apps", "[]", "[web]", "[get]"),
    ]);
    assert_eq!(got, want);
}

#[test]
fn role_binding() {
    let crb = json!({"metadata": {"name": "crb"}, "roleRef": {"kind": "ClusterRole", "name": "reader"},
        "subjects": [{"kind": "ServiceAccount", "name": "sa1", "namespace": "default"}, {"kind": "Group", "name": "devs"}]});
    let got = desc(
        "rbac.authorization.k8s.io/v1/clusterrolebindings",
        crb,
        vec![],
    );
    let want = s(&[
        p(14, "Name:", "crb"),
        p(14, "Labels:", "<none>"),
        p(14, "Annotations:", "<none>"),
        "Role:".into(),
        p(9, "  Kind:", "ClusterRole"),
        p(9, "  Name:", "reader"),
        "Subjects:".into(),
        p(18, "  Kind", &p(6, "Name", "Namespace")),
        p(18, "  ----", &p(6, "----", "---------")),
        p(18, "  ServiceAccount", &p(6, "sa1", "default")),
        p(18, "  Group", &p(6, "devs", "")),
    ]);
    assert_eq!(got, want);
}

fn node_fixture() -> Value {
    json!({
        "metadata": {"name": "bar", "uid": "uid", "creationTimestamp": "2024-01-01T00:00:00Z",
            "labels": {"kubernetes.io/hostname": "bar", "node-role.kubernetes.io/control-plane": ""}},
        "spec": {"unschedulable": true, "podCIDR": "10.244.0.0/24", "podCIDRs": ["10.244.0.0/24"],
            "providerID": "aws:///us-east-1a/i-123",
            "taints": [{"key": "node-role.kubernetes.io/control-plane", "effect": "NoSchedule"}]},
        "status": {
            "capacity": {"cpu": "8", "memory": "24Gi", "hugepages-2Mi": "4Gi", "hugepages-1Gi": "0", "pods": "110"},
            "allocatable": {"cpu": "4", "memory": "12Gi", "hugepages-2Mi": "2Gi", "hugepages-1Gi": "0", "pods": "110"},
            "conditions": [{"type": "Ready", "status": "True", "lastHeartbeatTime": "2024-01-01T11:59:00Z",
                "lastTransitionTime": "2024-01-01T00:05:00Z", "reason": "KubeletReady", "message": "kubelet is posting ready status"}],
            "addresses": [{"type": "InternalIP", "address": "10.0.0.10"}, {"type": "Hostname", "address": "bar"}],
            "nodeInfo": {"machineID": "m1", "systemUUID": "u1", "bootID": "b1", "kernelVersion": "6.1.0",
                "osImage": "Ubuntu 22.04", "operatingSystem": "linux", "architecture": "amd64",
                "containerRuntimeVersion": "containerd://1.7.0", "kubeletVersion": "v1.34.0", "kubeProxyVersion": ""}
        }
    })
}

#[test]
fn node_with_pods_and_lease() {
    let pod = |name: &str, node: &str, phase: &str| {
        json!({
            "metadata": {"name": name, "namespace": "foo", "creationTimestamp": "2024-01-01T11:00:00Z"},
            "spec": {"nodeName": node, "containers": [
                {"name": "cpu-mem", "image": "image:latest", "resources": {
                    "requests": {"cpu": "1", "memory": "1Gi"}, "limits": {"cpu": "2", "memory": "2Gi"}}},
                {"name": "hugepages", "image": "image:latest", "resources": {
                    "requests": {"hugepages-2Mi": "512Mi"}, "limits": {"hugepages-2Mi": "512Mi"}}}
            ]},
            "status": {"phase": phase}
        })
    };
    let rel = Rel {
        pods: Some(vec![
            pod("pod-with-resources", "bar", "Running"),
            pod("done", "bar", "Succeeded"),
            pod("elsewhere", "other", "Running"),
        ]),
        lease: Some(
            json!({"metadata": {"name": "bar", "namespace": "kube-node-lease"},
            "spec": {"holderIdentity": "bar", "renewTime": "2024-01-01T11:59:50.000000Z"}}),
        ),
        ..Default::default()
    };
    let events = vec![
        json!({"type": "Normal", "reason": "Starting", "source": {"component": "kubelet"},
        "message": "Starting kubelet.", "count": 1,
        "firstTimestamp": "2014-01-15T00:00:00Z", "lastTimestamp": "2014-01-15T00:00:00Z"}),
    ];
    let got = desc_rel("v1/nodes", node_fixture(), events, rel);
    let cond = |a: &str, b: &str, c: &str, d: &str, e: &str, f: &str| {
        p(10, a, &p(8, b, &p(34, c, &p(34, d, &p(15, e, f)))))
    };
    let pods_row = |a: &str, b: &str, c: &str, d: &str, e: &str, f: &str, g: &str| {
        p(
            30,
            a,
            &p(
                20,
                b,
                &p(2, "", &p(14, c, &p(12, d, &p(17, e, &p(15, f, g))))),
            ),
        )
    };
    let alloc = |a: &str, b: &str, c: &str| p(21, a, &p(13, b, c));
    let ev =
        |a: &str, b: &str, c: &str, d: &str, e: &str| p(10, a, &p(10, b, &p(6, c, &p(9, d, e))));
    let want = s(&[
        p(20, "Name:", "bar"),
        p(20, "Roles:", "control-plane"),
        p(20, "Labels:", "kubernetes.io/hostname=bar"),
        p(20, "", "node-role.kubernetes.io/control-plane="),
        p(20, "Annotations:", "<none>"),
        p(20, "CreationTimestamp:", "Mon, 01 Jan 2024 00:00:00 +0000"),
        p(
            20,
            "Taints:",
            "node-role.kubernetes.io/control-plane:NoSchedule",
        ),
        p(20, "Unschedulable:", "true"),
        "Lease:".into(),
        p(19, "  HolderIdentity:", "bar"),
        p(19, "  AcquireTime:", "<unset>"),
        p(19, "  RenewTime:", "Mon, 01 Jan 2024 11:59:50 +0000"),
        "Conditions:".into(),
        cond(
            "  Type",
            "Status",
            "LastHeartbeatTime",
            "LastTransitionTime",
            "Reason",
            "Message",
        ),
        cond(
            "  ----",
            "------",
            "-----------------",
            "------------------",
            "------",
            "-------",
        ),
        cond(
            "  Ready ",
            "True ",
            "Mon, 01 Jan 2024 11:59:00 +0000 ",
            "Mon, 01 Jan 2024 00:05:00 +0000 ",
            "KubeletReady ",
            "kubelet is posting ready status",
        ),
        "Addresses:".into(),
        p(15, "  InternalIP:", "10.0.0.10"),
        p(15, "  Hostname:", "bar"),
        "Capacity:".into(),
        p(18, "  cpu:", "8"),
        p(18, "  hugepages-1Gi:", "0"),
        p(18, "  hugepages-2Mi:", "4Gi"),
        p(18, "  memory:", "24Gi"),
        p(18, "  pods:", "110"),
        "Allocatable:".into(),
        p(18, "  cpu:", "4"),
        p(18, "  hugepages-1Gi:", "0"),
        p(18, "  hugepages-2Mi:", "2Gi"),
        p(18, "  memory:", "12Gi"),
        p(18, "  pods:", "110"),
        "System Info:".into(),
        p(30, "  Machine ID:", "m1"),
        p(30, "  System UUID:", "u1"),
        p(30, "  Boot ID:", "b1"),
        p(30, "  Kernel Version:", "6.1.0"),
        p(30, "  OS Image:", "Ubuntu 22.04"),
        p(30, "  Operating System:", "linux"),
        p(30, "  Architecture:", "amd64"),
        p(30, "  Container Runtime Version:", "containerd://1.7.0"),
        p(30, "  Kubelet Version:", "v1.34.0"),
        p(30, "  Kube-Proxy Version:", ""),
        p(30, "PodCIDR:", "10.244.0.0/24"),
        p(30, "PodCIDRs:", "10.244.0.0/24"),
        p(30, "ProviderID:", "aws:///us-east-1a/i-123"),
        p(30, "Non-terminated Pods:", "(1 in total)"),
        pods_row(
            "  Namespace",
            "Name",
            "CPU Requests",
            "CPU Limits",
            "Memory Requests",
            "Memory Limits",
            "Age",
        ),
        pods_row(
            "  ---------",
            "----",
            "------------",
            "----------",
            "---------------",
            "-------------",
            "---",
        ),
        pods_row(
            "  foo",
            "pod-with-resources",
            "1 (25%)",
            "2 (50%)",
            "1Gi (8%)",
            "2Gi (16%)",
            "60m",
        ),
        "Allocated resources:".into(),
        "  (Total limits may be over 100 percent, i.e., overcommitted.)".into(),
        alloc("  Resource", "Requests", "Limits"),
        alloc("  --------", "--------", "------"),
        alloc("  cpu", "1 (25%)", "2 (50%)"),
        alloc("  memory", "1Gi (8%)", "2Gi (16%)"),
        alloc("  ephemeral-storage", "0 (0%)", "0 (0%)"),
        alloc("  hugepages-1Gi", "0 (0%)", "0 (0%)"),
        alloc("  hugepages-2Mi", "512Mi (25%)", "512Mi (25%)"),
        "Events:".into(),
        ev("  Type", "Reason", "Age", "From", "Message"),
        ev("  ----", "------", "----", "----", "-------"),
        ev("  Normal", "Starting", "9y", "kubelet", "Starting kubelet."),
    ]);
    assert_eq!(got, want);
}

#[test]
fn node_without_related_omits_pod_sections() {
    let got = desc("v1/nodes", node_fixture(), vec![]);
    assert!(!got.contains("Non-terminated Pods"));
    assert!(!got.contains("Lease:"));
    assert!(got.ends_with(&format!("{}\n", p(30, "Events:", "<none>"))));
}

#[test]
fn namespace_with_quotas_and_limits() {
    let ns = json!({"metadata": {"name": "team-a", "labels": {"kubernetes.io/metadata.name": "team-a"}},
        "status": {"phase": "Active"}});
    let rel = Rel {
        resource_quotas: Some(vec![
            json!({"metadata": {"name": "q1", "namespace": "team-a"}, "spec": {"scopes": ["NotBestEffort"]},
                "status": {"hard": {"pods": "10", "requests.cpu": "4"}, "used": {"pods": "3", "requests.cpu": "500m"}}}),
            json!({"metadata": {"name": "q2", "namespace": "other"}, "status": {"hard": {"pods": "1"}}}),
        ]),
        limit_ranges: Some(vec![
            json!({"metadata": {"name": "lr", "namespace": "team-a"},
            "spec": {"limits": [{"type": "Container", "default": {"cpu": "500m"}, "defaultRequest": {"cpu": "100m"}, "max": {"cpu": "2"}}]}}),
        ]),
        ..Default::default()
    };
    let got = desc_rel("v1/namespaces", ns, vec![], rel);
    let lr = |a: &str, b: &str, c: &str, d: &str, e: &str, f: &str, g: &str| {
        p(
            12,
            a,
            &p(10, b, &p(5, c, &p(5, d, &p(17, e, &p(15, f, g))))),
        )
    };
    let want = s(&[
        p(14, "Name:", "team-a"),
        p(14, "Labels:", "kubernetes.io/metadata.name=team-a"),
        p(14, "Annotations:", "<none>"),
        p(14, "Status:", "Active"),
        String::new(),
        "Resource Quotas".into(),
        p(11, "  Name:", "q1"),
        p(11, "  Scopes:", "NotBestEffort"),
        "  * Matches all pods that have at least one resource requirement set. These pods have a burstable or guaranteed quality of service.".into(),
        p(16, "  Resource", &p(6, "Used", "Hard")),
        p(16, "  --------", &p(6, "---", "---")),
        p(16, "  pods", &p(6, "3", "10")),
        p(16, "  requests.cpu", &p(6, "500m", "4")),
        String::new(),
        "Resource Limits".into(),
        lr(" Type", "Resource", "Min", "Max", "Default Request", "Default Limit", "Max Limit/Request Ratio"),
        lr(" ----", "--------", "---", "---", "---------------", "-------------", "-----------------------"),
        lr(" Container", "cpu", "-", "2", "100m", "500m", "-"),
    ]);
    assert_eq!(got, want);
}

#[test]
fn namespace_empty_related_lists() {
    let ns = json!({"metadata": {"name": "x"}, "status": {"phase": "Terminating",
        "conditions": [{"type": "NamespaceDeletionContentFailure", "status": "True",
            "lastTransitionTime": "2014-01-15T00:00:00Z", "reason": "example reason", "message": "example message"}]}});
    let rel = Rel {
        resource_quotas: Some(vec![]),
        limit_ranges: Some(vec![]),
        ..Default::default()
    };
    let got = desc_rel("v1/namespaces", ns, vec![], rel);
    assert!(got.contains("No resource quota.\n"));
    assert!(got.ends_with("\nNo LimitRange resource.\n"));
    assert!(got.contains("NamespaceDeletionContentFailure  True    Wed, 15 Jan 2014 00:00:00 +0000  example reason  example message\n"));
}

/// kubectl describe_test.go TestDescribeResourceQuota.
#[test]
fn resource_quota_formats_used_like_hard() {
    let rq = json!({"metadata": {"name": "bar", "namespace": "foo"}, "status": {
        "hard": {"cpu": "1", "limits.cpu": "2", "limits.memory": "2G", "memory": "1G", "requests.cpu": "1", "requests.memory": "1G"},
        "used": {"cpu": "0", "limits.cpu": "0", "limits.memory": "0G", "memory": "0G", "requests.cpu": "0", "requests.memory": "1000Ki"}}});
    let got = desc("v1/resourcequotas", rq, vec![]);
    let want = s(&[
        p(17, "Name:", "bar"),
        p(17, "Namespace:", "foo"),
        p(17, "Resource", &p(7, "Used", "Hard")),
        p(17, "--------", &p(7, "----", "----")),
        p(17, "cpu", &p(7, "0", "1")),
        p(17, "limits.cpu", &p(7, "0", "2")),
        p(17, "limits.memory", &p(7, "0", "2G")),
        p(17, "memory", &p(7, "0", "1G")),
        p(17, "requests.cpu", &p(7, "0", "1")),
        p(17, "requests.memory", &p(7, "1024k", "1G")),
    ]);
    assert_eq!(got, want);
}

#[test]
fn limit_range() {
    let lr = json!({"metadata": {"name": "lr", "namespace": "foo"}, "spec": {"limits": [
        {"type": "Pod", "max": {"memory": "1Gi"}},
        {"type": "Container", "min": {"cpu": "100m"}, "maxLimitRequestRatio": {"cpu": "4"}}
    ]}});
    let got = desc("v1/limitranges", lr, vec![]);
    let row = |a: &str, b: &str, c: &str, d: &str, e: &str, f: &str, g: &str| {
        p(
            12,
            a,
            &p(10, b, &p(6, c, &p(5, d, &p(17, e, &p(15, f, g))))),
        )
    };
    let want = s(&[
        p(12, "Name:", "lr"),
        p(12, "Namespace:", "foo"),
        row(
            "Type",
            "Resource",
            "Min",
            "Max",
            "Default Request",
            "Default Limit",
            "Max Limit/Request Ratio",
        ),
        row(
            "----",
            "--------",
            "---",
            "---",
            "---------------",
            "-------------",
            "-----------------------",
        ),
        row("Pod", "memory", "-", "1Gi", "-", "-", "-"),
        row("Container", "cpu", "100m", "-", "-", "-", "4"),
    ]);
    assert_eq!(got, want);
}

#[test]
fn priority_class_and_pdb() {
    let pc = json!({"metadata": {"name": "high"}, "value": 1000000, "globalDefault": false,
        "preemptionPolicy": "PreemptLowerPriority", "description": "High priority"});
    let got = desc("scheduling.k8s.io/v1/priorityclasses", pc, vec![]);
    let want = s(&[
        p(19, "Name:", "high"),
        p(19, "Value:", "1000000"),
        p(19, "GlobalDefault:", "false"),
        p(19, "PreemptionPolicy:", "PreemptLowerPriority"),
        p(19, "Description:", "High priority"),
        p(19, "Annotations:", "<none>"),
        p(19, "Events:", "<none>"),
    ]);
    assert_eq!(got, want);
    let pdb = json!({"metadata": {"name": "web-pdb", "namespace": "prod"},
        "spec": {"minAvailable": 2, "selector": {"matchLabels": {"app": "web"}}},
        "status": {"disruptionsAllowed": 1, "currentHealthy": 3, "desiredHealthy": 2, "expectedPods": 3}});
    let got = desc("policy/v1/poddisruptionbudgets", pdb, vec![]);
    let want = s(&[
        p(16, "Name:", "web-pdb"),
        p(16, "Namespace:", "prod"),
        p(16, "Min available:", "2"),
        p(16, "Selector:", "app=web"),
        "Status:".into(),
        p(26, "    Allowed disruptions:", "1"),
        p(26, "    Current:", "3"),
        p(26, "    Desired:", "2"),
        p(26, "    Total:", "3"),
        p(26, "Events:", "<none>"),
    ]);
    assert_eq!(got, want);
}

#[test]
fn hpa_v2() {
    let hpa = json!({
        "metadata": {"name": "web", "namespace": "prod", "creationTimestamp": "2024-01-01T08:00:00Z"},
        "spec": {
            "scaleTargetRef": {"apiVersion": "apps/v1", "kind": "Deployment", "name": "web"},
            "minReplicas": 2, "maxReplicas": 10,
            "metrics": [
                {"type": "Resource", "resource": {"name": "cpu", "target": {"type": "Utilization", "averageUtilization": 80}}},
                {"type": "Resource", "resource": {"name": "memory", "target": {"type": "AverageValue", "averageValue": "500Mi"}}},
                {"type": "Pods", "pods": {"metric": {"name": "packets-per-second"}, "target": {"type": "AverageValue", "averageValue": "1k"}}}
            ],
            "behavior": {"scaleDown": {"stabilizationWindowSeconds": 300, "policies": [{"type": "Percent", "value": 100, "periodSeconds": 15}]}}
        },
        "status": {
            "currentReplicas": 3, "desiredReplicas": 3,
            "currentMetrics": [
                {"type": "Resource", "resource": {"name": "cpu", "current": {"averageUtilization": 45, "averageValue": "225m"}}},
                {"type": "Resource", "resource": {"name": "memory", "current": {"averageValue": "300Mi"}}}
            ],
            "conditions": [{"type": "AbleToScale", "status": "True", "reason": "ReadyForNewScale", "message": "recommended size matches current size"}]
        }
    });
    let got = desc("autoscaling/v2/horizontalpodautoscalers", hpa, vec![]);
    let cond = |a: &str, b: &str, c: &str, d: &str| p(15, a, &p(8, b, &p(18, c, d)));
    let want = s(&[
        p(55, "Name:", "web"),
        p(55, "Namespace:", "prod"),
        p(55, "Labels:", "<none>"),
        p(55, "Annotations:", "<none>"),
        p(55, "CreationTimestamp:", "Mon, 01 Jan 2024 08:00:00 +0000"),
        p(55, "Reference:", "Deployment/web"),
        p(55, "Metrics:", "( current / target )"),
        p(
            55,
            "  resource cpu on pods  (as a percentage of request):",
            "45% (225m) / 80%",
        ),
        p(55, "  resource memory on pods:", "300Mi / 500Mi"),
        p(55, "  \"packets-per-second\" on pods:", "<unknown> / 1k"),
        p(55, "Min replicas:", "2"),
        p(55, "Max replicas:", "10"),
        "Behavior:".into(),
        "  Scale Down:".into(),
        "    Stabilization Window: 300 seconds".into(),
        "    Select Policy: Max".into(),
        "    Policies:".into(),
        p(
            23,
            "      - Type: Percent",
            &p(12, "Value: 100", "Period: 15 seconds"),
        ),
        p(23, "Deployment pods:", "3 current / 3 desired"),
        "Conditions:".into(),
        cond("  Type", "Status", "Reason", "Message"),
        cond("  ----", "------", "------", "-------"),
        cond(
            "  AbleToScale",
            "True",
            "ReadyForNewScale",
            "recommended size matches current size",
        ),
        p(15, "Events:", "<none>"),
    ]);
    assert_eq!(got, want);
}

#[test]
fn hpa_v1() {
    let hpa = json!({"metadata": {"name": "h", "namespace": "d", "creationTimestamp": "2024-01-01T08:00:00Z"},
        "spec": {"scaleTargetRef": {"kind": "Deployment", "name": "web"}, "minReplicas": 1, "maxReplicas": 5,
            "targetCPUUtilizationPercentage": 50},
        "status": {"currentReplicas": 2, "desiredReplicas": 2, "currentCPUUtilizationPercentage": 30}});
    let got = desc("autoscaling/v1/horizontalpodautoscalers", hpa, vec![]);
    let want = s(&[
        p(26, "Name:", "h"),
        p(26, "Namespace:", "d"),
        p(26, "Labels:", "<none>"),
        p(26, "Annotations:", "<none>"),
        p(26, "CreationTimestamp:", "Mon, 01 Jan 2024 08:00:00 +0000"),
        p(26, "Reference:", "Deployment/web"),
        p(26, "Target CPU utilization:", "50%"),
        p(26, "Current CPU utilization:", "30%"),
        p(26, "Min replicas:", "1"),
        p(26, "Max replicas:", "5"),
        p(26, "Deployment pods:", "2 current / 2 desired"),
        p(26, "Events:", "<none>"),
    ]);
    assert_eq!(got, want);
}

#[test]
fn generic_custom_resource() {
    let cr = json!({
        "apiVersion": "example.com/v1", "kind": "Widget",
        "metadata": {"name": "w1", "namespace": "default", "uid": "u1", "creationTimestamp": "2024-01-01T00:00:00Z",
            "generation": 2, "resourceVersion": "42", "managedFields": [{"manager": "kubectl"}], "labels": {"a": "b"}},
        "spec": {"size": 3, "ratio": 1.5, "tags": ["x", "y"],
            "nested": {"enabled": true, "items": [{"name": "i1"}, {"name": "i2"}]}},
        "status": {"phase": "Ready"}
    });
    let ev = vec![
        json!({"type": "Normal", "reason": "Synced", "reportingComponent": "widget-controller",
        "message": "ok", "series": {"count": 4, "lastObservedTime": "2024-01-01T11:59:00.000000Z"},
        "eventTime": "2024-01-01T11:00:00.000000Z"}),
    ];
    let got = desc("example.com/v1/widgets", cr, ev);
    let evrow =
        |a: &str, b: &str, c: &str, d: &str, e: &str| p(10, a, &p(8, b, &p(19, c, &p(19, d, e))));
    let want = s(&[
        p(14, "Name:", "w1"),
        p(14, "Namespace:", "default"),
        p(14, "Labels:", "a=b"),
        p(14, "Annotations:", "<none>"),
        p(14, "API Version:", "example.com/v1"),
        p(14, "Kind:", "Widget"),
        "Metadata:".into(),
        p(23, "  Creation Timestamp:", "2024-01-01T00:00:00Z"),
        p(23, "  Generation:", "2"),
        p(23, "  Resource Version:", "42"),
        p(23, "  UID:", "u1"),
        "Spec:".into(),
        "  Nested:".into(),
        p(14, "    Enabled:", "true"),
        "    Items:".into(),
        p(13, "      Name:", "i1"),
        p(13, "      Name:", "i2"),
        p(13, "  Ratio:", "1.5"),
        p(13, "  Size:", "3"),
        "  Tags:".into(),
        "    x".into(),
        "    y".into(),
        "Status:".into(),
        p(10, "  Phase:", "Ready"),
        "Events:".into(),
        evrow("  Type", "Reason", "Age", "From", "Message"),
        evrow("  ----", "------", "----", "----", "-------"),
        evrow(
            "  Normal",
            "Synced",
            "60s (x4 over 60m)",
            "widget-controller",
            "ok",
        ),
    ]);
    assert_eq!(got, want);
}

#[test]
fn crd_uses_generic_describer() {
    let crd = json!({"apiVersion": "apiextensions.k8s.io/v1", "kind": "CustomResourceDefinition",
        "metadata": {"name": "widgets.example.com"},
        "spec": {"group": "example.com", "names": {"kind": "Widget", "plural": "widgets"}, "scope": "Namespaced"}});
    let got = desc(
        "apiextensions.k8s.io/v1/customresourcedefinitions",
        crd,
        vec![],
    );
    assert!(got.starts_with("Name:         widgets.example.com\nNamespace:    \n"));
    assert!(got.contains("Metadata:\nSpec:\n  Group:  example.com\n  Names:\n    Kind:    Widget\n    Plural:  widgets\n  Scope:     Namespaced\n"));
    assert!(got.ends_with("Events:      <none>\n"));
}

#[test]
fn public_api_converts_dynamic_objects() {
    let obj: kube::api::DynamicObject = serde_json::from_value(json!({
        "apiVersion": "v1", "kind": "ConfigMap",
        "metadata": {"name": "c", "namespace": "n"}, "data": {"k": "v"}
    }))
    .expect("dynamic object");
    let ev: kube::api::DynamicObject = serde_json::from_value(json!({
        "apiVersion": "v1", "kind": "Event", "metadata": {"name": "e", "namespace": "n"},
        "type": "Normal", "reason": "Created", "message": "made", "source": {"component": "me"},
        "firstTimestamp": "2024-01-01T11:00:00Z", "lastTimestamp": "2024-01-01T11:00:00Z", "count": 1
    }))
    .expect("event");
    let now = chrono::DateTime::parse_from_rfc3339("2024-01-01T12:00:00Z")
        .expect("now")
        .with_timezone(&chrono::Utc);
    let got =
        super::describe_at(&Gvr::parse("v1/configmaps"), &obj, &vec![ev], now).expect("describe");
    assert!(got.starts_with("Name:         c\nNamespace:    n\n"));
    assert!(got.contains("\nData\n====\nk:\n----\nv\n\n"));
    assert!(got.contains("  Normal  Created  60m   me    made\n"));
    let got = super::describe(&Gvr::parse("v1/configmaps"), &obj, &vec![]).expect("describe");
    assert!(got.ends_with("Events:  <none>\n"));
}

#[test]
fn invalid_selector_is_an_error_for_deployments() {
    let mut d = deployment_fixture();
    d["spec"]["selector"] = json!({"matchExpressions": [{"key": "a", "operator": "Bogus"}]});
    let err = run(
        &Gvr::parse("apps/v1/deployments"),
        &d,
        &[],
        &Rel::default(),
        now(),
        TimeZone::UTC,
    );
    assert!(err.is_err());
}

/// kubectl v0.35 describeWorkloadReference on pods.
#[test]
fn pod_workload_ref() {
    let mut pod = pod_fixture();
    pod["spec"]["workloadRef"] = json!({"name": "wl", "podGroup": "pg", "podGroupReplicaKey": "k"});
    let got = desc("v1/pods", pod, vec![]);
    assert!(
        got.contains("WorkloadRef:\n  Name:                wl\n  PodGroup:            pg\n  PodGroupReplicaKey:  k\nEvents:"),
        "{got}"
    );
}

/// kubectl CertificateSigningRequestDescriber: subject and SANs parsed out
/// of the PKCS#10 request, status from conditions.
#[test]
fn certificate_signing_request() {
    let csr = json!({"metadata": {"name": "csr-1", "creationTimestamp": "2024-01-01T11:00:00Z"},
        "spec": {"request": "LS0tLS1CRUdJTiBDRVJUSUZJQ0FURSBSRVFVRVNULS0tLS0KTUlJQlNUQ0I3d0lCQURCTU1Rc3dDUVlEVlFRR0V3SlZVekVWTUJNR0ExVUVDZ3dNYzNsemRHVnRPbTV2WkdWegpNUTB3Q3dZRFZRUUtEQVJ2Y21jeU1SY3dGUVlEVlFRRERBNXplWE4wWlcwNmJtOWtaVHB1TVRCWk1CTUdCeXFHClNNNDlBZ0VHQ0NxR1NNNDlBd0VIQTBJQUJCVDJRUmtSdzZ6NEF0MVFycXZDM29SNkZob0UzdXFVQ2VlU1VBWEoKSDZIdGNjWjBqKzB3ZTZ2b01QMUYzWlZYUlQ2RExRYVJPS0lmREJWY3hNaFF3ZXlnUVRBL0Jna3Foa2lHOXcwQgpDUTR4TWpBd01DNEdBMVVkRVFRbk1DV0NCR0V1YVcrQ0JHSXVhVytIQkFvQUFBR0JCWGhBZVM1NmhncHpjR2xtClptVTZMeTk2TUFvR0NDcUdTTTQ5QkFNQ0Ewa0FNRVlDSVFEdm4xSGN1TEpPR0RyNFJPS0hsV2tDNDZaT2RXUUkKVEJyYm5aSUpCajM3c3dJaEFOSEhHSTVzRVJUNEtWNGx3OWJUY2pwMzlRSngveDJWWkpVMVVqeEoxelBxCi0tLS0tRU5EIENFUlRJRklDQVRFIFJFUVVFU1QtLS0tLQo=", "signerName": "kubernetes.io/kubelet-serving",
                 "username": "system:node:n1", "expirationSeconds": 3600},
        "status": {"conditions": [{"type": "Approved"}], "certificate": "Y2VydA=="}});
    let got = desc(
        "certificates.k8s.io/v1/certificatesigningrequests",
        csr,
        vec![],
    );
    for want in [
        "Name:",
        "csr-1",
        "Requesting User:",
        "system:node:n1",
        "Signer:",
        "kubernetes.io/kubelet-serving",
        "Requested Duration:",
        "60m",
        "Status:",
        "Approved,Issued",
        "Common Name:",
        "system:node:n1",
        "Organization:",
        "system:nodes",
        "Country:",
        "US",
        "DNS Names:",
        "a.io",
        "b.io",
        "Email Addresses:",
        "x@y.z",
        "URIs:",
        "spiffe://z",
        "IP Addresses:",
        "10.0.0.1",
        "Events:",
    ] {
        assert!(got.contains(want), "{want}:\n{got}");
    }
    // A garbage request fails like kubectl.
    let bad = json!({"metadata": {"name": "x"}, "spec": {"request": "bm9wZQ=="}});
    let err = run(
        &Gvr::parse("certificates.k8s.io/v1/certificatesigningrequests"),
        &bad,
        &[],
        &Rel::default(),
        now(),
        TimeZone::UTC,
    )
    .unwrap_err()
    .to_string();
    assert!(err.starts_with("Error parsing CSR:"), "{err}");
}
