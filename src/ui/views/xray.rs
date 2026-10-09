//! Xray: k9s view/xray.go, ui/tree.go, model/tree.go and internal/xray.

use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::time::Duration;

use crossterm::event::KeyEvent;
use kube::api::DynamicObject;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::Frame;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::details::{DetailsView, YAML_ACTION};
use super::logs::LogsView;
use super::util::{self, SLine, Seg};
use crate::config::Styles;
use crate::k8s::{fqn, is_all_namespaces, namespaced, Accessor, Gvr, LogOptions, SharedAccessor};
use crate::ui::{AppEvent, Ctx, Dialog, FlashLevel, Hint, View, ViewCmd, ViewId};

pub(crate) const STATUS_OK: &str = "ok";
pub(crate) const STATUS_TOAST: &str = "toast";
pub(crate) const STATUS_COMPLETED: &str = "completed";
pub(crate) const STATUS_NOREF: &str = "noref";
const PATH_SEP: &str = "::";

pub(crate) const PODS: &str = "v1/pods";
pub(crate) const NAMESPACES: &str = "v1/namespaces";
pub(crate) const CONTAINERS: &str = "containers";
const SECRETS: &str = "v1/secrets";
const CONFIGMAPS: &str = "v1/configmaps";
const PVCS: &str = "v1/persistentvolumeclaims";
const SAS: &str = "v1/serviceaccounts";
const DEPLOYS: &str = "apps/v1/deployments";
const RSS: &str = "apps/v1/replicasets";
const STSS: &str = "apps/v1/statefulsets";
const DSS: &str = "apps/v1/daemonsets";
const SVCS: &str = "v1/services";
const DEFAULT_CONTAINER_ANN: &str = "kubectl.kubernetes.io/default-container";

/// Resources xray supports (view/command.go allowedCmds).
pub(crate) fn allowed(gvr: &Gvr) -> bool {
    [PODS, SVCS, DEPLOYS, DSS, STSS, RSS].contains(&gvr.to_string().as_str())
}

/// xray.TreeNode.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TreeNode {
    pub gvr: String,
    pub id: String,
    pub status: String,
    pub info: Option<String>,
    pub children: Vec<TreeNode>,
    /// Pod containers (default first), used to pick a shell/attach target.
    pub containers: Vec<String>,
    pub default_container: Option<String>,
}

impl TreeNode {
    pub fn new(gvr: &str, id: impl Into<String>) -> Self {
        TreeNode {
            gvr: gvr.into(),
            id: id.into(),
            status: STATUS_OK.into(),
            info: None,
            children: Vec::new(),
            containers: Vec::new(),
            default_container: None,
        }
    }

    fn is_leaf(&self) -> bool {
        self.children.is_empty()
    }

    fn find(&self, gvr: &str, id: &str) -> bool {
        (self.gvr == gvr && self.id == id) || self.children.iter().any(|c| c.find(gvr, id))
    }

    /// Child index path to the first node matching (gvr, id).
    fn find_path(&self, gvr: &str, id: &str) -> Option<Vec<usize>> {
        if self.gvr == gvr && self.id == id {
            return Some(Vec::new());
        }
        self.children.iter().enumerate().find_map(|(k, c)| {
            c.find_path(gvr, id).map(|mut p| {
                p.insert(0, k);
                p
            })
        })
    }

    fn at_mut(&mut self, path: &[usize]) -> &mut TreeNode {
        let mut n = self;
        for &k in path {
            n = &mut n.children[k];
        }
        n
    }

    fn find_mut(&mut self, gvr: &str, id: &str) -> Option<&mut TreeNode> {
        if self.gvr == gvr && self.id == id {
            return Some(self);
        }
        self.children.iter_mut().find_map(|c| c.find_mut(gvr, id))
    }

    /// TreeNode.Count.
    pub fn count(&self, gvr: &str) -> usize {
        usize::from(self.gvr == gvr) + self.children.iter().map(|c| c.count(gvr)).sum::<usize>()
    }

    pub fn sort(&mut self) {
        self.children
            .sort_by(|a, b| util::natural_cmp(&a.id, &b.id));
        for c in &mut self.children {
            c.sort();
        }
    }

    /// TreeNode.Flatten: specs of all leaves.
    fn flatten(&self, chain: &mut Vec<(String, String, String)>, out: &mut Vec<NodeSpec>) {
        for c in &self.children {
            chain.push((c.gvr.clone(), c.id.clone(), c.status.clone()));
            if c.is_leaf() {
                out.push(NodeSpec::from_chain(chain));
            } else {
                c.flatten(chain, out);
            }
            chain.pop();
        }
    }

    /// TreeNode.Filter.
    fn filter(&self, keep: impl Fn(&str) -> bool) -> Option<TreeNode> {
        let mut chain = vec![(self.gvr.clone(), self.id.clone(), self.status.clone())];
        let mut specs = Vec::new();
        self.flatten(&mut chain, &mut specs);
        let matches: Vec<NodeSpec> = specs
            .into_iter()
            .filter(|s| keep(&format!("{}{}", s.as_path(), s.as_status())))
            .collect();
        if matches.is_empty() {
            return None;
        }
        Some(hydrate(&matches))
    }
}

/// xray.Hydrate: rebuild a tree from leaf specs.
fn hydrate(specs: &[NodeSpec]) -> TreeNode {
    let mut root = TreeNode::new("", "");
    for spec in specs {
        let mut nav: Vec<usize> = Vec::new();
        for i in (0..spec.paths.len()).rev() {
            let (g, p, st) = (&spec.gvrs[i], &spec.paths[i], &spec.statuses[i]);
            let node = root.at_mut(&nav);
            if node.gvr.is_empty() && node.id.is_empty() {
                node.gvr = g.clone();
                node.id = p.clone();
                node.status = st.clone();
                continue;
            }
            match node.find_path(g, p) {
                Some(rel) => nav.extend(rel),
                None => {
                    let mut c = TreeNode::new(g, p.clone());
                    c.status = st.clone();
                    node.children.push(c);
                    nav.push(node.children.len() - 1);
                }
            }
        }
    }
    root
}

/// xray.NodeSpec: leaf-first chains of gvrs, paths and statuses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NodeSpec {
    pub gvrs: Vec<String>,
    pub paths: Vec<String>,
    pub statuses: Vec<String>,
}

impl NodeSpec {
    /// Build from a root-first chain.
    fn from_chain(chain: &[(String, String, String)]) -> Self {
        let mut s = NodeSpec {
            gvrs: Vec::new(),
            paths: Vec::new(),
            statuses: Vec::new(),
        };
        for (g, p, st) in chain.iter().rev() {
            s.gvrs.push(g.clone());
            s.paths.push(p.clone());
            s.statuses.push(st.clone());
        }
        s
    }
    pub fn gvr(&self) -> &str {
        &self.gvrs[0]
    }
    pub fn path(&self) -> &str {
        &self.paths[0]
    }
    pub fn status(&self) -> &str {
        &self.statuses[0]
    }
    pub fn parent_path(&self) -> Option<&str> {
        self.paths.get(1).map(String::as_str)
    }
    pub fn as_path(&self) -> String {
        self.paths.join(PATH_SEP)
    }
    pub fn as_status(&self) -> String {
        self.statuses.join(PATH_SEP)
    }
}

// ---------------------------------------------------------------------------
// Tree building (xray renderers).

fn s<'a>(v: &'a Value, ptr: &str) -> &'a str {
    v.pointer(ptr).and_then(Value::as_str).unwrap_or("")
}

fn i(v: &Value, ptr: &str) -> i64 {
    v.pointer(ptr).and_then(Value::as_i64).unwrap_or(0)
}

