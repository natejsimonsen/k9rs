//! Command line parsing and help, matching k9s (cobra + pflag): flag
//! syntax, error messages and the help/usage layout.

use std::fmt::Write as _;

use super::{Cli, Sub};

const LONG_DESC: &str = "K9s is a CLI to view and manage your Kubernetes clusters.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Bool,
    Str,
    Float32,
    StrArray,
}

struct FlagDef {
    name: &'static str,
    short: Option<char>,
    kind: Kind,
    usage: &'static str,
    /// pflag DefValue (`""`, `false`, `[]`, `2`, ...).
    default: String,
}

fn f(
    name: &'static str,
    short: Option<char>,
    kind: Kind,
    usage: &'static str,
    default: &str,
) -> FlagDef {
    FlagDef {
        name,
        short,
        kind,
        usage,
        default: default.to_string(),
    }
}

fn root_flags(log_file: &str) -> Vec<FlagDef> {
    use Kind::*;
    let mut v = vec![
        f(
            "refresh",
            Some('r'),
            Float32,
            "Specify the default refresh rate as a float (sec)",
            "2",
        ),
        f(
            "logLevel",
            Some('l'),
            Str,
            "Specify a log level (error, warn, info, debug)",
            "info",
        ),
        f("logFile", None, Str, "Specify the log file", log_file),
        f("headless", None, Bool, "Turn K9s header off", "false"),
        f("logoless", None, Bool, "Turn K9s logo off", "false"),
        f("crumbsless", None, Bool, "Turn K9s crumbs off", "false"),
        f(
            "splashless",
            None,
            Bool,
            "Turn K9s splash screen off",
            "false",
        ),
        f(
            "invert",
            None,
            Bool,
            "Invert skin (dark to light, light to dark), preserving colors",
            "false",
        ),
        f(
            "all-namespaces",
            Some('A'),
            Bool,
            "Launch K9s in all namespaces",
            "false",
        ),
        f(
            "command",
            Some('c'),
            Str,
            "Overrides the default resource to load when the application launches",
            "",
        ),
        f(
            "readonly",
            None,
            Bool,
            "Sets readOnly mode by overriding readOnly configuration setting",
            "false",
        ),
        f(
            "write",
            None,
            Bool,
            "Sets write mode by overriding the readOnly configuration setting",
            "false",
        ),
        f(
            "screen-dump-dir",
            None,
            Str,
            "Sets a path to a dir for a screen dumps",
            "",
        ),
        f(
            "kubeconfig",
            None,
            Str,
            "Path to the kubeconfig file to use for CLI requests",
            "",
        ),
        f(
            "request-timeout",
            None,
            Str,
            "The length of time to wait before giving up on a single server request",
            "",
        ),
        f(
            "context",
            None,
            Str,
            "The name of the kubeconfig context to use",
            "",
        ),
        f(
            "cluster",
            None,
            Str,
            "The name of the kubeconfig cluster to use",
            "",
        ),
        f(
            "user",
            None,
            Str,
            "The name of the kubeconfig user to use",
            "",
        ),
        f(
            "namespace",
            Some('n'),
            Str,
            "If present, the namespace scope for this CLI request",
            "",
        ),
        f(
            "as",
            None,
            Str,
            "Username to impersonate for the operation",
            "",
        ),
        f(
            "as-group",
            None,
            StrArray,
            "Group to impersonate for the operation",
            "[]",
        ),
        f(
            "insecure-skip-tls-verify",
            None,
            Bool,
            "If true, the server's caCertFile will not be checked for validity",
            "false",
        ),
        f(
            "certificate-authority",
            None,
            Str,
            "Path to a cert file for the certificate authority",
            "",
        ),
        f(
            "client-key",
            None,
            Str,
            "Path to a client key file for TLS",
            "",
        ),
        f(
            "client-certificate",
            None,
            Str,
            "Path to a client certificate file for TLS",
            "",
        ),
        f(
            "token",
            None,
            Str,
            "Bearer token for authentication to the API server",
            "",
        ),
        f("help", Some('h'), Bool, "help for k9s", "false"),
    ];
    v.sort_by(|a, b| a.name.cmp(b.name));
    v
}

