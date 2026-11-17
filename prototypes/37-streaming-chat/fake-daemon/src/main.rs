//! Fake Genie daemon for prototype 37 (throwaway).
//! JSON-RPC 2.0 over NDJSON on `\\.\pipe\genie-proto-37` (Windows) or a 0600 Unix socket.
//! Methods: `health`, `chat.start {scenario}`. Notifications: `chat.history`, `text.delta`, `text.ended`.

use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, BufWriter};
use tokio::sync::mpsc;

const CORPUS: &str = include_str!("corpus.md");
const VERSION: &str = "proto-37-0.1";

static CLIENTS: AtomicUsize = AtomicUsize::new(0);

fn now_us() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros() as u64
}

/// Crude ~4 chars/token splitter: a token ends at whitespace or after 4 non-space chars.
fn tokenize(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut n = 0;
    for c in s.chars() {
        if c.is_whitespace() {
            cur.push(c);
            out.push(std::mem::take(&mut cur));
            n = 0;
        } else {
            if n >= 4 {
                out.push(std::mem::take(&mut cur));
                n = 0;
            }
            cur.push(c);
            n += 1;
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// ~200 messages / ~100k tokens of history for the `long` scenario.
fn history() -> (Vec<Value>, usize) {
    let sections: Vec<&str> = CORPUS.split("\n## ").collect();
    let mut msgs = Vec::new();
    let mut tokens = 0;
    for i in 0..100 {
        let user = format!("Question {}: can you explain part {} again, with an example?", i + 1, i % sections.len());
        tokens += tokenize(&user).len();
        msgs.push(json!({"role": "user", "text": user}));
        // assistant reply: a rotating window of sections (~1000 tokens)
        let mut text = String::new();
        let take = 7;
        for k in 0..take {
            let s = sections[(i + k) % sections.len()];
            text.push_str("## ");
            text.push_str(s.trim_start_matches("## "));
            text.push_str("\n\n");
        }
        tokens += tokenize(&text).len();
        msgs.push(json!({"role": "assistant", "text": text}));
    }
    (msgs, tokens)
}

fn notif(method: &str, params: Value) -> String {
    json!({"jsonrpc": "2.0", "method": method, "params": params}).to_string()
}

async fn stream(out: mpsc::UnboundedSender<String>, stream_id: u64, scenario: String) {
    let (text, rate) = match scenario.as_str() {
        "burst" => (format!("{CORPUS}\n\n{CORPUS}\n\n{CORPUS}"), 1000.0),
        _ => (CORPUS.to_string(), 80.0),
    };
    if scenario == "long" {
        let (messages, tokens) = history();
        let _ = out.send(notif("chat.history", json!({"stream_id": stream_id, "messages": messages, "tokens": tokens})));
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    let toks = tokenize(&text);
    let start = Instant::now();
    let mut i = 0usize;
    let mut iv = tokio::time::interval(Duration::from_millis(1));
    iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    while i < toks.len() {
        iv.tick().await;
        let due = ((start.elapsed().as_secs_f64() * rate) as usize + 1).min(toks.len());
        while i < due {
            let line = notif("text.delta", json!({"stream_id": stream_id, "seq": i, "t_emit_us": now_us(), "text": toks[i]}));
            if out.send(line).is_err() {
                return;
            }
            i += 1;
        }
    }
    let _ = out.send(notif("text.ended", json!({"stream_id": stream_id, "tokens": toks.len(), "t_emit_us": now_us()})));
}

async fn handle<S: AsyncRead + AsyncWrite + Send + 'static>(s: S) {
    CLIENTS.fetch_add(1, Ordering::SeqCst);
    let (r, w) = tokio::io::split(s);
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        let mut w = BufWriter::new(w);
        while let Some(line) = rx.recv().await {
            if w.write_all(line.as_bytes()).await.is_err() || w.write_all(b"\n").await.is_err() {
                break;
            }
            // flush only when the queue is drained, so bursts go out in one write
            if rx.is_empty() && w.flush().await.is_err() {
                break;
            }
        }
    });
    let mut lines = BufReader::new(r).lines();
    let mut next_stream = 1u64;
    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(req) = serde_json::from_str::<Value>(&line) else { continue };
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let result = match method {
            "health" => json!({"version": VERSION, "pid": std::process::id(), "t_us": now_us()}),
            "chat.start" => {
                let scenario = req["params"]["scenario"].as_str().unwrap_or("normal").to_string();
                let sid = next_stream;
                next_stream += 1;
                tokio::spawn(stream(tx.clone(), sid, scenario));
                json!({"stream_id": sid})
            }
            _ => {
                let _ = tx.send(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "method not found"}}).to_string());
                continue;
            }
        };
        let _ = tx.send(json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string());
    }
    drop(tx);
    let _ = writer.await;
    CLIENTS.fetch_sub(1, Ordering::SeqCst);
}