fn arr<'a>(v: &'a Value, ptr: &str) -> &'a [Value] {
    v.pointer(ptr)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn obj_json(o: &DynamicObject) -> Value {
    serde_json::to_value(o).unwrap_or(Value::Null)
}

fn selector_string(m: Option<&serde_json::Map<String, Value>>) -> String {
    let mut kv: Vec<String> = m
        .map(|m| {
            m.iter()
                .map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or_default()))
                .collect()
        })
        .unwrap_or_default();
    kv.sort();
    kv.join(",")
}

/// Objects referenced by pods, prefetched once per refresh.
#[derive(Default)]
struct Refs {
    present: HashMap<&'static str, HashSet<String>>,
    sas: HashMap<String, Value>,
}

impl Refs {
    fn has(&self, gvr: &str, id: &str) -> bool {
        self.present
            .get(gvr)
            .map(|s| s.contains(id))
            .unwrap_or(false)
    }
}

struct Builder<'a> {
    acc: &'a dyn Accessor,
    ns: String,
    refs: Refs,
}

impl Builder<'_> {
    async fn prefetch(&mut self) -> anyhow::Result<()> {
        for g in [SECRETS, CONFIGMAPS, PVCS, SAS] {
            let oo = self
                .acc
                .list(&Gvr::parse(g), &self.ns, None)
                .await
                .unwrap_or_default();
            let mut set = HashSet::new();
            for o in oo {
                let id = fqn(
                    o.metadata.namespace.as_deref().unwrap_or(""),
                    o.metadata.name.as_deref().unwrap_or(""),
                );
                if g == SAS {
                    self.refs.sas.insert(id.clone(), obj_json(&o));
                }
                set.insert(id);
            }
            self.refs.present.insert(g, set);
        }
        Ok(())
    }

    /// xray.addRef + validate.
    fn add_ref(&self, parent: &mut TreeNode, gvr: &str, id: String, optional: Option<bool>) {
        if parent.find(gvr, &id) {
            return;
        }
        let mut n = TreeNode::new(gvr, id);
        if !self.refs.has(gvr, &n.id) && !optional.unwrap_or(false) {
            n.status = STATUS_NOREF.into();
        }
        parent.children.push(n);
    }

    /// xray.Container.
    fn container(&self, parent: &mut TreeNode, co: &Value) {
        let mut root = TreeNode::new(CONTAINERS, fqn(&self.ns, s(co, "/name")));
        let pns = namespaced(&parent.id).0.to_string();
        for e in arr(co, "/env") {
            if let Some(r) = e.pointer("/valueFrom/secretKeyRef") {
                self.add_ref(
                    &mut root,
                    SECRETS,
                    fqn(&pns, s(r, "/name")),
                    r.get("optional").and_then(Value::as_bool),
                );
            }
            if let Some(r) = e.pointer("/valueFrom/configMapKeyRef") {
                self.add_ref(
                    &mut root,
                    CONFIGMAPS,
                    fqn(&pns, s(r, "/name")),
                    r.get("optional").and_then(Value::as_bool),
                );
            }
        }
        for e in arr(co, "/envFrom") {
            if let Some(r) = e.get("configMapRef") {
                self.add_ref(
                    &mut root,
                    CONFIGMAPS,
                    fqn(&pns, s(r, "/name")),
                    r.get("optional").and_then(Value::as_bool),
                );
            }
            if let Some(r) = e.get("secretRef") {
                self.add_ref(
                    &mut root,
                    SECRETS,
                    fqn(&pns, s(r, "/name")),
                    r.get("optional").and_then(Value::as_bool),
                );
            }
        }
        parent.children.push(root);
    }

    /// xray.Pod.
    fn pod(&self, parent: &mut TreeNode, po: &Value) {
        let pns = s(po, "/metadata/namespace").to_string();
        let mut node = TreeNode::new(PODS, fqn(&pns, s(po, "/metadata/name")));
        let spec = po.get("spec").cloned().unwrap_or(Value::Null);
        for c in arr(&spec, "/initContainers") {
            self.container(&mut node, c);
        }
        let cos = arr(&spec, "/containers");
        for c in cos {
            self.container(&mut node, c);
        }
        // k9s indexes spec.Containers while walking ephemeral containers.
        for (idx, _) in arr(&spec, "/ephemeralContainers").iter().enumerate() {
            if let Some(c) = cos.get(idx) {
                self.container(&mut node, c);
            }
        }
        for v in arr(&spec, "/volumes") {
            if let Some(sec) = v.get("secret") {
                self.add_ref(
                    &mut node,
                    SECRETS,
                    fqn(&pns, s(sec, "/secretName")),
                    sec.get("optional").and_then(Value::as_bool),
                );
                continue;
            }
            if let Some(cm) = v.get("configMap") {
                self.add_ref(
                    &mut node,
                    CONFIGMAPS,
                    fqn(&pns, s(cm, "/name")),
                    cm.get("optional").and_then(Value::as_bool),
                );
                continue;
            }
            if let Some(pvc) = v.get("persistentVolumeClaim") {
                self.add_ref(&mut node, PVCS, fqn(&pns, s(pvc, "/claimName")), None);
            }
        }
        let sa = s(&spec, "/serviceAccountName");
        if !sa.is_empty() {
            let id = fqn(&pns, sa);
            match self.refs.sas.get(&id) {
                Some(sao) => {
                    let auto = spec
                        .get("automountServiceAccountToken")
                        .and_then(Value::as_bool);
                    self.service_account(&mut node, sao, auto);
                }
                None => self.add_ref(&mut node, SAS, id, None),
            }
        }
        // Containers for shell/attach (default container first).
        let names: Vec<String> = cos.iter().map(|c| s(c, "/name").to_string()).collect();
        let ann = s(
            po,
            &format!(
                "/metadata/annotations/{}",
                DEFAULT_CONTAINER_ANN.replace('/', "~1")
            ),
        );
        if !ann.is_empty() && names.iter().any(|n| n == ann) {
            node.default_container = Some(ann.to_string());
        }
        let mut all: Vec<String> = node.default_container.iter().cloned().collect();
        all.extend(
            names
                .into_iter()
                .filter(|n| Some(n) != node.default_container.as_ref()),
        );
        all.extend(
            arr(&spec, "/initContainers")
                .iter()
                .map(|c| s(c, "/name").to_string()),
        );
        node.containers = all;

        let (status, info) = pod_status(po);
        node.status = status;
        node.info = Some(info);

        let ns_id = format!("-/{pns}");
        if parent.find_mut(NAMESPACES, &ns_id).is_none() {
            parent
                .children
                .push(TreeNode::new(NAMESPACES, ns_id.clone()));
        }
        parent
            .find_mut(NAMESPACES, &ns_id)
            .expect("ns node")
            .children
            .push(node);
    }

    /// xray.ServiceAccount.
    fn service_account(&self, parent: &mut TreeNode, sa: &Value, auto: Option<bool>) {
        let sns = s(sa, "/metadata/namespace");
        let mut node = TreeNode::new(SAS, fqn(sns, s(sa, "/metadata/name")));
        for sec in arr(sa, "/secrets")
            .iter()
            .chain(arr(sa, "/imagePullSecrets"))
        {
            self.add_ref(&mut node, SECRETS, fqn(sns, s(sec, "/name")), None);
        }
        if let Some(a) = sa
            .get("automountServiceAccountToken")
            .and_then(Value::as_bool)
        {
            node.info = Some(format!("automount={a}"));
        }
        if let Some(a) = auto {
            node.info = Some(format!("automount={a}"));
        }
        parent.children.push(node);
    }

    /// Workloads owning pods (dp/rs/sts/ds/svc).
    async fn workload(&self, parent: &mut TreeNode, gvr: &str, o: &Value) -> anyhow::Result<()> {
        let ons = s(o, "/metadata/namespace").to_string();
        let mut root = TreeNode::new(gvr, fqn(&ons, s(o, "/metadata/name")));
        let sel = if gvr == SVCS {
            selector_string(o.pointer("/spec/selector").and_then(Value::as_object))
        } else {
            selector_string(
                o.pointer("/spec/selector/matchLabels")
                    .and_then(Value::as_object),
            )
        };
        let sel_opt = if sel.is_empty() {
            None
        } else {
            Some(sel.as_str())
        };
        let pods = self.acc.list(&Gvr::parse(PODS), &ons, sel_opt).await?;
        for p in &pods {
            self.pod(&mut root, &obj_json(p));
        }
        if root.is_leaf() {
            return Ok(());
        }
        let (status, info) = match gvr {
            DEPLOYS => {
                let r = o
                    .pointer("/spec/replicas")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                let a = i(o, "/status/availableReplicas");
                let u = i(o, "/status/unavailableReplicas");
                (a != r || u != 0, Some(format!("{a}/{r}/{u}")))
            }
            RSS => {
                let r = o
                    .pointer("/spec/replicas")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                let a = i(o, "/status/replicas");
                (a != r, Some(format!("{a}/{r}")))
            }
            STSS => {
                let r = o
                    .pointer("/spec/replicas")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                let a = i(o, "/status/currentReplicas");
                (a != r, Some(format!("{a}/{r}")))
            }
            DSS => {
                let d = i(o, "/status/desiredNumberScheduled");
                let a = i(o, "/status/numberAvailable");
                (d != a, Some(format!("{a}/{d}")))
            }
            _ => (false, None),
        };
        root.status = if status { STATUS_TOAST } else { STATUS_OK }.into();
        root.info = info;
        let ns_id = format!("-/{ons}");
        if parent.find_mut(NAMESPACES, &ns_id).is_none() {
            parent
                .children
                .push(TreeNode::new(NAMESPACES, ns_id.clone()));
        }
        parent
            .find_mut(NAMESPACES, &ns_id)
            .expect("ns node")
            .children
            .push(root);
        Ok(())
    }
}

