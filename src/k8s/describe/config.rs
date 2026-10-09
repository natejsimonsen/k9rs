//! ConfigMap, Secret and ServiceAccount describers.

use std::collections::BTreeSet;

use anyhow::Result;
use base64::Engine;
use serde_json::Value;

use super::events::describe_events;
use super::util::{print_annotations_multiline, print_labels_multiline, Ctx, J};
use super::writer::{PrefixWriter, L0};

/// Decoded length of a base64 `[]byte` field.
fn decoded_len(j: J) -> usize {
    let raw = j.as_str();
    base64::engine::general_purpose::STANDARD
        .decode(raw)
        .map(|b| b.len())
        .unwrap_or(raw.len())
}

pub(crate) fn describe_config_map(ctx: &Ctx, cm: J, events: &[Value]) -> Result<String> {
    let meta = cm.get("metadata");
    let mut w = PrefixWriter::new();
    w.write(L0, &format!("Name:\t{}\n", meta.s("name")));
    w.write(L0, &format!("Namespace:\t{}\n", meta.s("namespace")));
    print_labels_multiline(&mut w, "Labels", &meta.smap("labels"));
    print_annotations_multiline(&mut w, "Annotations", &meta.smap("annotations"));
    w.write(L0, "\nData\n====\n");
    for k in cm.keys("data") {
        w.write(L0, &format!("{k}:\n----\n"));
        w.write(L0, &format!("{}\n", cm.get("data").key(k).as_str()));
        w.write(L0, "\n");
    }
    w.write(L0, "\nBinaryData\n====\n");
    for k in cm.keys("binaryData") {
        w.write(
            L0,
            &format!("{k}: {} bytes\n", decoded_len(cm.get("binaryData").key(k))),
        );
    }
    w.write(L0, "\n");
    describe_events(ctx, &mut w, events);
    Ok(w.finish())
}

/// Secret values are never shown: every key prints its byte count. (kubectl
/// prints the `token` of service-account-token secrets in clear text.)
pub(crate) fn describe_secret(_ctx: &Ctx, secret: J, _events: &[Value]) -> Result<String> {
    let meta = secret.get("metadata");
    let mut w = PrefixWriter::new();
    w.write(L0, &format!("Name:\t{}\n", meta.s("name")));
    w.write(L0, &format!("Namespace:\t{}\n", meta.s("namespace")));
    print_labels_multiline(&mut w, "Labels", &meta.smap("labels"));
    print_annotations_multiline(&mut w, "Annotations", &meta.smap("annotations"));
    w.write(L0, &format!("\nType:\t{}\n", secret.s("type")));
    w.write(L0, "\nData\n====\n");
    for k in secret.keys("data") {
        w.write(
            L0,
            &format!("{k}:\t{} bytes\n", decoded_len(secret.get("data").key(k))),
        );
    }
    Ok(w.finish())
}

const SA_NAME_KEY: &str = "kubernetes.io/service-account.name";
const SA_UID_KEY: &str = "kubernetes.io/service-account.uid";

pub(crate) fn describe_service_account(ctx: &Ctx, sa: J, events: &[Value]) -> Result<String> {
    let meta = sa.get("metadata");
    let pull: Vec<String> = sa
        .arr("imagePullSecrets")
        .into_iter()
        .map(|s| s.s("name").to_string())
        .collect();
    let mount: Vec<String> = sa
        .arr("secrets")
        .into_iter()
        .map(|s| s.s("name").to_string())
        .collect();
    let mut tokens = Vec::new();
    let mut missing = BTreeSet::new();
    if let Some(secrets) = &ctx.rel.secrets {
        let mut existing = BTreeSet::new();
        for s in secrets.iter().map(J) {
            if s.s("metadata.namespace") != meta.s("namespace") {
                continue;
            }
            if s.s("type") == "kubernetes.io/service-account-token" {
                let ann = s.get("metadata.annotations");
                if ann.key(SA_NAME_KEY).as_str() == meta.s("name")
                    && ann.key(SA_UID_KEY).as_str() == meta.s("uid")
                {
                    tokens.push(s.s("metadata.name").to_string());
                }
            }
            existing.insert(s.s("metadata.name").to_string());
        }
        for n in mount.iter().chain(pull.iter()) {
            if !existing.contains(n) {
                missing.insert(n.clone());
            }
        }
    }
    let mut w = PrefixWriter::new();
    w.write(L0, &format!("Name:\t{}\n", meta.s("name")));
    w.write(L0, &format!("Namespace:\t{}\n", meta.s("namespace")));
    print_labels_multiline(&mut w, "Labels", &meta.smap("labels"));
    print_annotations_multiline(&mut w, "Annotations", &meta.smap("annotations"));
    let empty_header = "                   ";
    let sections = [
        ("Image pull secrets:", &pull),
        ("Mountable secrets: ", &mount),
        ("Tokens:            ", &tokens),
    ];
    for (header, names) in sections {
        if names.is_empty() {
            w.write(L0, &format!("{header}\t<none>\n"));
            continue;
        }
        let mut prefix = header;
        for n in names.iter() {
            if missing.contains(n) {
                w.write(L0, &format!("{prefix}\t{n} (not found)\n"));
            } else {
                w.write(L0, &format!("{prefix}\t{n}\n"));
            }
            prefix = empty_header;
        }
    }
    describe_events(ctx, &mut w, events);
    Ok(w.finish())
}