/// Exit after `idle` with no clients, so a detached daemon doesn't linger forever.
fn idle_watchdog(idle: Duration) {
    tokio::spawn(async move {
        let mut since = Instant::now();
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            if CLIENTS.load(Ordering::SeqCst) > 0 {
                since = Instant::now();
            } else if since.elapsed() > idle {
                eprintln!("fake-daemon: idle, exiting");
                std::process::exit(0);
            }
        }
    });
}

#[cfg(windows)]
mod platform {
    use super::*;
    use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
    use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;

    pub const PIPE: &str = r"\\.\pipe\genie-proto-37";

    /// SDDL: protected DACL, generic-all for the owner (the current user) only.
    fn user_only_sa() -> Box<SECURITY_ATTRIBUTES> {
        let sddl: Vec<u16> = "D:P(A;;GA;;;OW)\0".encode_utf16().collect();
        let mut sd = std::ptr::null_mut();
        let ok = unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), 1, &mut sd, std::ptr::null_mut()) };
        assert!(ok != 0, "SDDL conversion failed");
        Box::new(SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: sd, bInheritHandle: 0 })
    }

    fn create(first: bool) -> std::io::Result<NamedPipeServer> {
        let mut sa = user_only_sa(); // leaked SD per instance: fine for a prototype
        unsafe {
            ServerOptions::new()
                .first_pipe_instance(first)
                .reject_remote_clients(true)
                .create_with_security_attributes_raw(PIPE, &mut *sa as *mut _ as *mut std::ffi::c_void)
        }
    }

    pub async fn serve() -> std::io::Result<()> {
        unsafe { windows_sys::Win32::Media::timeBeginPeriod(1) };
        let mut server = create(true)?; // fails if another daemon already owns the pipe
        eprintln!("fake-daemon {} listening on {PIPE} (pid {})", VERSION, std::process::id());
        loop {
            server.connect().await?;
            let conn = server;
            server = create(false)?;
            tokio::spawn(handle(conn));
        }
    }
}

#[cfg(unix)]
mod platform {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    pub fn socket_path() -> std::path::PathBuf {
        let dir = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
        dir.join(format!("genie-proto-37-{user}.sock"))
    }

    pub async fn serve() -> std::io::Result<()> {
        let path = socket_path();
        if tokio::net::UnixStream::connect(&path).await.is_ok() {
            return Err(std::io::Error::new(std::io::ErrorKind::AddrInUse, "daemon already running"));
        }
        let _ = std::fs::remove_file(&path);
        let listener = tokio::net::UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        eprintln!("fake-daemon {} listening on {} (pid {})", VERSION, path.display(), std::process::id());
        loop {
            let (s, _) = listener.accept().await?;
            tokio::spawn(handle(s));
        }
    }
    // Note: a real daemon would set umask(0o077) before bind to close the chmod race.
}

#[tokio::main]
async fn main() {
    let idle_secs = std::env::var("GENIE_DAEMON_IDLE_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(120u64);
    idle_watchdog(Duration::from_secs(idle_secs));
    if let Err(e) = platform::serve().await {
        eprintln!("fake-daemon: {e}");
        std::process::exit(1);
    }
}