/// render.Pod.Phase + xray Pod.validate.
pub(crate) fn pod_status(po: &Value) -> (String, String) {
    let ss = arr(po, "/status/containerStatuses");
    let ready = ss
        .iter()
        .filter(|c| c.get("ready").and_then(Value::as_bool).unwrap_or(false))
        .count();
    let mut status = if ready != ss.len() {
        STATUS_TOAST
    } else {
        STATUS_OK
    };
    if pod_phase(po) == "Completed" {
        status = STATUS_COMPLETED;
    }
    (status.into(), format!("{ready}/{}", ss.len()))
}

/// render.Pod.Phase.
pub(crate) fn pod_phase(po: &Value) -> String {
    let mut status = s(po, "/status/phase").to_string();
    let reason = s(po, "/status/reason");
    let deleting = po
        .pointer("/metadata/deletionTimestamp")
        .is_some_and(|v| !v.is_null());
    if !reason.is_empty() {
        if deleting && reason == "NodeLost" {
            return "Unknown".into();
        }
        status = reason.to_string();
    }
    let inits = arr(po, "/spec/initContainers");
    let sidecars: HashSet<&str> = inits
        .iter()
        .filter(|c| s(c, "/restartPolicy") == "Always")
        .map(|c| s(c, "/name"))
        .collect();
    let ics = arr(po, "/status/initContainerStatuses");
    for (idx, cs) in ics.iter().enumerate() {
        let term = cs.pointer("/state/terminated");
        let wait_reason = s(cs, "/state/waiting/reason");
        let restartable = sidecars.contains(s(cs, "/name"));
        let r = if let Some(t) = term.filter(|t| !t.is_null()) {
            let code = i(t, "/exitCode");
            if code == 0 {
                continue;
            }
            let reason = s(t, "/reason");
            let sig = i(t, "/signal");
            if !reason.is_empty() {
                format!("Init:{reason}")
            } else if sig != 0 {
                format!("Init:Signal:{sig}")
            } else {
                format!("Init:ExitCode:{code}")
            }
        } else if restartable && cs.get("started").and_then(Value::as_bool).unwrap_or(false) {
            if cs.get("ready").and_then(Value::as_bool).unwrap_or(false) {
                continue;
            }
            format!("Init:{idx}/{}", inits.len())
        } else if !wait_reason.is_empty() && wait_reason != "PodInitializing" {
            format!("Init:{wait_reason}")
        } else {
            format!("Init:{idx}/{}", inits.len())
        };
        return r;
    }
    let mut running = false;
    for cs in arr(po, "/status/containerStatuses").iter().rev() {
        let wr = s(cs, "/state/waiting/reason");
        let term = cs.pointer("/state/terminated").filter(|t| !t.is_null());
        if !wr.is_empty() {
            status = wr.to_string();
        } else if let Some(t) = term {
            let tr = s(t, "/reason");
            if !tr.is_empty() {
                status = tr.to_string();
            } else if i(t, "/signal") != 0 {
                status = format!("Signal:{}", i(t, "/signal"));
            } else {
                status = format!("ExitCode:{}", i(t, "/exitCode"));
            }
        } else if cs.get("ready").and_then(Value::as_bool).unwrap_or(false)
            && cs.pointer("/state/running").is_some_and(|v| !v.is_null())
        {
            running = true;
        }
    }
    if running && status == "Completed" {
        status = "Running".into();
    }
    if !deleting {
        return status;
    }
    "Terminating".into()
}

/// model.Tree.reconcile: build the xray tree for `gvr` in `ns`.
pub(crate) async fn build_tree(
    acc: &dyn Accessor,
    gvr: &Gvr,
    ns: &str,
    labels: Option<&str>,
) -> anyhow::Result<TreeNode> {
    let ns = if is_all_namespaces(ns) { "" } else { ns }.to_string();
    let g = gvr.to_string();
    let mut b = Builder {
        acc,
        ns: ns.clone(),
        refs: Refs::default(),
    };
    b.prefetch().await?;
    let oo = acc.list(gvr, &ns, labels).await?;
    let mut root = TreeNode::new(&g, gvr.r());
    for o in &oo {
        let v = obj_json(o);
        match g.as_str() {
            PODS => b.pod(&mut root, &v),
            DEPLOYS | RSS | STSS | DSS | SVCS => b.workload(&mut root, &g, &v).await?,
            _ => anyhow::bail!("no tree renderer defined for this resource"),
        }
    }
    root.sort();
    Ok(root)
}

// ---------------------------------------------------------------------------
// Titles (xray/tree_node.go).

fn emoji(gvr: &str) -> &'static str {
    match gvr {
        NAMESPACES => "🗂 ",
        "v1/nodes" => "🖥 ",
        PODS => "🚛",
        SVCS => "💁‍♀️",
        SAS => "💳",
        "v1/persistentvolumes" => "📚",
        PVCS => "🎟 ",
        SECRETS => "🔒",
        CONFIGMAPS => "🗺 ",
        DEPLOYS => "🪂",
        STSS => "🎎",
        DSS => "😈",
        RSS => "👯‍♂️",
        "issue_0" => "👍",
        "issue_1" => "🔊",
        "issue_2" => "☣️ ",
        "issue_3" => "🧨",
        "autoscaling/v1/horizontalpodautoscalers" | "autoscaling/v2/horizontalpodautoscalers" => {
            "♎️"
        }
        "rbac.authorization.k8s.io/v1/clusterroles"
        | "rbac.authorization.k8s.io/v1/clusterrolebindings" => "👩‍",
        "rbac.authorization.k8s.io/v1/roles" | "rbac.authorization.k8s.io/v1/rolebindings" => "👨🏻‍",
        "networking.k8s.io/v1/networkpolicies" => "📕",
        "policy/v1/poddisruptionbudgets" => "🏷 ",
        CONTAINERS => "🐳",
        "report" => "🧼",
        _ => "📎",
    }
}

