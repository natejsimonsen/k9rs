//! Command history (k9s `model.History`): a bounded list of executed command
//! lines with a cursor used by `[` (back), `]` (forward) and `-` (last view).

use super::Interpreter;

/// k9s model.MaxHistory.
pub const MAX_HISTORY: usize = 20;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct History {
    commands: Vec<String>,
    limit: usize,
    current: isize,
}

impl Default for History {
    fn default() -> Self {
        History::new(MAX_HISTORY)
    }
}

impl History {
    pub fn new(limit: usize) -> Self {
        History {
            commands: Vec::new(),
            limit,
            current: -1,
        }
    }

    /// All commands, oldest first.
    pub fn list(&self) -> &[String] {
        &self.commands
    }

    fn at(&self, idx: isize) -> Option<String> {
        if idx < 0 {
            return None;
        }
        self.commands.get(idx as usize).cloned()
    }

    /// The most recent command; moves the cursor to it.
    pub fn top(&mut self) -> Option<String> {
        self.current = self.commands.len() as isize - 1;
        self.at(self.current)
    }

    /// The command `idx` entries back from the end (1 = most recent); moves the cursor.
    pub fn last(&mut self, idx: usize) -> Option<String> {
        self.current = self.commands.len() as isize - idx as isize;
        self.at(self.current)
    }

    /// Moves the cursor back one entry (`[`).
    pub fn back(&mut self) -> Option<String> {
        if self.is_empty() || self.current <= 0 {
            return None;
        }
        self.current -= 1;
        self.at(self.current)
    }

    /// Moves the cursor forward one entry (`]`).
    pub fn forward(&mut self) -> Option<String> {
        self.current += 1;
        if self.is_empty() || self.current >= self.commands.len() as isize {
            return None;
        }
        self.at(self.current)
    }

    /// Removes the most recent entry; true if the list changed.
    pub fn pop(&mut self) -> bool {
        self.pop_n(1)
    }

    fn pop_n(&mut self, n: usize) -> bool {
        if self.is_empty() || n > self.commands.len() {
            return false;
        }
        self.commands.truncate(self.commands.len() - n);
        self.current = self.commands.len() as isize - 1;
        true
    }

    /// Adds a command (lowercased). Blank lines, repeats of the top entry and
    /// pushes past the limit are ignored; entries after the cursor are dropped.
    pub fn push(&mut self, c: &str) {
        if c.is_empty() || self.commands.len() >= self.limit {
            return;
        }
        if self.top().as_deref() == Some(c) {
            return;
        }
        if self.current < self.commands.len() as isize - 1 {
            self.commands.truncate((self.current + 1).max(0) as usize);
        }
        self.commands.push(c.to_lowercase());
        self.current = self.commands.len() as isize - 1;
    }

    /// Re-pushes the top command with its namespace switched to `ns`.
    pub fn switch_ns(&mut self, ns: &str) {
        let Some(c) = self.top() else {
            return;
        };
        let mut i = Interpreter::new(&c);
        i.switch_ns(ns);
        let line = i.get_line();
        if i.ns_arg().is_some() && line != c {
            self.push(&line);
        }
    }

    pub fn clear(&mut self) {
        self.commands.clear();
        self.current = -1;
    }

    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pushed(limit: usize, cmds: &[&str]) -> History {
        let mut h = History::new(limit);
        for c in cmds {
            h.push(c);
        }
        h
    }

    #[test]
    fn clear() {
        let mut h = History::new(3);
        for i in 1..5 {
            h.push(&format!("cmd{i}"));
        }
        assert_eq!(h.list(), ["cmd1", "cmd2", "cmd3"]);
        h.clear();
        assert!(h.is_empty());
    }

    #[test]
    fn push() {
        let mut h = pushed(3, &["cmd1", "cmd2", "cmd3"]);
        h.push("cmd1");
        h.push("");
        assert_eq!(h.list(), ["cmd1", "cmd2", "cmd3"]);
        let h = pushed(5, &["po", "po", "DP"]);
        assert_eq!(h.list(), ["po", "dp"]);
    }

    #[test]
    fn top() {
        let three = ["cmd1", "cmd2", "cmd3"];
        for (push, pop, e) in [
            (&[][..], 0, None),
            (&three[..], 3, None),
            (&three[..], 0, Some("cmd3")),
            (&three[..], 1, Some("cmd2")),
            (&three[..], 2, Some("cmd1")),
        ] {
            let mut h = pushed(3, push);
            for _ in 0..pop {
                h.pop();
            }
            assert_eq!(h.top().as_deref(), e);
        }
    }

    #[test]
    fn back() {
        let three = ["cmd1", "cmd2", "cmd3"];
        for (push, pop, e) in [
            (&[][..], 0, None),
            (&three[..], 3, None),
            (&three[..], 0, Some("cmd2")),
            (&three[..], 1, Some("cmd1")),
            (&three[..], 2, None),
        ] {
            let mut h = pushed(3, push);
            for _ in 0..pop {
                h.pop();
            }
            assert_eq!(h.back().as_deref(), e);
        }
    }

    #[test]
    fn forward() {
        let three = ["cmd1", "cmd2", "cmd3"];
        for (push, back, e) in [
            (&[][..], 0, None),
            (&three[..], 2, Some("cmd2")),
            (&three[..], 1, Some("cmd3")),
            (&three[..], 3, Some("cmd2")),
            (&three[..], 0, None),
        ] {
            let mut h = pushed(3, push);
            for _ in 0..back {
                h.back();
            }
            assert_eq!(h.forward().as_deref(), e);
        }
    }

    #[test]
    fn push_after_back_truncates() {
        let mut h = pushed(10, &["a", "b", "c"]);
        h.back();
        h.back();
        // push() calls top() first, which resets the cursor to the end, so
        // nothing is dropped (k9s behavior).
        h.push("d");
        assert_eq!(h.list(), ["a", "b", "c", "d"]);
    }

    #[test]
    fn last_and_pop() {
        let mut h = pushed(10, &["a", "b", "c"]);
        assert_eq!(h.last(1).as_deref(), Some("c"));
        assert_eq!(h.last(2).as_deref(), Some("b"));
        assert_eq!(h.last(4), None);
        assert!(h.pop());
        assert_eq!(h.list(), ["a", "b"]);
        let mut e = History::new(3);
        assert!(!e.pop());
    }

    #[test]
    fn switch_ns() {
        let mut h = pushed(10, &["v1/pods default"]);
        h.switch_ns("kube-system");
        assert_eq!(h.list(), ["v1/pods default", "v1/pods kube-system"]);
        h.switch_ns("kube-system");
        assert_eq!(h.list().len(), 2);
        let mut h = pushed(10, &["v1/pods"]);
        h.switch_ns("fred");
        assert_eq!(h.list(), ["v1/pods", "v1/pods fred"]);
        let mut h = pushed(10, &["v1/pods fred"]);
        h.switch_ns("");
        assert_eq!(h.list(), ["v1/pods fred"]);
        let mut h = History::new(3);
        h.switch_ns("x");
        assert!(h.is_empty());
    }
}