fn sub_flags(cmd: &str) -> Vec<FlagDef> {
    let mut v = Vec::new();
    v.push(FlagDef {
        name: "help",
        short: Some('h'),
        kind: Kind::Bool,
        usage: match cmd {
            "info" => "help for info",
            "version" => "help for version",
            _ => "help for help",
        },
        default: "false".into(),
    });
    if cmd == "version" {
        v.push(f(
            "short",
            Some('s'),
            Kind::Bool,
            "Prints K9s version info in short format",
            "false",
        ));
    }
    v
}

struct Cmd {
    name: &'static str,
    short: &'static str,
    long: &'static str,
    use_line: &'static str,
}

const SUBS: &[Cmd] = &[
    Cmd {
        name: "help",
        short: "Help about any command",
        long: "Help provides help for any command in the application.\nSimply type k9s help [path to command] for full details.",
        use_line: "k9s help [command]",
    },
    Cmd {
        name: "info",
        short: "List K9s configurations info",
        long: "",
        use_line: "k9s info",
    },
    Cmd {
        name: "version",
        short: "Print version/build info",
        long: "Print version/build information",
        use_line: "k9s version",
    },
];

fn is_zero_default(fd: &FlagDef) -> bool {
    match fd.kind {
        Kind::Bool => fd.default == "false",
        Kind::Str => fd.default.is_empty(),
        Kind::Float32 => fd.default == "0",
        Kind::StrArray => fd.default == "[]",
    }
}

fn go_quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// pflag FlagUsages.
fn flag_usages(defs: &[FlagDef]) -> String {
    let mut lines = Vec::new();
    let mut maxlen = 0;
    for fd in defs {
        let mut line = match fd.short {
            Some(s) => format!("  -{s}, --{}", fd.name),
            None => format!("      --{}", fd.name),
        };
        let varname = match fd.kind {
            Kind::Bool => "",
            Kind::Str => "string",
            Kind::Float32 => "float32",
            Kind::StrArray => "stringArray",
        };
        if !varname.is_empty() {
            line.push(' ');
            line.push_str(varname);
        }
        // pflag counts the \x00 separator in maxlen.
        maxlen = maxlen.max(line.len() + 1);
        let mut usage = fd.usage.to_string();
        if !is_zero_default(fd) {
            if fd.kind == Kind::Str {
                let _ = write!(usage, " (default {})", go_quote(&fd.default));
            } else {
                let _ = write!(usage, " (default {})", fd.default);
            }
        }
        lines.push((line, usage));
    }
    let mut out = String::new();
    for (l, u) in lines {
        let pad = " ".repeat(maxlen - l.len());
        let _ = writeln!(out, "{l} {pad} {u}");
    }
    out
}

/// cobra UsageString for the root command.
pub fn root_usage(log_file: &str) -> String {
    let pad = SUBS.iter().map(|c| c.name.len()).max().unwrap_or(0);
    let mut s = String::from("Usage:\n  k9s [flags]\n  k9s [command]\n\nAvailable Commands:\n");
    for c in SUBS {
        let _ = writeln!(s, "  {:<pad$} {}", c.name, c.short);
    }
    s.push_str("\nFlags:\n");
    s.push_str(flag_usages(&root_flags(log_file)).trim_end());
    s.push_str("\n\nUse \"k9s [command] --help\" for more information about a command.\n");
    s
}

/// cobra help output for the root command.
pub fn root_help(log_file: &str) -> String {
    format!("{LONG_DESC}\n\n{}", root_usage(log_file))
}

fn sub_usage(c: &Cmd) -> String {
    let mut s = format!("Usage:\n  {} [flags]\n", c.use_line);
    s.push_str("\nFlags:\n");
    s.push_str(flag_usages(&sub_flags(c.name)).trim_end());
    s.push('\n');
    s
}

/// cobra help output for a subcommand (`info`, `version`, `help`).
pub fn sub_help(name: &str) -> Option<String> {
    let c = SUBS.iter().find(|c| c.name == name)?;
    let desc = if c.long.is_empty() { c.short } else { c.long };
    Some(format!("{desc}\n\n{}", sub_usage(c)))
}

