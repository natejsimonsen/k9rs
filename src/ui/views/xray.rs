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
    /// Node identity: path plus its rank among same-id siblings (a pod's
    /// container and service account often share an id, hence a path).
    key: String,
}

pub(crate) struct XrayView {
    id: ViewId,
    gvr: Gvr,
    ns: String,
    model: Option<TreeNode>,
    shown: Option<TreeNode>,
    count: usize,
    query: String,
    /// ui.Tree selectedItem: set on selection changes, kept when the node
    /// disappears.
    selected: String,
    /// tview currentNode (row key); falls back to the root when not visible.
    cur: String,
    /// Set on rebuild: the current node becomes the last node whose path is
    /// `selected` (update walks the whole tree calling SetCurrentNode).
    cur_from_selected: bool,
    /// Rebuilt trees expand every node whose path is the selected item.
    force_open: String,
    /// GVR of the node the key actions were last bound for (refreshActions
    /// runs on Init with no node, then on each selection change).
    act: Option<String>,
    /// `act` as of the last redraw: k9s hydrates the menu while drawing the
    /// tree, after the menu itself was drawn, so it lags one redraw behind.
    menu_act: Option<String>,
    /// Plugin hints bound by the last refreshActions, and as last drawn.
    act_plugins: Vec<Hint>,
    menu_plugins: Vec<Hint>,
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
    /// gvr -> singular name (node categories).
    categs: HashMap<String, String>,
    /// Visible rows; rebuilt only when the tree or expansions change.
    rows_cache: std::sync::Mutex<Option<std::sync::Arc<Vec<TreeRow>>>>,
}

impl XrayView {
    pub(crate) fn new(ctx: &mut Ctx, gvr: Gvr, ns: String) -> Self {
        let (no_icons, rate) = ctx
            .config
            .read()
            .map(|c| (c.k9s.ui.no_icons, c.k9s.get_refresh_rate()))
            .unwrap_or((false, 2.0));
        let ns = if is_all_namespaces(&ns) {
            String::new()
        } else {
            ns
        };
        let mut v = XrayView {
            id: ctx.new_view_id(),
            gvr,
            ns,
            model: None,
            shown: None,
            count: 0,
            query: String::new(),
            selected: String::new(),
            cur: String::new(),
            cur_from_selected: false,
            force_open: String::new(),
            act: None,
            menu_act: None,
            act_plugins: Vec::new(),
            menu_plugins: Vec::new(),
            expand_nodes: true,
            expanded: HashMap::new(),
            top: 0,
            height: 0,
            gen: 0,
            token: None,
            no_icons,
            refresh: crate::config::k9s::secs(rate.max(0.5) as f64),
            styles: ctx.styles.clone(),
            resources: Vec::new(),
            categs: HashMap::new(),
            rows_cache: Default::default(),
        };
        v.set_resources(&ctx.resources);
        v
    }

    fn set_resources(&mut self, rr: &[crate::k8s::ResourceMeta]) {
        if self.resources == rr {
            return;
        }
        self.resources = rr.to_vec();
        self.categs = rr
            .iter()
            .map(|m| (m.gvr.to_string(), m.singular_name.clone()))
            .collect();
        self.invalidate();
    }

    fn invalidate(&self) {
        *self.rows_cache.lock().expect("rows cache") = None;
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
        self.invalidate();
        // Every rebuild resets expansions to ExpandNodes, except the
        // selected item which is expanded and made current.
        self.expanded.clear();
        if let Some(root) = &self.shown {
            if self.selected.is_empty() {
                self.selected = root.id.clone();
            }
            self.force_open = self.selected.clone();
            self.cur_from_selected = true;
        }
    }

    fn rows(&self) -> std::sync::Arc<Vec<TreeRow>> {
        let mut cache = self.rows_cache.lock().expect("rows cache");
        if let Some(rows) = cache.as_ref() {
            return rows.clone();
        }
        let mut out = Vec::new();
        if let Some(root) = &self.shown {
            let mut chain = vec![(root.gvr.clone(), root.id.clone(), root.status.clone())];
            self.walk(root, 0, 0, &mut chain, &mut Vec::new(), &mut out);
        }
        let rows = std::sync::Arc::new(out);
        *cache = Some(rows.clone());
        rows
    }

    fn categ(&self, gvr: &str) -> String {
        if gvr == CONTAINERS {
            return "container".into();
        }
        self.categs.get(gvr).cloned().unwrap_or_default()
    }

