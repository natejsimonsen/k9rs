//! Command line parsing, help and shell completion, matching k9s (cobra
//! v1.10 + pflag): command lookup, flag syntax, error messages, the
//! help/usage layout and `completion` / `__complete`.

use std::fmt::Write as _;

use super::{Cli, Sub};

const SHORT_DESC: &str = "A graphical CLI for your Kubernetes cluster management.";
const LONG_DESC: &str = "K9rs is a CLI to view and manage your Kubernetes clusters.";

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
        f("headless", None, Bool, "Turn K9rs header off", "false"),
        f("logoless", None, Bool, "Turn K9rs logo off", "false"),
        f("crumbsless", None, Bool, "Turn K9rs crumbs off", "false"),
        f(
            "splashless",
            None,
            Bool,
            "Turn K9rs splash screen off",
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
            "Launch K9rs in all namespaces",
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
    ];
    v.sort_by(|a, b| a.name.cmp(b.name));
    v
}

/// A shell `k9s completion <shell>` generates a script for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    Powershell,
}

impl Shell {
    /// cobra Gen*Completion output for `k9s`; `desc` false asks the script
    /// for `__completeNoDesc`.
    pub fn script(self, desc: bool) -> String {
        let s = match self {
            Shell::Bash => include_str!("completion/bash.txt"),
            Shell::Zsh => include_str!("completion/zsh.txt"),
            Shell::Fish => include_str!("completion/fish.txt"),
            Shell::Powershell => include_str!("completion/powershell.txt"),
        };
        if desc {
            s.to_string()
        } else {
            s.replacen(" __complete ", " __completeNoDesc ", 1)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Id {
    Root,
    Help,
    Info,
    Version,
    Completion,
    Shell(Shell),
    Complete,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Args {
    /// cobra `Args: nil` (legacyArgs).
    Legacy,
    None,
    MinOne,
}

/// A cobra command.
struct Cmd {
    id: Id,
    /// cobra Use.
    usage: &'static str,
    aliases: &'static [&'static str],
    short: String,
    long: String,
    runnable: bool,
    args: Args,
    disable_flags_in_use_line: bool,
    disable_flag_parsing: bool,
    hidden: bool,
    /// Local flags, sorted by name (help included).
    flags: Vec<FlagDef>,
    /// Names of the persistent flags among `flags`.
    persistent: &'static [&'static str],
    parent: Option<usize>,
    /// Children, sorted by name.
    subs: Vec<usize>,
}

impl Cmd {
    fn name(&self) -> &'static str {
        self.usage.split(' ').next().unwrap_or(self.usage)
    }
}

fn help_flag(name: &str) -> FlagDef {
    FlagDef {
        name: "help",
        short: Some('h'),
        kind: Kind::Bool,
        usage: Box::leak(format!("help for {name}").into_boxed_str()),
        default: "false".into(),
    }
}

/// k9s rootCmd with cobra's default help, completion and __complete
/// commands.
struct Tree {
    cmds: Vec<Cmd>,
}

const ROOT: usize = 0;

impl Tree {
    fn new(log_file: &str) -> Tree {
        let mut t = Tree { cmds: Vec::new() };
        t.add(
            None,
            Id::Root,
            "k9rs",
            SHORT_DESC,
            LONG_DESC,
            true,
            root_flags(log_file),
        );
        let shell_short = |s: &str| format!("Generate the autocompletion script for {s}");
        let no_desc = || {
            vec![f(
                "no-descriptions",
                None,
                Kind::Bool,
                "disable completion descriptions",
                "false",
            )]
        };
        t.add(
            Some(ROOT),
            Id::Help,
            "help [command]",
            "Help about any command",
            "Help provides help for any command in the application.\nSimply type k9rs help [path to command] for full details.",
            true,
            vec![],
        );
        t.add(
            Some(ROOT),
            Id::Info,
            "info",
            "List K9rs configurations info",
            "",
            true,
            vec![],
        );
        let v = t.add(
            Some(ROOT),
            Id::Version,
            "version",
            "Print version/build info",
            "Print version/build information",
            true,
            vec![f(
                "short",
                Some('s'),
                Kind::Bool,
                "Prints K9rs version info in short format",
                "false",
            )],
        );
        t.cmds[v].persistent = &["short"];
        let c = t.add(
            Some(ROOT),
            Id::Completion,
            "completion",
            "Generate the autocompletion script for the specified shell",
            "Generate the autocompletion script for k9rs for the specified shell.\nSee each sub-command's help for details on how to use the generated script.\n",
            false,
            vec![],
        );
        t.cmds[c].args = Args::None;
        let bash = t.add(
            Some(c),
            Id::Shell(Shell::Bash),
            "bash",
            &shell_short("bash"),
            "Generate the autocompletion script for the bash shell.

This script depends on the 'bash-completion' package.
If it is not installed already, you can install it via your OS's package manager.

To load completions in your current shell session:

\tsource <(k9rs completion bash)

To load completions for every new session, execute once:

#### Linux:

\tk9rs completion bash > /etc/bash_completion.d/k9rs

#### macOS:

\tk9rs completion bash > $(brew --prefix)/etc/bash_completion.d/k9rs

You will need to start a new shell for this setup to take effect.
",
            true,
            no_desc(),
        );
        t.cmds[bash].disable_flags_in_use_line = true;
        t.add(
            Some(c),
            Id::Shell(Shell::Zsh),
            "zsh",
            &shell_short("zsh"),
            "Generate the autocompletion script for the zsh shell.

If shell completion is not already enabled in your environment you will need
to enable it.  You can execute the following once:

\techo \"autoload -U compinit; compinit\" >> ~/.zshrc

To load completions in your current shell session:

\tsource <(k9rs completion zsh)

To load completions for every new session, execute once:

#### Linux:

\tk9rs completion zsh > \"${fpath[1]}/_k9rs\"

#### macOS:

\tk9rs completion zsh > $(brew --prefix)/share/zsh/site-functions/_k9rs

You will need to start a new shell for this setup to take effect.
",
            true,
            no_desc(),
        );
        t.add(
            Some(c),
            Id::Shell(Shell::Fish),
            "fish",
            &shell_short("fish"),
            "Generate the autocompletion script for the fish shell.

To load completions in your current shell session:

\tk9rs completion fish | source

To load completions for every new session, execute once:

\tk9rs completion fish > ~/.config/fish/completions/k9rs.fish

You will need to start a new shell for this setup to take effect.
",
            true,
            no_desc(),
        );
        t.add(
            Some(c),
            Id::Shell(Shell::Powershell),
            "powershell",
            &shell_short("powershell"),
            "Generate the autocompletion script for powershell.

To load completions in your current shell session:

\tk9rs completion powershell | Out-String | Invoke-Expression

To load completions for every new session, add the output of the above command
to your powershell profile.
",
            true,
            no_desc(),
        );
        for i in [bash, bash + 1, bash + 2, bash + 3] {
            t.cmds[i].args = Args::None;
        }
        let cc = t.add(
            Some(ROOT),
            Id::Complete,
            "__complete [command-line]",
            "Request shell completion choices for the specified command-line",
            "__complete is a special command that is used by the shell completion logic\nto request completion choices for the specified command-line.",
            true,
            vec![],
        );
        let cmd = &mut t.cmds[cc];
        cmd.aliases = &["__completeNoDesc"];
        cmd.disable_flags_in_use_line = true;
        cmd.disable_flag_parsing = true;
        cmd.hidden = true;
        cmd.args = Args::MinOne;
        t
    }

