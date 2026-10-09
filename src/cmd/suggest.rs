//! Prompt suggestions (k9s `view.App.suggestCommand`, `view.Browser.suggestFilter`,
//! `cmd.SuggestSubCommand`) and the suggestion cycling of `model.FishBuff`.
//!
//! Suggestions are completion SUFFIXES: for input `po k` and namespace
//! `kube-system` the suggestion is `ube-system`. The prompt shows it greyed
//! after the cursor and appends it on accept.

use std::collections::BTreeSet;

use super::interpreter::{builtin_alias_names, Interpreter};
use super::History;
use crate::config::Aliases;
use crate::k8s::NAMESPACE_ALL;

/// cmd.ShouldAddSuggest: the remaining suffix when `suggest` extends `command`.
pub fn should_add_suggest(command: &str, suggest: &str) -> Option<String> {
    if command != suggest && suggest.starts_with(command) {
        return Some(suggest[command.len()..].to_string());
    }
    None
}

/// cmd.SuggestSubCommand: namespace or context completions for `command`,
/// sorted.
pub fn suggest_sub_command(
    command: &str,
    namespaces: &[String],
    contexts: &[String],
) -> Vec<String> {
    let p = Interpreter::new(command);
    let mut out = Vec::new();
    if p.is_cow_cmd() || p.is_help_cmd() || p.is_alias_cmd() || p.is_bail_cmd() || p.is_dir_cmd() {
        return out;
    }
    if p.is_xray_cmd() {
        match p.xray_args() {
            Some((_, ns)) if !ns.is_empty() => out = complete_ns(&ns, namespaces),
            _ => return Vec::new(),
        }
    } else if p.is_context_cmd() {
        let Some(n) = p.context_arg() else {
            return Vec::new();
        };
        out = complete_ctx(command, &n, contexts);
    } else if p.has_ns() {
        if let Some(n) = p.has_context() {
            out = complete_ctx(command, &n, contexts);
        }
        if out.is_empty() {
            let Some(ns) = p.ns_arg() else {
                return Vec::new();
            };
            out = complete_ns(&ns, namespaces);
        }
    } else if let Some(n) = p.has_context() {
        out = complete_ctx(command, &n, contexts);
    }
    out.sort();
    out
}

fn complete_ns(s: &str, nn: &[String]) -> Vec<String> {
    let s = s.to_lowercase();
    let mut out = Vec::new();
    if let Some(sg) = should_add_suggest(&s, NAMESPACE_ALL) {
        out.push(sg);
    }
    for ns in nn {
        if let Some(sg) = should_add_suggest(&s, ns) {
            out.push(sg);
        }
    }
    out
}

fn complete_ctx(command: &str, s: &str, contexts: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for c in contexts {
        if let Some(sg) = should_add_suggest(s, c) {
            if s.is_empty() && !command.ends_with(' ') {
                out.push(format!(" {sg}"));
                continue;
            }
            out.push(sg);
        }
    }
    out
}

/// `:` prompt suggestions (App.suggestCommand) for non-empty input: alias
/// completions over the alias map (plus k9s built-ins), then namespace /
/// context completions; sorted. Empty input yields nothing here; see
/// [`suggest_command`] for the history fallback.
pub fn suggest(
    input: &str,
    aliases: &Aliases,
    namespaces: &[String],
    contexts: &[String],
) -> Vec<String> {
    if input.is_empty() {
        return Vec::new();
    }
    let ls = input.to_lowercase();
    let mut names: BTreeSet<&str> = aliases.aliases.keys().map(String::as_str).collect();
    for n in builtin_alias_names() {
        names.insert(n);
    }
    let mut out: Vec<String> = names
        .into_iter()
        .filter_map(|a| should_add_suggest(&ls, a))
        .collect();
    out.extend(suggest_sub_command(input, namespaces, contexts));
    out.sort();
    out
}

/// Full App.suggestCommand: empty input suggests the command history
/// (full lines, oldest first); otherwise [`suggest`].
pub fn suggest_command(
    input: &str,
    aliases: &Aliases,
    history: &History,
    namespaces: &[String],
    contexts: &[String],
) -> Vec<String> {
    if input.is_empty() {
        return history.list().to_vec();
    }
    suggest(input, aliases, namespaces, contexts)
}

/// `/` prompt suggestions (Browser.suggestFilter) from the filter history:
/// empty input lists the history; otherwise suffixes of entries extending
/// the lowercased input, in history order.
pub fn suggest_filter(input: &str, history: &History) -> Vec<String> {
    if input.is_empty() {
        return history.list().to_vec();
    }
    let s = input.to_lowercase();
    history
        .list()
        .iter()
        .filter(|h| **h != s && h.starts_with(&s))
        .map(|h| h.replacen(&s, "", 1))
        .collect()
}