fn node_title(n: &TreeNode, no_icons: bool, categ: &str) -> String {
    let name = namespaced(&n.id).1;
    let (color, status) = match n.status.as_str() {
        STATUS_TOAST => ("orangered", "TOAST"),
        STATUS_NOREF => ("orange", "TOAST_REF"),
        _ => ("white", "OK"),
    };
    let mut t = if !no_icons {
        format!("{} [{color}::b]{name}[::]", emoji(&n.gvr))
    } else if categ.is_empty() {
        format!(" [white::b][{color}::b]{name}[::]")
    } else {
        format!(" [gray::-]{categ}/[white::b][{color}::b]{name}[::]")
    };
    if !n.is_leaf() {
        t += &format!("[white::d]({}[-::d])[-::-]", n.children.len());
    }
    if let Some(info) = &n.info {
        t += &format!(" [antiquewhite::][{info}][::]");
    }
    if status != "OK" {
        let sep = if no_icons { "  " } else { " " };
        t += &format!("{sep}[gray::-][yellow:{color}:b]{status}[gray::-]");
    }
    t
}

fn title_case(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

// ---------------------------------------------------------------------------
// View.

struct TreeResult {
    gen: u64,
    result: Result<TreeNode, String>,
}

struct OpenDetails {
    title: String,
    path: String,
    result: Result<String, String>,
}

/// A visible tree row.
struct TreeRow {
    spec: NodeSpec,
    depth: usize,
    /// For each ancestor depth 1..depth: whether that ancestor is a last child.
    last_chain: Vec<bool>,
    title: String,
    expanded: bool,
}

pub(crate) struct XrayView {
    id: ViewId,
    gvr: Gvr,
    ns: String,
    model: Option<TreeNode>,
    shown: Option<TreeNode>,
    count: usize,
    query: String,
    selected: String,
    expand_nodes: bool,
    expanded: HashMap<String, bool>,
    top: usize,
    height: usize,
    gen: u64,
    token: Option<CancellationToken>,
    no_icons: bool,
    refresh: Duration,
    styles: Styles,
    resources: Vec<crate::k8s::ResourceMeta>,
}

impl XrayView {
    pub(crate) fn new(ctx: &mut Ctx, gvr: Gvr, ns: String) -> Self {
        let (no_icons, rate) = ctx
            .config
            .read()
            .map(|c| (c.k9s.ui.no_icons, c.k9s.refresh_rate))
            .unwrap_or((false, 2.0));
        let ns = if is_all_namespaces(&ns) {
            String::new()
        } else {
            ns
        };
        XrayView {
            id: ctx.new_view_id(),
            gvr,
            ns,
            model: None,
            shown: None,
            count: 0,
            query: String::new(),
            selected: String::new(),
            expand_nodes: true,
            expanded: HashMap::new(),
            top: 0,
            height: 0,
            gen: 0,
            token: None,
            no_icons,
            refresh: Duration::from_secs_f32(rate.max(0.5)),
            styles: ctx.styles.clone(),
            resources: ctx.resources.clone(),
        }
    }

    fn labels(&self) -> Option<String> {
        if util::is_label_selector(&self.query) {
            let sel = self.query[2..].trim().to_string();
            if !sel.is_empty() {
                return Some(sel);
            }
        }
        None
    }

    fn start_watch(&mut self, acc: SharedAccessor, ctx: &Ctx) {
        self.stop_watch();
        self.gen += 1;
        let token = CancellationToken::new();
        self.token = Some(token.clone());
        let (gvr, ns, labels, rate, gen, view) = (
            self.gvr.clone(),
            self.ns.clone(),
            self.labels(),
            self.refresh,
            self.gen,
            self.id,
        );
        let tx = ctx.sender();
        ctx.spawn(async move {
            let mut delay = Duration::ZERO;
            loop {
                tokio::select! {
                    _ = token.cancelled() => return,
                    _ = tokio::time::sleep(delay) => {}
                }
                delay = if delay.is_zero() {
                    Duration::from_millis(500)
                } else {
                    rate
                };
                let result = build_tree(acc.as_ref(), &gvr, &ns, labels.as_deref())
                    .await
                    .map_err(|e| e.to_string());
                if token.is_cancelled() {
                    return;
                }
                let ev = AppEvent::Custom {
                    view,
                    payload: Box::new(TreeResult { gen, result }),
                };
                if tx.send(ev).is_err() {
                    return;
                }
            }
        });
    }

    fn stop_watch(&mut self) {
        if let Some(t) = self.token.take() {
            t.cancel();
        }
    }

    fn filtered(&self) -> Option<TreeNode> {
        let root = self.model.as_ref()?;
        let q = self.query.as_str();
        if q.is_empty() || util::is_label_selector(q) {
            return Some(root.clone());
        }
        if let Some(f) = util::fuzzy_selector(q) {
            let f = f.trim().to_string();
            return root.filter(|p| util::fuzzy_indices(&f, p).is_some());
        }
        let (inv, q) = match q.strip_prefix('!') {
            Some(r) => (true, r.trim()),
            None => (false, q),
        };
        let rx = util::ci_regex(q)?;
        root.filter(|p| {
            let hit = p.split(PATH_SEP).any(|t| rx.is_match(t));
            hit != inv
        })
    }

    /// Xray.update: rebuild display state.
    fn update(&mut self) {
        self.shown = self.filtered();
        self.expanded.clear();
        if let Some(root) = &self.shown {
            if self.selected.is_empty() {
                self.selected = root.id.clone();
            }
        }
    }

    fn rows(&self) -> Vec<TreeRow> {
        let mut out = Vec::new();
        let Some(root) = &self.shown else {
            return out;
        };
        let mut chain = vec![(root.gvr.clone(), root.id.clone(), root.status.clone())];
        self.walk(root, 0, &mut chain, &mut Vec::new(), &mut out);
        out
    }

    fn categ(&self, gvr: &str) -> String {
        if gvr == CONTAINERS {
            return "container".into();
        }
        self.resources
            .iter()
            .find(|m| m.gvr.to_string() == gvr)
            .map(|m| m.singular_name.clone())
            .unwrap_or_default()
    }

    fn walk(
        &self,
        n: &TreeNode,
        depth: usize,
        chain: &mut Vec<(String, String, String)>,
        lasts: &mut Vec<bool>,
        out: &mut Vec<TreeRow>,
    ) {
        let spec = NodeSpec::from_chain(chain);
        let path = spec.as_path();
        let expanded = if depth == 0 {
            true
        } else {
            self.expanded
                .get(&path)
                .copied()
                .unwrap_or(self.expand_nodes || path == self.selected)
        };
        out.push(TreeRow {
            spec,
            depth,
            last_chain: lasts.clone(),
            title: node_title(n, self.no_icons, &self.categ(&n.gvr)),
            expanded,
        });
        if !expanded {
            return;
        }
        let len = n.children.len();
        for (k, c) in n.children.iter().enumerate() {
            chain.push((c.gvr.clone(), c.id.clone(), c.status.clone()));
            lasts.push(k + 1 == len);
            self.walk(c, depth + 1, chain, lasts, out);
            lasts.pop();
            chain.pop();
        }
    }

    fn selected_index(&self, rows: &[TreeRow]) -> usize {
        rows.iter()
            .position(|r| r.spec.as_path() == self.selected)
            .unwrap_or(0)
    }

    fn selected_spec(&self) -> Option<NodeSpec> {
        let rows = self.rows();
        if rows.is_empty() {
            return None;
        }
        let i = self.selected_index(&rows);
        rows.into_iter().nth(i).map(|r| r.spec)
    }

    fn selected_node(&self) -> Option<TreeNode> {
        let spec = self.selected_spec()?;
        let mut nav = self.shown.as_ref()?;
        for k in (0..spec.paths.len().saturating_sub(1)).rev() {
            nav = nav
                .children
                .iter()
                .find(|c| c.gvr == spec.gvrs[k] && c.id == spec.paths[k])?;
        }
        Some(nav.clone())
    }

    /// Discovery metadata; containers are a k9s pseudo resource without one.
    fn meta(&self, gvr: &str) -> Option<crate::k8s::ResourceMeta> {
        self.resources
            .iter()
            .find(|m| m.gvr.to_string() == gvr)
            .cloned()
    }

    fn can(meta: &crate::k8s::ResourceMeta, verbs: &[&str]) -> bool {
        verbs.iter().any(|v| meta.can(v))
    }

    fn title_markup(&self) -> String {
        let base = format!("Xray-{}", title_case(self.gvr.r()));
        let ns = if self.ns.is_empty() {
            "all"
        } else {
            self.ns.as_str()
        };
        let count = util::as_thousands(self.count as i64);
        let t = if ns == crate::k8s::CLUSTER_SCOPE {
            format!(" [fg:bg:b]{base}[fg:bg:-][[count:bg:b]{count}[fg:bg:-]][fg:bg:-] ")
        } else {
            format!(
                " [fg:bg:b]{base}([hilite:bg:b]{ns}[fg:bg:-])[fg:bg:-][[count:bg:b]{count}[fg:bg:-]][fg:bg:-] "
            )
        };
        let mut title = util::skin_title(&t, &self.styles);
        if !self.query.is_empty() {
            title += &util::search_title(&self.query, &self.styles);
        }
        title
    }

    fn move_sel(&mut self, delta: isize) {
        let rows = self.rows();
        if rows.is_empty() {
            return;
        }
        let cur = self.selected_index(&rows) as isize;
        let next = (cur + delta).clamp(0, rows.len() as isize - 1) as usize;
        self.selected = rows[next].spec.as_path();
    }

    fn open_details(&self, ctx: &Ctx, spec: &NodeSpec, yaml: bool) {
        let acc = ctx.accessor.clone();
        let tx = ctx.sender();
        let view = self.id;
        let gvr = Gvr::parse(spec.gvr());
        let path = spec.path().to_string();
        ctx.spawn(async move {
            let (title, result) = if yaml {
                let r = acc.to_yaml(&gvr, &path, false).await.map_err(|e| {
                    format!(
                        "unable to get resource {} -- {e}",
                        util::go_quote(&gvr.to_string())
                    )
                });
                (YAML_ACTION.to_string(), r)
            } else {
                let r = acc
                    .describe(&gvr, &path)
                    .await
                    .map_err(|e| format!("Describe command failed: {e}"));
                ("Describe".to_string(), r)
            };
            let _ = tx.send(AppEvent::Custom {
                view,
                payload: Box::new(OpenDetails {
                    title,
                    path,
                    result,
                }),
            });
        });
    }

    /// Pod path + container for a pod or container node.
    fn pod_target(&self, spec: &NodeSpec) -> (String, Option<String>) {
        if spec.gvr() == CONTAINERS {
            let co = namespaced(spec.path()).1.to_string();
            return (spec.parent_path().unwrap_or_default().to_string(), Some(co));
        }
        (spec.path().to_string(), None)
    }

    fn pick_container(
        &self,
        then: impl FnOnce(String) -> Vec<ViewCmd> + Send + 'static,
    ) -> Vec<ViewCmd> {
        let node = self.selected_node();
        let cc = node
            .as_ref()
            .map(|n| n.containers.clone())
            .unwrap_or_default();
        if let Some(d) = node.as_ref().and_then(|n| n.default_container.clone()) {
            return then(d);
        }
        if cc.len() == 1 {
            return then(cc[0].clone());
        }
        vec![ViewCmd::Dialog(Dialog::Prompt {
            title: "Containers".into(),
            label: format!("Container ({}):", cc.join(", ")),
            value: cc.first().cloned().unwrap_or_default(),
            on_ok: Box::new(move |_ctx: &mut Ctx, co: String| then(co)),
        })]
    }

    fn shell(&self, ctx: &Ctx, attach: bool) -> Vec<ViewCmd> {
        let Some(spec) = self.selected_spec() else {
            return Vec::new();
        };
        if spec.status() != STATUS_OK {
            return vec![util::flash(
                FlashLevel::Err,
                format!("{} is not in a running state", spec.path()),
            )];
        }
        let (path, co) = self.pod_target(&spec);
        let acc = ctx.accessor.clone();
        let run = move |co: String| -> Vec<ViewCmd> {
            let (ns, pod) = namespaced(&path);
            let argv = if attach {
                let mut a: Vec<String> = vec!["kubectl".into(), "attach".into(), "-it".into()];
                if !ns.is_empty() {
                    a.extend(["-n".into(), ns.to_string()]);
                }
                a.push(pod.to_string());
                if !co.is_empty() {
                    a.extend(["-c".into(), co]);
                }
                a
            } else {
                acc.exec_argv(ns, pod, &co, "sh")
            };
            let what = if attach { "Attach" } else { "Shell" };
            vec![ViewCmd::Suspend {
                argv,
                env: acc.shell_env(),
                on_done: Some(Box::new(
                    move |_ctx: &mut Ctx, st: std::process::ExitStatus| {
                        if st.success() {
                            Vec::new()
                        } else {
                            vec![util::flash(
                                FlashLevel::Err,
                                format!("{what} exec failed: {st}"),
                            )]
                        }
                    },
                )),
            }]
        };
        match co {
            Some(c) => run(c),
            None => self.pick_container(run),
        }
    }

    fn logs(&self, ctx: &mut Ctx, prev: bool) -> Vec<ViewCmd> {
        let Some(spec) = self.selected_spec() else {
            return Vec::new();
        };
        let (path, co) = self.pod_target(&spec);
        let (ns, pod) = namespaced(&path);
        let tail = ctx.config.read().map(|c| c.k9s.logger.tail).unwrap_or(100);
        let opts = LogOptions {
            namespace: ns.to_string(),
            path: pod.to_string(),
            container: co,
            previous: prev,
            lines: tail,
            ..Default::default()
        };
        vec![ViewCmd::Push(Box::new(LogsView::new(ctx, opts)))]
    }

    fn delete(&self, spec: &NodeSpec) -> Vec<ViewCmd> {
        let Some(meta) = self.meta(spec.gvr()) else {
            return Vec::new();
        };
        let gvr = Gvr::parse(spec.gvr());
        let path = spec.path().to_string();
        let own = self.gvr.to_string();
        let view = self.id;
        let msg = format!("Delete {} {}?", meta.singular_name, path);
        vec![ViewCmd::Dialog(Dialog::Delete {
            msg,
            on_ok: Box::new(move |ctx: &mut Ctx, opts: crate::k8s::DeleteOptions| {
                let acc = ctx.accessor.clone();
                let tx = ctx.sender();
                let (g, p) = (gvr.clone(), path.clone());
                ctx.spawn(async move {
                    let ev = match acc.delete(&g, &p, opts).await {
                        Ok(()) => AppEvent::Info {
                            view,
                            msg: format!("{own} `{p} deleted successfully"),
                        },
                        Err(e) => AppEvent::Error {
                            view,
                            msg: format!("Delete failed with `{e}"),
                        },
                    };
                    let _ = tx.send(ev);
                });
                vec![util::flash(
                    FlashLevel::Info,
                    format!("Delete resource {gvr} {path}"),
                )]
            }),
        })]
    }
}

impl View for XrayView {
    fn owns_frame(&self) -> bool {
        true
    }
    fn id(&self) -> ViewId {
        self.id
    }

    fn name(&self) -> String {
        "XRay".into()
    }

    fn title(&self) -> String {
        util::plain(&util::markup_line(
            &self.title_markup(),
            util::title_base(&self.styles),
        ))
    }

    fn hints(&self) -> Vec<Hint> {
        let mut hh = vec![
            Hint::new("space", "Expand/Collapse"),
            Hint::new("x", "Expand/Collapse All"),
        ];
        let spec = self.selected_spec();
        let g = spec
            .as_ref()
            .map(|s| s.gvr().to_string())
            .unwrap_or_default();
        if g != NAMESPACES && g != CONTAINERS {
            hh.push(Hint::new("enter", "Goto"));
        }
        if spec.is_some() {
            if let Some(m) = &self.meta(&g) {
                if Self::can(m, &["patch", "update"]) {
                    hh.push(Hint::new("e", "Edit"));
                }
                if Self::can(m, &["delete"]) {
                    hh.push(Hint::new("ctrl-d", "Delete"));
                }
                hh.push(Hint::new("y", YAML_ACTION));
                hh.push(Hint::new("d", "Describe"));
            }
            match g.as_str() {
                CONTAINERS => hh.extend([
                    Hint::new("s", "Shell"),
                    Hint::new("l", "Logs"),
                    Hint::new("p", "Logs Previous"),
                ]),
                PODS => hh.extend([
                    Hint::new("s", "Shell"),
                    Hint::new("a", "Attach"),
                    Hint::new("l", "Logs"),
                    Hint::new("p", "Logs Previous"),
                ]),
                _ => {}
            }
        }
        util::sort_hints(&mut hh);
        hh
    }

    fn start(&mut self, ctx: &mut Ctx) -> Vec<ViewCmd> {
        self.styles = ctx.styles.clone();
        self.resources = ctx.resources.clone();
        let acc = ctx.accessor.clone();
        self.start_watch(acc, ctx);
        Vec::new()
    }

    fn stop(&mut self, _ctx: &mut Ctx) {
        self.stop_watch();
    }

    fn handle_key(&mut self, key: KeyEvent, ctx: &mut Ctx) -> Vec<ViewCmd> {
        self.resources = ctx.resources.clone();
        let k = util::key_label(&key);
        let spec = self.selected_spec();
        let g = spec
            .as_ref()
            .map(|s| s.gvr().to_string())
            .unwrap_or_default();
        let meta = self.meta(&g);
        match k.as_str() {
            "/" => vec![ViewCmd::OpenFilterPrompt],
            "esc" => vec![ViewCmd::Pop],
            "j" | "down" => {
                self.move_sel(1);
                Vec::new()
            }
            "k" | "up" => {
                self.move_sel(-1);
                Vec::new()
            }
            "g" | "home" => {
                self.move_sel(isize::MIN / 2);
                Vec::new()
            }
            "shift-g" | "end" => {
                self.move_sel(isize::MAX / 2);
                Vec::new()
            }
            "ctrl-f" | "pgdn" => {
                self.move_sel(self.height.max(1) as isize);
                Vec::new()
            }
            "ctrl-b" | "pgup" => {
                self.move_sel(-(self.height.max(1) as isize));
                Vec::new()
            }
            "space" => {
                if let Some(s) = &spec {
                    let rows = self.rows();
                    let idx = self.selected_index(&rows);
                    if let Some(r) = rows.get(idx) {
                        self.expanded.insert(s.as_path(), !r.expanded);
                    }
                }
                Vec::new()
            }
            "x" => {
                self.expand_nodes = !self.expand_nodes;
                self.expanded.clear();
                Vec::new()
            }
            "enter" => {
                let Some(s) = spec else {
                    return Vec::new();
                };
                if g == NAMESPACES || g == CONTAINERS || s.path().split('/').count() == 1 {
                    return Vec::new();
                }
                let ns = namespaced(s.path()).0;
                let ns = if ns.is_empty() { "all" } else { ns };
                vec![ViewCmd::Command(format!("{} {ns}", s.gvr()))]
            }
            "e" if meta
                .as_ref()
                .is_some_and(|m| Self::can(m, &["patch", "update"])) =>
            {
                let s = spec.expect("spec");
                let argv = ctx.accessor.edit_argv(&Gvr::parse(s.gvr()), s.path());
                vec![ViewCmd::Suspend {
                    argv,
                    env: ctx.accessor.shell_env(),
                    on_done: Some(Box::new(|_ctx: &mut Ctx, st: std::process::ExitStatus| {
                        if st.success() {
                            Vec::new()
                        } else {
                            vec![util::flash(
                                FlashLevel::Err,
                                format!("Edit exec failed: {st}"),
                            )]
                        }
                    })),
                }]
            }
            "ctrl-d" if meta.as_ref().is_some_and(|m| Self::can(m, &["delete"])) => {
                self.delete(&spec.expect("spec"))
            }
            "y" | "d" if meta.is_some() => {
                self.open_details(ctx, &spec.expect("spec"), k == "y");
                Vec::new()
            }
            "s" if g == PODS || g == CONTAINERS => self.shell(ctx, false),
            "a" if g == PODS => self.shell(ctx, true),
            "l" if g == PODS || g == CONTAINERS => self.logs(ctx, false),
            "p" if g == PODS || g == CONTAINERS => self.logs(ctx, true),
            _ => Vec::new(),
        }
    }

    fn on_event(&mut self, ev: &AppEvent, ctx: &mut Ctx) -> Vec<ViewCmd> {
        match ev {
            AppEvent::Custom { view, payload } if *view == self.id => {
                if let Some(r) = payload.downcast_ref::<TreeResult>() {
                    if r.gen != self.gen {
                        return Vec::new();
                    }
                    return match &r.result {
                        Err(e) => vec![util::flash(FlashLevel::Err, e.clone())],
                        Ok(root) => {
                            if self.model.as_ref() != Some(root) {
                                self.count = root.count(&self.gvr.to_string());
                                self.model = Some(root.clone());
                                self.update();
                            }
                            Vec::new()
                        }
                    };
                }
                if let Some(o) = payload.downcast_ref::<OpenDetails>() {
                    return match &o.result {
                        Err(e) => vec![util::flash(FlashLevel::Err, e.clone())],
                        Ok(text) => vec![ViewCmd::Push(Box::new(DetailsView::fixed(
                            ctx,
                            &o.title,
                            o.path.clone(),
                            true,
                            true,
                            text,
                        )))],
                    };
                }
                Vec::new()
            }
            AppEvent::Info { view, msg } if *view == self.id => {
                vec![util::flash(FlashLevel::Info, msg.clone())]
            }
            AppEvent::Error { view, msg } if *view == self.id => {
                vec![util::flash(FlashLevel::Err, msg.clone())]
            }
            _ => Vec::new(),
        }
    }

    fn on_filter(&mut self, text: &str, ctx: &mut Ctx) -> Vec<ViewCmd> {
        let was_labels = self.labels();
        self.query = text.to_string();
        self.selected.clear();
        if self.labels() != was_labels {
            let acc = ctx.accessor.clone();
            self.start_watch(acc, ctx);
        }
        self.update();
        Vec::new()
    }

    fn draw(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &Ctx) {
        self.styles = ctx.styles.clone();
        self.resources = ctx.resources.clone();
        let s = self.styles.clone();
        let buf = frame.buffer_mut();
        let bg = Style::default().fg(s.xray_fg).bg(s.xray_bg);
        let title = util::markup_line(&self.title_markup(), util::title_base(&s));
        let border = bg.fg(s.border_focus).add_modifier(Modifier::BOLD);
        let inner = util::pad_x(util::draw_box(buf, area, &title, border, bg), 1);
        self.height = inner.height as usize;
        if inner.height == 0 || inner.width == 0 {
            return;
        }
        let rows = self.rows();
        let node_style = Style::default().fg(s.xray_cursor).bg(s.xray_bg);
        if rows.is_empty() {
            let line = vec![Seg::new(
                "No data...",
                node_style.bg(s.xray_cursor).fg(s.xray_cursor_text),
            )];
            util::put_line(buf, inner.x, inner.y, &line, inner.width);
            return;
        }
        let sel = self.selected_index(&rows);
        let h = self.height;
        if sel < self.top {
            self.top = sel;
        } else if sel >= self.top + h {
            self.top = sel + 1 - h;
        }
        if self.top + h > rows.len() {
            self.top = rows.len().saturating_sub(h);
        }
        let gfx = Style::default().fg(s.xray_graphic).bg(s.xray_bg);
        for (k, r) in rows.iter().enumerate().skip(self.top).take(h) {
            let y = inner.y + (k - self.top) as u16;
            if r.depth > 0 {
                for (a, last) in r.last_chain.iter().enumerate().take(r.depth - 1) {
                    if !last {
                        let x = inner.x + (a * 3) as u16;
                        if x < inner.right() {
                            buf.set_string(x, y, "│", gfx);
                        }
                    }
                }
                let gx = inner.x + ((r.depth - 1) * 3) as u16;
                let glyph = if *r.last_chain.last().unwrap_or(&true) {
                    "└──"
                } else {
                    "├──"
                };
                if gx < inner.right() {
                    let w = (inner.right() - gx) as usize;
                    buf.set_stringn(gx, y, glyph, w, gfx);
                }
            }
            let tx = inner.x + (r.depth * 3) as u16;
            if tx >= inner.right() {
                continue;
            }
            let base = if k == sel {
                node_style.bg(s.xray_cursor).fg(s.xray_cursor_text)
            } else {
                node_style
            };
            let mut line: SLine = util::markup_line(&r.title, base);
            if k == sel {
                for seg in &mut line {
                    seg.style.bg = Some(s.xray_cursor);
                }
            }
            util::put_line(buf, tx, y, &line, inner.right() - tx);
        }
    }

    fn filter_text(&self) -> String {
        self.query.clone()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::k8s::fake::{obj, FakeAccessor};
    use crossterm::event::{KeyCode, KeyModifiers};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use serde_json::json;
    use std::sync::{Arc, RwLock};
    use tokio::sync::mpsc;

    fn cluster() -> FakeAccessor {
        let f = FakeAccessor::new();
        f.add(
            DEPLOYS,
            obj(json!({"apiVersion":"apps/v1","kind":"Deployment",
            "metadata":{"name":"web","namespace":"default"},
            "spec":{"replicas":2,"selector":{"matchLabels":{"app":"web"}}},
            "status":{"availableReplicas":1,"unavailableReplicas":1}})),
        );
        f.add(
            DEPLOYS,
            obj(json!({"apiVersion":"apps/v1","kind":"Deployment",
            "metadata":{"name":"empty","namespace":"default"},
            "spec":{"replicas":1,"selector":{"matchLabels":{"app":"none"}}}})),
        );
        for (n, ready) in [("web-2", true), ("web-10", false)] {
            f.add(PODS, obj(json!({"apiVersion":"v1","kind":"Pod",
                "metadata":{"name":n,"namespace":"default","labels":{"app":"web"}},
                "spec":{"serviceAccountName":"builder",
                    "containers":[{"name":"nginx","env":[{"name":"P","valueFrom":{"secretKeyRef":{"name":"creds","key":"p"}}}]}],
                    "volumes":[{"name":"c","configMap":{"name":"cfg"}},{"name":"d","persistentVolumeClaim":{"claimName":"data"}}]},
                "status":{"phase":"Running","containerStatuses":[{"name":"nginx","ready":ready,"state":{"running":{}}}]}})));
        }
        f.add(SECRETS, obj(json!({"apiVersion":"v1","kind":"Secret","metadata":{"name":"creds","namespace":"default"}})));
        f.add(CONFIGMAPS, obj(json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"cfg","namespace":"default"}})));
        f.add(SAS, obj(json!({"apiVersion":"v1","kind":"ServiceAccount","metadata":{"name":"builder","namespace":"default"},"automountServiceAccountToken":false})));
        f
    }

    fn ctx(f: FakeAccessor) -> (Ctx, mpsc::UnboundedReceiver<AppEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut c = Ctx::new(
            Arc::new(f),
            Arc::new(RwLock::new(crate::config::Config::default())),
            Styles::default(),
            "default".into(),
            tx,
        );
        c.resources = crate::k8s::fake::default_resources();
        (c, rx)
    }

    #[tokio::test]
    async fn builds_deployment_tree() {
        let f = cluster();
        let root = build_tree(&f, &Gvr::parse(DEPLOYS), "default", None)
            .await
            .unwrap();
        assert_eq!(root.id, "deployments");
        assert_eq!(root.children.len(), 1);
        let ns = &root.children[0];
        assert_eq!((ns.gvr.as_str(), ns.id.as_str()), (NAMESPACES, "-/default"));
        let dp = &ns.children[0];
        assert_eq!(dp.id, "default/web");
        assert_eq!(dp.status, STATUS_TOAST);
        assert_eq!(dp.info.as_deref(), Some("1/2/1"));
        // deploy -> ns -> pods (natural order: web-2 before web-10).
        let pods = &dp.children[0].children;
        assert_eq!(pods[0].id, "default/web-2");
        assert_eq!(pods[1].id, "default/web-10");
        assert_eq!(pods[0].status, STATUS_OK);
        assert_eq!(pods[1].status, STATUS_TOAST);
        assert_eq!(pods[0].info.as_deref(), Some("1/1"));
        let kids: Vec<(&str, &str, &str)> = pods[0]
            .children
            .iter()
            .map(|c| (c.gvr.as_str(), c.id.as_str(), c.status.as_str()))
            .collect();
        assert!(kids.contains(&(CONFIGMAPS, "default/cfg", STATUS_OK)));
        assert!(kids.contains(&(PVCS, "default/data", STATUS_NOREF)));
        assert!(kids.contains(&(CONTAINERS, "default/nginx", STATUS_OK)));
        let sa = pods[0].children.iter().find(|c| c.gvr == SAS).unwrap();
        assert_eq!(sa.info.as_deref(), Some("automount=false"));
        let co = pods[0]
            .children
            .iter()
            .find(|c| c.gvr == CONTAINERS)
            .unwrap();
        assert_eq!(co.children[0].id, "default/creds");
        assert_eq!(root.count(DEPLOYS), 2);
    }

    #[test]
    fn filter_and_hydrate() {
        let mut root = TreeNode::new(PODS, "pods");
        let mut ns = TreeNode::new(NAMESPACES, "-/default");
        let mut p1 = TreeNode::new(PODS, "default/a");
        p1.children.push(TreeNode::new(CONTAINERS, "default/c1"));
        let mut p2 = TreeNode::new(PODS, "default/b");
        p2.children.push(TreeNode::new(CONTAINERS, "default/c2"));
        ns.children.extend([p1, p2]);
        root.children.push(ns);
        let f = root.filter(|p| p.contains("c2")).unwrap();
        assert_eq!(f.children[0].children.len(), 1);
        assert_eq!(f.children[0].children[0].id, "default/b");
        assert!(root.filter(|p| p.contains("zzz")).is_none());
    }

    #[test]
    fn titles() {
        let mut n = TreeNode::new(DEPLOYS, "default/web");
        n.status = STATUS_TOAST.into();
        n.info = Some("1/2/1".into());
        n.children.push(TreeNode::new(PODS, "default/p"));
        let t = node_title(&n, false, "");
        assert_eq!(
            t,
            "🪂 [orangered::b]web[::][white::d](1[-::d])[-::-] [antiquewhite::][1/2/1][::] [gray::-][yellow:orangered:b]TOAST[gray::-]"
        );
        let l = util::markup_line(&t, Style::default());
        assert_eq!(util::plain(&l), "🪂 web(1) [1/2/1] TOAST");
        let toast = l.iter().find(|s| s.text == "TOAST").unwrap();
        assert_eq!(toast.style.bg, util::color_by_name("orangered"));
        let n = TreeNode::new(SECRETS, "default/s");
        assert_eq!(
            node_title(&n, true, "secret"),
            " [gray::-]secret/[white::b][white::b]s[::]"
        );
        assert_eq!(node_title(&n, true, ""), " [white::b][white::b]s[::]");
    }

    #[test]
    fn phases() {
        let po = json!({"status":{"phase":"Running","containerStatuses":[{"ready":false,"state":{"terminated":{"reason":"Completed","exitCode":0}}}]}});
        assert_eq!(pod_phase(&po), "Completed");
        assert_eq!(pod_status(&po).0, STATUS_COMPLETED);
        let po = json!({"spec":{"initContainers":[{"name":"i"}]},"status":{"phase":"Pending","initContainerStatuses":[{"name":"i","state":{"waiting":{"reason":"PodInitializing"}}}]}});
        assert_eq!(pod_phase(&po), "Init:0/1");
        let po = json!({"metadata":{"deletionTimestamp":"x"},"status":{"phase":"Running"}});
        assert_eq!(pod_phase(&po), "Terminating");
    }

    #[tokio::test]
    async fn view_renders_navigates_and_acts() {
        let (mut c, mut rx) = ctx(cluster());
        let mut v = XrayView::new(&mut c, Gvr::parse(DEPLOYS), "default".into());
        v.start(&mut c);
        let ev = rx.recv().await.unwrap();
        v.on_event(&ev, &mut c);
        assert_eq!(v.title(), " Xray-Deployments(default)[2] ");
        let mut t = Terminal::new(TestBackend::new(70, 16)).unwrap();
        t.draw(|f| v.draw(f, f.area(), &c)).unwrap();
        let b = t.backend().buffer().clone();
        // Wide emoji cells are followed by a blank continuation cell.
        let row = |y: u16| -> String {
            (0..70)
                .map(|x| b[(x, y)].symbol())
                .collect::<String>()
                .replace("  ", " ")
        };
        assert!(row(0).contains(" Xray-Deployments(default)[2] "));
        assert!(row(1).contains("🪂 deployments(1)"), "{}", row(1));
        assert!(row(2).contains("└──🗂 default(1)"), "{}", row(2));
        assert!(row(3).contains("└──🪂 web(1) [1/2/1] TOAST"), "{}", row(3));
        assert_eq!(b[(2, 2)].fg, c.styles.xray_graphic);
        // selected root row has cursor background.
        assert_eq!(b[(4, 1)].bg, c.styles.xray_cursor);
        // navigate to the deployment and goto.
        v.handle_key(
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
            &mut c,
        );
        v.handle_key(
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
            &mut c,
        );
        let cmds = v.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &mut c);
        assert!(matches!(&cmds[0], ViewCmd::Command(s) if s == "apps/v1/deployments default"));
        // collapse it.
        v.handle_key(
            KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
            &mut c,
        );
        assert_eq!(v.rows().len(), 3);
        v.handle_key(
            KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
            &mut c,
        );
        // go to a pod and ask for logs.
        for _ in 0..2 {
            v.handle_key(
                KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
                &mut c,
            );
        }
        assert_eq!(v.selected_spec().unwrap().path(), "default/web-2");
        let cmds = v.handle_key(
            KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE),
            &mut c,
        );
        match &cmds[0] {
            ViewCmd::Push(view) => assert_eq!(view.title(), " Logs(default/web-2)[tail] "),
            _ => panic!("expected logs push"),
        }
        assert!(v.hints().iter().any(|h| h.description == "Attach"));
        let cmds = v.handle_key(
            KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
            &mut c,
        );
        assert!(
            matches!(&cmds[0], ViewCmd::Suspend { argv, .. } if argv.contains(&"nginx".to_string()))
        );
        // yaml opens a details view asynchronously.
        v.handle_key(
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE),
            &mut c,
        );
        let ev = loop {
            let ev = rx.recv().await.unwrap();
            if matches!(&ev, AppEvent::Custom { payload, .. } if payload.is::<OpenDetails>()) {
                break ev;
            }
        };
        let cmds = v.on_event(&ev, &mut c);
        match &cmds[0] {
            ViewCmd::Push(view) => assert_eq!(view.title(), " YAML(default/web-2) "),
            _ => panic!("expected yaml push"),
        }
        // filter keeps matching branches only.
        v.on_filter("web-10", &mut c);
        assert!(v.title().ends_with("</web-10> "));
        let paths: Vec<String> = v.rows().iter().map(|r| r.spec.path().to_string()).collect();
        assert!(paths.contains(&"default/web-10".to_string()));
        assert!(!paths.contains(&"default/web-2".to_string()));
    }

    #[tokio::test]
    async fn delete_dialog() {
        let (mut c, mut rx) = ctx(cluster());
        let mut v = XrayView::new(&mut c, Gvr::parse(PODS), "default".into());
        v.start(&mut c);
        let ev = rx.recv().await.unwrap();
        v.on_event(&ev, &mut c);
        assert_eq!(v.title(), " Xray-Pods(default)[3] ");
        for _ in 0..2 {
            v.handle_key(
                KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
                &mut c,
            );
        }
        let cmds = v.handle_key(
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
            &mut c,
        );
        let ViewCmd::Dialog(Dialog::Delete { msg, on_ok }) = cmds.into_iter().next().unwrap()
        else {
            panic!("expected delete dialog");
        };
        assert_eq!(msg, "Delete pod default/web-2?");
        let out = on_ok(&mut c, Default::default());
        assert!(
            matches!(&out[0], ViewCmd::Flash(FlashLevel::Info, m) if m == "Delete resource v1/pods default/web-2")
        );
        let ev = loop {
            let ev = rx.recv().await.unwrap();
            if matches!(ev, AppEvent::Info { .. }) {
                break ev;
            }
        };
        let out = v.on_event(&ev, &mut c);
        assert!(
            matches!(&out[0], ViewCmd::Flash(FlashLevel::Info, m) if m == "v1/pods `default/web-2 deleted successfully")
        );
    }
}
