//! kubectl's genericDescriber: recursive field printing for any object
//! (CRDs and every kind without a dedicated describer).

use anyhow::Result;
use serde_json::{Map, Value};

use super::events::describe_events;
use super::util::{go_value, print_annotations_multiline, print_labels_multiline, Ctx, J};
use super::writer::{PrefixWriter, L0};

const SKIP: [&str; 5] = [
    ".metadata.managedFields",
    ".metadata.name",
    ".metadata.namespace",
    ".metadata.labels",
    ".metadata.annotations",
];

pub(crate) fn describe_generic(ctx: &Ctx, obj: J, events: &[Value]) -> Result<String> {
    let mut w = PrefixWriter::new();
    let meta = obj.get("metadata");
    w.write(L0, &format!("Name:\t{}\n", meta.s("name")));
    w.write(L0, &format!("Namespace:\t{}\n", meta.s("namespace")));
    print_labels_multiline(&mut w, "Labels", &meta.smap("labels"));
    print_annotations_multiline(&mut w, "Annotations", &meta.smap("annotations"));
    if let Value::Object(content) = obj.0 {
        print_unstructured(&mut w, L0, content, "");
    }
    describe_events(ctx, &mut w, events);
    Ok(w.finish())
}

fn print_unstructured(
    w: &mut PrefixWriter,
    level: usize,
    content: &Map<String, Value>,
    prefix: &str,
) {
    let mut fields: Vec<&String> = content.keys().collect();
    fields.sort();
    for field in fields {
        let value = &content[field];
        let skip_expr = format!("{prefix}.{field}");
        if SKIP.contains(&skip_expr.as_str()) {
            continue;
        }
        match value {
            Value::Object(m) => {
                w.write(level, &format!("{}:\n", smart_label_for(field)));
                print_unstructured(w, level + 1, m, &skip_expr);
            }
            Value::Array(items) => {
                w.write(level, &format!("{}:\n", smart_label_for(field)));
                for child in items {
                    match child {
                        Value::Object(m) => print_unstructured(w, level + 1, m, &skip_expr),
                        other => w.write(level + 1, &format!("{}\n", go_value(other))),
                    }
                }
            }
            other => {
                w.write(
                    level,
                    &format!("{}:\t{}\n", smart_label_for(field), go_value(other)),
                );
            }
        }
    }
}

/// smartLabelFor: `creationTimestamp` -> `Creation Timestamp`, `uid` -> `UID`.
pub(crate) fn smart_label_for(field: &str) -> String {
    if field.chars().any(|r| !r.is_alphabetic() && r != '-') {
        return field.to_string();
    }
    const ACRONYMS: [&str; 5] = ["API", "URL", "UID", "OSB", "GUID"];
    camelcase_split(field)
        .into_iter()
        .filter(|p| p != "_")
        .map(|p| {
            let upper = p.to_uppercase();
            if ACRONYMS.contains(&upper.as_str()) {
                upper
            } else {
                title(&p)
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// strings.Title: upper-case the first letter of each word.
fn title(s: &str) -> String {
    let mut out = String::new();
    let mut prev_letter = false;
    for c in s.chars() {
        if !prev_letter && c.is_alphabetic() {
            out.extend(c.to_uppercase());
        } else {
            out.push(c);
        }
        prev_letter = c.is_alphanumeric() || c == '_' || c == '\'';
    }
    out
}

/// github.com/fatih/camelcase.Split.
fn camelcase_split(src: &str) -> Vec<String> {
    let class = |r: char| {
        if r.is_lowercase() {
            1
        } else if r.is_uppercase() {
            2
        } else if r.is_numeric() {
            3
        } else {
            4
        }
    };
    let mut runs: Vec<Vec<char>> = Vec::new();
    let mut last = 0;
    for r in src.chars() {
        let c = class(r);
        if c == last && !runs.is_empty() {
            runs.last_mut().expect("run").push(r);
        } else {
            runs.push(vec![r]);
        }
        last = c;
    }
    for i in 0..runs.len().saturating_sub(1) {
        let upper_then_lower = runs[i].first().is_some_and(|c| c.is_uppercase())
            && runs[i + 1].first().is_some_and(|c| c.is_lowercase());
        if upper_then_lower {
            if let Some(moved) = runs[i].pop() {
                runs[i + 1].insert(0, moved);
            }
        }
    }
    runs.into_iter()
        .filter(|r| !r.is_empty())
        .map(|r| r.into_iter().collect())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smart_labels() {
        assert_eq!(smart_label_for("creationTimestamp"), "Creation Timestamp");
        assert_eq!(smart_label_for("apiVersion"), "API Version");
        assert_eq!(smart_label_for("uid"), "UID");
        assert_eq!(smart_label_for("resourceVersion"), "Resource Version");
        assert_eq!(smart_label_for("URLPath"), "URL Path");
        assert_eq!(smart_label_for("foo-bar"), "Foo - Bar");
        assert_eq!(smart_label_for("x509"), "x509");
        assert_eq!(smart_label_for("my_field"), "my_field");
        assert_eq!(smart_label_for("ownerReferences"), "Owner References");
        assert_eq!(
            smart_label_for("blockOwnerDeletion"),
            "Block Owner Deletion"
        );
    }
}