    #[allow(clippy::too_many_arguments)]
    fn add(
        &mut self,
        parent: Option<usize>,
        id: Id,
        usage: &str,
        short: &str,
        long: &str,
        runnable: bool,
        mut flags: Vec<FlagDef>,
    ) -> usize {
        let usage: &'static str = Box::leak(usage.to_string().into_boxed_str());
        let name = usage.split(' ').next().unwrap_or(usage);
        flags.push(help_flag(name));
        flags.sort_by(|a, b| a.name.cmp(b.name));
        let i = self.cmds.len();
        self.cmds.push(Cmd {
            id,
            usage,
            aliases: &[],
            short: short.into(),
            long: long.into(),
            runnable,
            args: Args::Legacy,
            disable_flags_in_use_line: false,
            disable_flag_parsing: false,
            hidden: false,
            flags,
            persistent: &[],
            parent,
            subs: vec![],
        });
        if let Some(p) = parent {
            self.cmds[p].subs.push(i);
            let mut subs = std::mem::take(&mut self.cmds[p].subs);
            subs.sort_by(|a, b| self.cmds[*a].name().cmp(self.cmds[*b].name()));
            self.cmds[p].subs = subs;
        }
        i
    }

    fn path(&self, c: usize) -> String {
        match self.cmds[c].parent {
            Some(p) => format!("{} {}", self.path(p), self.cmds[c].name()),
            None => self.cmds[c].name().to_string(),
        }
    }

    /// cobra IsAvailableCommand.
    fn available(&self, c: usize) -> bool {
        let cmd = &self.cmds[c];
        if cmd.hidden || cmd.id == Id::Help {
            return false;
        }
        cmd.runnable || self.has_available_subs(c)
    }

    fn has_available_subs(&self, c: usize) -> bool {
        self.cmds[c].subs.iter().any(|s| self.available(*s))
    }

    /// cobra NamePadding.
    fn name_padding(&self, c: usize) -> usize {
        const MIN_NAME_PADDING: usize = 11;
        let max = match self.cmds[c].parent {
            Some(p) => self.cmds[p]
                .subs
                .iter()
                .map(|s| self.cmds[*s].name().len())
                .max()
                .unwrap_or(0),
            None => 0,
        };
        max.max(MIN_NAME_PADDING)
    }

    /// cobra UseLine.
    fn use_line(&self, c: usize) -> String {
        let cmd = &self.cmds[c];
        let mut l = match cmd.parent {
            Some(p) => format!("{} {}", self.path(p), cmd.usage),
            None => cmd.usage.to_string(),
        };
        if !cmd.disable_flags_in_use_line && !l.contains("[flags]") {
            l.push_str(" [flags]");
        }
        l
    }

    /// cobra defaultUsageFunc.
    fn usage(&self, c: usize) -> String {
        self.usage_with(c, true)
    }

    /// `help_flag` false: cobra adds the help flag only to the command it
    /// executes or shows help for.
    fn usage_with(&self, c: usize, help_flag: bool) -> String {
        let cmd = &self.cmds[c];
        let mut s = String::from("Usage:");
        if cmd.runnable {
            let _ = write!(s, "\n  {}", self.use_line(c));
        }
        let subs = self.has_available_subs(c);
        if subs {
            let _ = write!(s, "\n  {} [command]", self.path(c));
        }
        if !cmd.aliases.is_empty() {
            let _ = write!(
                s,
                "\n\nAliases:\n  {}, {}",
                cmd.name(),
                cmd.aliases.join(", ")
            );
        }
        if subs {
            s.push_str("\n\nAvailable Commands:");
            for &sc in &cmd.subs {
                if self.available(sc) || self.cmds[sc].id == Id::Help {
                    let pad = self.name_padding(sc);
                    let _ = write!(
                        s,
                        "\n  {:<pad$} {}",
                        self.cmds[sc].name(),
                        self.cmds[sc].short
                    );
                }
            }
        }
        s.push_str("\n\nFlags:\n");
        let flags: Vec<&FlagDef> = cmd
            .flags
            .iter()
            .filter(|f| help_flag || f.name != "help")
            .collect();
        s.push_str(flag_usages(&flags).trim_end());
        if subs {
            let _ = write!(
                s,
                "\n\nUse \"{} [command] --help\" for more information about a command.",
                self.path(c)
            );
        }
        s.push('\n');
        s
    }

    /// cobra defaultHelpFunc.
    fn help(&self, c: usize) -> String {
        let cmd = &self.cmds[c];
        let desc = if cmd.long.is_empty() {
            &cmd.short
        } else {
            &cmd.long
        };
        let mut s = String::new();
        let desc = desc.trim_end();
        if !desc.is_empty() {
            let _ = write!(s, "{desc}\n\n");
        }
        if cmd.runnable || !cmd.subs.is_empty() {
            s.push_str(&self.usage(c));
        }
        s
    }

