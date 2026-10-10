//! `k9s info` and `k9s version` output (cmd/info.go, cmd/version.go).

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::config::paths::Locations;
use crate::config::Config;

/// ANSI cyan (color.Cyan).
pub const CYAN: u8 = 36;
/// ANSI red (color.Red).
pub const RED: u8 = 31;

/// color.Colorize.
pub fn colorize(s: &str, c: u8) -> String {
    if c == 0 {
        return s.to_string();
    }
    format!("\x1b[{c}m{s}\x1b[0m")
}

fn print_logo(out: &mut dyn Write, c: u8) -> std::io::Result<()> {
    for l in crate::logo::LOGO_SMALL {
        writeln!(out, "{}", colorize(l, c))?;
    }
    writeln!(out)
}

/// cmd/root.go recover: red logo, then `Boom!! <err>.`.
pub fn print_boom(out: &mut dyn Write, err: &str) -> std::io::Result<()> {
    print_logo(out, RED)?;
    write!(out, "{}", colorize("Boom!! ", RED))?;
    writeln!(out, "{err}.")
}

/// printTuple: `color` 0 prints plain.
fn print_tuple(
    out: &mut dyn Write,
    width: usize,
    section: &str,
    value: &str,
    color: u8,
) -> std::io::Result<()> {
    if color != 0 {
        writeln!(
            out,
            "{:<width$} {value}",
            colorize(&format!("{section}:"), color)
        )
    } else {
        writeln!(out, "{section:<width$} {value}")
    }
}

/// cmd.printVersion.
pub fn print_version(
    out: &mut dyn Write,
    short: bool,
    version: &str,
    commit: &str,
    date: &str,
) -> std::io::Result<()> {
    let color = if short { 0 } else { CYAN };
    if !short {
        print_logo(out, color)?;
    }
    print_tuple(out, 20, "Version", version, color)?;
    print_tuple(out, 20, "Commit", commit, color)?;
    print_tuple(out, 20, "Date", date, color)
}

/// cmd.printInfo.
pub fn print_info(
    out: &mut dyn Write,
    version: &str,
    locs: &Locations,
    log_file: &Path,
    screen_dumps: &Path,
) -> std::io::Result<()> {
    let w = 27;
    let p = |p: &Path| p.to_string_lossy().into_owned();
    print_logo(out, CYAN)?;
    print_tuple(out, w, "Version", version, CYAN)?;
    print_tuple(out, w, "Config", &p(&locs.config_file), CYAN)?;
    print_tuple(out, w, "Custom Views", &p(&locs.views_file), CYAN)?;
    print_tuple(out, w, "Plugins", &p(&locs.plugins_file), CYAN)?;
    print_tuple(out, w, "Hotkeys", &p(&locs.hotkeys_file), CYAN)?;
    print_tuple(out, w, "Aliases", &p(&locs.aliases_file), CYAN)?;
    print_tuple(out, w, "Skins", &p(&locs.skins_dir), CYAN)?;
    print_tuple(out, w, "Context Configs", &p(&locs.contexts_dir), CYAN)?;
    print_tuple(out, w, "Logs", &p(log_file), CYAN)?;
    print_tuple(out, w, "Benchmarks", &p(&locs.benchmarks_dir), CYAN)?;
    print_tuple(out, w, "ScreenDumps", &p(screen_dumps), CYAN)
}

/// Go *fs.PathError text, e.g. `open /x: no such file or directory`.
pub fn go_path_error(op: &str, path: &Path, e: &std::io::Error) -> String {
    let mut msg = e.to_string();
    if let Some(i) = msg.find(" (os error ") {
        msg.truncate(i);
    }
    let mut cs = msg.chars();
    let msg = match cs.next() {
        Some(c) => c.to_lowercase().chain(cs).collect(),
        None => msg,
    };
    format!("{op} {}: {msg}", path.display())
}

