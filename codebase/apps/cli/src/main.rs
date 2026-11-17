//! `genie`: the CLI binary (spec/07-cli.md). The same binary runs the shared daemon
//! (`genie daemon`). Everything testable lives in the `genie_cli` library.

use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use genie_cli::args::{ChatAction, Cli, Command, DaemonAction, RunArgs};
use genie_cli::connect::{self, Link};
use genie_cli::session::{Spec, Target};
use genie_cli::tui::run as tui;
use genie_cli::{cmd, errors, headless, hook_adapter, import};
use genie_client::Endpoint;
use genie_daemon::{DaemonConfig, DaemonError, EXIT_ALREADY_RUNNING};
use genie_protocol::Channel;
use genie_protocol::api::daemon::DaemonRestartParams;
use genie_protocol::common::{Mode, SessionKind};
use genie_protocol::handshake::ClientKind;
use genie_protocol::methods::{self as m, DaemonStatus};
use tokio_util::sync::CancellationToken;

fn main() -> ExitCode {
    let cli = Cli::parse();
    if cli.version {
        println!("genie {}", genie_cli::args::version_line());
        return ExitCode::SUCCESS;
    }
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    match rt.block_on(run(cli)) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("genie: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

async fn link(channel: Channel) -> Result<Link> {
    connect::shared(channel, ClientKind::Cli, false).await
}

/// Run a subcommand body with a connection and real stdout/stderr, then close.
macro_rules! with_conn {
    ($channel:expr, |$conn:ident, $out:ident, $err:ident| $body:expr) => {{
        let l = link($channel).await?;
        let mut so = std::io::stdout();
        let mut se = std::io::stderr();
        let code = {
            let $conn = &l.conn;
            let $out: &mut (dyn Write + Send) = &mut so;
            let $err: &mut (dyn Write + Send) = &mut se;
            $body
        };
        l.shutdown().await;
        Ok(code)
    }};
}

async fn run(cli: Cli) -> Result<u8> {
    let channel = cli.channel;
    let Some(command) = cli.command else {
        return run_session(channel, cli.run).await;
    };
    match command {
        Command::Daemon {
            action: None,
            idle_timeout_secs,
            standalone,
            run_dir,
        } => run_daemon(channel, idle_timeout_secs, standalone, run_dir).await,
        Command::Daemon {
            action: Some(a), ..
        } => daemon_action(channel, a).await,
        Command::Status { standalone, json } => {
            let st = if standalone {
                status_standalone(channel).await?
            } else {
                status_shared(channel).await?
            };
            print_status(&st, json)?;
            Ok(0)
        }
        Command::Doctor => doctor(channel).await,
        Command::Update => {
            eprintln!(
                "genie update: self-update is not available in this build yet; install the \
                 latest release for your channel ({}) from the download page.",
                genie_cli::args::BUILD_CHANNEL
            );
            Ok(1)
        }
        Command::Chat { action } => match action.unwrap_or(ChatAction::List { json: false }) {
            ChatAction::List { json } => {
                with_conn!(channel, |c, o, e| cmd::chat_list(c, json, o, e).await)
            }
            ChatAction::Open { id } => tui::run(chat_options(channel, Target::Resume(id))).await,
            ChatAction::New => tui::run(chat_options(channel, Target::New)).await,
        },
        Command::Sessions { all, json } => {
            let kind = (!all).then_some(SessionKind::Code);
            with_conn!(channel, |c, o, e| cmd::sessions(c, kind, json, o, e).await)
        }
        Command::Approvals { action } => {
            with_conn!(channel, |c, o, e| cmd::approvals(c, action, o, e).await)
        }
        Command::Export {
            session,
            sanitize,
            format,
        } => with_conn!(channel, |c, o, e| cmd::export(
            c, session, format, sanitize, o, e
        )
        .await),
        Command::Config { action } => {
            let dir = cwd();
            let home = tui::genie_home();
            with_conn!(channel, |c, o, e| cmd::config(
                c,
                action,
                &dir,
                home.as_deref(),
                o,
                e
            )
            .await)
        }
        Command::Login {
            provider,
            device_code,
        } => {
            let secret = if device_code {
                None
            } else {
                Some(read_secret(&format!("API key for {provider}: "))?)
            };
            with_conn!(channel, |c, o, e| cmd::login(
                c,
                provider,
                device_code,
                secret,
                o,
                e
            )
            .await)
        }
        Command::Logout { provider } => {
            with_conn!(channel, |c, o, e| cmd::logout(c, provider, o, e).await)
        }
        Command::Mcp { action } => {
            let dir = cwd();
            with_conn!(channel, |c, o, e| cmd::mcp(c, action, &dir, o, e).await)
        }
        Command::Trust { action } => {
            let dir = cwd();
            with_conn!(channel, |c, o, e| cmd::trust(c, action, &dir, o, e).await)
        }
        Command::Import(args) => {
            let home = std::env::home_dir().context("no home directory")?;
            let roots = import::Roots::detect(home, cwd());
            let mut confirm = |q: &str| ask_yes_no(q);
            let mut out = std::io::stdout();
            cmd::import_cmd(
                &args,
                &roots,
                &mut out,
                &mut confirm,
                &mut import::NoKeychain,
            )
        }
        Command::HookAdapter { script, .. } => {
            let code = hook_adapter::run(&script)?;
            Ok(u8::try_from(code).unwrap_or(1))
        }
    }
}

fn chat_options(channel: Channel, target: Target) -> tui::Options {
    tui::Options {
        channel,
        standalone: false,
        target,
        spec: Spec {
            kind: SessionKind::Chat,
            mode: None,
            ..Spec::code()
        },
        cwd: cwd(),
        prompt: None,
        bypass_allowed: false,
        print_history: true,
        genie_home: tui::genie_home(),
    }
}

fn spec_from(run: &RunArgs) -> Spec {
    Spec {
        kind: SessionKind::Code,
        model: run.model.clone(),
        effort: run.effort.clone(),
        agent: run.agent.clone(),
        mode: run.mode(),
        worktree: run.worktree.clone(),
        overlay: overlay_from(run),
    }
}

/// `--allow`/`--deny`/`--settings`/`--add-dir` as the session-only settings layer.
fn overlay_from(run: &RunArgs) -> Option<genie_protocol::api::session::SessionSettingsOverlay> {
    if run.allow.is_empty()
        && run.deny.is_empty()
        && run.settings.is_none()
        && run.add_dir.is_empty()
    {
        return None;
    }
    let abs = |p: &std::path::PathBuf| {
        std::path::absolute(p)
            .unwrap_or_else(|_| p.clone())
            .to_string_lossy()
            .into_owned()
    };
    Some(genie_protocol::api::session::SessionSettingsOverlay {
        allow: run.allow.clone(),
        deny: run.deny.clone(),
        settings_path: run.settings.as_ref().map(abs),
        add_dirs: run.add_dir.iter().map(abs).collect(),
        enable_browser: false,
    })
}

fn target_from(run: &RunArgs) -> Target {
    match (&run.resume, run.continue_) {
        (Some(id), _) => Target::Resume(id.clone()),
        (None, true) => Target::Continue,
        _ => Target::New,
    }
}

/// `genie [prompt]`: TUI on a terminal, headless on `-p` or any non-TTY (§1.4, §4.1).
async fn run_session(channel: Channel, run: RunArgs) -> Result<u8> {
    let tty = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    if run.print || !tty {
        return run_headless(channel, run).await;
    }
    if run.worktree.as_deref().is_some_and(|n| !n.is_empty()) {
        eprintln!(
            "genie: worktree names are not supported by this engine build; using a new unnamed worktree"
        );
    }
    let opts = tui::Options {
        channel,
        standalone: run.standalone,
        target: target_from(&run),
        spec: spec_from(&run),
        cwd: cwd(),
        prompt: run.prompt.clone(),
        bypass_allowed: run.dangerously_skip_permissions,
        print_history: false,
        genie_home: tui::genie_home(),
    };
    tui::run(opts).await
}

async fn run_headless(channel: Channel, run: RunArgs) -> Result<u8> {
    let mut prompt = run.prompt.clone().unwrap_or_default();
    if !std::io::stdin().is_terminal() {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s)?;
        if !s.trim().is_empty() {
            prompt = if prompt.is_empty() {
                s
            } else {
                format!("{prompt}\n\n{s}")
            };
        }
    }
    if prompt.trim().is_empty() {
        eprintln!("genie: no prompt (pass one as an argument or on stdin)");
        return Ok(headless::EXIT_ERROR);
    }
    let target = target_from(&run);
    if target == Target::Resume(String::new()) {
        eprintln!("genie: --resume needs a session id in headless mode");
        return Ok(headless::EXIT_ERROR);
    }
    let mut opts = headless::Options::new(prompt);
    opts.format = run.output_format;
    opts.target = target;
    opts.spec = spec_from(&run);
    opts.cwd = cwd();
    opts.max_turns = run.max_turns;
    opts.ask_timeout = run.ask_timeout;
    opts.on_ask = run.on_ask;
    opts.ci = std::env::var("CI").is_ok_and(|v| v == "true" || v == "1");
    opts.trust = run.trust;
    opts.allow = run.allow.clone();
    opts.deny = run.deny.clone();
    opts.settings = run.settings.clone();
    opts.add_dirs = run.add_dir.clone();
    if opts.spec.mode == Some(Mode::Bypass) {
        eprintln!(
            "genie: bypass mode: no prompts; deny rules, policy and sensitive-action confirmations still apply"
        );
    }

    let mut l = connect::connect(channel, ClientKind::Headless, run.standalone, true).await?;
    let cancel = CancellationToken::new();
    let mut so = std::io::stdout();
    let mut se = std::io::stderr();
    let outcome = {
        let fut = headless::run(
            &l.conn,
            &mut l.rx,
            &opts,
            headless::Sinks {
                out: &mut so,
                err: &mut se,
            },
            cancel.clone(),
        );
        tokio::pin!(fut);
        loop {
            tokio::select! {
                r = &mut fut => break r?,
                r = tokio::signal::ctrl_c(), if !cancel.is_cancelled() => {
                    if r.is_ok() {
                        cancel.cancel();
                    }
                }
            }
        }
    };
    l.shutdown().await;
    Ok(outcome.exit_code)
}

fn ask_yes_no(q: &str) -> bool {
    if !std::io::stdin().is_terminal() {
        return false;
    }
    eprint!("{q} [y/N] ");
    let _ = std::io::stderr().flush();
    let mut s = String::new();
    std::io::stdin().read_line(&mut s).is_ok() && matches!(s.trim(), "y" | "Y" | "yes")
}

/// Read a secret without echo (raw mode, masked).
fn read_secret(prompt: &str) -> Result<String> {
    use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
    if !std::io::stdin().is_terminal() {
        let mut s = String::new();
        std::io::stdin().read_line(&mut s)?;
        return Ok(s.trim().to_string());
    }
    eprint!("{prompt}");
    let _ = std::io::stderr().flush();
    crossterm::terminal::enable_raw_mode()?;
    let mut s = String::new();
    let res = loop {
        match event::read() {
            Ok(Event::Key(k)) if k.kind != KeyEventKind::Release => match k.code {
                KeyCode::Enter => break Ok(()),
                KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                    break Err(anyhow::anyhow!("cancelled"));
                }
                KeyCode::Backspace => {
                    s.pop();
                }
                KeyCode::Char(c) => s.push(c),
                _ => {}
            },
            Ok(Event::Paste(p)) => s.push_str(p.trim()),
            Ok(_) => {}
            Err(e) => break Err(e.into()),
        }
    };
    let _ = crossterm::terminal::disable_raw_mode();
    eprintln!();
    res.map(|()| s)
}