    /// cobra findNext: a child by name or alias.
    fn find_next(&self, c: usize, name: &str) -> Option<usize> {
        self.cmds[c].subs.iter().copied().find(|s| {
            let sc = &self.cmds[*s];
            sc.name() == name || sc.aliases.contains(&name)
        })
    }

    /// cobra Find: the command for `args` and the remaining args.
    fn find(&self, args: &[String]) -> (usize, Vec<String>, Result<(), String>) {
        let mut c = ROOT;
        let mut args = args.to_vec();
        loop {
            let names = strip_flags(&args, &self.cmds[c].flags);
            let Some(next) = names.first() else { break };
            let Some(sc) = self.find_next(c, next) else {
                break;
            };
            args = args_minus_first(&args, next, &self.cmds[c].flags);
            c = sc;
        }
        let cmd = &self.cmds[c];
        let mut res = Ok(());
        if cmd.args == Args::Legacy && !cmd.subs.is_empty() && cmd.parent.is_none() {
            // legacyArgs: root command with subcommands checks them.
            if let Some(a) = strip_flags(&args, &cmd.flags).first() {
                res = Err(format!(
                    "unknown command {} for {}{}",
                    go_quote(a),
                    go_quote(&self.path(c)),
                    self.suggestions(c, a)
                ));
            }
        }
        (c, args, res)
    }

    /// cobra findSuggestions (SuggestionsFor, minimum distance 2).
    fn suggestions(&self, c: usize, arg: &str) -> String {
        let typed = arg.to_lowercase();
        let out: Vec<&str> = self.cmds[c]
            .subs
            .iter()
            .filter(|s| self.available(**s))
            .map(|s| self.cmds[*s].name())
            .filter(|n| {
                levenshtein(&typed, &n.to_lowercase()) <= 2 || n.to_lowercase().starts_with(&typed)
            })
            .collect();
        if out.is_empty() {
            return String::new();
        }
        let mut s = String::from("\n\nDid you mean this?\n");
        for o in out {
            let _ = writeln!(s, "\t{o}");
        }
        s
    }
}

fn find_def<'a>(defs: &'a [FlagDef], name: &str) -> Option<&'a FlagDef> {
    defs.iter().find(|d| d.name == name)
}

fn find_short(defs: &[FlagDef], c: char) -> Option<&FlagDef> {
    defs.iter().find(|d| d.short == Some(c))
}

/// cobra hasNoOptDefVal (bool flags take no value). The help flag is not
/// defined yet when cobra looks for sub commands.
fn no_opt(defs: &[FlagDef], name: &str) -> bool {
    find_def(defs, name).is_some_and(|d| d.name != "help" && d.kind == Kind::Bool)
}

fn short_no_opt(defs: &[FlagDef], name: &str) -> bool {
    let mut cs = name.chars();
    match (cs.next(), cs.next()) {
        (Some(c), None) => {
            find_short(defs, c).is_some_and(|d| d.name != "help" && d.kind == Kind::Bool)
        }
        _ => false,
    }
}

/// Whether `s` is `--flag value` / `-f value` taking the next arg (cobra
/// stripFlags / argsMinusFirstX).
fn takes_value(s: &str, defs: &[FlagDef]) -> bool {
    if s.contains('=') {
        return false;
    }
    if let Some(n) = s.strip_prefix("--") {
        return !no_opt(defs, n);
    }
    s.starts_with('-') && s.len() == 2 && !short_no_opt(defs, &s[1..])
}

/// cobra stripFlags: the non-flag args.
fn strip_flags(args: &[String], defs: &[FlagDef]) -> Vec<String> {
    let mut cmds = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let s = &args[i];
        i += 1;
        if s == "--" {
            break;
        }
        if takes_value(s, defs) {
            if args.len() - i <= 1 {
                break;
            }
            i += 1;
            continue;
        }
        if !s.is_empty() && !s.starts_with('-') {
            cmds.push(s.clone());
        }
    }
    cmds
}

/// cobra argsMinusFirstX.
fn args_minus_first(args: &[String], x: &str, defs: &[FlagDef]) -> Vec<String> {
    let mut pos = 0;
    while pos < args.len() {
        let s = &args[pos];
        if s == "--" {
            break;
        }
        if takes_value(s, defs) {
            pos += 2;
            continue;
        }
        if !s.starts_with('-') && s == x {
            let mut r = args.to_vec();
            r.remove(pos);
            return r;
        }
        pos += 1;
    }
    args.to_vec()
}

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
fn flag_usages(defs: &[&FlagDef]) -> String {
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
    Tree::new(log_file).usage(ROOT)
}

/// cobra help output for the root command.
pub fn root_help(log_file: &str) -> String {
    Tree::new(log_file).help(ROOT)
}

/// Shell completion request (`k9s __complete ...`), resolved by
/// `complete::run`.
#[derive(Debug, Clone, PartialEq)]
pub struct CompleteReq {
    /// Static completions.
    pub comps: Vec<String>,
    /// ShellCompDirective.
    pub directive: u32,
    /// Error reported to stderr before the completions.
    pub err: Option<String>,
    /// Flag value completion that needs the kubeconfig or the cluster.
    pub dynamic: Option<(FlagComp, String)>,
    /// Called as `__completeNoDesc`.
    pub no_desc: bool,
}

/// k9s initK8sFlagCompletion functions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlagComp {
    Context,
    Cluster,
    User,
    Namespace,
}

