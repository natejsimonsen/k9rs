//! Embeds the git commit and build date shown by `k9rs version` (k9s gets
//! them from goreleaser ldflags). `K9RS_COMMIT` / `K9RS_DATE` win; without
//! them and without a git checkout (e.g. a crates.io tarball) `version`
//! prints k9s's defaults, "dev" and "n/a".

use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    for v in ["K9RS_COMMIT", "K9RS_DATE", "SOURCE_DATE_EPOCH"] {
        println!("cargo:rerun-if-env-changed={v}");
    }
    let commit = std::env::var("K9RS_COMMIT").ok().or_else(git_commit);
    if let Some(c) = &commit {
        println!("cargo:rustc-env=K9RS_COMMIT={c}");
    }
    let date = std::env::var("K9RS_DATE").ok().or_else(|| {
        let epoch = std::env::var("SOURCE_DATE_EPOCH")
            .ok()
            .and_then(|s| s.parse::<u64>().ok());
        // Without a commit (crates.io tarball, Nix's default 1980
        // SOURCE_DATE_EPOCH) the date stays "n/a" too.
        commit.as_ref()?;
        match epoch {
            Some(e) => Some(rfc3339(e)),
            None => SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .map(|d| rfc3339(d.as_secs())),
        }
    });
    if let Some(d) = date {
        println!("cargo:rustc-env=K9RS_DATE={d}");
    }
}

fn git(args: &[&str]) -> Option<String> {
    let dir = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// HEAD's full sha, rebuilding when HEAD moves.
fn git_commit() -> Option<String> {
    let sha = git(&["rev-parse", "HEAD"])?;
    let mut watch = vec!["HEAD".to_string()];
    watch.extend(git(&["symbolic-ref", "-q", "HEAD"]));
    watch.push("packed-refs".into());
    for w in watch {
        // --git-path resolves worktree vs common dirs.
        if let Some(p) = git(&["rev-parse", "--path-format=absolute", "--git-path", &w]) {
            if std::path::Path::new(&p).exists() {
                println!("cargo:rerun-if-changed={p}");
            }
        }
    }
    Some(sha)
}

/// `2006-01-02T15:04:05Z` (goreleaser's .Date).
fn rfc3339(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Civil from days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}