// ---------------------------------------------------------------- daemon

async fn run_daemon(
    channel: Channel,
    idle: u64,
    standalone: bool,
    run_dir: Option<PathBuf>,
) -> Result<u8> {
    let endpoint = match &run_dir {
        Some(dir) => Endpoint::in_dir(dir, channel)?,
        None => Endpoint::for_channel(channel)?,
    };
    let mut cfg = DaemonConfig::new(endpoint);
    cfg.idle_timeout = Duration::from_secs(idle);
    cfg.lease_on_stdin = standalone;
    match genie_daemon::run(cfg).await {
        Ok(_) => Ok(0),
        Err(DaemonError::AlreadyRunning) => {
            eprintln!("already_running");
            Ok(u8::try_from(EXIT_ALREADY_RUNNING).unwrap_or(1))
        }
        Err(e) => Err(e.into()),
    }
}

async fn daemon_action(channel: Channel, action: DaemonAction) -> Result<u8> {
    match action {
        DaemonAction::Status { json } => {
            let st = status_shared(channel).await?;
            print_status(&st, json)?;
            Ok(0)
        }
        DaemonAction::Stop | DaemonAction::Restart { .. } => {
            let force = matches!(action, DaemonAction::Restart { force: true });
            let endpoint = Endpoint::for_channel(channel)?;
            if !endpoint.lock_held() {
                println!("The engine is not running.");
                return Ok(0);
            }
            let l = link(channel).await?;
            let r = l
                .conn
                .call::<m::DaemonRestart>(DaemonRestartParams {
                    stop_running: force,
                })
                .await;
            l.shutdown().await;
            if let Err(e) = r {
                eprintln!(
                    "genie: {} (use --force to stop running turns)",
                    errors::describe(&e)
                );
                return Ok(1);
            }
            if matches!(action, DaemonAction::Restart { .. }) {
                tokio::time::sleep(Duration::from_millis(300)).await;
                let st = status_shared(channel).await?;
                println!("Engine restarted (pid {}).", st.pid);
            } else {
                println!("Engine stopping.");
            }
            Ok(0)
        }
        DaemonAction::Logs { lines } => {
            let endpoint = Endpoint::for_channel(channel)?;
            let dir = endpoint.logs_dir();
            let newest =
                newest_file(&dir).with_context(|| format!("no logs in {}", dir.display()))?;
            let text = std::fs::read_to_string(&newest)?;
            let all: Vec<&str> = text.lines().collect();
            for l in &all[all.len().saturating_sub(lines)..] {
                println!("{l}");
            }
            eprintln!("({})", newest.display());
            Ok(0)
        }
    }
}

