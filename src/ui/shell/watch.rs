//! Config file watchers (ui/config.go ConfigWatcher, SkinsDirWatcher,
//! CustomViewsWatcher), started only when `k9s.ui.reactive` is set. k9s uses
//! fsnotify; this polls file stamps, which yields the same create / write /
//! remove notifications for the reload logic in the app.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

/// How often the watched files are checked.
pub(crate) const POLL: Duration = Duration::from_millis(250);

/// Which watcher saw the change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Watch {
    /// config.yaml (Create/Write only).
    Config,
    /// The active context's config.yaml (Create/Write only).
    ContextConfig,
    /// A file in the skins dir (any change but chmod).
    Skin(PathBuf),
    /// views.yaml (any change but chmod).
    Views,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Change {
    Created,
    Written,
    Removed,
}

/// What a watcher looks at; `context` follows context switches (k9s
/// restarts its watchers in Halt/Resume).
#[derive(Clone, Debug)]
pub(crate) struct Targets {
    pub config: PathBuf,
    pub context: Arc<Mutex<Option<PathBuf>>>,
    /// Watched only when it exists at startup (w.Add of a missing dir fails).
    pub skins_dir: Option<PathBuf>,
    /// Watched only when it exists at startup.
    pub views: Option<PathBuf>,
}

type Stamp = (SystemTime, u64);

fn stamp(p: &Path) -> Option<Stamp> {
    let m = std::fs::metadata(p).ok()?;
    if !m.is_file() {
        return None;
    }
    Some((m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len()))
}

/// Snapshots of the watched files and the changes between two polls.
#[derive(Default)]
pub(crate) struct Poller {
    seen: HashMap<PathBuf, Stamp>,
    primed: bool,
}

impl Poller {
    fn files(t: &Targets) -> Vec<(Watch, PathBuf)> {
        let mut out = vec![(Watch::Config, t.config.clone())];
        if let Some(c) = t.context.lock().ok().and_then(|c| c.clone()) {
            out.push((Watch::ContextConfig, c));
        }
        if let Some(v) = &t.views {
            out.push((Watch::Views, v.clone()));
        }
        if let Some(d) = &t.skins_dir {
            if let Ok(rd) = std::fs::read_dir(d) {
                let mut ff: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
                ff.sort();
                for f in ff {
                    out.push((Watch::Skin(f.clone()), f));
                }
            }
        }
        out
    }

    /// Changes since the previous poll (none on the first one).
    pub(crate) fn poll(&mut self, t: &Targets) -> Vec<(Watch, Change)> {
        let files = Self::files(t);
        let mut now: HashMap<PathBuf, Stamp> = HashMap::new();
        let mut out = Vec::new();
        for (w, p) in &files {
            let cur = stamp(p);
            let prev = self.seen.get(p).copied();
            if let Some(s) = cur {
                now.insert(p.clone(), s);
            }
            if !self.primed {
                continue;
            }
            let change = match (prev, cur) {
                (None, Some(_)) => Some(Change::Created),
                (Some(a), Some(b)) if a != b => Some(Change::Written),
                (Some(_), None) => Some(Change::Removed),
                _ => None,
            };
            if let Some(c) = change {
                out.push((w.clone(), c));
            }
        }
        // Skin files that vanished from the dir listing.
        if self.primed {
            if let Some(d) = &t.skins_dir {
                let mut gone: Vec<&PathBuf> = self
                    .seen
                    .keys()
                    .filter(|p| p.parent() == Some(d.as_path()) && !now.contains_key(*p))
                    .filter(|p| !files.iter().any(|(_, f)| f == *p))
                    .collect();
                gone.sort();
                for p in gone {
                    out.push((Watch::Skin(p.clone()), Change::Removed));
                }
            }
        }
        self.seen = now;
        self.primed = true;
        out
    }
}

/// Whether the app reloads for this change (the watchers' event filters).
pub(crate) fn wanted(w: &Watch, c: Change) -> bool {
    match w {
        Watch::Config | Watch::ContextConfig => c != Change::Removed,
        Watch::Skin(_) | Watch::Views => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bump(p: &Path, body: &str) {
        std::fs::write(p, body).unwrap();
        // Make the stamp differ even within the mtime granularity.
        let t = SystemTime::now() + Duration::from_secs(5);
        let f = std::fs::File::options().write(true).open(p).unwrap();
        f.set_modified(t).unwrap();
    }

    #[test]
    fn reports_creates_writes_and_removes() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.yaml");
        let skins = dir.path().join("skins");
        std::fs::create_dir_all(&skins).unwrap();
        std::fs::write(&cfg, "k9s: {}\n").unwrap();
        let ctxf = dir.path().join("ctx.yaml");
        let t = Targets {
            config: cfg.clone(),
            context: Arc::new(Mutex::new(Some(ctxf.clone()))),
            skins_dir: Some(skins.clone()),
            views: Some(dir.path().join("views.yaml")),
        };
        let mut p = Poller::default();
        assert!(p.poll(&t).is_empty());
        assert!(p.poll(&t).is_empty());
        bump(&cfg, "k9s:\n  refreshRate: 3\n");
        std::fs::write(&ctxf, "k9s: {}\n").unwrap();
        let sk = skins.join("dracula.yaml");
        std::fs::write(&sk, "k9s: {}\n").unwrap();
        let got = p.poll(&t);
        assert_eq!(
            got,
            vec![
                (Watch::Config, Change::Written),
                (Watch::ContextConfig, Change::Created),
                (Watch::Skin(sk.clone()), Change::Created),
            ]
        );
        std::fs::remove_file(&sk).unwrap();
        std::fs::remove_file(&ctxf).unwrap();
        let got = p.poll(&t);
        assert_eq!(
            got,
            vec![
                (Watch::ContextConfig, Change::Removed),
                (Watch::Skin(sk), Change::Removed),
            ]
        );
        assert!(!wanted(&Watch::ContextConfig, Change::Removed));
        assert!(wanted(&Watch::Views, Change::Removed));
    }
}
