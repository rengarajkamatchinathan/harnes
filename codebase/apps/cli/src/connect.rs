//! Connecting to the engine (07 §1.4, §4.5): `ensure` on the shared daemon with a
//! "Starting engine…" line, or a private `--standalone` daemon bound to our stdin.

use std::ffi::OsString;
use std::io::{IsTerminal, Write};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use genie_client::connection::{ClientError, ClientInfo};
use genie_client::{
    Connection, Endpoint, EnsureOptions, EnsureOutcome, EnsureState, Incoming, ensure,
};
use genie_protocol::api::daemon::UnlockParams;
use genie_protocol::handshake::{AskKind, ClientCapabilities, ClientKind};
use genie_protocol::methods as m;
use genie_protocol::{Channel, ErrorKind};
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

/// Connect to the shared daemon, starting it if needed. A daemon whose secrets file is
/// locked (02 §2) gets the passphrase from the terminal, then we reconnect.
pub async fn shared(channel: Channel, kind: ClientKind, quiet: bool) -> Result<Link> {
    let info = client_info(kind);
    let endpoint = Endpoint::for_channel(channel)?;
    let exe = std::env::current_exe().context("locating the genie binary")?;
    let args: Vec<OsString> = vec!["daemon".into(), "--channel".into(), channel.as_str().into()];
    let opts = EnsureOptions::new(endpoint, info.clone(), Some(exe), args);
    let mut conn = ensure_once(&opts, quiet).await?;
    if conn.init.locked.is_some() {
        conn = unlock_and_reconnect(conn, &opts, quiet).await?;
    }
    Link::from_connection(conn, info)
}

async fn ensure_once(opts: &EnsureOptions, quiet: bool) -> Result<Connection> {
    let conn = match ensure(opts, progress(quiet)).await? {
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
    Ok(conn)
}

/// Locked phase: prompt, `secrets.unlock`, then wait for the engine to come back
/// unlocked (the daemon restarts in-process and drops our connection).
async fn unlock_and_reconnect(
    conn: Connection,
    opts: &EnsureOptions,
    quiet: bool,
) -> Result<Connection> {
    if !std::io::stdin().is_terminal() {
        bail!(
            "Genie's secrets file is locked. Set GENIE_PASSPHRASE, or run `genie` in a terminal \
             to enter the passphrase."
        );
    }
    let mut ask = |attempt: u32| {
        read_secret(if attempt == 1 {
            "Passphrase for Genie's secrets: "
        } else {
            "Wrong passphrase. Try again: "
        })
    };
    unlock_with(&conn, &mut ask).await?;
    conn.close().await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        tokio::time::sleep(Duration::from_millis(200)).await;
        if let Ok(c) = ensure_once(opts, quiet).await {
            if c.init.locked.is_none() {
                return Ok(c);
            }
            c.close().await;
        }
        if tokio::time::Instant::now() > deadline {
            bail!("the engine did not come back after unlocking");
        }
    }
}

/// Send `secrets.unlock` with passphrases from `ask(attempt)` (3 attempts). `Ok(false)`
/// when the daemon was not locked; `Ok(true)` once unlocked.
pub async fn unlock_with(
    conn: &Connection,
    ask: &mut dyn FnMut(u32) -> Result<String>,
) -> Result<bool> {
    if conn.init.locked.is_none() {
        return Ok(false);
    }
    for attempt in 1..=3 {
        let passphrase = ask(attempt)?;
        if passphrase.is_empty() {
            bail!("no passphrase given");
        }
        match conn
            .call::<m::SecretsUnlock>(UnlockParams { passphrase })
            .await
        {
            Ok(r) if r.ok => return Ok(true),
            Ok(_) => {}
            Err(ClientError::Rpc(e)) if e.kind() == ErrorKind::InvalidParams => {}
            Err(e) => return Err(e.into()),
        }
    }
    bail!("wrong passphrase (3 attempts); set GENIE_PASSPHRASE or try again")
}

/// Read a secret without echo (raw mode, masked); a plain line when stdin is not a
/// terminal.
pub fn read_secret(prompt: &str) -> Result<String> {
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
    let mut conn = match ensure(&opts, |_| {}).await? {
        EnsureOutcome::Ready(c) | EnsureOutcome::Degraded(c) => c,
        EnsureOutcome::BusyUpdating { .. } => bail!("standalone daemon is busy"),
    };
    if conn.init.locked.is_some() {
        conn = unlock_and_reconnect(conn, &opts, true).await?;
    }
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use serde_json::{Value, json};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    /// A daemon in its locked phase: answers `initialize` with `locked` and accepts only
    /// the passphrase `right`.
    async fn fake_locked(stream: tokio::io::DuplexStream) -> Vec<String> {
        let (r, mut w) = tokio::io::split(stream);
        let mut lines = BufReader::new(r).lines();
        let mut tried = vec![];
        while let Ok(Some(l)) = lines.next_line().await {
            let v: Value = serde_json::from_str(&l).unwrap();
            let id = v["id"].clone();
            let reply = match v["method"].as_str() {
                Some("initialize") => json!({"jsonrpc": "2.0", "id": id, "result": {
                    "engine_version": "1.0.0", "build": "x", "channel": "dev",
                    "protocol": genie_protocol::PROTOCOL_VERSION,
                    "locked": {"reason": "passphrase_required"}}}),
                Some("secrets.unlock") => {
                    let p = v["params"]["passphrase"].as_str().unwrap().to_string();
                    tried.push(p.clone());
                    if p == "right" {
                        json!({"jsonrpc": "2.0", "id": id, "result": {"ok": true}})
                    } else {
                        json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32602,
                            "message": "wrong passphrase"}})
                    }
                }
                _ => continue,
            };
            w.write_all(format!("{reply}\n").as_bytes()).await.unwrap();
        }
        tried
    }

    #[tokio::test]
    async fn unlock_retries_a_wrong_passphrase() {
        let (client, server) = genie_testkit::memory_pair();
        let fake = tokio::spawn(fake_locked(server));
        let info = client_info(ClientKind::Cli);
        let conn = Connection::handshake(Box::new(client), &info, "t".into())
            .await
            .unwrap();
        assert!(conn.init.locked.is_some());
        let mut answers = vec!["right".to_string(), "nope".to_string()];
        let mut asked = vec![];
        let mut ask = |n: u32| {
            asked.push(n);
            Ok(answers.pop().unwrap())
        };
        assert!(unlock_with(&conn, &mut ask).await.unwrap());
        assert_eq!(asked, [1, 2]);
        conn.close().await;
        assert_eq!(fake.await.unwrap(), ["nope", "right"]);
    }

    #[tokio::test]
    async fn three_wrong_passphrases_give_up() {
        let (client, server) = genie_testkit::memory_pair();
        let _fake = tokio::spawn(fake_locked(server));
        let conn =
            Connection::handshake(Box::new(client), &client_info(ClientKind::Cli), "t".into())
                .await
                .unwrap();
        let mut ask = |_| Ok("bad".to_string());
        let e = unlock_with(&conn, &mut ask).await.unwrap_err();
        assert!(e.to_string().contains("wrong passphrase"), "{e}");
    }
}
