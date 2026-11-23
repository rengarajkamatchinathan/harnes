//! Command line (07 §2): interactive flags, headless flags (§4.1) and subcommands (§2.2).

use std::path::PathBuf;
use std::time::Duration;

use clap::{Args, Parser, Subcommand, ValueEnum};
use genie_protocol::Channel;
use genie_protocol::common::Mode;

/// The channel is a build-time setting (01 §2.1 rule 6, D40). Without one, a debug
/// build is `dev` so it never shares a data dir (and DB) with an installed release
/// build: the two keep the DB key in different key stores (`secrets::dev_secrets_mode`).
pub const BUILD_CHANNEL: &str = match option_env!("GENIE_CHANNEL") {
    Some(c) => c,
    None if cfg!(debug_assertions) => "dev",
    None => "stable",
};
const COMMIT: &str = match option_env!("GENIE_COMMIT") {
    Some(c) => c,
    None => "unknown",
};
const BUILD_DATE: &str = match option_env!("GENIE_BUILD_DATE") {
    Some(c) => c,
    None => "unknown",
};

/// `genie --version`: channel, commit, build date (D40).
pub fn version_line() -> String {
    format!(
        "{} ({BUILD_CHANNEL}, commit {COMMIT}, built {BUILD_DATE})",
        env!("CARGO_PKG_VERSION")
    )
}