/// Outcome of parsing the command line.
#[derive(Debug, PartialEq)]
pub enum Parsed {
    /// Run with these flags.
    Run(Box<Cli>),
    /// Print this text to stdout and exit 0.
    Help(String),
    /// Print this text to stderr and exit 0 (cobra writes help command
    /// usage there).
    HelpErr(String),
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

type Values = Vec<(&'static str, String)>;

/// pflag FlagSet.Parse result.
struct FlagSet {
    vals: Values,
    /// Positional args.
    args: Vec<String>,
}

impl FlagSet {
    fn changed(&self, name: &str) -> bool {
        self.vals.iter().any(|(k, _)| *k == name)
    }

    fn last(&self, name: &str) -> Option<String> {
        self.vals
            .iter()
            .rev()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.clone())
    }

    fn is_true(&self, name: &str) -> bool {
        self.last(name).as_deref() == Some("true")
    }
}

/// pflag parsing of `args` against `defs`. On error, the flags and args
/// parsed so far are returned with it.
fn parse_flags(defs: &[FlagDef], args: &[String]) -> (FlagSet, Result<(), String>) {
    let mut fs = FlagSet {
        vals: Vec::new(),
        args: Vec::new(),
    };
    let r = parse_into(defs, args, &mut fs);
    (fs, r)
}

fn parse_into(defs: &[FlagDef], args: &[String], fs: &mut FlagSet) -> Result<(), String> {
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        i += 1;
        if a.len() < 2 || !a.starts_with('-') {
            fs.args.push(a.clone());
            continue;
        }
        if a == "--" {
            fs.args.extend(args[i..].iter().cloned());
            break;
        }
        if let Some(rest) = a.strip_prefix("--") {
            if rest.starts_with('-') || rest.starts_with('=') {
                return Err(format!("bad flag syntax: {a}"));
            }
            let (name, inline) = match rest.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (rest, None),
            };
            let Some(fd) = find_def(defs, name) else {
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
            fs.vals.push((fd.name, check(fd, &v)?));
            continue;
        }
        let chars: Vec<char> = a[1..].chars().collect();
        let mut j = 0;
        while j < chars.len() {
            let c = chars[j];
            let shorts: String = chars[j..].iter().collect();
            let Some(fd) = find_short(defs, c) else {
                return Err(format!("unknown shorthand flag: '{c}' in -{shorts}"));
            };
            let rest: String = chars[j + 1..].iter().collect();
            if let Some(v) = rest.strip_prefix('=') {
                fs.vals.push((fd.name, check(fd, v)?));
                break;
            }
            if fd.kind == Kind::Bool {
                fs.vals.push((fd.name, "true".into()));
                j += 1;
                continue;
            }
            let v = if !rest.is_empty() {
                rest
            } else if i < args.len() {
                i += 1;
                args[i - 1].clone()
            } else {
                return Err(format!("flag needs an argument: '{c}' in -{shorts}"));
            };
            fs.vals.push((fd.name, check(fd, &v)?));
            break;
        }
    }
    Ok(())
}

/// pflag InvalidValueError names the flag `-s, --long` when it has a
/// shorthand, however it was given.
fn check(fd: &FlagDef, v: &str) -> Result<String, String> {
    let shown = match fd.short {
        Some(s) => format!("-{s}, --{}", fd.name),
        None => format!("--{}", fd.name),
    };
    let bad = |cause: String| {
        format!(
            "invalid argument {} for \"{shown}\" flag: {cause}",
            go_quote(v)
        )
    };
    match fd.kind {
        Kind::Bool => parse_go_bool(v).map(|b| b.to_string()).ok_or_else(|| {
            bad(format!(
                "strconv.ParseBool: parsing {}: invalid syntax",
                go_quote(v)
            ))
        }),
        Kind::Float32 => match parse_go_float32(v) {
            Ok(_) => Ok(v.to_string()),
            Err(e) => Err(bad(format!(
                "strconv.ParseFloat: parsing {}: {e}",
                go_quote(v)
            ))),
        },
        _ => Ok(v.to_string()),
    }
}

/// strconv.ParseFloat(v, 32), decimal forms.
fn parse_go_float32(v: &str) -> Result<f32, &'static str> {
    let body = v.trim_start_matches(['+', '-']);
    let lower = body.to_ascii_lowercase();
    if matches!(lower.as_str(), "inf" | "infinity" | "nan") && body.len() + 1 >= v.len() {
        return v.parse::<f32>().map_err(|_| "invalid syntax");
    }
    if body.is_empty()
        || !body
            .bytes()
            .all(|b| b.is_ascii_digit() || b".eE+-".contains(&b))
    {
        return Err("invalid syntax");
    }
    match v.parse::<f32>() {
        Ok(f) if f.is_infinite() => Err("value out of range"),
        Ok(f) => Ok(f),
        Err(_) => Err("invalid syntax"),
    }
}

fn parse_go_bool(v: &str) -> Option<bool> {
    match v {
        "1" | "t" | "T" | "TRUE" | "true" | "True" => Some(true),
        "0" | "f" | "F" | "FALSE" | "false" | "False" => Some(false),
        _ => None,
    }
}

/// Root flags into a Cli.
fn root_cli(fs: &FlagSet) -> Cli {
    let s = |n: &str| fs.last(n).filter(|v| !v.is_empty());
    Cli {
        command: None,
        help: false,
        refresh: fs
            .last("refresh")
            .and_then(|v| v.parse().ok())
            .unwrap_or(2.0),
        log_level: fs.last("logLevel").unwrap_or_else(|| "info".into()),
        log_file: fs.last("logFile"),
        headless: fs.is_true("headless"),
        logoless: fs.is_true("logoless"),
        crumbsless: fs.is_true("crumbsless"),
        splashless: fs.is_true("splashless"),
        invert: fs.is_true("invert"),
        all_namespaces: fs.is_true("all-namespaces"),
        command_override: s("command"),
        readonly: fs.is_true("readonly"),
        write: fs.is_true("write"),
        screen_dump_dir: s("screen-dump-dir"),
        kubeconfig: s("kubeconfig"),
        request_timeout: s("request-timeout"),
        context: s("context"),
        cluster: s("cluster"),
        user: s("user"),
        namespace: s("namespace"),
        impersonate: s("as"),
        impersonate_group: fs
            .vals
            .iter()
            .filter(|(k, _)| *k == "as-group")
            .map(|(_, v)| v.clone())
            .collect(),
        insecure_skip_tls_verify: fs.is_true("insecure-skip-tls-verify"),
        certificate_authority: s("certificate-authority"),
        client_key: s("client-key"),
        client_certificate: s("client-certificate"),
        token: s("token"),
    }
}