/// Outcome of parsing the command line.
#[derive(Debug, PartialEq)]
pub enum Parsed {
    /// Run with these flags.
    Run(Box<Cli>),
    /// Print this text to stdout and exit 0.
    Help(String),
    /// Print this text to stderr and exit 1.
    Fail(String),
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// cobra SuggestionsFor (minimum distance 2, or prefix match).
fn suggestions(arg: &str) -> String {
    let mut out = Vec::new();
    for c in SUBS {
        let d = levenshtein(&arg.to_lowercase(), c.name);
        if d <= 2 || c.name.starts_with(&arg.to_lowercase()) {
            out.push(c.name);
        }
    }
    if out.is_empty() {
        return String::new();
    }
    let mut s = String::from("\n\nDid you mean this?\n");
    for o in out {
        let _ = writeln!(s, "\t{o}");
    }
    s
}

type Values = Vec<(&'static str, String)>;

/// pflag parsing of `args` against `defs`. Returns flag values and
/// positional args.
fn parse_flags(defs: &[FlagDef], args: &[String]) -> Result<(Values, Vec<String>), String> {
    let mut vals: Values = Vec::new();
    let mut pos = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        i += 1;
        if a == "--" {
            pos.extend(args[i..].iter().cloned());
            break;
        }
        if let Some(rest) = a.strip_prefix("--") {
            let (name, inline) = match rest.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (rest, None),
            };
            let Some(fd) = defs.iter().find(|d| d.name == name) else {
                return Err(format!("unknown flag: --{name}"));
            };
            let v = match (fd.kind, inline) {
                (_, Some(v)) => v,
                (Kind::Bool, None) => "true".into(),
                (_, None) => {
                    if i < args.len() {
                        i += 1;
                        args[i - 1].clone()
                    } else {
                        return Err(format!("flag needs an argument: --{name}"));
                    }
                }
            };
            vals.push((fd.name, check(fd, &v, &format!("--{}", fd.name))?));
            continue;
        }
        if a.len() > 1 && a.starts_with('-') {
            let chars: Vec<char> = a[1..].chars().collect();
            let mut j = 0;
            while j < chars.len() {
                let c = chars[j];
                let Some(fd) = defs.iter().find(|d| d.short == Some(c)) else {
                    return Err(format!("unknown shorthand flag: '{c}' in {a}"));
                };
                let rest: String = chars[j + 1..].iter().collect();
                let flag_name = format!("-{c}, --{}", fd.name);
                if let Some(v) = rest.strip_prefix('=') {
                    vals.push((fd.name, check(fd, v, &flag_name)?));
                    break;
                }
                if fd.kind == Kind::Bool {
                    vals.push((fd.name, "true".into()));
                    j += 1;
                    continue;
                }
                let v = if !rest.is_empty() {
                    rest
                } else if i < args.len() {
                    i += 1;
                    args[i - 1].clone()
                } else {
                    return Err(format!("flag needs an argument: '{c}' in -{c}"));
                };
                vals.push((fd.name, check(fd, &v, &flag_name)?));
                break;
            }
            continue;
        }
        pos.push(a.clone());
    }
    Ok((vals, pos))
}

fn check(fd: &FlagDef, v: &str, shown: &str) -> Result<String, String> {
    match fd.kind {
        Kind::Bool => parse_go_bool(v)
            .map(|b| b.to_string())
            .ok_or_else(|| format!("invalid argument {} for \"{shown}\" flag: strconv.ParseBool: parsing {}: invalid syntax", go_quote(v), go_quote(v))),
        Kind::Float32 => v
            .parse::<f32>()
            .ok()
            .map(|_| v.to_string())
            .ok_or_else(|| format!("invalid argument {} for \"{shown}\" flag: strconv.ParseFloat: parsing {}: invalid syntax", go_quote(v), go_quote(v))),
        _ => Ok(v.to_string()),
    }
}

fn parse_go_bool(v: &str) -> Option<bool> {
    match v {
        "1" | "t" | "T" | "TRUE" | "true" | "True" => Some(true),
        "0" | "f" | "F" | "FALSE" | "false" | "False" => Some(false),
        _ => None,
    }
}

