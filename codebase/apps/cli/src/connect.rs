//! Connecting to the engine (07 §1.4, §4.5): `ensure` on the shared daemon with a
//! "Starting engine…" line, or a private `--standalone` daemon bound to our stdin.

use std::ffi::OsString;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use genie_client::connection::ClientInfo;
use genie_client::{
    Connection, Endpoint, EnsureOptions, EnsureOutcome, EnsureState, Incoming, ensure,
};
use genie_protocol::Channel;
use genie_protocol::handshake::{AskKind, ClientCapabilities, ClientKind};
use tokio::sync::mpsc::UnboundedReceiver;

/// A live connection plus whatever keeps it alive (a standalone daemon child).
pub struct Link {
    pub conn: Connection,
    pub rx: UnboundedReceiver<Incoming>,
    pub info: ClientInfo,
    standalone: Option<Standalone>,
}

struct Standalone {
    child: tokio::process::Child,
    _dir: tempfile::TempDir,
}

impl Link {
    /// Wrap an existing connection (tests, in-memory transport).
    pub fn from_connection(conn: Connection, info: ClientInfo) -> Result<Link> {
        let rx = conn
            .take_incoming()
            .context("connection incoming stream already taken")?;
        Ok(Link {
            conn,
            rx,
            info,
            standalone: None,
        })
    }

    /// Close the connection and, for `--standalone`, end the daemon's stdin lease.
    pub async fn shutdown(self) {
        let Link {
            conn, standalone, ..
        } = self;
        conn.close().await;
        if let Some(mut s) = standalone {
            drop(s.child.stdin.take());
            let _ = tokio::time::timeout(Duration::from_secs(10), s.child.wait()).await;
        }
    }
}

/// Who this client is. A fresh instance id per run: the CLI does not reclaim leases
/// across runs.
pub fn client_info(kind: ClientKind) -> ClientInfo {
    let asks = match kind {
        // The TUI renders permission prompts and simple forms in its footer.
        ClientKind::Cli => vec![AskKind::Permission, AskKind::Question, AskKind::Elicitation],
        // Headless answers asks itself from the event stream (07 §4.4).
        _ => vec![],
    };
    ClientInfo {
        kind,
        version: env!("CARGO_PKG_VERSION").to_string(),
        instance_id: format!(
            "{}_{}",
            match kind {
                ClientKind::Headless => "headless",
                _ => "cli",
            },
            ulid::Ulid::new().to_string().to_lowercase()
        ),
        capabilities: ClientCapabilities {
            asks,
            notifications: false,
            ..Default::default()
        },
    }
}

fn progress(quiet: bool) -> impl Fn(EnsureState) {
    move |s| {
        if quiet {
            return;
        }
        match s {
            EnsureState::Spawn => eprintln!("Starting engine…"),
            EnsureState::Restarting => eprintln!("Updating engine…"),
            _ => {}
        }
    }
}

/// Connect to the shared daemon, starting it if needed.
pub async fn shared(channel: Channel, kind: ClientKind, quiet: bool) -> Result<Link> {
    let info = client_info(kind);
    let endpoint = Endpoint::for_channel(channel)?;
    let exe = std::env::current_exe().context("locating the genie binary")?;
    let args: Vec<OsString> = vec!["daemon".into(), "--channel".into(), channel.as_str().into()];
    let opts = EnsureOptions::new(endpoint, info.clone(), Some(exe), args);
    let conn = match ensure(&opts, progress(quiet)).await? {
        EnsureOutcome::Ready(c) => c,
        EnsureOutcome::Degraded(c) => {
            if !quiet {
                eprintln!("Engine will update when tasks finish.");
            }
            c
        }
        EnsureOutcome::BusyUpdating { running } => {
            bail!("Genie is finishing {running} task(s) before updating. Try again shortly.")
        }
    };
    Link::from_connection(conn, info)
}

/// `--standalone` (01 §4.4): a private daemon on a temp endpoint, owned through its stdin.
pub async fn standalone(channel: Channel, kind: ClientKind) -> Result<Link> {
    let info = client_info(kind);
    let dir = tempfile::Builder::new()
        .prefix("genie-standalone-")
        .tempdir()?;
    let exe = std::env::current_exe()?;
    let child = tokio::process::Command::new(exe)
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
    let opts = EnsureOptions::new(endpoint, info.clone(), None, vec![]);
    let conn = match ensure(&opts, |_| {}).await? {
        EnsureOutcome::Ready(c) | EnsureOutcome::Degraded(c) => c,
        EnsureOutcome::BusyUpdating { .. } => bail!("standalone daemon is busy"),
    };
    let mut link = Link::from_connection(conn, info)?;
    link.standalone = Some(Standalone { child, _dir: dir });
    Ok(link)
}

/// Shared or standalone.
pub async fn connect(
    channel: Channel,
    kind: ClientKind,
    standalone_: bool,
    quiet: bool,
) -> Result<Link> {
    if standalone_ {
        standalone(channel, kind).await
    } else {
        shared(channel, kind, quiet).await
    }
}
