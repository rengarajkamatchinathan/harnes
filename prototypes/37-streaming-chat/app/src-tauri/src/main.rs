//! Prototype 37 Tauri shell (throwaway): connects to (or spawns) the fake daemon, relays
//! JSON-RPC notifications to the webview over a `tauri::ipc::Channel`, coalescing deltas
//! every ~16 ms, and reconnects when the daemon dies.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{ipc::Channel, Manager, State};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

fn wall_ms() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64() * 1000.0
}

#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum Ev {
    Status { state: String, detail: String, pid: u32, reconnect_ms: Option<f64>, detect_ms: Option<f64> },
    Deltas { text: String, emits_ms: Vec<f64>, first_seq: u64, last_seq: u64, sent_ms: f64 },
    Ended { stream_id: u64, tokens: u64 },
    History { messages: Value, tokens: u64 },
}

#[derive(Default)]
struct Batch {
    text: String,
    emits: Vec<f64>,
    first: Option<u64>,
    last: u64,
}

#[derive(Default)]
struct Inner {
    chan: Option<Channel<Ev>>,
    writer: Option<mpsc::UnboundedSender<String>>,
    pending: HashMap<u64, oneshot::Sender<Value>>,
    next_id: u64,
    pid: u32,
    kill_at: Option<Instant>,
    last_status: Option<Ev>,
    reconnects: Vec<Value>,
}

struct Relay {
    inner: Mutex<Inner>,
    batch: Mutex<Batch>,
    start_wall_ms: f64,
    bench: bool,
    coalesce_ms: u64,
}
type Shared = Arc<Relay>;

impl Relay {
    fn send(&self, ev: Ev) {
        let mut i = self.inner.lock().unwrap();
        if matches!(ev, Ev::Status { .. }) {
            i.last_status = Some(ev.clone());
        }
        if let Some(c) = &i.chan {
            let _ = c.send(ev);
        }
    }
    fn status(&self, state: &str, detail: &str, pid: u32, reconnect_ms: Option<f64>, detect_ms: Option<f64>) {
        eprintln!("[relay] {state}: {detail}");
        self.send(Ev::Status { state: state.into(), detail: detail.into(), pid, reconnect_ms, detect_ms });
    }
    fn flush(&self) {
        let b = std::mem::take(&mut *self.batch.lock().unwrap());
        if let Some(first) = b.first {
            self.send(Ev::Deltas { text: b.text, emits_ms: b.emits, first_seq: first, last_seq: b.last, sent_ms: wall_ms() });
        }
    }
    fn on_line(&self, line: &str) {
        let Ok(v) = serde_json::from_str::<Value>(line) else { return };
        if let Some(id) = v.get("id").and_then(|i| i.as_u64()) {
            if let Some(tx) = self.inner.lock().unwrap().pending.remove(&id) {
                let _ = tx.send(v);
            }
            return;
        }
        let p = &v["params"];
        match v["method"].as_str().unwrap_or("") {
            "text.delta" => {
                let seq = p["seq"].as_u64().unwrap_or(0);
                let emit = p["t_emit_us"].as_f64().unwrap_or(0.0) / 1000.0;
                let text = p["text"].as_str().unwrap_or("");
                if self.coalesce_ms == 0 {
                    self.send(Ev::Deltas { text: text.into(), emits_ms: vec![emit], first_seq: seq, last_seq: seq, sent_ms: wall_ms() });
                } else {
                    let mut b = self.batch.lock().unwrap();
                    b.text.push_str(text);
                    b.emits.push(emit);
                    b.first.get_or_insert(seq);
                    b.last = seq;
                }
            }
            "text.ended" => {
                self.flush();
                self.send(Ev::Ended { stream_id: p["stream_id"].as_u64().unwrap_or(0), tokens: p["tokens"].as_u64().unwrap_or(0) });
            }
            "chat.history" => {
                self.flush();
                self.send(Ev::History { messages: p["messages"].clone(), tokens: p["tokens"].as_u64().unwrap_or(0) });
            }
            _ => {}
        }
    }
}

// ---------- platform transport ----------
#[cfg(windows)]
mod transport {
    pub type Conn = tokio::net::windows::named_pipe::NamedPipeClient;
    pub const ADDR: &str = r"\\.\pipe\genie-proto-37";
    pub async fn connect() -> std::io::Result<Conn> {
        tokio::net::windows::named_pipe::ClientOptions::new().open(ADDR)
    }
    pub fn detach(cmd: &mut std::process::Command) {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
}
#[cfg(unix)]
mod transport {
    pub type Conn = tokio::net::UnixStream;
    pub fn path() -> std::path::PathBuf {
        let dir = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
        dir.join(format!("genie-proto-37-{user}.sock"))
    }
    pub async fn connect() -> std::io::Result<Conn> {
        tokio::net::UnixStream::connect(path()).await
    }
    pub fn detach(cmd: &mut std::process::Command) {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0); // own process group: survives the app and its signals
    }
}