/// getScreenDumpDirForInfo: the config file `screenDumpDir`, else `dumps`.
pub fn screen_dump_dir_for_info(config_file: Option<&Path>, dumps: &Path) -> PathBuf {
    let Some(f) = config_file.filter(|f| !f.as_os_str().is_empty()) else {
        return dumps.to_path_buf();
    };
    let bb = match std::fs::read_to_string(f) {
        Ok(b) => b,
        Err(e) => {
            let e = go_path_error("open", f, &e);
            tracing::error!(error = %e, "Unable to reads k9rs config file");
            return dumps.to_path_buf();
        }
    };
    let cfg: Config = match serde_yaml::from_str::<serde_yaml::Value>(&bb)
        .map_err(anyhow::Error::from)
        .and_then(|v| match v.get("k9s") {
            Some(k) if !k.is_null() => Ok(Some(serde_yaml::from_value(k.clone())?)),
            _ => Ok(None),
        }) {
        Ok(Some(k)) => Config { k9s: k },
        Ok(None) => return dumps.to_path_buf(),
        Err(e) => {
            tracing::error!(error = %e, "Unable to unmarshal k9rs config file");
            return dumps.to_path_buf();
        }
    };
    let mut k = cfg.k9s;
    k.app_screen_dump_dir_or(dumps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::paths::{MapEnv, Xdg, K9S_ENV_CONFIG_DIR};

    fn td(p: &str) -> PathBuf {
        format!("{}/src/config/testdata/cmd/{p}", env!("CARGO_MANIFEST_DIR")).into()
    }

    #[test]
    fn screen_dumps_for_info() {
        let d = Path::new("/state/k9s/screen-dumps");
        assert_eq!(
            screen_dump_dir_for_info(Some(&td("k9s.yaml")), d),
            PathBuf::from("/tmp")
        );
        assert_eq!(screen_dump_dir_for_info(None, d), d);
        assert_eq!(screen_dump_dir_for_info(Some(Path::new("")), d), d);
        assert_eq!(screen_dump_dir_for_info(Some(Path::new("invalid")), d), d);
        assert_eq!(screen_dump_dir_for_info(Some(&td("k9s1.yaml")), d), d);
    }

    #[test]
    fn go_errors() {
        let e = std::fs::read("/definitely/not/here").unwrap_err();
        assert_eq!(
            go_path_error("open", Path::new("/definitely/not/here"), &e),
            "open /definitely/not/here: no such file or directory"
        );
    }

    fn colored(sec: &str, val: &str, width: usize) -> String {
        let c = format!("\x1b[36m{sec}:\x1b[0m");
        format!("{c}{} {val}", " ".repeat(width - c.len()))
    }

    #[test]
    fn version_output() {
        let mut out = Vec::new();
        print_version(&mut out, true, "v0.50.18", "abc", "2026-01-01").unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!(
                "Version{} v0.50.18\nCommit{} abc\nDate{} 2026-01-01\n",
                " ".repeat(13),
                " ".repeat(14),
                " ".repeat(16)
            )
        );
        let mut out = Vec::new();
        print_version(&mut out, false, "v0.50.18", "abc", "n/a").unwrap();
        let s = String::from_utf8(out).unwrap();
        let logo: String = crate::logo::LOGO_SMALL
            .iter()
            .map(|l| format!("\x1b[36m{l}\x1b[0m\n"))
            .collect();
        assert_eq!(
            s,
            format!(
                "{logo}\n{}\n{}\n{}\n",
                colored("Version", "v0.50.18", 20),
                colored("Commit", "abc", 20),
                colored("Date", "n/a", 20)
            )
        );
        assert_eq!(colored("Version", "x", 20), "\x1b[36mVersion:\x1b[0m    x");
    }

    #[test]
    fn info_output() {
        let env = MapEnv::new(&[(K9S_ENV_CONFIG_DIR, "/cfg")]);
        let locs = Locations::resolve(&env, &Xdg::resolve(&env, Path::new("/h"), false));
        let mut out = Vec::new();
        print_info(
            &mut out,
            "v0.50.18",
            &locs,
            Path::new("/tmp/fred/k9s/k9s.log"),
            Path::new("/cfg/screen-dumps"),
        )
        .unwrap();
        let s = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = s.lines().collect();
        assert_eq!(lines.len(), 6 + 1 + 11);
        assert_eq!(lines[6], "");
        let e = [
            ("Version", "v0.50.18"),
            ("Config", "/cfg/config.yaml"),
            ("Custom Views", "/cfg/views.yaml"),
            ("Plugins", "/cfg/plugins.yaml"),
            ("Hotkeys", "/cfg/hotkeys.yaml"),
            ("Aliases", "/cfg/aliases.yaml"),
            ("Skins", "/cfg/skins"),
            ("Context Configs", "/cfg/clusters"),
            ("Logs", "/tmp/fred/k9s/k9s.log"),
            ("Benchmarks", "/cfg/benchmarks"),
            ("ScreenDumps", "/cfg/screen-dumps"),
        ];
        for (i, (k, v)) in e.iter().enumerate() {
            assert_eq!(lines[7 + i], colored(k, v, 27));
        }
        assert_eq!(lines[7], "\x1b[36mVersion:\x1b[0m           v0.50.18");
        assert_eq!(colorize("x", 0), "x");
    }

    #[test]
    fn boom_message() {
        let mut out = Vec::new();
        print_boom(&mut out, "index out of bounds").unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("\x1b[31m ____  __ ________"), "{s:?}");
        assert!(
            s.ends_with("\x1b[31mBoom!! \x1b[0mindex out of bounds.\n"),
            "{s:?}"
        );
    }
}
