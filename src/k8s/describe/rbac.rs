//! RBAC describers: Role/ClusterRole (rules broken down and compacted like
//! kubectl's rbac util) and RoleBinding/ClusterRoleBinding.

use anyhow::Result;
use serde_json::Value;

use super::util::{go_slice, print_annotations_multiline, print_labels_multiline, Ctx, J};
use super::writer::{PrefixWriter, L0, L1};

#[derive(Clone, Debug, Default, PartialEq)]
struct Rule {
    verbs: Vec<String>,
    api_groups: Vec<String>,
    resources: Vec<String>,
    resource_names: Vec<String>,
    non_resource_urls: Vec<String>,
}

impl Rule {
    /// The gogo-protobuf generated PolicyRule.String() used as sort key.
    fn go_string(&self) -> String {
        format!(
            "&PolicyRule{{Verbs:{},APIGroups:{},Resources:{},ResourceNames:{},NonResourceURLs:{},}}",
            go_slice(&self.verbs),
            go_slice(&self.api_groups),
            go_slice(&self.resources),
            go_slice(&self.resource_names),
            go_slice(&self.non_resource_urls)
        )
    }
}

/// rbac.BreakdownRule.
fn breakdown(rule: J) -> Vec<Rule> {
    let (groups, resources, verbs, names, urls) = (
        rule.strs("apiGroups"),
        rule.strs("resources"),
        rule.strs("verbs"),
        rule.strs("resourceNames"),
        rule.strs("nonResourceURLs"),
    );
    let mut out = Vec::new();
    for g in &groups {
        for r in &resources {
            for v in &verbs {
                if names.is_empty() {
                    out.push(Rule {
                        verbs: vec![v.clone()],
                        api_groups: vec![g.clone()],
                        resources: vec![r.clone()],
                        ..Default::default()
                    });
                } else {
                    for n in &names {
                        out.push(Rule {
                            verbs: vec![v.clone()],
                            api_groups: vec![g.clone()],
                            resources: vec![r.clone()],
                            resource_names: vec![n.clone()],
                            ..Default::default()
                        });
                    }
                }
            }
        }
    }
    for u in &urls {
        for v in &verbs {
            out.push(Rule {
                verbs: vec![v.clone()],
                non_resource_urls: vec![u.clone()],
                ..Default::default()
            });
        }
    }
    out
}

/// rbac.CompactRules: merge verbs of rules on the same group/resource/name.
fn compact(rules: Vec<Rule>) -> Vec<Rule> {
    let mut compacted = Vec::new();
    let mut simple: Vec<((String, String, Option<String>), Rule)> = Vec::new();
    for rule in rules {
        let is_simple = rule.resource_names.len() <= 1
            && rule.non_resource_urls.is_empty()
            && rule.api_groups.len() == 1
            && rule.resources.len() == 1;
        if !is_simple {
            compacted.push(rule);
            continue;
        }
        let key = (
            rule.api_groups[0].clone(),
            rule.resources[0].clone(),
            rule.resource_names.first().cloned(),
        );
        match simple.iter_mut().find(|(k, _)| *k == key) {
            Some((_, existing)) => {
                for v in &rule.verbs {
                    if !existing.verbs.contains(v) {
                        existing.verbs.push(v.clone());
                    }
                }
            }
            None => simple.push((key, rule)),
        }
    }
    compacted.extend(simple.into_iter().map(|(_, r)| r));
    compacted
}

/// CombineResourceGroup.
fn combine_resource_group(resources: &[String], groups: &[String]) -> String {
    let Some(first) = resources.first() else {
        return String::new();
    };
    let (res, sub) = match first.split_once('/') {
        Some((r, s)) => (r, Some(s)),
        None => (first.as_str(), None),
    };
    let mut combine = res.to_string();
    if let Some(g) = groups.first() {
        if !g.is_empty() {
            combine = format!("{combine}.{g}");
        }
    }
    if let Some(s) = sub {
        combine = format!("{combine}/{s}");
    }
    combine
}

pub(crate) fn describe_role(_ctx: &Ctx, role: J, _events: &[Value]) -> Result<String> {
    let meta = role.get("metadata");
    let rules: Vec<Rule> = role.arr("rules").into_iter().flat_map(breakdown).collect();
    let mut rules = compact(rules);
    rules.sort_by_key(Rule::go_string);
    let mut w = PrefixWriter::new();
    w.write(L0, &format!("Name:\t{}\n", meta.s("name")));
    print_labels_multiline(&mut w, "Labels", &meta.smap("labels"));
    print_annotations_multiline(&mut w, "Annotations", &meta.smap("annotations"));
    w.write(L0, "PolicyRule:\n");
    w.write(L1, "Resources\tNon-Resource URLs\tResource Names\tVerbs\n");
    w.write(L1, "---------\t-----------------\t--------------\t-----\n");
    for r in &rules {
        w.write(
            L1,
            &format!(
                "{}\t{}\t{}\t{}\n",
                combine_resource_group(&r.resources, &r.api_groups),
                go_slice(&r.non_resource_urls),
                go_slice(&r.resource_names),
                go_slice(&r.verbs)
            ),
        );
    }
    Ok(w.finish())
}

pub(crate) fn describe_binding(_ctx: &Ctx, b: J, _events: &[Value]) -> Result<String> {
    let meta = b.get("metadata");
    let mut w = PrefixWriter::new();
    w.write(L0, &format!("Name:\t{}\n", meta.s("name")));
    print_labels_multiline(&mut w, "Labels", &meta.smap("labels"));
    print_annotations_multiline(&mut w, "Annotations", &meta.smap("annotations"));
    w.write(L0, "Role:\n");
    w.write(L1, &format!("Kind:\t{}\n", b.s("roleRef.kind")));
    w.write(L1, &format!("Name:\t{}\n", b.s("roleRef.name")));
    w.write(L0, "Subjects:\n");
    w.write(L1, "Kind\tName\tNamespace\n");
    w.write(L1, "----\t----\t---------\n");
    for s in b.arr("subjects") {
        w.write(
            L1,
            &format!("{}\t{}\t{}\n", s.s("kind"), s.s("name"), s.s("namespace")),
        );
    }
    Ok(w.finish())
}