    fn walk(
        &self,
        n: &TreeNode,
        depth: usize,
        nth: usize,
        chain: &mut Vec<(String, String, String)>,
        lasts: &mut Vec<bool>,
        out: &mut Vec<TreeRow>,
    ) {
        let spec = NodeSpec::from_chain(chain);
        let path = spec.as_path();
        let key = format!("{path}#{nth}");
        let expanded = self
            .expanded
            .get(&key)
            .copied()
            .unwrap_or(depth == 0 || self.expand_nodes || path == self.force_open);
        out.push(TreeRow {
            spec,
            depth,
            last_chain: lasts.clone(),
            title: node_title(n, self.no_icons, &self.categ(&n.gvr)),
            expanded,
            key,
        });
        if !expanded {
            return;
        }
        let len = n.children.len();
        for (k, c) in n.children.iter().enumerate() {
            chain.push((c.gvr.clone(), c.id.clone(), c.status.clone()));
            lasts.push(k + 1 == len);
            let nth = n.children[..k].iter().filter(|o| o.id == c.id).count();
            self.walk(c, depth + 1, nth, chain, lasts, out);
            lasts.pop();
            chain.pop();
        }
    }

    fn selected_index(&self, rows: &[TreeRow]) -> usize {
        rows.iter().position(|r| r.key == self.cur).unwrap_or(0)
    }