/// Parse k9s arguments (without the program name): cobra ExecuteC.
pub fn parse(args: &[String], log_file: &str) -> Parsed {
    let t = Tree::new(log_file);
    let (c, rest, found) = t.find(args);
    if let Err(e) = found {
        return Parsed::Fail(format!(
            "Error: {e}\nRun '{} --help' for usage.\n",
            t.path(c)
        ));
    }
    let cmd = &t.cmds[c];
    let fail = |e: String| Parsed::Fail(format!("Error: {e}\n{}\n", t.usage(c)));
    let (fs, pos) = if cmd.disable_flag_parsing {
        let fs = FlagSet {
            vals: vec![],
            args: rest.clone(),
        };
        let pos = rest.clone();
        (fs, pos)
    } else {
        let (fs, r) = parse_flags(&cmd.flags, &rest);
        if let Err(e) = r {
            return fail(e);
        }
        let pos = fs.args.clone();
        (fs, pos)
    };
    if fs.is_true("help") || !cmd.runnable {
        return Parsed::Help(t.help(c));
    }
    match cmd.args {
        Args::None if !pos.is_empty() => {
            return fail(format!(
                "unknown command {} for {}",
                go_quote(&pos[0]),
                go_quote(&t.path(c))
            ))
        }
        Args::MinOne if pos.is_empty() => {
            return fail("requires at least 1 arg(s), only received 0".into())
        }
        _ => {}
    }
    match cmd.id {
        Id::Root => Parsed::Run(Box::new(root_cli(&fs))),
        Id::Help => {
            let (hc, _, e) = t.find(&pos);
            if e.is_err() {
                let topics: Vec<String> = pos.iter().map(|a| format!("`{a}`")).collect();
                return Parsed::HelpErr(format!(
                    "Unknown help topic [{}]\n{}",
                    topics.join(" "),
                    t.usage_with(ROOT, false)
                ));
            }
            Parsed::Help(t.help(hc))
        }
        Id::Info => Parsed::Run(Box::new(Cli {
            command: Some(Sub::Info),
            ..Cli::default()
        })),
        Id::Version => Parsed::Run(Box::new(Cli {
            command: Some(Sub::Version {
                short: fs.is_true("short"),
            }),
            ..Cli::default()
        })),
        Id::Shell(sh) => Parsed::Help(sh.script(!fs.is_true("no-descriptions"))),
        Id::Completion => Parsed::Help(t.help(c)),
        Id::Complete => {
            let no_desc = args
                .iter()
                .find(|a| a.starts_with("__complete"))
                .is_some_and(|a| a == "__completeNoDesc");
            let (req, cli) = t.complete(&pos, no_desc);
            Parsed::Run(Box::new(Cli {
                command: Some(Sub::Complete(Box::new(req))),
                ..cli
            }))
        }
    }
}

/// pflag isFlagArg.
fn is_flag_arg(a: &str) -> bool {
    (a.len() >= 3 && a.starts_with("--"))
        || (a.len() >= 2 && a.starts_with('-') && !a.starts_with("--"))
}

/// ShellCompDirective values.
pub mod directive {
    pub const ERROR: u32 = 1;
    pub const NO_SPACE: u32 = 2;
    pub const NO_FILE_COMP: u32 = 4;
    pub const DEFAULT: u32 = 0;

    /// ShellCompDirective.string.
    pub fn name(d: u32) -> String {
        let names = [
            "ShellCompDirectiveError",
            "ShellCompDirectiveNoSpace",
            "ShellCompDirectiveNoFileComp",
            "ShellCompDirectiveFilterFileExt",
            "ShellCompDirectiveFilterDirs",
            "ShellCompDirectiveKeepOrder",
        ];
        if d >= 1 << names.len() {
            return format!("ERROR: unexpected ShellCompDirective value: {d}");
        }
        let on: Vec<&str> = names
            .iter()
            .enumerate()
            .filter(|(i, _)| d & (1 << i) != 0)
            .map(|(_, n)| *n)
            .collect();
        if on.is_empty() {
            "ShellCompDirectiveDefault".into()
        } else {
            on.join(", ")
        }
    }
}

fn go_list(v: &[String]) -> String {
    format!("[{}]", v.join(" "))
}