/// Index of the subcommand token (cobra Find/stripFlags), if any.
fn find_sub(defs: &[FlagDef], args: &[String]) -> Option<usize> {
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--" {
            return None;
        }
        if let Some(rest) = a.strip_prefix("--") {
            if !rest.contains('=') {
                if let Some(fd) = defs.iter().find(|d| d.name == rest) {
                    if fd.kind != Kind::Bool {
                        i += 1;
                    }
                }
            }
        } else if a.len() > 1 && a.starts_with('-') {
            let body = &a[1..];
            if !body.contains('=') && body.chars().count() == 1 {
                let c = body.chars().next().unwrap_or(' ');
                if let Some(fd) = defs.iter().find(|d| d.short == Some(c)) {
                    if fd.kind != Kind::Bool {
                        i += 1;
                    }
                }
            }
        } else {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Parse k9s arguments (without the program name).
pub fn parse(args: &[String], log_file: &str) -> Parsed {
    let defs = root_flags(log_file);
    let fail = |e: String, usage: String| Parsed::Fail(format!("Error: {e}\n{usage}\n"));
    if let Some(idx) = find_sub(&defs, args) {
        let name = args[idx].as_str();
        let Some(cmd) = SUBS.iter().find(|c| c.name == name) else {
            return Parsed::Fail(format!(
                "Error: unknown command {} for \"k9s\"{}\nRun 'k9s --help' for usage.\n",
                go_quote(name),
                suggestions(name)
            ));
        };
        let rest: Vec<String> = args
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != idx)
            .map(|(_, a)| a.clone())
            .collect();
        let sdefs = sub_flags(cmd.name);
        let (vals, pos) = match parse_flags(&sdefs, &rest) {
            Ok(v) => v,
            Err(e) => return fail(e, sub_usage(cmd)),
        };
        let flag = |n: &str| {
            vals.iter()
                .rev()
                .find(|(k, _)| *k == n)
                .map(|(_, v)| v.as_str())
        };
        if flag("help") == Some("true") {
            return Parsed::Help(sub_help(cmd.name).unwrap_or_default());
        }
        return match cmd.name {
            "help" => match pos.first() {
                None => Parsed::Help(root_help(log_file)),
                Some(t) => match sub_help(t) {
                    Some(h) => Parsed::Help(h),
                    None => Parsed::Help(format!(
                        "Unknown help topic [`{t}`]\n{}",
                        root_usage(log_file)
                    )),
                },
            },
            "info" => Parsed::Run(Box::new(Cli {
                command: Some(Sub::Info),
                ..Cli::default()
            })),
            _ => Parsed::Run(Box::new(Cli {
                command: Some(Sub::Version {
                    short: flag("short") == Some("true"),
                }),
                ..Cli::default()
            })),
        };
    }
    let (vals, pos) = match parse_flags(&defs, args) {
        Ok(v) => v,
        Err(e) => return fail(e, root_usage(log_file)),
    };
    let last = |n: &str| {
        vals.iter()
            .rev()
            .find(|(k, _)| *k == n)
            .map(|(_, v)| v.clone())
    };
    let b = |n: &str| last(n).as_deref() == Some("true");
    if b("help") {
        return Parsed::Help(root_help(log_file));
    }
    if let Some(p) = pos.first() {
        return Parsed::Fail(format!(
            "Error: unknown command {} for \"k9s\"{}\nRun 'k9s --help' for usage.\n",
            go_quote(p),
            suggestions(p)
        ));
    }
    let s = |n: &str| last(n).filter(|v| !v.is_empty());
    Parsed::Run(Box::new(Cli {
        command: None,
        help: false,
        refresh: last("refresh").and_then(|v| v.parse().ok()).unwrap_or(2.0),
        log_level: last("logLevel").unwrap_or_else(|| "info".into()),
        log_file: last("logFile"),
        headless: b("headless"),
        logoless: b("logoless"),
        crumbsless: b("crumbsless"),
        splashless: b("splashless"),
        invert: b("invert"),
        all_namespaces: b("all-namespaces"),
        command_override: s("command"),
        readonly: b("readonly"),
        write: b("write"),
        screen_dump_dir: s("screen-dump-dir"),
        kubeconfig: s("kubeconfig"),
        request_timeout: s("request-timeout"),
        context: s("context"),
        cluster: s("cluster"),
        user: s("user"),
        namespace: s("namespace"),
        impersonate: s("as"),
        impersonate_group: vals
            .iter()
            .filter(|(k, _)| *k == "as-group")
            .map(|(_, v)| v.clone())
            .collect(),
        insecure_skip_tls_verify: b("insecure-skip-tls-verify"),
        certificate_authority: s("certificate-authority"),
        client_key: s("client-key"),
        client_certificate: s("client-certificate"),
        token: s("token"),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Parsed {
        let v: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        parse(&v, "/tmp/k9s.log")
    }

    fn run(args: &[&str]) -> Cli {
        match p(args) {
            Parsed::Run(c) => *c,
            other => panic!("{args:?}: {other:?}"),
        }
    }

    #[test]
    fn defaults() {
        let c = run(&[]);
        assert_eq!(c.refresh, 2.0);
        assert_eq!(c.log_level, "info");
        assert!(c.log_file.is_none());
        assert!(c.command.is_none());
        assert!(!c.all_namespaces);
    }

    #[test]
    fn flags() {
        let c = run(&[
            "-A",
            "-n",
            "kube-system",
            "--context=ctx1",
            "-c",
            "dp",
            "-r",
            "5.5",
            "--readonly",
            "--write=false",
            "--as-group",
            "g1",
            "--as-group=g2",
            "--headless",
            "-lwarn",
            "--logFile",
            "/x.log",
            "--insecure-skip-tls-verify",
            "--request-timeout",
            "10s",
            "--screen-dump-dir",
            "/sd",
            "--invert=true",
            "--splashless",
            "--crumbsless",
            "--logoless",
            "--kubeconfig",
            "/k",
            "--cluster",
            "cl",
            "--user",
            "u",
            "--as",
            "bob",
            "--token",
            "t",
            "--certificate-authority",
            "ca",
            "--client-key",
            "ck",
            "--client-certificate",
            "cc",
        ]);
        assert!(c.all_namespaces);
        assert_eq!(c.namespace.as_deref(), Some("kube-system"));
        assert_eq!(c.context.as_deref(), Some("ctx1"));
        assert_eq!(c.command_override.as_deref(), Some("dp"));
        assert_eq!(c.refresh, 5.5);
        assert!(c.readonly && !c.write);
        assert_eq!(c.impersonate_group, vec!["g1", "g2"]);
        assert!(c.headless && c.logoless && c.crumbsless && c.splashless && c.invert);
        assert_eq!(c.log_level, "warn");
        assert_eq!(c.log_file.as_deref(), Some("/x.log"));
        assert!(c.insecure_skip_tls_verify);
        assert_eq!(c.request_timeout.as_deref(), Some("10s"));
        assert_eq!(c.screen_dump_dir.as_deref(), Some("/sd"));
        assert_eq!(c.kubeconfig.as_deref(), Some("/k"));
        assert_eq!(c.cluster.as_deref(), Some("cl"));
        assert_eq!(c.user.as_deref(), Some("u"));
        assert_eq!(c.impersonate.as_deref(), Some("bob"));
        assert_eq!(c.token.as_deref(), Some("t"));
        assert_eq!(c.certificate_authority.as_deref(), Some("ca"));
        assert_eq!(c.client_key.as_deref(), Some("ck"));
        assert_eq!(c.client_certificate.as_deref(), Some("cc"));
        // Combined shorthands and `-n=ns`.
        let c = run(&["-An=fred"]);
        assert!(c.all_namespaces);
        assert_eq!(c.namespace.as_deref(), Some("fred"));
        let c = run(&["-nfred"]);
        assert_eq!(c.namespace.as_deref(), Some("fred"));
    }

    #[test]
    fn subcommands() {
        assert_eq!(run(&["info"]).command, Some(Sub::Info));
        assert_eq!(
            run(&["version"]).command,
            Some(Sub::Version { short: false })
        );
        assert_eq!(
            run(&["version", "-s"]).command,
            Some(Sub::Version { short: true })
        );
        assert_eq!(
            run(&["version", "--short"]).command,
            Some(Sub::Version { short: true })
        );
        // Root flags before the sub command are found but not accepted by it.
        assert!(matches!(p(&["-n", "x", "info"]), Parsed::Fail(_)));
    }

    #[test]
    fn errors() {
        let Parsed::Fail(e) = p(&["--blee"]) else {
            panic!()
        };
        assert!(
            e.starts_with("Error: unknown flag: --blee\nUsage:\n  k9s [flags]\n"),
            "{e}"
        );
        assert!(e.ends_with("for more information about a command.\n\n"));
        let Parsed::Fail(e) = p(&["-x"]) else {
            panic!()
        };
        assert!(e.starts_with("Error: unknown shorthand flag: 'x' in -x\n"));
        let Parsed::Fail(e) = p(&["-n"]) else {
            panic!()
        };
        assert!(
            e.starts_with("Error: flag needs an argument: 'n' in -n\n"),
            "{e}"
        );
        let Parsed::Fail(e) = p(&["--namespace"]) else {
            panic!()
        };
        assert!(e.starts_with("Error: flag needs an argument: --namespace\n"));
        let Parsed::Fail(e) = p(&["-r", "fast"]) else {
            panic!()
        };
        assert!(e.starts_with(
            "Error: invalid argument \"fast\" for \"-r, --refresh\" flag: strconv.ParseFloat: parsing \"fast\": invalid syntax\n"
        ), "{e}");
        let Parsed::Fail(e) = p(&["--headless=maybe"]) else {
            panic!()
        };
        assert!(e.starts_with(
            "Error: invalid argument \"maybe\" for \"--headless\" flag: strconv.ParseBool: parsing \"maybe\": invalid syntax\n"
        ), "{e}");
        assert_eq!(
            p(&["inf"]),
            Parsed::Fail("Error: unknown command \"inf\" for \"k9s\"\n\nDid you mean this?\n\tinfo\n\nRun 'k9s --help' for usage.\n".into())
        );
        assert_eq!(
            p(&["zorg"]),
            Parsed::Fail(
                "Error: unknown command \"zorg\" for \"k9s\"\nRun 'k9s --help' for usage.\n".into()
            )
        );
        let Parsed::Fail(e) = p(&["version", "--blee"]) else {
            panic!()
        };
        assert_eq!(
            e,
            "Error: unknown flag: --blee\nUsage:\n  k9s version [flags]\n\nFlags:\n  -h, --help    help for version\n  -s, --short   Prints K9s version info in short format\n\n"
        );
    }

    #[test]
    fn help() {
        let Parsed::Help(h) = p(&["--help"]) else {
            panic!()
        };
        let expected = r#"K9s is a CLI to view and manage your Kubernetes clusters.

Usage:
  k9s [flags]
  k9s [command]

Available Commands:
  help    Help about any command
  info    List K9s configurations info
  version Print version/build info

Flags:
  -A, --all-namespaces                 Launch K9s in all namespaces
      --as string                      Username to impersonate for the operation
      --as-group stringArray           Group to impersonate for the operation
      --certificate-authority string   Path to a cert file for the certificate authority
      --client-certificate string      Path to a client certificate file for TLS
      --client-key string              Path to a client key file for TLS
      --cluster string                 The name of the kubeconfig cluster to use
  -c, --command string                 Overrides the default resource to load when the application launches
      --context string                 The name of the kubeconfig context to use
      --crumbsless                     Turn K9s crumbs off
      --headless                       Turn K9s header off
  -h, --help                           help for k9s
      --insecure-skip-tls-verify       If true, the server's caCertFile will not be checked for validity
      --invert                         Invert skin (dark to light, light to dark), preserving colors
      --kubeconfig string              Path to the kubeconfig file to use for CLI requests
      --logFile string                 Specify the log file (default "/tmp/k9s.log")
  -l, --logLevel string                Specify a log level (error, warn, info, debug) (default "info")
      --logoless                       Turn K9s logo off
  -n, --namespace string               If present, the namespace scope for this CLI request
      --readonly                       Sets readOnly mode by overriding readOnly configuration setting
  -r, --refresh float32                Specify the default refresh rate as a float (sec) (default 2)
      --request-timeout string         The length of time to wait before giving up on a single server request
      --screen-dump-dir string         Sets a path to a dir for a screen dumps
      --splashless                     Turn K9s splash screen off
      --token string                   Bearer token for authentication to the API server
      --user string                    The name of the kubeconfig user to use
      --write                          Sets write mode by overriding the readOnly configuration setting

Use "k9s [command] --help" for more information about a command.
"#;
        assert_eq!(h, expected);
        assert_eq!(p(&["-h"]), Parsed::Help(expected.into()));
        assert_eq!(p(&["help"]), Parsed::Help(expected.into()));
        assert_eq!(
            p(&["info", "-h"]),
            Parsed::Help("List K9s configurations info\n\nUsage:\n  k9s info [flags]\n\nFlags:\n  -h, --help   help for info\n".into())
        );
        assert_eq!(
            p(&["help", "version"]),
            Parsed::Help(
                "Print version/build information\n\nUsage:\n  k9s version [flags]\n\nFlags:\n  -h, --help    help for version\n  -s, --short   Prints K9s version info in short format\n".into()
            )
        );
        let Parsed::Help(h) = p(&["help", "zorg"]) else {
            panic!()
        };
        assert!(h.starts_with("Unknown help topic [`zorg`]\nUsage:\n"));
        let Parsed::Help(h) = p(&["help", "-h"]) else {
            panic!()
        };
        assert!(h.starts_with("Help provides help for any command in the application.\nSimply type k9s help [path to command] for full details.\n\nUsage:\n  k9s help [command] [flags]\n"));
    }
}