    fn selected_spec(&self) -> Option<NodeSpec> {
        let rows = self.rows();
        if rows.is_empty() {
            return None;
        }
        let i = self.selected_index(&rows);
        rows.get(i).map(|r| r.spec.clone())
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

    /// pluginActions scope: x.meta is the xray resource until a node is
    /// selected, then the selected node's resource.
    fn plugin_aliases(&self, ctx: &Ctx) -> Vec<String> {
        let g = self.act.clone().unwrap_or_else(|| self.gvr.to_string());
        crate::ui::browser::scope_aliases(ctx, &Gvr::parse(&g))
    }

    fn refresh_plugins(&mut self, ctx: &Ctx) {
        // Hotkeys are shared actions: KeyActions.Hints leaves them out.
        self.act_plugins = crate::ui::shell::plugin_hints(ctx, &self.plugin_aliases(ctx));
    }

    /// Xray.k9sEnv selection for plugins.
    fn plugin_selection(&self, spec: &NodeSpec) -> crate::ui::shell::Selection {
        let mut sel = crate::ui::shell::Selection {
            path: spec.path().to_string(),
            ..Default::default()
        };
        let (_, name) = namespaced(spec.path());
        sel.filter = Some(if self.query.is_empty() {
            name.to_string()
        } else {
            self.query.clone()
        });
        if spec.gvr() == CONTAINERS {
            sel.container = Some(name.to_string());
            if let Some(pp) = spec.parent_path() {
                sel.pod = Some(namespaced(pp).1.to_string());
            }
        }
        sel
    }

    /// refreshActions node bindings for resource `act` (None before any
    /// selection: nothing bound).
    fn act_hints(&self, act: Option<&str>) -> Vec<Hint> {
        let mut hh = Vec::new();
        let Some(g) = act else {
            return hh;
        };
        if let Some(m) = &self.meta(g) {
            if Self::can(m, &["patch", "update"]) {
                hh.push(Hint::new("e", "Edit"));
            }
            if Self::can(m, &["delete"]) {
                hh.push(Hint::new("ctrl-d", "Delete"));
            }
            hh.push(Hint::new("y", YAML_ACTION));
            hh.push(Hint::new("d", "Describe"));
        }
        match g {
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
        hh
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
        self.select(&rows, next);
    }

    /// tview TreeView treeParent/treeChild movements (K/J).
    fn move_tree(&mut self, child: bool) {
        let rows = self.rows();
        if rows.is_empty() {
            return;
        }
        let cur = self.selected_index(&rows);
        let d = rows[cur].depth;
        let next = if child {
            rows.get(cur + 1)
                .filter(|r| r.depth == d + 1)
                .map(|_| cur + 1)
        } else {
            rows[..cur].iter().rposition(|r| r.depth + 1 == d)
        };
        if let Some(n) = next {
            self.select(&rows, n);
        }
    }

    /// tview fires the changed callback (Xray refreshActions) only when the
    /// selected index moves.
    fn select(&mut self, rows: &[TreeRow], next: usize) {
        if next == self.selected_index(rows) {
            return;
        }
        self.cur = rows[next].key.clone();
        self.selected = rows[next].spec.as_path();
        self.act = Some(rows[next].spec.gvr().to_string());
    }

    /// tview process: a current node that is no longer visible silently
    /// becomes the first node (no changed callback).
    fn normalize(&mut self) {
        let rows = self.rows();
        let Some(first) = rows.first() else {
            return;
        };
        if std::mem::take(&mut self.cur_from_selected) {
            if let Some(r) = rows
                .iter()
                .rev()
                .find(|r| r.spec.as_path() == self.selected)
            {
                self.cur = r.key.clone();
            }
        }
        if !rows.iter().any(|r| r.key == self.cur) {
            self.cur = first.key.clone();
        }
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
        ctx: &mut Ctx,
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
        // pod.go containerShellIn/containerAttachIn: the Containers Picker.
        vec![ViewCmd::Push(crate::ui::browser::containers_picker(
            ctx,
            cc,
            Box::new(move |_ctx, _, co| then(co)),
        ))]
    }

    fn shell(&self, ctx: &mut Ctx, attach: bool) -> Vec<ViewCmd> {
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
            let banner = crate::ui::browser::shell_banner(&path, &co);
            let argv = if attach {
                let mut a: Vec<String> = vec!["kubectl".into(), "attach".into(), "-it".into()];
                if !ns.is_empty() {
                    a.extend(["-n".into(), ns.to_string()]);
                }
                a.push(pod.to_string());
                if !co.is_empty() {
                    a.extend(["-c".into(), co.clone()]);
                }
                a
            } else {
                acc.exec_argv(ns, pod, &co, "sh")
            };
            let what = if attach { "Attach" } else { "Shell" };
            let command = match argv.iter().position(|a| a == "--") {
                Some(i) if !attach => argv[i + 1..].to_vec(),
                _ => Vec::new(),
            };
            vec![ViewCmd::Exec {
                ns: ns.to_string(),
                pod: pod.to_string(),
                container: co.clone(),
                command,
                attach,
                banner,
                label: what.into(),
                fallback: argv,
                env: acc.shell_env(),
                on_done: Some(Box::new(
                    move |_ctx: &mut Ctx, st: std::process::ExitStatus| {
                        match crate::ui::browser::exec_failure(&st) {
                            Some(e) => vec![util::flash(
                                FlashLevel::Err,
                                format!("{what} exec failed: {e}"),
                            )],
                            None => Vec::new(),
                        }
                    },
                )),
            }]
        };
        match co {
            Some(c) => run(c),
            None => self.pick_container(ctx, run),
        }
    }

    fn logs(&self, ctx: &mut Ctx, prev: bool) -> Vec<ViewCmd> {
        let Some(spec) = self.selected_spec() else {
            return Vec::new();
        };
        let (path, co) = self.pod_target(&spec);
        let (ns, pod) = namespaced(&path);
        // xray.go showLogs: CanForResource(ns, pods, list) first.
        let (acc, pods, nsc) = (ctx.accessor.clone(), Gvr::parse(PODS), ns.to_string());
        let can = crate::ui::browser::block_on(async move { acc.can(&pods, &nsc, "list").await });
        match can {
            Ok(true) => {}
            Ok(false) => {
                let msg = crate::ui::browser::access_denied(ns, &Gvr::parse(PODS));
                return vec![util::flash(FlashLevel::Err, msg)];
            }
            Err(e) => return vec![util::flash(FlashLevel::Err, e.to_string())],
        }
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
    /// Xray.ExtraHints: xray.EmojiInfo (resource -> icon) unless noIcons.
    fn extra_hints(&self, _ctx: &Ctx) -> Vec<(String, String)> {
        if self.no_icons {
            return Vec::new();
        }
        let gvrs = [
            CONTAINERS,
            NAMESPACES,
            PODS,
            SVCS,
            SAS,
            "v1/persistentvolumes",
            PVCS,
            SECRETS,
            CONFIGMAPS,
            DEPLOYS,
            STSS,
            DSS,
        ];
        let mut out: Vec<(String, String)> = gvrs
            .iter()
            .map(|g| (Gvr::parse(g).r().to_string(), emoji(g).to_string()))
            .collect();
        out.sort();
        out
    }

    /// Xray.Init: SetBorderColor(Xray().FgColor).
    fn blur_border(&self, styles: &Styles) -> ratatui::style::Color {
        styles.xray_fg
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
        let g = self.menu_act.clone().unwrap_or_default();
        if g != NAMESPACES && g != CONTAINERS {
            hh.push(Hint::new("enter", "Goto"));
        }
        hh.extend(self.act_hints(self.menu_act.as_deref()));
        hh.extend(self.menu_plugins.iter().cloned());
        util::sort_hints(&mut hh);
        hh
    }

    fn start(&mut self, ctx: &mut Ctx) -> Vec<ViewCmd> {
        self.styles = ctx.styles.clone();
        self.set_resources(&ctx.resources);
        self.refresh_plugins(ctx);
        self.menu_act = self.act.clone();
        self.menu_plugins = self.act_plugins.clone();
        let acc = ctx.accessor.clone();
        self.start_watch(acc, ctx);
        Vec::new()
    }

    fn stop(&mut self, _ctx: &mut Ctx) {
        self.stop_watch();
    }

    fn handle_key(&mut self, key: KeyEvent, ctx: &mut Ctx) -> Vec<ViewCmd> {
        self.set_resources(&ctx.resources);
        self.menu_act = self.act.clone();
        self.menu_plugins = self.act_plugins.clone();
        self.normalize();
        let out = self.handle(key, ctx);
        self.refresh_plugins(ctx);
        out
    }

    fn on_event(&mut self, ev: &AppEvent, ctx: &mut Ctx) -> Vec<ViewCmd> {
        self.on_ev(ev, ctx)
    }

    /// tview TreeView.MouseHandler: a click selects the node under the
    /// mouse (changed callback) and fires its selected func (expand toggle);
    /// the wheel moves the selection up/down.
    fn on_mouse(&mut self, m: crate::ui::Mouse, area: Rect, ctx: &mut Ctx) -> Vec<ViewCmd> {
        use crate::ui::Mouse;
        self.menu_act = self.act.clone();
        self.menu_plugins = self.act_plugins.clone();
        self.normalize();
        match m {
            Mouse::Click { y, .. } => {
                let rows = self.rows();
                let i = (y as i64 - (area.y as i64 + 1)) + self.top as i64;
                if i >= 0 && (i as usize) < rows.len() {
                    let i = i as usize;
                    self.select(&rows, i);
                    let r = &rows[i];
                    self.expanded.insert(r.key.clone(), !r.expanded);
                    self.invalidate();
                }
            }
            Mouse::ScrollUp { .. } => self.move_sel(-1),
            Mouse::ScrollDown { .. } => self.move_sel(1),
        }
        self.refresh_plugins(ctx);
        // A click fires the changed callback right away (not while drawing),
        // so the menu is current; wheel moves lag like keys.
        if matches!(m, Mouse::Click { .. }) {
            self.menu_act = self.act.clone();
            self.menu_plugins = self.act_plugins.clone();
        }
        Vec::new()
    }

    fn on_filter(&mut self, text: &str, ctx: &mut Ctx) -> Vec<ViewCmd> {
        self.on_filter_text(text, ctx)
    }

    fn draw(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &Ctx) {
        self.draw_tree(frame, area, ctx)
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

/// Keys xray binds itself (xray.go bindKeys + refreshActions).
const XRAY_OWN_KEYS: [&str; 13] = [
    "space", "x", "/", "esc", "enter", "e", "ctrl-d", "y", "d", "s", "a", "l", "p",
];

impl XrayView {
    fn handle(&mut self, key: KeyEvent, ctx: &mut Ctx) -> Vec<ViewCmd> {
        let k = util::key_label(&key);
        // pluginActions: bound after the view's own keys (no override here).
        let own = XRAY_OWN_KEYS;
        // hotKeyActions(x, aa): aa holds the node actions and plugins of the
        // last refreshActions; a key taken there needs override. The merge
        // into the view's actions then beats the base keys (space, x, enter).
        if let Some(hk) = crate::ui::shell::plugins::hotkey_for_key(ctx, &key) {
            let aliases = self.plugin_aliases(ctx);
            let taken = self
                .act_hints(self.act.as_deref())
                .iter()
                .any(|h| h.mnemonic == k)
                || crate::ui::shell::plugin_for_key(ctx, &aliases, &key).is_some();
            if !taken || hk.override_ {
                // x.EnvFn()(): the selected node's k9sEnv.
                let cmd = match self.selected_spec() {
                    Some(spec) => crate::ui::shell::plugins::substitute(
                        &crate::ui::shell::plugins::plugin_env(ctx, &self.plugin_selection(&spec)),
                        &hk.command,
                    ),
                    None => crate::ui::shell::plugins::hotkey_command(ctx, &hk.command),
                };
                return vec![crate::ui::shell::plugins::hotkey_goto(cmd, hk.keep_history)];
            }
        }
        if !own.contains(&k.as_str()) {
            let aliases = self.plugin_aliases(ctx);
            if let Some(p) = crate::ui::shell::plugin_for_key(ctx, &aliases, &key) {
                return match self.selected_spec() {
                    Some(spec) => {
                        crate::ui::shell::run_plugin(ctx, &p, &self.plugin_selection(&spec))
                    }
                    None => Vec::new(),
                };
            }
        }
        let spec = self.selected_spec();
        // Bound actions follow the last refreshActions, not the cursor.
        let g = self.act.clone().unwrap_or_default();
        let bound = self.act.is_some();
        let meta = self.meta(&g).filter(|_| bound);
        match k.as_str() {
            "/" => vec![ViewCmd::OpenFilterPrompt],
            // resetCmd: CmdBuff.Reset re-filters with an empty buffer, then PrevCmd.
            "esc" => {
                if !self.query.is_empty() {
                    self.on_filter("", ctx);
                }
                vec![ViewCmd::Pop]
            }
            "j" | "down" | "right" => {
                self.move_sel(1);
                Vec::new()
            }
            "k" | "up" | "left" => {
                self.move_sel(-1);
                Vec::new()
            }
            "shift-j" => {
                self.move_tree(true);
                Vec::new()
            }
            "shift-k" => {
                self.move_tree(false);
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
                if spec.is_some() {
                    let rows = self.rows();
                    let idx = self.selected_index(&rows);
                    if let Some(r) = rows.get(idx) {
                        self.expanded.insert(r.key.clone(), !r.expanded);
                        self.invalidate();
                    }
                }
                Vec::new()
            }
            "x" => {
                // toggleCollapseCmd: every node but the root.
                self.expand_nodes = !self.expand_nodes;
                self.force_open.clear();
                let root = self.rows().first().map(|r| r.key.clone());
                self.expanded.retain(|p, _| Some(p) == root.as_ref());
                self.invalidate();
                Vec::new()
            }
            "enter" => {
                let Some(s) = spec else {
                    return Vec::new();
                };
                if g == NAMESPACES || g == CONTAINERS || s.path().split('/').count() == 1 {
                    return Vec::new();
                }
                vec![ViewCmd::Goto {
                    cmd: s.gvr().to_string(),
                    path: s.path().to_string(),
                }]
            }
            // Bound actions can outlive the selection (e.g. a filter
            // emptied the tree): no selection, no action.
            "e" if spec.is_some()
                && meta
                    .as_ref()
                    .is_some_and(|m| Self::can(m, &["patch", "update"])) =>
            {
                let Some(s) = spec else {
                    return Vec::new();
                };
                vec![ViewCmd::Edit {
                    gvr: Gvr::parse(s.gvr()),
                    path: s.path().to_string(),
                    on_done: None,
                }]
            }
            "ctrl-d" if meta.as_ref().is_some_and(|m| Self::can(m, &["delete"])) => match spec {
                Some(s) => self.delete(&s),
                None => Vec::new(),
            },
            "y" | "d" if meta.is_some() => {
                if let Some(s) = spec {
                    self.open_details(ctx, &s, k == "y");
                }
                Vec::new()
            }
            "s" if g == PODS || g == CONTAINERS => self.shell(ctx, false),
            "a" if g == PODS => self.shell(ctx, true),
            "l" if g == PODS || g == CONTAINERS => self.logs(ctx, false),
            "p" if g == PODS || g == CONTAINERS => self.logs(ctx, true),
            _ => Vec::new(),
        }
    }

    fn on_ev(&mut self, ev: &AppEvent, ctx: &mut Ctx) -> Vec<ViewCmd> {
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
                                // TreeChanged queues a full redraw.
                                self.menu_act = self.act.clone();
                                self.menu_plugins = self.act_plugins.clone();
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

    fn on_filter_text(&mut self, text: &str, ctx: &mut Ctx) -> Vec<ViewCmd> {
        let was_labels = self.labels();
        self.query = text.to_string();
        if self.labels() != was_labels {
            let acc = ctx.accessor.clone();
            self.start_watch(acc, ctx);
        }
        self.update();
        Vec::new()
    }

    fn draw_tree(&mut self, frame: &mut Frame<'_>, area: Rect, ctx: &Ctx) {
        self.styles = ctx.styles.clone();
        self.set_resources(&ctx.resources);
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
        self.normalize();
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

    /// xray.go refreshActions: hotkeys bind after the view's own keys; a
    /// taken key needs `override`.
    #[tokio::test]
    async fn hotkeys_bind_on_xray() {
        let (mut c, _rx) = ctx(cluster());
        let hk = |key: &str, cmd: &str, ov: bool| crate::config::HotKey {
            short_cut: key.into(),
            description: cmd.into(),
            command: cmd.into(),
            override_: ov,
            ..Default::default()
        };
        c.hotkeys
            .hot_keys
            .insert("a".into(), hk("Shift-H", "dp", false));
        c.hotkeys.hot_keys.insert("b".into(), hk("y", "svc", false));
        let mut v = XrayView::new(&mut c, Gvr::parse(DEPLOYS), "default".into());
        let shift_h = KeyEvent::new(KeyCode::Char('H'), KeyModifiers::SHIFT);
        let out = v.handle_key(shift_h, &mut c);
        assert!(matches!(&out[..], [ViewCmd::Command(s)] if s == "dp"));
        // `y` is only bound once a node with a YAML action is selected (k9s
        // refreshActions; before that the hotkey owns it).
        v.act = Some(DEPLOYS.into());
        let y = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE);
        let out = v.handle_key(y, &mut c);
        assert!(
            !matches!(&out[..], [ViewCmd::Command(s)] if s == "svc"),
            "taken"
        );
        c.hotkeys.hot_keys.insert("b".into(), hk("y", "svc", true));
        let out = v.handle_key(y, &mut c);
        assert!(
            matches!(&out[..], [ViewCmd::Command(s)] if s == "svc"),
            "override"
        );
    }

    /// xray.go: model.SetRefreshRate(K9s.RefreshDuration()): the 2s floor
    /// and the --refresh flag apply (was the raw config value).
    #[tokio::test]
    async fn refresh_rate_follows_k9s() {
        let (mut c, _rx) = ctx(cluster());
        c.config.write().unwrap().k9s.refresh_rate = 0.5;
        let v = XrayView::new(&mut c, Gvr::parse(DEPLOYS), "default".into());
        assert_eq!(v.refresh, std::time::Duration::from_secs(2));
        c.config.write().unwrap().k9s.manual.refresh_rate = 5.0;
        let v = XrayView::new(&mut c, Gvr::parse(DEPLOYS), "default".into());
        assert_eq!(v.refresh, std::time::Duration::from_secs(5));
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
        // xray.go gotoCmd: gotoResource(gvr, path, false, true) pushes an instance view.
        assert!(
            matches!(&cmds[0], ViewCmd::Goto { cmd, path } if cmd == "apps/v1/deployments" && path == "default/web")
        );
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
            matches!(&cmds[0], ViewCmd::Exec { container, fallback, .. } if container == "nginx" && fallback.contains(&"nginx".to_string()))
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

    /// pod.go containerShellIn: several containers open the Containers Picker.
    #[tokio::test]
    async fn multi_container_shell_picker() {
        let f = cluster();
        f.add(
            PODS,
            obj(json!({"apiVersion":"v1","kind":"Pod",
            "metadata":{"name":"multi","namespace":"default"},
            "spec":{"containers":[{"name":"a"},{"name":"b"}]},
            "status":{"phase":"Running","containerStatuses":[
                {"name":"a","ready":true,"state":{"running":{}}},
                {"name":"b","ready":true,"state":{"running":{}}}]}})),
        );
        let (mut c, mut rx) = ctx(f);
        let mut v = XrayView::new(&mut c, Gvr::parse(PODS), "default".into());
        v.start(&mut c);
        let ev = rx.recv().await.unwrap();
        v.on_event(&ev, &mut c);
        for _ in 0..10 {
            if v.selected_spec()
                .is_some_and(|s| s.path() == "default/multi")
            {
                break;
            }
            v.handle_key(key('j'), &mut c);
        }
        let cmds = v.handle_key(key('s'), &mut c);
        let Some(ViewCmd::Push(p)) = cmds.into_iter().next() else {
            panic!("expected the containers picker");
        };
        assert_eq!(p.title(), " Containers Picker ");
    }

    fn key(c: char) -> KeyEvent {
        let m = if c.is_ascii_uppercase() {
            KeyModifiers::SHIFT
        } else {
            KeyModifiers::NONE
        };
        KeyEvent::new(KeyCode::Char(c), m)
    }

    fn descs(v: &XrayView) -> Vec<String> {
        v.hints().into_iter().map(|h| h.description).collect()
    }

    /// One pod whose container and service account share the id
    /// `default/builder`, i.e. the same xray path.
    fn dup_cluster() -> FakeAccessor {
        let f = FakeAccessor::new();
        f.add(PODS, obj(json!({"apiVersion":"v1","kind":"Pod",
            "metadata":{"name":"solo","namespace":"default"},
            "spec":{"serviceAccountName":"builder","containers":[{"name":"builder"}]},
            "status":{"phase":"Running","containerStatuses":[{"name":"builder","ready":true,"state":{"running":{}}}]}})));
        f.add(SAS, obj(json!({"apiVersion":"v1","kind":"ServiceAccount","metadata":{"name":"builder","namespace":"default"}})));
        f
    }

    async fn loaded(
        f: FakeAccessor,
        gvr: &str,
    ) -> (XrayView, Ctx, mpsc::UnboundedReceiver<AppEvent>) {
        let (mut c, mut rx) = ctx(f);
        let mut v = XrayView::new(&mut c, Gvr::parse(gvr), "default".into());
        v.start(&mut c);
        let ev = rx.recv().await.unwrap();
        v.on_event(&ev, &mut c);
        (v, c, rx)
    }

    /// xray.go refreshActions runs in Init (no node yet) and then from the
    /// tree changed callback; the menu is hydrated while the tree draws, so
    /// it shows the actions of the previous selection until the next redraw.
    #[tokio::test]
    async fn menu_follows_selection_changes_one_redraw_late() {
        let (mut v, mut c, _rx) = loaded(cluster(), DEPLOYS).await;
        assert_eq!(
            descs(&v),
            ["Expand/Collapse", "Expand/Collapse All", "Goto"]
        );
        // Root actions are not bound: no describe before the cursor moves.
        assert!(v.handle_key(key('d'), &mut c).is_empty());
        v.handle_key(key('j'), &mut c); // namespace
        assert_eq!(
            descs(&v),
            ["Expand/Collapse", "Expand/Collapse All", "Goto"]
        );
        v.handle_key(key('j'), &mut c); // deployment
        let ns = [
            "Delete",
            "Describe",
            "Edit",
            "Expand/Collapse",
            "Expand/Collapse All",
            YAML_ACTION,
        ];
        assert_eq!(descs(&v), ns);
        // A tree change redraws everything.
        v.menu_act = None;
        v.model = None;
        let root = build_tree(&*c.accessor, &Gvr::parse(DEPLOYS), "default", None)
            .await
            .unwrap();
        v.on_event(
            &AppEvent::Custom {
                view: v.id,
                payload: Box::new(TreeResult {
                    gen: v.gen,
                    result: Ok(root),
                }),
            },
            &mut c,
        );
        assert!(descs(&v).contains(&"Goto".to_string()));
        assert!(descs(&v).contains(&"Describe".to_string()));
    }

    /// tview: a current node hidden by a collapse silently becomes the root;
    /// J/K move to the first child/parent; Left/Right move like Up/Down.
    #[tokio::test]
    async fn collapse_child_parent_moves() {
        let (mut v, mut c, _rx) = loaded(cluster(), DEPLOYS).await;
        v.handle_key(key('j'), &mut c);
        v.handle_key(key('j'), &mut c);
        assert_eq!(v.selected_spec().unwrap().path(), "default/web");
        v.handle_key(key('x'), &mut c);
        assert_eq!(v.rows().len(), 2);
        v.handle_key(key('j'), &mut c);
        assert_eq!(v.selected_spec().unwrap().gvr(), NAMESPACES);
        v.handle_key(key(' '), &mut c);
        v.handle_key(key('J'), &mut c);
        assert_eq!(v.selected_spec().unwrap().path(), "default/web");
        v.handle_key(key('K'), &mut c);
        assert_eq!(v.selected_spec().unwrap().gvr(), NAMESPACES);
        v.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE), &mut c);
        assert_eq!(v.selected_spec().unwrap().path(), "default/web");
        v.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE), &mut c);
        assert_eq!(v.selected_spec().unwrap().gvr(), NAMESPACES);
        // The root can be collapsed too.
        v.handle_key(key('g'), &mut c);
        v.handle_key(key(' '), &mut c);
        assert_eq!(v.rows().len(), 1);
    }

    /// Xray.update calls SetCurrentNode on every node matching the selected
    /// path, so the last same-path sibling wins after a rebuild.
    #[tokio::test]
    async fn rebuild_selects_last_same_path_node() {
        let (mut v, mut c, _rx) = loaded(dup_cluster(), PODS).await;
        for _ in 0..3 {
            v.handle_key(key('j'), &mut c);
        }
        assert_eq!(v.selected_spec().unwrap().gvr(), CONTAINERS);
        v.model = None;
        let root = build_tree(&*c.accessor, &Gvr::parse(PODS), "default", None)
            .await
            .unwrap();
        v.on_event(
            &AppEvent::Custom {
                view: v.id,
                payload: Box::new(TreeResult {
                    gen: v.gen,
                    result: Ok(root),
                }),
            },
            &mut c,
        );
        v.handle_key(key('z'), &mut c);
        assert_eq!(v.selected_spec().unwrap().gvr(), SAS);
    }

    /// resetCmd: esc resets the filter buffer and goes back; enter on a
    /// resource pushes its single instance view (gotoResource(gvr, path)).
    #[tokio::test]
    async fn esc_resets_filter_enter_goes_to_instance() {
        let (mut v, mut c, _rx) = loaded(cluster(), DEPLOYS).await;
        v.on_filter("empty", &mut c);
        assert!(v.title().contains("</empty>"));
        let cmds = v.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut c);
        assert!(matches!(cmds[..], [ViewCmd::Pop]));
        assert_eq!(v.filter_text(), "");
        assert!(!v.title().contains("</"));
        v.handle_key(key('j'), &mut c);
        v.handle_key(key('j'), &mut c);
        let cmds = v.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &mut c);
        assert!(
            matches!(&cmds[..], [ViewCmd::Goto { cmd, path }] if cmd == DEPLOYS && path == "default/web")
        );
    }

