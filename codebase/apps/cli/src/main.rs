//! `genie`: the CLI binary. The same binary runs the shared daemon (`genie daemon`).
//!
//! This milestone (spec/01-architecture.md) ships the engine plumbing commands:
//! `genie daemon`, `genie status` and a first `genie doctor`. The TUI, `-p` headless mode
//! and the importer arrive with spec 07.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use genie_client::connection::ClientInfo;
use genie_client::{Endpoint, EnsureOptions, EnsureOutcome, EnsureState, ensure};
use genie_daemon::{DaemonConfig, DaemonError, EXIT_ALREADY_RUNNING};
use genie_protocol::Channel;
use genie_protocol::handshake::{ClientCapabilities, ClientKind};
use genie_protocol::methods::{self, DaemonStatus};

/// The channel is a build-time setting (01 §2.1 rule 6, D40). `--channel` exists for
/// development builds.
const BUILD_CHANNEL: &str = match option_env!("GENIE_CHANNEL") {
    Some(c) => c,
    None => "stable",
};

#[derive(Parser)]
#[command(name = "genie", version, about = "Genie: multi-provider AI harness")]
struct Cli {
    #[arg(long, global = true, env = "GENIE_CHANNEL_OVERRIDE", default_value = BUILD_CHANNEL, hide = true)]
    channel: Channel,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the shared engine daemon (normally started automatically).
    Daemon {
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
        /// Use a private daemon instead of the shared one (CI).
        #[arg(long)]
        standalone: bool,
        #[arg(long)]
        json: bool,
    },
    /// Check the installation.
    Doctor,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
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
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<ExitCode> {
    match cli.command {
        Command::Daemon {
            idle_timeout_secs,
            standalone,
            run_dir,
        } => {
            let endpoint = match &run_dir {
                Some(dir) => Endpoint::in_dir(dir, cli.channel)?,
                None => Endpoint::for_channel(cli.channel)?,
            };
            let mut cfg = DaemonConfig::new(endpoint);
            cfg.idle_timeout = Duration::from_secs(idle_timeout_secs);
            cfg.lease_on_stdin = standalone;
            match genie_daemon::run(cfg).await {
                Ok(_) => Ok(ExitCode::SUCCESS),
                Err(DaemonError::AlreadyRunning) => {
                    eprintln!("already_running");
                    Ok(ExitCode::from(
                        u8::try_from(EXIT_ALREADY_RUNNING).unwrap_or(1),
                    ))
                }
                Err(e) => Err(e.into()),
            }
        }
        Command::Status { standalone, json } => {
            let st = if standalone {
                status_standalone(cli.channel).await?
            } else {
                status_shared(cli.channel).await?
            };
            if json {
                println!("{}", serde_json::to_string_pretty(&st)?);
            } else {
                println!("engine   {} ({})", st.version, st.channel);
                println!("pid      {}", st.pid);
                println!("endpoint {}", st.socket);
                println!("uptime   {}s", st.uptime_s);
                println!("running  {} session(s)", st.sessions_running);
                println!("clients  {}", st.clients.len());
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Doctor => doctor(cli.channel).await,
    }
}

fn client_info() -> ClientInfo {
    ClientInfo {
        kind: ClientKind::Cli,
        version: env!("CARGO_PKG_VERSION").to_string(),
        instance_id: format!("cli_{}", ulid::Ulid::new().to_string().to_lowercase()),
        capabilities: ClientCapabilities::default(),
    }
}

fn progress(s: EnsureState) {
    match s {
        EnsureState::Spawn => eprintln!("Starting engine…"),
        EnsureState::Restarting => eprintln!("Updating engine…"),
        _ => {}
    }
}

async fn connect_shared(channel: Channel) -> Result<genie_client::Connection> {
    let endpoint = Endpoint::for_channel(channel)?;
    let exe = std::env::current_exe().context("locating the genie binary")?;
    let args: Vec<OsString> = vec!["daemon".into(), "--channel".into(), channel.as_str().into()];
    let opts = EnsureOptions::new(endpoint, client_info(), Some(exe), args);
    match ensure(&opts, progress).await? {
        EnsureOutcome::Ready(c) => Ok(c),
        EnsureOutcome::Degraded(c) => {
            eprintln!("Engine will update when tasks finish.");
            Ok(c)
        }
        EnsureOutcome::BusyUpdating { running } => {
            bail!("Genie is finishing {running} task(s) before updating. Try again shortly.")
        }
    }
}

async fn status_shared(channel: Channel) -> Result<DaemonStatus> {
    let conn = connect_shared(channel).await?;
    Ok(conn
        .request(methods::DAEMON_STATUS, serde_json::Value::Null)
        .await?)
}

/// `--standalone` (01 §4.4): private daemon on a temp endpoint with an ephemeral token,
/// owned by this process through its stdin lease.
async fn status_standalone(channel: Channel) -> Result<DaemonStatus> {
    let dir = tempfile::Builder::new()
        .prefix("genie-standalone-")
        .tempdir()?;
    let exe = std::env::current_exe()?;
    let mut child = tokio::process::Command::new(exe)
        .args([
            "daemon",
            "--standalone",
            "--channel",
            channel.as_str(),
            "--run-dir",
        ])
        .arg(dir.path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .context("starting standalone daemon")?;
    let endpoint = Endpoint::in_dir(dir.path(), channel)?;
    let opts = EnsureOptions::new(endpoint, client_info(), None, vec![]);
    let conn = match ensure(&opts, |_| {}).await? {
        EnsureOutcome::Ready(c) | EnsureOutcome::Degraded(c) => c,
        EnsureOutcome::BusyUpdating { .. } => bail!("standalone daemon is busy"),
    };
    let st: DaemonStatus = conn
        .request(methods::DAEMON_STATUS, serde_json::Value::Null)
        .await?;
    conn.close().await;
    drop(child.stdin.take()); // end the lease
    let _ = tokio::time::timeout(Duration::from_secs(10), child.wait()).await;
    Ok(st)
}

async fn doctor(channel: Channel) -> Result<ExitCode> {
    let mut failed = false;
    let mut row = |name: &str, ok: bool, detail: String| {
        failed |= !ok;
        println!("{} {name:<18} {detail}", if ok { "ok  " } else { "FAIL" });
    };
    match status_shared(channel).await {
        Ok(st) => row(
            "daemon",
            true,
            format!(
                "engine {} on {} (cli {})",
                st.version,
                st.socket,
                env!("CARGO_PKG_VERSION")
            ),
        ),
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
    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}