impl Tree {
    /// cobra getCompletions. Also returns the root flags given on the line
    /// (k9s completion functions read the kubeconfig flags).
    fn complete(&self, args: &[String], no_desc: bool) -> (CompleteReq, Cli) {
        let mut req = CompleteReq {
            comps: vec![],
            directive: directive::DEFAULT,
            err: None,
            dynamic: None,
            no_desc,
        };
        let mut to_complete = args.last().cloned().unwrap_or_default();
        let trimmed = &args[..args.len().saturating_sub(1)];
        let (c, mut final_args, found) = self.find(trimmed);
        if found.is_err() {
            req.err = Some(format!(
                "unable to find a command for arguments: {}",
                go_list(trimmed)
            ));
            return (req, Cli::default());
        }
        let cmd = &self.cmds[c];

        // checkIfFlagCompletion.
        let mut flag: Option<&FlagDef> = None;
        // (error, is a flagCompError).
        let mut flag_err: Option<(String, bool)> = None;
        let mut flag_name = String::new();
        let mut with_equal = false;
        let org_args = final_args.clone();
        let org_last = to_complete.clone();
        if to_complete.starts_with('-') {
            if let Some(idx) = to_complete.find('=') {
                flag_name = if to_complete[..idx].starts_with("--") {
                    to_complete[2..idx].to_string()
                } else {
                    to_complete[idx - 1..idx].to_string()
                };
                to_complete = to_complete[idx + 1..].to_string();
                with_equal = true;
            }
        } else if let Some(prev) = final_args.last().cloned() {
            if is_flag_arg(&prev) && !prev.contains('=') {
                flag_name = match prev.strip_prefix("--") {
                    Some(n) => n.to_string(),
                    None => prev.chars().last().map(String::from).unwrap_or_default(),
                };
                final_args.pop();
            }
        }
        if !flag_name.is_empty() {
            let fd = if flag_name.chars().count() == 1 {
                find_short(&cmd.flags, flag_name.chars().next().unwrap_or(' '))
            } else {
                find_def(&cmd.flags, &flag_name)
            };
            match fd {
                None => {
                    final_args = org_args.clone();
                    to_complete = org_last.clone();
                    flag_err = Some((
                        format!(
                            "Subcommand '{}' does not support flag '{flag_name}'",
                            cmd.name()
                        ),
                        true,
                    ));
                }
                Some(fd) if !with_equal && fd.kind == Kind::Bool => final_args = org_args.clone(),
                Some(fd) => flag = Some(fd),
            }
        }

        let mut with_dash = final_args.clone();
        with_dash.push("--".into());
        let (fs_dash, _) = parse_flags(&cmd.flags, &with_dash);
        let (fs, r) = parse_flags(&cmd.flags, &final_args);
        let cli = if cmd.id == Id::Root {
            root_cli(&fs)
        } else {
            Cli::default()
        };
        if let Err(e) = r {
            req.err = Some(format!(
                "Error while parsing flags from args {}: {e}",
                go_list(&final_args)
            ));
            return (req, cli);
        }
        let flag_completion = fs_dash.args.len() <= fs.args.len();
        if let Some((e, comp_err)) = flag_err {
            if !comp_err || flag_completion {
                req.err = Some(e);
                return (req, cli);
            }
        }
        if fs.changed("help") {
            req.directive = directive::NO_FILE_COMP;
            return (req, cli);
        }
        let final_args = if cmd.disable_flag_parsing {
            final_args
        } else {
            fs.args.clone()
        };

        if flag.is_none()
            && to_complete.starts_with('-')
            && !to_complete.contains('=')
            && flag_completion
        {
            for fd in &cmd.flags {
                if fs.changed(fd.name) && fd.kind != Kind::StrArray {
                    continue;
                }
                let long = format!("--{}", fd.name);
                if long.starts_with(&to_complete) {
                    req.comps.push(format!("{long}\t{}", fd.usage));
                }
                if let Some(s) = fd.short {
                    let short = format!("-{s}");
                    if short.starts_with(&to_complete) {
                        req.comps.push(format!("{short}\t{}", fd.usage));
                    }
                }
            }
            req.directive = directive::NO_FILE_COMP;
            if req.comps.len() == 1 && req.comps[0].ends_with('=') {
                req.directive = directive::NO_SPACE;
            }
            return (req, cli);
        }
        if flag.is_none() {
            let local_changed = cmd
                .flags
                .iter()
                .any(|f| !cmd.persistent.contains(&f.name) && fs.changed(f.name));
            if final_args.is_empty() && !local_changed {
                for &sc in &cmd.subs {
                    if self.available(sc) || self.cmds[sc].id == Id::Help {
                        let s = &self.cmds[sc];
                        if s.name().starts_with(&to_complete) {
                            req.comps.push(format!("{}\t{}", s.name(), s.short));
                        }
                        req.directive = directive::NO_FILE_COMP;
                    }
                }
            }
        }
        match flag.filter(|_| flag_completion) {
            Some(fd) if cmd.id == Id::Root => {
                let fc = match fd.name {
                    "context" => Some(FlagComp::Context),
                    "cluster" => Some(FlagComp::Cluster),
                    "user" => Some(FlagComp::User),
                    "namespace" => Some(FlagComp::Namespace),
                    _ => None,
                };
                if let Some(fc) = fc {
                    req.dynamic = Some((fc, to_complete));
                }
            }
            Some(_) => {}
            None => match cmd.id {
                Id::Completion | Id::Shell(_) => req.directive = directive::NO_FILE_COMP,
                Id::Help => {
                    req.directive = directive::NO_FILE_COMP;
                    let (hc, _, e) = self.find(&final_args);
                    if e.is_ok() {
                        let hcmd = &self.cmds[hc];
                        for &sc in &hcmd.subs {
                            let s = &self.cmds[sc];
                            if (self.available(sc) || s.id == Id::Help)
                                && s.name().starts_with(&to_complete)
                            {
                                req.comps.push(format!("{}\t{}", s.name(), s.short));
                            }
                        }
                    }
                }
                _ => {}
            },
        }
        (req, cli)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Parsed {
        let v: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        parse(&v, "/tmp/k9rs.log")
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
            e.starts_with("Error: unknown flag: --blee\nUsage:\n  k9rs [flags]\n"),
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
            Parsed::Fail("Error: unknown command \"inf\" for \"k9rs\"\n\nDid you mean this?\n\tinfo\n\nRun 'k9rs --help' for usage.\n".into())
        );
        assert_eq!(
            p(&["zorg"]),
            Parsed::Fail(
                "Error: unknown command \"zorg\" for \"k9rs\"\nRun 'k9rs --help' for usage.\n"
                    .into()
            )
        );
        let Parsed::Fail(e) = p(&["version", "--blee"]) else {
            panic!()
        };
        assert_eq!(
            e,
            "Error: unknown flag: --blee\nUsage:\n  k9rs version [flags]\n\nFlags:\n  -h, --help    help for version\n  -s, --short   Prints K9rs version info in short format\n\n"
        );
    }

