//! k9s log file setup: slog with the tint handler, RFC3339 times, levels
//! from `--logLevel`, appended to the log file (mode 0600).

use std::fmt::Write as _;
use std::path::Path;

use anyhow::{anyhow, Result};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::registry::LookupSpan;

/// cmd.parseLevel: unknown values mean info.
pub fn parse_level(level: &str) -> Level {
    match level {
        "debug" => Level::DEBUG,
        "warn" => Level::WARN,
        "error" => Level::ERROR,
        _ => Level::INFO,
    }
}

const FAINT: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

fn level_tag(l: &Level) -> String {
    match *l {
        Level::ERROR => "\x1b[91mERR\x1b[0m".into(),
        Level::WARN => "\x1b[93mWRN\x1b[0m".into(),
        Level::INFO => "\x1b[92mINF\x1b[0m".into(),
        _ => "DBG".into(),
    }
}

#[derive(Default)]
struct Fields {
    msg: String,
    attrs: String,
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.msg = value.to_string();
        } else {
            let v = if value.is_empty()
                || value.contains(char::is_whitespace)
                || value.contains('"')
                || value.contains('=')
            {
                format!("{value:?}")
            } else {
                value.to_string()
            };
            let _ = write!(self.attrs, " {FAINT}{}={RESET}{v}", field.name());
        }
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.msg = format!("{value:?}");
        } else {
            self.record_str(field, &format!("{value:?}"));
        }
    }
}

/// tint.Handler style line: `<time> <LVL> <msg> key=value ...`.
pub struct TintFormat;

impl<S, N> FormatEvent<S, N> for TintFormat
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        _ctx: &FmtContext<'_, S, N>,
        mut w: Writer<'_>,
        event: &Event<'_>,
    ) -> std::fmt::Result {
        let mut f = Fields::default();
        event.record(&mut f);
        writeln!(
            w,
            "{}",
            format_line(
                &chrono::Local::now(),
                event.metadata().level(),
                &f.msg,
                &f.attrs
            )
        )
    }
}

fn format_line(
    t: &chrono::DateTime<chrono::Local>,
    level: &Level,
    msg: &str,
    attrs: &str,
) -> String {
    let ts = t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    format!("{FAINT}{ts}{RESET} {} {msg}{attrs}", level_tag(level))
}

/// Open (append/create, 0600) the log file and install the subscriber.
pub fn init(path: &Path, level: &str) -> Result<()> {
    let mut o = std::fs::OpenOptions::new();
    o.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(crate::config::paths::DEFAULT_FILE_MOD);
    }
    let file = o
        .open(path)
        .map_err(|e| anyhow!("log file {:?} init failed: {e}", path.to_string_lossy()))?;
    let sub = tracing_subscriber::fmt()
        .with_max_level(parse_level(level))
        .with_writer(std::sync::Mutex::new(file))
        .event_format(TintFormat)
        .finish();
    // Already set (tests or embedding): keep the existing one.
    let _ = tracing::subscriber::set_global_default(sub);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels() {
        assert_eq!(parse_level("debug"), Level::DEBUG);
        assert_eq!(parse_level("warn"), Level::WARN);
        assert_eq!(parse_level("error"), Level::ERROR);
        assert_eq!(parse_level("info"), Level::INFO);
        assert_eq!(parse_level("blee"), Level::INFO);
    }

    #[test]
    fn line_format() {
        use chrono::TimeZone;
        let t = chrono::Local.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap();
        let l = format_line(
            &t,
            &Level::INFO,
            "K9s starting up...",
            " \x1b[2mctx=\x1b[0mfred",
        );
        assert!(l.starts_with("\x1b[2m2026-01-02T03:04:05"), "{l}");
        assert!(
            l.ends_with("\x1b[0m \x1b[92mINF\x1b[0m K9s starting up... \x1b[2mctx=\x1b[0mfred"),
            "{l}"
        );
        let mut f = Fields::default();
        f.record_str(&field("path"), "/a b");
        assert_eq!(f.attrs, " \x1b[2mpath=\x1b[0m\"/a b\"");
    }

    fn field(name: &'static str) -> Field {
        struct Cs;
        impl tracing::callsite::Callsite for Cs {
            fn set_interest(&self, _: tracing::subscriber::Interest) {}
            fn metadata(&self) -> &tracing::Metadata<'_> {
                &META
            }
        }
        static CS: Cs = Cs;
        static META: tracing::Metadata<'static> = tracing::Metadata::new(
            "test",
            "test",
            Level::INFO,
            None,
            None,
            None,
            tracing::field::FieldSet::new(&["path"], tracing::callsite::Identifier(&CS)),
            tracing::metadata::Kind::EVENT,
        );
        let _ = name;
        META.fields().field("path").unwrap()
    }

    #[test]
    fn init_creates_file() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("k9s.log");
        init(&p, "debug").unwrap();
        assert!(p.exists());
        let e = init(&tmp.path().join("nope/k9s.log"), "info")
            .unwrap_err()
            .to_string();
        assert!(e.starts_with("log file "), "{e}");
    }
}