#[derive(Debug, Parser)]
#[command(
    name = "genie",
    about = "Genie: multi-provider AI harness",
    disable_version_flag = true,
    subcommand_precedence_over_arg = true
)]
pub struct Cli {
    #[arg(long, global = true, env = "GENIE_CHANNEL_OVERRIDE", default_value = BUILD_CHANNEL, hide = true)]
    pub channel: Channel,
    /// Print version, channel, commit and build date.
    #[arg(short = 'V', long = "version")]
    pub version: bool,
    #[command(flatten)]
    pub run: RunArgs,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PermissionMode {
    Default,
    AcceptEdits,
    Plan,
    Bypass,
}

impl PermissionMode {
    pub fn to_mode(self) -> Mode {
        match self {
            PermissionMode::Default => Mode::Default,
            PermissionMode::AcceptEdits => Mode::AcceptEdits,
            PermissionMode::Plan => Mode::Plan,
            PermissionMode::Bypass => Mode::Bypass,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum OutputFormat {
    #[default]
    Text,
    Json,
    StreamJson,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum OnAsk {
    /// Pause and wait for an answer from any client (default).
    #[default]
    Wait,
    /// Deny every ask immediately (recommended for CI).
    Deny,
}

/// Flags of `genie [prompt]` (§2.1) and `genie -p` (§4.1).
#[derive(Debug, Clone, Default, Args)]
pub struct RunArgs {
    /// Prompt to start with (headless: the whole task).
    pub prompt: Option<String>,
    /// Headless: run the prompt (or stdin) and print the result.
    #[arg(short = 'p', long = "print")]
    pub print: bool,
    /// Continue the most recent session in this folder.
    #[arg(short = 'c', long = "continue")]
    pub continue_: bool,
    /// Resume a session (picker without an id).
    #[arg(short = 'r', long, num_args = 0..=1, default_missing_value = "")]
    pub resume: Option<String>,
    /// Start as an agent.
    #[arg(long)]
    pub agent: Option<String>,
    #[arg(long, env = "GENIE_MODEL")]
    pub model: Option<String>,
    #[arg(long)]
    pub effort: Option<String>,
    #[arg(long, value_enum)]
    pub permission_mode: Option<PermissionMode>,
    /// Bypass mode (still honours deny rules, policy and sensitive-action confirmations).
    #[arg(long)]
    pub dangerously_skip_permissions: bool,
    /// Run in a new worktree.
    #[arg(long, num_args = 0..=1, default_missing_value = "")]
    pub worktree: Option<String>,
    #[arg(long = "add-dir")]
    pub add_dir: Vec<PathBuf>,
    /// Session-only settings layer.
    #[arg(long)]
    pub settings: Option<PathBuf>,
    /// Trust this folder for this run.
    #[arg(long)]
    pub trust: bool,
    #[arg(long, value_enum, default_value_t)]
    pub output_format: OutputFormat,
    /// Allow rule (D25 syntax), e.g. `Bash(git status:*)`.
    #[arg(long)]
    pub allow: Vec<String>,
    /// Deny rule (D25 syntax).
    #[arg(long)]
    pub deny: Vec<String>,
    #[arg(long)]
    pub max_turns: Option<u32>,
    /// Deny and exit 3 when an ask waits longer than this (e.g. 30s, 5m).
    #[arg(long, value_parser = parse_duration)]
    pub ask_timeout: Option<Duration>,
    #[arg(long, value_enum, default_value_t)]
    pub on_ask: OnAsk,
    /// Private daemon that exits with this process (CI containers).
    #[arg(long)]
    pub standalone: bool,
    /// Offer the browser tools in this (headless) session (06 §10).
    #[arg(long)]
    pub enable_browser: bool,
}

impl RunArgs {
    /// The effective mode: `--dangerously-skip-permissions` wins.
    pub fn mode(&self) -> Option<Mode> {
        if self.dangerously_skip_permissions {
            Some(Mode::Bypass)
        } else {
            self.permission_mode.map(PermissionMode::to_mode)
        }
    }

    /// `--allow`/`--deny`/`--settings`/`--add-dir`/`--enable-browser` as the session-only
    /// settings layer (paths made absolute).
    pub fn overlay(&self) -> Option<genie_protocol::api::session::SessionSettingsOverlay> {
        if self.allow.is_empty()
            && self.deny.is_empty()
            && self.settings.is_none()
            && self.add_dir.is_empty()
            && !self.enable_browser
        {
            return None;
        }
        let abs = |p: &PathBuf| {
            std::path::absolute(p)
                .unwrap_or_else(|_| p.clone())
                .to_string_lossy()
                .into_owned()
        };
        Some(genie_protocol::api::session::SessionSettingsOverlay {
            allow: self.allow.clone(),
            deny: self.deny.clone(),
            settings_path: self.settings.as_ref().map(abs),
            add_dirs: self.add_dir.iter().map(abs).collect(),
            enable_browser: self.enable_browser,
        })
    }
}

/// `30`, `30s`, `5m`, `1h`, `250ms`.
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    let (num, unit) = match s.find(|c: char| !c.is_ascii_digit() && c != '.') {
        Some(i) => s.split_at(i),
        None => (s, "s"),
    };
    let n: f64 = num.parse().map_err(|_| format!("bad duration `{s}`"))?;
    let secs = match unit {
        "ms" => n / 1000.0,
        "s" | "" => n,
        "m" => n * 60.0,
        "h" => n * 3600.0,
        _ => return Err(format!("bad duration unit in `{s}` (use ms, s, m or h)")),
    };
    if !secs.is_finite() || secs < 0.0 {
        return Err(format!("bad duration `{s}`"));
    }
    Ok(Duration::from_secs_f64(secs))
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List, read and continue desktop chats in text.
    Chat {
        #[command(subcommand)]
        action: Option<ChatAction>,
    },
    /// List sessions (Code by default).
    Sessions {
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// Answer pending asks from another terminal.
    Approvals {
        #[command(subcommand)]
        action: Option<ApprovalsAction>,
    },
    /// One-time import from Claude Code, Claude Desktop or OpenCode.
    Import(ImportArgs),
    /// Export a session.
    Export {
        session: String,
        #[arg(long)]
        sanitize: bool,
        #[arg(long, value_enum, default_value = "json")]
        format: ExportFormatArg,
    },
    /// Configuration through the daemon.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Health checks.
    Doctor,
    /// Self-update for CLI-only installs.
    Update,
    /// Daemon control; bare `genie daemon` runs it.
    Daemon {
        #[command(subcommand)]
        action: Option<DaemonAction>,
        /// Exit after this many seconds with no clients and no work.
        #[arg(long, default_value_t = genie_daemon::IDLE_TIMEOUT.as_secs())]
        idle_timeout_secs: u64,
        /// Private daemon owned by a parent process; exits on stdin EOF (01 §4.4).
        #[arg(long, requires = "run_dir")]
        standalone: bool,
        /// Root directory for a standalone daemon's endpoint files.
        #[arg(long)]
        run_dir: Option<PathBuf>,
    },
    /// Connect to (starting if needed) the engine and print its status.
    Status {
        #[arg(long)]
        standalone: bool,
        #[arg(long)]
        json: bool,
    },
    /// Add provider credentials.
    Login {
        provider: String,
        #[arg(long)]
        device_code: bool,
    },
    /// Remove provider credentials.
    Logout { provider: String },
    /// MCP servers.
    Mcp {
        #[command(subcommand)]
        action: Option<McpAction>,
    },
    /// Workspace trust.
    Trust {
        #[command(subcommand)]
        action: Option<TrustAction>,
    },
    /// Shim that runs an imported Claude hook script under Genie's hook envelope.
    HookAdapter {
        #[arg(value_enum)]
        flavor: HookFlavor,
        /// The hook command (a shell command line).
        #[arg(last = true, required = true)]
        script: Vec<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum ChatAction {
    /// Desktop chats: title, project, updated.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Print the active branch and continue the chat here.
    Open { id: String },
    /// Start a new chat.
    New,
}

#[derive(Debug, Subcommand)]
pub enum ApprovalsAction {
    List {
        #[arg(long)]
        json: bool,
    },
    Approve {
        id: String,
        /// Save an "always" rule with the ask's suggested scope.
        #[arg(long)]
        always: bool,
    },
    Deny {
        id: String,
        #[arg(long, short)]
        message: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ImportSource {
    ClaudeCode,
    ClaudeDesktop,
    Opencode,
}

#[derive(Debug, Args)]
pub struct ImportArgs {
    #[arg(value_enum)]
    pub source: Option<ImportSource>,
    /// Only project-level items.
    #[arg(long, conflicts_with = "user")]
    pub project: bool,
    /// Only user-level items.
    #[arg(long)]
    pub user: bool,
    /// Preview only; nothing is written.
    #[arg(long)]
    pub dry_run: bool,
    /// Apply without asking (also confirms appending to an existing GENIE.md).
    #[arg(long, short)]
    pub yes: bool,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ExportFormatArg {
    Json,
    Md,
}

#[derive(Debug, Subcommand)]
pub enum ConfigAction {
    Get {
        key: Option<String>,
    },
    Set {
        key: String,
        /// JSON value (bare words are strings).
        value: String,
        #[arg(long, value_enum, default_value = "user")]
        scope: ScopeArg,
    },
    /// Show configuration problems.
    Doctor,
    /// Show where settings files live.
    Path,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ScopeArg {
    User,
    Project,
    Local,
}

#[derive(Debug, Subcommand)]
pub enum DaemonAction {
    Status {
        #[arg(long)]
        json: bool,
    },
    Stop,
    Restart {
        /// Stop running turns instead of waiting for them.
        #[arg(long)]
        force: bool,
    },
    Logs {
        #[arg(long, short = 'n', default_value_t = 50)]
        lines: usize,
    },
}

#[derive(Debug, Subcommand)]
pub enum McpAction {
    List,
    Add {
        name: String,
        #[arg(long, value_enum, default_value = "user")]
        scope: ScopeArg,
        /// Remote server URL instead of a command.
        #[arg(long)]
        url: Option<String>,
        /// Command and arguments of a local server.
        #[arg(last = true)]
        command: Vec<String>,
    },
    Remove {
        name: String,
        #[arg(long, value_enum, default_value = "user")]
        scope: ScopeArg,
    },
    Auth {
        name: String,
    },
    Logs {
        name: String,
        #[arg(long, short = 'n', default_value_t = 100)]
        lines: u32,
    },
}

#[derive(Debug, Subcommand)]
pub enum TrustAction {
    List,
    Revoke { path: PathBuf },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum HookFlavor {
    Claude,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("genie").chain(args.iter().copied())).unwrap()
    }

    #[test]
    fn prompt_and_subcommands_coexist() {
        let c = parse(&["fix the bug"]);
        assert_eq!(c.run.prompt.as_deref(), Some("fix the bug"));
        assert!(c.command.is_none());
        let c = parse(&["chat", "list"]);
        assert!(matches!(
            c.command,
            Some(Command::Chat {
                action: Some(ChatAction::List { .. })
            })
        ));
        let c = parse(&["daemon"]);
        assert!(matches!(
            c.command,
            Some(Command::Daemon { action: None, .. })
        ));
    }

    #[test]
    fn headless_flags() {
        let c = parse(&[
            "-p",
            "hello",
            "--output-format",
            "stream-json",
            "--on-ask",
            "deny",
            "--ask-timeout",
            "5m",
            "--allow",
            "Bash(git status:*)",
            "--resume",
            "ses_1",
        ]);
        assert!(c.run.print);
        assert_eq!(c.run.prompt.as_deref(), Some("hello"));
        assert_eq!(c.run.output_format, OutputFormat::StreamJson);
        assert_eq!(c.run.on_ask, OnAsk::Deny);
        assert_eq!(c.run.ask_timeout, Some(Duration::from_secs(300)));
        assert_eq!(c.run.resume.as_deref(), Some("ses_1"));
        let c = parse(&["-r"]);
        assert_eq!(c.run.resume.as_deref(), Some(""));
        let c = parse(&["--dangerously-skip-permissions"]);
        assert_eq!(c.run.mode(), Some(Mode::Bypass));
    }

    #[test]
    fn enable_browser_goes_into_the_session_layer() {
        assert!(parse(&["-p", "x"]).run.overlay().is_none());
        let o = parse(&["-p", "x", "--enable-browser"])
            .run
            .overlay()
            .unwrap();
        assert!(o.enable_browser && o.allow.is_empty());
        let o = parse(&["--allow", "WebFetch"]).run.overlay().unwrap();
        assert!(!o.enable_browser);
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("250ms").unwrap(), Duration::from_millis(250));
        assert_eq!(parse_duration("2").unwrap(), Duration::from_secs(2));
        assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
        assert!(parse_duration("3x").is_err());
    }

    #[test]
    fn hook_adapter_takes_the_rest() {
        let c = parse(&["hook-adapter", "claude", "--", "./check.sh", "--strict"]);
        match c.command {
            Some(Command::HookAdapter { script, .. }) => {
                assert_eq!(script, ["./check.sh", "--strict"]);
            }
            other => panic!("{other:?}"),
        }
    }
}