    /// xray.go refreshActions also binds plugins scoped to the selected
    /// node's resource (x.meta), the xray resource before any selection.
    #[tokio::test]
    async fn plugins_follow_selected_resource() {
        let (mut c, mut rx) = ctx(cluster());
        c.plugins.plugins.insert(
            "dp".into(),
            crate::config::Plugin {
                scopes: vec!["deployments".into()],
                short_cut: "Shift-Z".into(),
                description: "Dp only".into(),
                command: "echo".into(),
                confirm: true,
                args: vec!["$NAMESPACE".into(), "$NAME".into()],
                ..Default::default()
            },
        );
        let mut v = XrayView::new(&mut c, Gvr::parse(DEPLOYS), "default".into());
        v.start(&mut c);
        let ev = rx.recv().await.unwrap();
        v.on_event(&ev, &mut c);
        assert!(descs(&v).contains(&"Dp only".to_string()));
        v.handle_key(key('j'), &mut c); // namespace
        v.handle_key(key('j'), &mut c); // menu now shows the namespace node's
        assert!(!descs(&v).contains(&"Dp only".to_string()));
        // The deployment is selected: the plugin runs on it (confirm first).
        let cmds = v.handle_key(key('Z'), &mut c);
        match &cmds[..] {
            [ViewCmd::Dialog(crate::ui::Dialog::Confirm { title, msg, .. })] => {
                assert_eq!(title, "Confirm Dp only");
                assert_eq!(msg, "Run?\necho default web");
            }
            _ => panic!("expected confirm"),
        }
        // Blurred xray border falls back to the xray fg color.
        assert_eq!(v.blur_border(&c.styles), c.styles.xray_fg);
    }