fn newest_file(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.path().is_file())
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
        .map(|e| e.path())
}

fn print_status(st: &DaemonStatus, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(st)?);
    } else {
        println!("engine   {} ({})", st.version, st.channel);
        println!("pid      {}", st.pid);
        println!("endpoint {}", st.socket);
        println!("uptime   {}s", st.uptime_s);
        println!("running  {} session(s)", st.sessions_running);
        println!("clients  {}", st.clients.len());
    }
    Ok(())
}

async fn status_shared(channel: Channel) -> Result<DaemonStatus> {
    let l = link(channel).await?;
    let st = l
        .conn
        .request(m::DAEMON_STATUS, serde_json::Value::Null)
        .await;
    l.shutdown().await;
    Ok(st?)
}

/// `--standalone` (01 §4.4): private daemon on a temp endpoint, owned via its stdin.
async fn status_standalone(channel: Channel) -> Result<DaemonStatus> {
    let l = connect::standalone(channel, ClientKind::Cli).await?;
    let st: Result<DaemonStatus, _> = l
        .conn
        .request(m::DAEMON_STATUS, serde_json::Value::Null)
        .await;
    l.shutdown().await;
    Ok(st?)
}

/// `genie doctor` (01 §8): the daemon's `doctor.run` checks; local git/rg checks when the
/// daemon can't be reached.
async fn doctor(channel: Channel) -> Result<u8> {
    use genie_protocol::api::daemon::{CheckStatus, DoctorResult};
    use genie_protocol::methods::Method;
    let mut failed = false;
    let mut row = |name: &str, ok: bool, detail: String| {
        failed |= !ok;
        println!("{} {name:<18} {detail}", if ok { "ok  " } else { "FAIL" });
    };
    let remote: Result<DoctorResult> = async {
        let l = link(channel).await?;
        let r = l
            .conn
            .request(m::DoctorRun::NAME, serde_json::Value::Null)
            .await;
        l.shutdown().await;
        Ok(r?)
    }
    .await;
    match remote {
        Ok(res) => {
            println!("     {:<18} cli {}", "client", env!("CARGO_PKG_VERSION"));
            for c in res.checks {
                let tag = match c.status {
                    CheckStatus::Ok => "ok  ",
                    CheckStatus::Warn => "warn",
                    CheckStatus::Fail => "FAIL",
                    CheckStatus::Unknown => "?   ",
                };
                failed |= c.status == CheckStatus::Fail;
                let link = c.fix_link.map(|l| format!("  → {l}")).unwrap_or_default();
                println!("{tag} {:<18} {}{link}", c.id, c.detail);
            }
            return Ok(u8::from(failed));
        }
        Err(e) => row("daemon", false, format!("{e:#}")),
    }
    for (name, args) in [("git", ["--version"]), ("rg", ["--version"])] {
        match tokio::process::Command::new(name).args(args).output().await {
            Ok(o) if o.status.success() => {
                let v = String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_string();
                row(name, true, v);
            }
            _ => row(name, false, format!("`{name}` not found on PATH")),
        }
    }
    Ok(u8::from(failed))
}