fn daemon_path() -> PathBuf {
    if let Some(p) = std::env::var_os("GENIE_DAEMON_PATH") {
        return p.into();
    }
    let exe = std::env::current_exe().unwrap();
    exe.with_file_name(if cfg!(windows) { "fake-daemon.exe" } else { "fake-daemon" })
}

fn spawn_daemon(relay: &Relay) {
    let path = daemon_path();
    let mut cmd = std::process::Command::new(&path);
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    transport::detach(&mut cmd);
    match cmd.spawn() {
        Ok(c) => relay.status("spawning", &format!("spawned daemon pid {}", c.id()), 0, None, None),
        Err(e) => relay.status("spawning", &format!("spawn {} failed: {e}", path.display()), 0, None, None),
    }
}

async fn connect_or_spawn(relay: &Relay) -> transport::Conn {
    let mut spawned_at: Option<Instant> = None;
    let mut delay = Duration::from_millis(5);
    loop {
        match transport::connect().await {
            Ok(c) => return c,
            Err(_) => {
                if spawned_at.map_or(true, |t| t.elapsed() > Duration::from_secs(3)) {
                    spawn_daemon(relay);
                    spawned_at = Some(Instant::now());
                }
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_millis(50));
            }
        }
    }
}

async fn manager(relay: Shared) {
    loop {
        relay.status("connecting", "connecting to daemon", 0, None, None);
        let conn = connect_or_spawn(&relay).await;
        let (r, mut w) = tokio::io::split(conn);
        let mut reader = BufReader::new(r);
        // health handshake (id 0), before the read loop takes over
        let hs = json!({"jsonrpc": "2.0", "id": 0, "method": "health"}).to_string() + "\n";
        if w.write_all(hs.as_bytes()).await.is_err() {
            continue;
        }
        let mut line = String::new();
        let pid = loop {
            line.clear();
            match tokio::time::timeout(Duration::from_secs(2), reader.read_line(&mut line)).await {
                Ok(Ok(n)) if n > 0 => {
                    let v: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
                    if v["id"] == json!(0) {
                        break v["result"]["pid"].as_u64().map(|p| p as u32);
                    }
                }
                _ => break None,
            }
        };
        let Some(pid) = pid else {
            relay.status("reconnecting", "health handshake failed", 0, None, None);
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        };
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            while let Some(l) = rx.recv().await {
                if w.write_all(l.as_bytes()).await.is_err() {
                    break;
                }
            }
        });
        let reconnect_ms = {
            let mut i = relay.inner.lock().unwrap();
            i.writer = Some(tx);
            i.pid = pid;
            let r = i.kill_at.take().map(|k| k.elapsed().as_secs_f64() * 1000.0);
            if let Some(ms) = r {
                i.reconnects.push(json!(ms));
            }
            r
        };
        relay.status("connected", &format!("daemon pid {pid}"), pid, reconnect_ms, None);
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(n) if n > 0 => relay.on_line(line.trim_end()),
                _ => break,
            }
        }
        let detect_ms = {
            let mut i = relay.inner.lock().unwrap();
            i.writer = None;
            i.pending.clear();
            i.kill_at.map(|k| k.elapsed().as_secs_f64() * 1000.0)
        };
        relay.flush();
        relay.status("reconnecting", "daemon connection lost (EOF)", 0, None, detect_ms);
    }
}

async fn rpc(relay: &Relay, method: &str, params: Value) -> Result<Value, String> {
    let (tx, rx) = oneshot::channel();
    {
        let mut i = relay.inner.lock().unwrap();
        i.next_id += 1;
        let id = i.next_id;
        let w = i.writer.clone().ok_or("daemon not connected")?;
        i.pending.insert(id, tx);
        let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string() + "\n";
        w.send(line).map_err(|_| "writer closed")?;
    }
    let v = tokio::time::timeout(Duration::from_secs(5), rx).await.map_err(|_| "rpc timeout")?.map_err(|_| "connection lost")?;
    if let Some(e) = v.get("error") {
        return Err(e.to_string());
    }
    Ok(v["result"].clone())
}

// ---------- commands ----------
#[tauri::command]
fn subscribe(relay: State<Shared>, on_event: Channel<Ev>) -> Value {
    let mut i = relay.inner.lock().unwrap();
    if let Some(s) = &i.last_status {
        let _ = on_event.send(s.clone());
    }
    i.chan = Some(on_event);
    json!({
        "bench": relay.bench,
        "start_wall_ms": relay.start_wall_ms,
        "coalesce_ms": relay.coalesce_ms,
        "scenarios": std::env::var("GENIE_BENCH_SCENARIOS").ok(),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
    })
}

#[tauri::command]
async fn chat_start(relay: State<'_, Shared>, scenario: String) -> Result<Value, String> {
    rpc(&relay, "chat.start", json!({ "scenario": scenario })).await
}

