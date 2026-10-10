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
        // k9s logs its own slog lines only, not the client libraries'.
        if !event.metadata().target().starts_with("k9rs") {
            return Ok(());
        }
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

/// slog TextHandler value quoting (needsQuoting + strconv.Quote).
fn slog_value(v: &str) -> String {
    let quote = v.is_empty()
        || v.chars()
            .any(|c| c == ' ' || c == '=' || c == '"' || c.is_whitespace() || c.is_control());
    if quote {
        let mut q = String::from("\"");
        for c in v.chars() {
            match c {
                '"' => q.push_str("\\\""),
                '\\' => q.push_str("\\\\"),
                '\n' => q.push_str("\\n"),
                '\t' => q.push_str("\\t"),
                '\r' => q.push_str("\\r"),
                c if c.is_control() => {
                    let _ = write!(q, "\\x{:02x}", c as u32);
                }
                c => q.push(c),
            }
        }
        q.push('"');
        q
    } else {
        v.to_string()
    }
}

#[derive(Default)]
struct PlainFields {
    msg: String,
    attrs: String,
}

impl Visit for PlainFields {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.msg = value.to_string();
        } else {
            let _ = write!(self.attrs, " {}={}", field.name(), slog_value(value));
        }
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.record_str(field, &format!("{value:?}"));
    }
}

/// Go's default slog logger line: `2006/01/02 15:04:05 LEVEL msg k=v`.
struct GoDefaultFormat;

fn go_level(l: &Level) -> &'static str {
    match *l {
        Level::ERROR => "ERROR",
        Level::WARN => "WARN",
        Level::INFO => "INFO",
        _ => "DEBUG",
    }
}

impl<S, N> FormatEvent<S, N> for GoDefaultFormat
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
        // Only k9s's own logs, not the client libraries'.
        if !event.metadata().target().starts_with("k9rs") {
            return Ok(());
        }
        let mut f = PlainFields::default();
        event.record(&mut f);
        writeln!(
            w,
            "{} {} {}{}",
            chrono::Local::now().format("%Y/%m/%d %H:%M:%S"),
            go_level(event.metadata().level()),
            f.msg,
            f.attrs
        )
    }
}

/// Log to stderr like Go's default slog logger (k9s sub commands run
/// before the log file is set up).
pub fn init_stderr() {
    let sub = tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .with_writer(std::io::stderr)
        .event_format(GoDefaultFormat)
        .finish();
    let _ = tracing::subscriber::set_global_default(sub);
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

    #[test]
    fn go_default_values() {
        assert_eq!(slog_value("fred"), "fred");
        assert_eq!(slog_value(""), "\"\"");
        assert_eq!(
            slog_value("open /a b: no such file"),
            "\"open /a b: no such file\""
        );
        assert_eq!(slog_value("a\"b"), "\"a\\\"b\"");
        let mut f = PlainFields::default();
        f.record_str(&field("path"), "/x");
        assert_eq!(f.attrs, " path=/x");
        assert_eq!(go_level(&Level::WARN), "WARN");
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
