//! stdio transport: newline-delimited JSON-RPC on the server's stdin/stdout; stderr goes
//! to the server's log ring. The process runs in pipe mode inside its own kill-tree
//! (`genie-process`), with a minimal inherited environment plus the configured `env`.

use std::path::Path;

use genie_process::pipe::{Env, PipeSpec, SpawnHook, StdinMode, spawn_pipe};
use genie_protocol::Message;
use genie_secrets::ExposeSecret;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use super::{CHANNEL, CloseReason, ConnectEnv, Inbound, TransportPair};
use crate::config::ServerConfig;
use crate::error::McpError;
use crate::logs::LogRing;

/// Longest accepted stdout line (one JSON-RPC message).
pub const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

/// Run the JSON-RPC framing over any pair of streams (tests use `tokio::io::duplex`).
/// `exit` delivers the process exit description, used as the close reason.
pub fn from_streams<R, W>(
    reader: R,
    writer: W,
    tasks: &TaskTracker,
    logs: LogRing,
    close: CancellationToken,
    exit: Option<oneshot::Receiver<String>>,
) -> TransportPair
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (out_tx, mut out_rx) = mpsc::channel::<Message>(CHANNEL);
    let (in_tx, in_rx) = mpsc::channel::<Inbound>(CHANNEL);

    // Writer: one message per line.
    let wclose = close.clone();
    tasks.spawn(async move {
        let mut w = writer;
        loop {
            let msg = tokio::select! {
                () = wclose.cancelled() => break,
                m = out_rx.recv() => match m { Some(m) => m, None => break },
            };
            let mut line = msg.to_line();
            line.push('\n');
            if w.write_all(line.as_bytes()).await.is_err() || w.flush().await.is_err() {
                break;
            }
        }
        let _ = w.shutdown().await;
    });

    // Reader: parse lines; non-JSON stdout noise is logged, not fatal.
    let rclose = close.clone();
    tasks.spawn(async move {
        let mut r = BufReader::new(reader);
        let mut buf = Vec::new();
        let reason = loop {
            buf.clear();
            let n = tokio::select! {
                () = rclose.cancelled() => break CloseReason::Local,
                n = read_line_capped(&mut r, &mut buf) => n,
            };
            match n {
                Ok(0) => {
                    let why = match exit {
                        Some(rx) => tokio::time::timeout(std::time::Duration::from_secs(2), rx)
                            .await
                            .ok()
                            .and_then(Result::ok)
                            .unwrap_or_else(|| "server closed stdout".into()),
                        None => "server closed stdout".into(),
                    };
                    break CloseReason::Eof(why);
                }
                Ok(_) => {}
                Err(e) => break CloseReason::Error(format!("reading stdout: {e}")),
            }
            let line = String::from_utf8_lossy(&buf);
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match Message::parse(line) {
                Ok(m) => {
                    if in_tx.send(Inbound::Message(m)).await.is_err() {
                        return;
                    }
                }
                Err(_) => logs.push("stdout", line),
            }
        };
        let _ = in_tx.send(Inbound::Closed(reason)).await;
    });

    TransportPair {
        tx: out_tx,
        rx: in_rx,
        close,
    }
}

async fn read_line_capped<R: AsyncBufReadExt + Unpin>(
    r: &mut R,
    buf: &mut Vec<u8>,
) -> std::io::Result<usize> {
    let mut total = 0;
    loop {
        let avail = r.fill_buf().await?;
        if avail.is_empty() {
            return Ok(total);
        }
        let (chunk, done) = match avail.iter().position(|b| *b == b'\n') {
            Some(i) => (&avail[..=i], true),
            None => (avail, false),
        };
        let n = chunk.len();
        if buf.len() + n > MAX_LINE_BYTES {
            return Err(std::io::Error::other("message line exceeds 16 MiB"));
        }
        buf.extend_from_slice(chunk);
        r.consume(n);
        total += n;
        if done {
            return Ok(total);
        }
    }
}

/// Spawn a stdio server and wire it up.
pub fn spawn_process(
    cfg: &ServerConfig,
    env: &ConnectEnv,
    hook: Option<&dyn SpawnHook>,
    default_cwd: Option<&Path>,
) -> Result<TransportPair, McpError> {
    let ServerConfig::Stdio {
        command,
        args,
        env: vars,
        cwd,
    } = cfg
    else {
        return Err(McpError::Config("not a stdio server".into()));
    };
    let mut penv = Env::minimal();
    for (k, v) in vars {
        let resolved = env
            .secrets
            .resolve(v)
            .map_err(|e| McpError::Config(format!("env `{k}`: {e}")))?;
        penv.set
            .insert(k.clone(), resolved.expose_secret().to_string());
    }
    let mut spec = PipeSpec::new(command)
        .args(args)
        .env(penv)
        .stdin(StdinMode::Piped);
    if let Some(c) = cwd.as_deref().map(Path::new).or(default_cwd) {
        spec = spec.cwd(c);
    }
    let mut child = spawn_pipe(spec, hook).map_err(|e| McpError::Transport(e.to_string()))?;
    let (Some(stdin), Some(stdout), Some(stderr)) =
        (child.take_stdin(), child.take_stdout(), child.take_stderr())
    else {
        return Err(McpError::Transport("child pipes missing".into()));
    };
    env.logs.push(
        "lifecycle",
        &format!("started `{command}` (pid {})", child.id().unwrap_or(0)),
    );
    let close = env.parent.child_token();

    // stderr → log ring.
    let logs = env.logs.clone();
    let sclose = close.clone();
    env.tasks.spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        loop {
            tokio::select! {
                () = sclose.cancelled() => break,
                l = lines.next_line() => match l {
                    Ok(Some(l)) => logs.push("stderr", &l),
                    _ => break,
                },
            }
        }
    });

    // Process owner: kill the tree when the transport closes; report the exit.
    let (exit_tx, exit_rx) = oneshot::channel();
    let logs = env.logs.clone();
    let wclose = close.clone();
    env.tasks.spawn(async move {
        let msg = match child.wait_or_cancel(&wclose).await {
            Ok(genie_process::pipe::RunOutcome::Exited(s)) => {
                format!("server process exited ({s})")
            }
            Ok(_) => "server process stopped".to_string(),
            Err(e) => format!("waiting for server process: {e}"),
        };
        logs.push("lifecycle", &msg);
        let _ = exit_tx.send(msg);
        drop(child);
    });

    Ok(from_streams(
        stdout,
        stdin,
        &env.tasks,
        env.logs.clone(),
        close,
        Some(exit_rx),
    ))
}