/// Prompt buffer with suggestion cycling (k9s `model.FishBuff` minus the
/// listener plumbing). The caller recomputes suggestions after edits and
/// feeds them through [`FishBuff::set_suggestions`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FishBuff {
    buff: String,
    suggestion: String,
    suggestions: Vec<String>,
    index: isize,
}

impl Default for FishBuff {
    fn default() -> Self {
        FishBuff {
            buff: String::new(),
            suggestion: String::new(),
            suggestions: Vec::new(),
            index: -1,
        }
    }
}

impl FishBuff {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn text(&self) -> &str {
        &self.buff
    }
    /// The suggestion currently shown after the text.
    pub fn suggestion(&self) -> &str {
        &self.suggestion
    }
    pub fn is_empty(&self) -> bool {
        self.buff.is_empty()
    }
    /// CmdBuff.SetText: replace (`wipe`) or append the text.
    pub fn set_text(&mut self, text: &str, suggestion: &str, wipe: bool) {
        if wipe {
            self.buff = text.to_string();
        } else {
            self.buff.push_str(text);
        }
        self.suggestion = suggestion.to_string();
    }
    /// Appends a character.
    pub fn add(&mut self, c: char) {
        self.buff.push(c);
    }
    /// Removes the last character and the shown suggestion.
    pub fn delete(&mut self) {
        if self.buff.pop().is_some() {
            self.suggestion.clear();
        }
    }
    /// Clears text and suggestion.
    pub fn clear_text(&mut self) {
        self.buff.clear();
        self.suggestion.clear();
    }
    /// fireSuggestionChanged: install fresh suggestions; the first is shown.
    pub fn set_suggestions(&mut self, ss: Vec<String>) {
        self.suggestions = ss;
        if self.suggestions.is_empty() {
            self.index = -1;
            self.suggestion.clear();
        } else {
            self.index = 0;
            self.suggestion = self.suggestions[0].clone();
        }
    }
    pub fn suggestions(&self) -> &[String] {
        &self.suggestions
    }
    pub fn current_suggestion(&self) -> Option<&str> {
        if self.index < 0 {
            return None;
        }
        self.suggestions
            .get(self.index as usize)
            .map(String::as_str)
    }
    /// Cycles forward (wraps), showing the new suggestion.
    pub fn next_suggestion(&mut self) -> Option<String> {
        if self.suggestions.is_empty() {
            return None;
        }
        self.index = if self.index < 0 { 0 } else { self.index + 1 };
        if self.index >= self.suggestions.len() as isize {
            self.index = 0;
        }
        self.suggestion = self.suggestions[self.index as usize].clone();
        Some(self.suggestion.clone())
    }
    /// Cycles backward (wraps), showing the new suggestion.
    pub fn prev_suggestion(&mut self) -> Option<String> {
        if self.suggestions.is_empty() {
            return None;
        }
        self.index = if self.index < 0 { 0 } else { self.index - 1 };
        if self.index < 0 {
            self.index = self.suggestions.len() as isize - 1;
        }
        self.suggestion = self.suggestions[self.index as usize].clone();
        Some(self.suggestion.clone())
    }
    pub fn clear_suggestions(&mut self) {
        self.suggestions.clear();
        self.index = -1;
    }
    /// Accepts the shown suggestion into the text.
    pub fn accept_suggestion(&mut self) {
        let s = std::mem::take(&mut self.suggestion);
        self.buff.push_str(&s);
        self.clear_suggestions();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn suggest_sub_command_cases() {
        // view/cmd/helpers_test.go TestSuggestSubCommand
        let nss = s(&["kube-system", "kube-public", "default", "nginx-ingress"]);
        let ctxs = s(&["develop", "test", "pre", "prod"]);
        for (cmd, e) in [
            ("q", s(&[])),
            ("xray  dp", s(&[])),
            ("help  k", s(&[])),
            ("ctx p", s(&["re", "rod"])),
            ("ctx   p", s(&["re", "rod"])),
            ("ctx pr", s(&["e", "od"])),
            ("ctx", s(&[" develop", " pre", " prod", " test"])),
            ("ctx ", s(&["develop", "pre", "prod", "test"])),
            ("context   d", s(&["evelop"])),
            ("contexts   t", s(&["est"])),
            ("po ", s(&[])),
            ("po  x", s(&[])),
            ("po k", s(&["ube-public", "ube-system"])),
            ("po  kube-", s(&["public", "system"])),
            ("po a", s(&["ll"])),
            ("xray po k", s(&["ube-public", "ube-system"])),
            ("po @d", s(&["evelop"])),
            ("po kube-system @p", s(&["re", "rod"])),
            ("dir /tmp", s(&[])),
        ] {
            assert_eq!(suggest_sub_command(cmd, &nss, &ctxs), e, "{cmd:?}");
        }
    }

    #[test]
    fn should_add() {
        assert_eq!(should_add_suggest("po", "pods").as_deref(), Some("ds"));
        assert_eq!(should_add_suggest("po", "po"), None);
        assert_eq!(should_add_suggest("x", "po"), None);
        assert_eq!(should_add_suggest("", "po").as_deref(), Some("po"));
    }

    #[test]
    fn suggest_aliases() {
        let mut a = Aliases::default();
        a.define("v1/pods", &["po", "pod", "pods", "v1/pods"]);
        a.define("apps/v1/deployments", &["dp", "deploy"]);
        a.define("v1/persistentvolumes", &["pv"]);
        let nss = s(&["default", "kube-system"]);
        let ctxs = s(&["dev"]);
        assert_eq!(
            suggest("po", &a, &nss, &ctxs),
            s(&["d", "ds", "rtforward", "rtforwards"])
        );
        assert_eq!(
            suggest("PO", &a, &nss, &ctxs),
            s(&["d", "ds", "rtforward", "rtforwards"])
        );
        assert_eq!(suggest("de", &a, &nss, &ctxs), s(&["ploy"]));
        assert_eq!(suggest("po d", &a, &nss, &ctxs), s(&["efault"]));
        assert_eq!(suggest("ctx d", &a, &nss, &ctxs), s(&["ev"]));
        assert_eq!(suggest("zz", &a, &nss, &ctxs), s(&[]));
        assert_eq!(suggest("", &a, &nss, &ctxs), s(&[]));
        assert_eq!(suggest("pu", &a, &nss, &ctxs), s(&["lse", "lses"]));
    }

    #[test]
    fn suggest_command_history() {
        let a = Aliases::default();
        let mut h = History::new(5);
        assert!(suggest_command("", &a, &h, &[], &[]).is_empty());
        h.push("po");
        h.push("dp kube-system");
        assert_eq!(
            suggest_command("", &a, &h, &[], &[]),
            s(&["po", "dp kube-system"])
        );
        assert_eq!(suggest_command("hel", &a, &h, &[], &[]), s(&["m", "p"]));
    }

    #[test]
    fn suggest_filter_cases() {
        let mut h = History::new(5);
        assert!(suggest_filter("", &h).is_empty());
        h.push("fred");
        h.push("freddy");
        h.push("blee");
        assert_eq!(suggest_filter("", &h), s(&["fred", "freddy", "blee"]));
        assert_eq!(suggest_filter("FR", &h), s(&["ed", "eddy"]));
        assert_eq!(suggest_filter("fred", &h), s(&["dy"]));
        assert!(suggest_filter("z", &h).is_empty());
    }

    #[test]
    fn fish_buff_cycle() {
        // model/fish_buff_test.go TestFishAdd / TestFishDelete
        let mut f = FishBuff::new();
        f.add('b');
        f.set_suggestions(s(&["blee", "brew"]));
        assert_eq!(f.suggestion(), "blee");
        assert_eq!(f.current_suggestion(), Some("blee"));
        assert_eq!(f.next_suggestion().as_deref(), Some("brew"));
        assert_eq!(f.prev_suggestion().as_deref(), Some("blee"));
        assert_eq!(f.prev_suggestion().as_deref(), Some("brew"));
        assert_eq!(f.next_suggestion().as_deref(), Some("blee"));

        let mut f = FishBuff::new();
        f.add('a');
        f.delete();
        assert!(f.is_empty());
        f.set_suggestions(s(&["blee", "duh"]));
        assert_eq!(f.current_suggestion(), Some("blee"));
        assert_eq!(f.next_suggestion().as_deref(), Some("duh"));
        assert_eq!(f.prev_suggestion().as_deref(), Some("blee"));

        f.set_text("po", "ds", true);
        f.accept_suggestion();
        assert_eq!(f.text(), "pods");
        assert_eq!(f.current_suggestion(), None);
        assert_eq!(f.next_suggestion(), None);
        f.set_suggestions(vec![]);
        assert_eq!(f.suggestion(), "");
        f.set_text("x", "", false);
        assert_eq!(f.text(), "podsx");
        f.clear_text();
        assert!(f.is_empty());
    }
}