#[tauri::command]
fn kill_daemon(relay: State<Shared>) -> Result<Value, String> {
    let pid = relay.inner.lock().unwrap().pid;
    if pid == 0 {
        return Err("no daemon pid".into());
    }
    let mut sys = sysinfo::System::new().expect("sysinfo");
    let spid = sysinfo::Pid::from_u32(pid);
    sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[spid]), true);
    let p = sys.process(spid).ok_or("daemon process not found")?;
    relay.inner.lock().unwrap().kill_at = Some(Instant::now());
    let ok = p.kill();
    Ok(json!({ "pid": pid, "killed": ok, "t_wall_ms": wall_ms() }))
}

/// RSS of this process + descendants (WebView2 / WebKit helpers), daemon reported separately.
#[tauri::command]
fn metrics() -> Value {
    let mut sys = sysinfo::System::new().expect("sysinfo");
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    let me = sysinfo::Pid::from_u32(std::process::id());
    let mut tree = vec![me];
    let mut changed = true;
    while changed {
        changed = false;
        for (pid, p) in sys.processes() {
            if !tree.contains(pid) && p.parent().is_some_and(|pp| tree.contains(&pp)) {
                tree.push(*pid);
                changed = true;
            }
        }
    }
    let (mut app_b, mut daemon_b, mut n) = (0u64, 0u64, 0);
    let mut self_b = 0;
    for pid in &tree {
        if let Some(p) = sys.process(*pid) {
            let name = p.name().to_string_lossy().to_lowercase();
            if name.starts_with("fake-daemon") {
                daemon_b += p.memory();
            } else {
                app_b += p.memory();
                n += 1;
            }
            if *pid == me {
                self_b = p.memory();
            }
        }
    }
    let mb = |b: u64| (b as f64 / 1048576.0 * 10.0).round() / 10.0;
    json!({ "rss_shell_mb": mb(self_b), "rss_app_tree_mb": mb(app_b), "app_tree_processes": n, "rss_daemon_mb": mb(daemon_b) })
}

#[tauri::command]
fn write_results(app: tauri::AppHandle, relay: State<Shared>, mut results: Value) -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let size_mb = |p: &std::path::Path| std::fs::metadata(p).map(|m| (m.len() as f64 / 1048576.0 * 100.0).round() / 100.0).ok();
    results["rust"] = json!({
        "exe": exe.display().to_string(),
        "exe_mb": size_mb(&exe),
        "daemon_exe_mb": size_mb(&daemon_path()),
        "coalesce_ms": relay.coalesce_ms,
        "reconnects_ms": relay.inner.lock().unwrap().reconnects.clone(),
        "metrics_at_end": metrics(),
    });
    let out = std::env::var("GENIE_BENCH_OUT").unwrap_or_else(|_| "bench-results.json".into());
    std::fs::write(&out, serde_json::to_string_pretty(&results).unwrap()).map_err(|e| e.to_string())?;
    let full = std::fs::canonicalize(&out).map(|p| p.display().to_string()).unwrap_or(out);
    eprintln!("[bench] wrote {full}");
    if relay.bench {
        let h = app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            h.exit(0);
        });
    }
    Ok(full)
}

#[tauri::command]
fn log(msg: String) {
    eprintln!("[web] {msg}");
}

fn main() {
    let start_wall_ms = wall_ms();
    #[cfg(windows)]
    unsafe {
        windows_sys::Win32::Media::timeBeginPeriod(1);
    }
    let bench = std::env::args().any(|a| a == "--bench") || std::env::var("GENIE_BENCH").is_ok_and(|v| v == "1");
    let coalesce_ms = std::env::var("GENIE_COALESCE_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(16);
    let relay: Shared = Arc::new(Relay {
        inner: Mutex::new(Inner::default()),
        batch: Mutex::new(Batch::default()),
        start_wall_ms,
        bench,
        coalesce_ms,
    });
    let r2 = relay.clone();
    tauri::Builder::default()
        .manage(relay)
        .setup(move |app| {
            let r = r2.clone();
            tauri::async_runtime::spawn(manager(r.clone()));
            if r.coalesce_ms > 0 {
                tauri::async_runtime::spawn(async move {
                    let mut iv = tokio::time::interval(Duration::from_millis(r.coalesce_ms));
                    iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                    loop {
                        iv.tick().await;
                        r.flush();
                    }
                });
            }
            if r2.bench {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.set_always_on_top(true); // avoid occlusion throttling during the bench
                }
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![subscribe, chat_start, kill_daemon, metrics, write_results, log])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_, ev| match ev {
            tauri::RunEvent::ExitRequested { code, .. } => eprintln!("[shell] exit requested: {code:?}"),
            tauri::RunEvent::WindowEvent { label, event: tauri::WindowEvent::Destroyed, .. } => eprintln!("[shell] window {label} destroyed"),
            tauri::RunEvent::WindowEvent { label, event: tauri::WindowEvent::CloseRequested { .. }, .. } => eprintln!("[shell] window {label} close requested"),
            _ => {}
        });
}