    /// tview TreeView.MouseHandler: a click selects (changed fires at once,
    /// so the menu is current) and toggles the node; the wheel moves.
    #[tokio::test]
    async fn mouse_click_selects_and_toggles() {
        let (mut v, mut c, _rx) = loaded(cluster(), DEPLOYS).await;
        let area = Rect::new(0, 0, 70, 16);
        let n = v.rows().len();
        // Row 2 (the deployment) sits below the border at y = 1 + 2.
        v.on_mouse(crate::ui::Mouse::Click { x: 8, y: 3 }, area, &mut c);
        assert_eq!(v.selected_spec().unwrap().path(), "default/web");
        assert!(v.rows().len() < n, "clicked node collapsed");
        assert!(descs(&v).contains(&"Describe".to_string()));
        v.on_mouse(crate::ui::Mouse::ScrollUp { x: 8, y: 3 }, area, &mut c);
        assert_eq!(v.selected_spec().unwrap().gvr(), NAMESPACES);
    }

    /// hotKeyActions details: the selected node's env, base keys lose.
    #[tokio::test]
    async fn hotkeys_use_node_env_and_beat_base_keys() {
        let (mut v, mut c, _rx) = loaded(cluster(), DEPLOYS).await;
        let hk = |sc: &str, cmd: &str| crate::config::HotKey {
            short_cut: sc.into(),
            description: "hk".into(),
            command: cmd.into(),
            ..Default::default()
        };
        c.hotkeys
            .hot_keys
            .insert("a".into(), hk("Shift-P", "pods $NAMESPACE $NAME"));
        c.hotkeys.hot_keys.insert("b".into(), hk("x", "cm"));
        v.handle_key(key('j'), &mut c);
        v.handle_key(key('j'), &mut c); // the deployment
        let cmds = v.handle_key(key('P'), &mut c);
        assert!(matches!(&cmds[..], [ViewCmd::Command(s)] if s == "pods default web"));
        // Shared actions: hotkeys stay out of the menu/help hints.
        assert!(!descs(&v).contains(&"hk".to_string()));
        assert!(matches!(&v.handle_key(key('x'), &mut c)[..], [ViewCmd::Command(s)] if s == "cm"));
    }

    /// xray.go showLogs: no pod list access -> error flash, no logs view.
    #[tokio::test]
    async fn logs_need_pod_list_access() {
        let f = cluster();
        f.denied
            .lock()
            .unwrap()
            .push((PODS.into(), "default".into()));
        let (mut v, mut c, _rx) = loaded(f, PODS).await;
        for _ in 0..2 {
            v.handle_key(key('j'), &mut c);
        }
        assert_eq!(v.selected_spec().unwrap().gvr(), PODS);
        let cmds = v.handle_key(key('l'), &mut c);
        assert!(
            matches!(&cmds[..], [ViewCmd::Flash(FlashLevel::Err, m)] if m == "[list] access denied on resource \"default\":\"v1/pods\""),
            "{}",
            cmds.len()
        );
    }
}