    #[test]
    fn help() {
        let Parsed::Help(h) = p(&["--help"]) else {
            panic!()
        };
        // cobra lists its default `completion` command and pads names to
        // minNamePadding (11); the old expectation had neither.
        let expected = r#"K9rs is a CLI to view and manage your Kubernetes clusters.

Usage:
  k9rs [flags]
  k9rs [command]

Available Commands:
  completion  Generate the autocompletion script for the specified shell
  help        Help about any command
  info        List K9rs configurations info
  version     Print version/build info

Flags:
  -A, --all-namespaces                 Launch K9rs in all namespaces
      --as string                      Username to impersonate for the operation
      --as-group stringArray           Group to impersonate for the operation
      --certificate-authority string   Path to a cert file for the certificate authority
      --client-certificate string      Path to a client certificate file for TLS
      --client-key string              Path to a client key file for TLS
      --cluster string                 The name of the kubeconfig cluster to use
  -c, --command string                 Overrides the default resource to load when the application launches
      --context string                 The name of the kubeconfig context to use
      --crumbsless                     Turn K9rs crumbs off
      --headless                       Turn K9rs header off
  -h, --help                           help for k9rs
      --insecure-skip-tls-verify       If true, the server's caCertFile will not be checked for validity
      --invert                         Invert skin (dark to light, light to dark), preserving colors
      --kubeconfig string              Path to the kubeconfig file to use for CLI requests
      --logFile string                 Specify the log file (default "/tmp/k9rs.log")
  -l, --logLevel string                Specify a log level (error, warn, info, debug) (default "info")
      --logoless                       Turn K9rs logo off
  -n, --namespace string               If present, the namespace scope for this CLI request
      --readonly                       Sets readOnly mode by overriding readOnly configuration setting
  -r, --refresh float32                Specify the default refresh rate as a float (sec) (default 2)
      --request-timeout string         The length of time to wait before giving up on a single server request
      --screen-dump-dir string         Sets a path to a dir for a screen dumps
      --splashless                     Turn K9rs splash screen off
      --token string                   Bearer token for authentication to the API server
      --user string                    The name of the kubeconfig user to use
      --write                          Sets write mode by overriding the readOnly configuration setting

Use "k9rs [command] --help" for more information about a command.
"#;
        assert_eq!(h, expected);
        assert_eq!(p(&["-h"]), Parsed::Help(expected.into()));
        assert_eq!(p(&["help"]), Parsed::Help(expected.into()));
        assert_eq!(
            p(&["info", "-h"]),
            Parsed::Help("List K9rs configurations info\n\nUsage:\n  k9rs info [flags]\n\nFlags:\n  -h, --help   help for info\n".into())
        );
        assert_eq!(
            p(&["help", "version"]),
            Parsed::Help(
                "Print version/build information\n\nUsage:\n  k9rs version [flags]\n\nFlags:\n  -h, --help    help for version\n  -s, --short   Prints K9rs version info in short format\n".into()
            )
        );
        // cobra prints unknown help topics to stderr (was stdout here).
        let Parsed::HelpErr(h) = p(&["help", "zorg"]) else {
            panic!()
        };
        assert!(h.starts_with("Unknown help topic [`zorg`]\nUsage:\n"));
        let Parsed::Help(h) = p(&["help", "-h"]) else {
            panic!()
        };
        assert!(h.starts_with("Help provides help for any command in the application.\nSimply type k9rs help [path to command] for full details.\n\nUsage:\n  k9rs help [command] [flags]\n"));
        // An unknown topic goes to stderr, root usage without the help flag.
        let Parsed::HelpErr(h) = p(&["help", "zorg", "x"]) else {
            panic!()
        };
        assert!(h.starts_with("Unknown help topic [`zorg` `x`]\nUsage:\n"));
        assert!(!h.contains("help for k9rs"));
        // `-h` is not a known flag when cobra looks for the sub command.
        assert_eq!(p(&["-h", "info"]), Parsed::Help(expected.into()));
        assert_eq!(p(&["--help", "info"]), Parsed::Help(expected.into()));
    }

    const COMPLETION_HELP: &str =
        "Generate the autocompletion script for k9rs for the specified shell.
See each sub-command's help for details on how to use the generated script.

Usage:
  k9rs completion [command]

Available Commands:
  bash        Generate the autocompletion script for bash
  fish        Generate the autocompletion script for fish
  powershell  Generate the autocompletion script for powershell
  zsh         Generate the autocompletion script for zsh

Flags:
  -h, --help   help for completion

Use \"k9rs completion [command] --help\" for more information about a command.
";

    #[test]
    fn completion_command() {
        for args in [
            &["completion"][..],
            &["completion", "-h"],
            &["completion", "zorg"],
            &["help", "completion"],
            &["help", "completion", "zorg"],
        ] {
            assert_eq!(p(args), Parsed::Help(COMPLETION_HELP.into()), "{args:?}");
        }
        assert_eq!(
            p(&["completion", "--no-descriptions"]),
            Parsed::Fail(format!(
                "Error: unknown flag: --no-descriptions\n{}\n",
                COMPLETION_HELP
                    .split_once("Usage:")
                    .map(|(_, u)| format!("Usage:{u}"))
                    .unwrap()
            ))
        );
        let bash_usage = "Usage:\n  k9rs completion bash\n\nFlags:\n  -h, --help              help for bash\n      --no-descriptions   disable completion descriptions\n";
        assert_eq!(
            p(&["completion", "bash", "extra"]),
            Parsed::Fail(format!(
                "Error: unknown command \"extra\" for \"k9rs completion bash\"\n{bash_usage}\n"
            ))
        );
        let Parsed::Help(h) = p(&["help", "completion", "bash"]) else {
            panic!()
        };
        assert!(h.starts_with(
            "Generate the autocompletion script for the bash shell.\n\nThis script depends"
        ));
        assert!(h.ends_with(&format!("take effect.\n\n{bash_usage}")), "{h}");
        let Parsed::Help(h) = p(&["completion", "zsh", "-h"]) else {
            panic!()
        };
        assert!(h.contains("\nUsage:\n  k9rs completion zsh [flags]\n"));
        for (sh, first) in [
            ("bash", "# bash completion V2 for k9rs"),
            ("zsh", "#compdef k9rs"),
            ("fish", "# fish completion for k9rs"),
            ("powershell", "# powershell completion for k9rs"),
        ] {
            let Parsed::Help(s) = p(&["completion", sh]) else {
                panic!()
            };
            assert!(s.starts_with(first), "{sh}");
            assert_eq!(s.matches(" __complete ").count(), 1, "{sh}");
            let Parsed::Help(nd) = p(&["completion", sh, "--no-descriptions"]) else {
                panic!()
            };
            assert_eq!(nd, s.replace(" __complete ", " __completeNoDesc "));
        }
        // The scripts always register `k9rs`; the header keeps cobra's
        // `%-36s` padding.
        let b = Shell::Bash.script(true);
        assert!(b.starts_with(&format!(
            "# bash completion V2 for {:<36} -*- shell-script -*-\n",
            "k9rs"
        )));
        assert!(
            b.contains("complete -o default -F __start_k9rs k9rs"),
            "registration"
        );
        assert!(!b.contains("k9s"));
        let f = Shell::Fish.script(false);
        assert!(f.contains("K9RS_ACTIVE_HELP=0") && f.contains(" __completeNoDesc "));
        let z = Shell::Zsh.script(true);
        assert!(
            z.starts_with("#compdef k9rs\ncompdef _k9rs k9rs\n"),
            "{}",
            &z[..40]
        );
        let Parsed::Fail(e) = p(&["comp"]) else {
            panic!()
        };
        assert!(e.contains("Did you mean this?\n\tcompletion\n"), "{e}");
        // The help command is never suggested.
        assert_eq!(
            p(&["hel"]),
            Parsed::Fail(
                "Error: unknown command \"hel\" for \"k9rs\"\nRun 'k9rs --help' for usage.\n"
                    .into()
            )
        );
    }

    fn comp(args: &[&str]) -> CompleteReq {
        let mut v = vec!["__complete"];
        v.extend_from_slice(args);
        match p(&v) {
            Parsed::Run(c) => match c.command {
                Some(Sub::Complete(r)) => *r,
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
    }

    fn names(r: &CompleteReq) -> Vec<&str> {
        r.comps
            .iter()
            .map(|c| c.split('\t').next().unwrap())
            .collect()
    }

    #[test]
    fn complete_requests() {
        let r = comp(&[""]);
        assert_eq!(names(&r), ["completion", "help", "info", "version"]);
        assert_eq!(r.comps[2], "info\tList K9rs configurations info");
        assert_eq!(r.directive, directive::NO_FILE_COMP);
        assert_eq!(comp(&["--", ""]).comps.len(), 4);
        let r = comp(&["--c"]);
        assert_eq!(
            names(&r),
            [
                "--certificate-authority",
                "--client-certificate",
                "--client-key",
                "--cluster",
                "--command",
                "--context",
                "--crumbsless"
            ]
        );
        let r = comp(&["-"]);
        assert_eq!(&names(&r)[..3], ["--all-namespaces", "-A", "--as"]);
        assert!(r
            .comps
            .contains(&"--logFile\tSpecify the log file".to_string()));
        // Set flags are not offered again, arrays are.
        let r = comp(&["-A", "--as-group", "x", "-"]);
        assert!(!names(&r).contains(&"-A") && names(&r).contains(&"--as-group"));
        assert_eq!(names(&comp(&["-n"])), ["-n"]);
        assert_eq!(names(&comp(&["version", "-s", "-"])), ["--help", "-h"]);
        assert_eq!(
            names(&comp(&["completion", "bash", "-"])),
            ["--help", "-h", "--no-descriptions"]
        );
        assert_eq!(names(&comp(&["completion", "b"])), ["bash"]);
        assert_eq!(names(&comp(&["help", "c"])), ["completion"]);
        assert_eq!(names(&comp(&["help", "completion", ""])).len(), 4);
        for (args, d) in [
            (&["info", ""][..], directive::DEFAULT),
            (&["version", ""], directive::DEFAULT),
            (&["-A", ""], directive::DEFAULT),
            (&["--headless", ""], directive::DEFAULT),
            (&["-c", ""], directive::DEFAULT),
            (&["-r="], directive::DEFAULT),
            (&["-h", ""], directive::NO_FILE_COMP),
            (&["completion", "bash", ""], directive::NO_FILE_COMP),
        ] {
            let r = comp(args);
            assert!(r.comps.is_empty(), "{args:?}");
            assert_eq!(r.directive, d, "{args:?}");
            assert_eq!(r.dynamic, None, "{args:?}");
        }
        let r = comp(&["--context", "ct"]);
        assert_eq!(r.dynamic, Some((FlagComp::Context, "ct".into())));
        assert_eq!(
            comp(&["--context="]).dynamic,
            Some((FlagComp::Context, "".into()))
        );
        assert_eq!(
            comp(&["--user=u"]).dynamic,
            Some((FlagComp::User, "u".into()))
        );
        assert_eq!(
            comp(&["--cluster", ""]).dynamic,
            Some((FlagComp::Cluster, "".into()))
        );
        assert_eq!(
            comp(&["-n", "k"]).dynamic,
            Some((FlagComp::Namespace, "k".into()))
        );
        // Earlier root flags reach the completion functions.
        let Parsed::Run(c) = p(&["__complete", "--kubeconfig", "/kc", "--context", ""]) else {
            panic!()
        };
        assert_eq!(c.kubeconfig.as_deref(), Some("/kc"));
        assert_eq!(
            comp(&["zorg", ""]).err.as_deref(),
            Some("unable to find a command for arguments: [zorg]")
        );
        assert_eq!(
            comp(&["--blee", ""]).err.as_deref(),
            Some("Error while parsing flags from args [--blee]: unknown flag: --blee")
        );
        assert!(!comp(&[""]).no_desc);
        let Parsed::Run(c) = p(&["__completeNoDesc", ""]) else {
            panic!()
        };
        assert!(matches!(c.command, Some(Sub::Complete(r)) if r.no_desc));
        assert_eq!(
            p(&["__complete"]),
            Parsed::Fail("Error: requires at least 1 arg(s), only received 0\nUsage:\n  k9rs __complete [command-line]\n\nAliases:\n  __complete, __completeNoDesc\n\nFlags:\n  -h, --help   help for __complete\n\n".into())
        );
    }

    #[test]
    fn pflag_errors() {
        let first = |args: &[&str]| match p(args) {
            Parsed::Fail(e) => e.lines().next().unwrap().to_string(),
            other => panic!("{other:?}"),
        };
        assert_eq!(first(&["-Ax"]), "Error: unknown shorthand flag: 'x' in -x");
        assert_eq!(first(&["-An"]), "Error: flag needs an argument: 'n' in -n");
        assert_eq!(
            first(&["--refresh=fast"]),
            "Error: invalid argument \"fast\" for \"-r, --refresh\" flag: strconv.ParseFloat: parsing \"fast\": invalid syntax"
        );
        assert_eq!(
            first(&["-r", "1e40"]),
            "Error: invalid argument \"1e40\" for \"-r, --refresh\" flag: strconv.ParseFloat: parsing \"1e40\": value out of range"
        );
        assert_eq!(first(&["--=x"]), "Error: bad flag syntax: --=x");
        assert_eq!(first(&["---x"]), "Error: bad flag syntax: ---x");
        // An unknown flag swallows the next arg when looking for commands.
        let Parsed::Fail(e) = p(&["--blee", "info"]) else {
            panic!()
        };
        assert!(e.starts_with("Error: unknown flag: --blee\nUsage:\n  k9rs [flags]\n"));
        assert_eq!(run(&["-r", "inf"]).refresh, f32::INFINITY);
    }
}
