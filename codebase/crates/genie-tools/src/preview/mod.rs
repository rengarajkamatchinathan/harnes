//! Dev servers (ticket 47 phase 2, Claude Code Desktop's preview servers): entries in the
//! session folder's `.genie/launch.json`, started on `genie-process` kill-trees, watched
//! until they listen, with their output kept for `preview_logs` / `preview.logs`.
//!
//! * One [`DevServers`] per daemon, shared by the agent tools ([`tools`]) and the daemon's
//!   `preview.*` methods (the Browser tab's **Dev servers** menu).
//! * A server is keyed by session and `name`. Starting a running one returns it as is.
//! * Readiness: TCP connect to `port` (or, without one, the first port the process tree
//!   listens on). A server that comes up after [`DevServers::start`] stopped waiting still
//!   turns `running`.
//! * `PORT` is set to the chosen port; `autoPort: true` picks a free one.
//! * Every change goes to the [`OnChange`] hook (the daemon's `preview.changed`).
//! * Servers outlive turns. `preview_stop`, `preview.stop`, session delete and daemon exit
//!   kill the whole tree.

pub mod config;
pub mod tools;

use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use genie_process::pipe::TailBuffer;
use genie_process::pipe::{Env, PipeSpec, RunOutcome, StdinMode, spawn_pipe};
use genie_protocol::api::browser::{PreviewServer, PreviewState};
use tokio::io::AsyncReadExt;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

pub use config::{LAUNCH_FILE, LaunchFile, ServerConfig};
pub use tools::tools;

use crate::shell::SessionDirs;

/// Called with a session's full server list after every change.
pub type OnChange = Arc<dyn Fn(&str, Vec<PreviewServer>) + Send + Sync>;

const DEFAULT_READY_TIMEOUT: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(250);
const LOG_CAP: usize = 512 * 1024;

struct Server {
    session_id: String,
    name: String,
    info: Mutex<PreviewServer>,
    logs: Mutex<TailBuffer>,
    cancel: CancellationToken,
    state: watch::Sender<PreviewState>,
}

impl Server {
    fn info(&self) -> MutexGuard<'_, PreviewServer> {
        lock(&self.info)
    }

    fn snapshot(&self) -> PreviewServer {
        self.info().clone()
    }

    fn set(&self, f: impl FnOnce(&mut PreviewServer)) {
        let state = {
            let mut i = self.info();
            f(&mut i);
            i.state
        };
        self.state.send_replace(state);
    }

    fn log(&self, bytes: &[u8]) {
        lock(&self.logs).push(bytes);
    }

    fn log_text(&self) -> String {
        let raw = lock(&self.logs).to_vec();
        let text = String::from_utf8_lossy(&raw);
        crate::output::strip_ansi(&text).replace("\r\n", "\n")
    }
}

/// The daemon's dev servers.
pub struct DevServers {
    dirs: Arc<dyn SessionDirs>,
    tasks: TaskTracker,
    shutdown: CancellationToken,
    servers: Arc<Mutex<Vec<Arc<Server>>>>,
    on_change: Arc<Mutex<Option<OnChange>>>,
    /// How long [`DevServers::start`] waits for the port.
    pub ready_timeout: Duration,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Last `n` lines of `text`.
pub fn tail_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.trim_end_matches('\n').lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

impl DevServers {
    pub fn new(
        dirs: Arc<dyn SessionDirs>,
        tasks: TaskTracker,
        shutdown: CancellationToken,
    ) -> DevServers {
        DevServers {
            dirs,
            tasks,
            shutdown,
            servers: Arc::new(Mutex::new(Vec::new())),
            on_change: Arc::new(Mutex::new(None)),
            ready_timeout: DEFAULT_READY_TIMEOUT,
        }
    }

    pub fn set_on_change(&self, f: OnChange) {
        *lock(&self.on_change) = Some(f);
    }

    fn changed(&self, session_id: &str) {
        self.notifier()(session_id);
    }

    /// Calls the hook with a session's list; holds no borrow of `self` (tasks outlive calls).
    fn notifier(&self) -> impl Fn(&str) + Send + Sync + 'static {
        let servers = Arc::downgrade(&self.servers);
        let hook = self.on_change.clone();
        move |sid: &str| {
            let Some(h) = lock(&hook).clone() else {
                return;
            };
            let Some(v) = servers.upgrade() else {
                return;
            };
            let list: Vec<PreviewServer> = lock(&v)
                .iter()
                .filter(|s| s.session_id == sid)
                .map(|s| s.snapshot())
                .collect();
            h(sid, list);
        }
    }

    /// The session's project folder.
    pub fn root(&self, session_id: &str) -> Result<PathBuf, String> {
        self.dirs.initial_cwd(session_id).ok_or_else(|| {
            "dev servers run in a Code session's folder; this session has none".to_string()
        })
    }

    pub fn launch_file(&self, session_id: &str) -> Result<LaunchFile, String> {
        LaunchFile::load(&self.root(session_id)?)
    }

    /// Add or replace an entry in the session's `launch.json` (created on first use).
    pub fn save_config(&self, session_id: &str, cfg: ServerConfig) -> Result<(), String> {
        let root = self.root(session_id)?;
        let mut f = LaunchFile::load(&root)?;
        f.upsert(cfg);
        f.save(&root)
    }

    /// Servers of the session, in start order.
    pub fn list(&self, session_id: &str) -> Vec<PreviewServer> {
        lock(&self.servers)
            .iter()
            .filter(|s| s.session_id == session_id)
            .map(|s| s.snapshot())
            .collect()
    }

    fn find(&self, session_id: &str, name: &str) -> Option<Arc<Server>> {
        lock(&self.servers)
            .iter()
            .find(|s| s.session_id == session_id && s.name == name)
            .cloned()
    }

    pub fn get(&self, session_id: &str, name: &str) -> Option<PreviewServer> {
        self.find(session_id, name).map(|s| s.snapshot())
    }

    /// The last `lines` lines of a server's output (ANSI stripped).
    pub fn logs(&self, session_id: &str, name: &str, lines: usize) -> Option<String> {
        self.find(session_id, name)
            .map(|s| tail_lines(&s.log_text(), lines))
    }

    /// Stop one server (and forget it). `false` if there was none.
    pub fn stop(&self, session_id: &str, name: &str) -> bool {
        let removed = {
            let mut v = lock(&self.servers);
            let before = v.len();
            let mut gone = Vec::new();
            v.retain(|s| {
                let hit = s.session_id == session_id && s.name == name;
                if hit {
                    gone.push(s.clone());
                }
                !hit
            });
            for s in &gone {
                s.cancel.cancel();
            }
            before != v.len()
        };
        if removed {
            self.changed(session_id);
        }
        removed
    }

    /// Stop every server of the session; how many there were.
    pub fn stop_all(&self, session_id: &str) -> usize {
        let gone: Vec<Arc<Server>> = {
            let mut v = lock(&self.servers);
            let (gone, keep): (Vec<_>, Vec<_>) =
                v.drain(..).partition(|s| s.session_id == session_id);
            *v = keep;
            gone
        };
        for s in &gone {
            s.cancel.cancel();
        }
        if !gone.is_empty() {
            self.changed(session_id);
        }
        gone.len()
    }

    /// Session deleted or evicted.
    pub fn end_session(&self, session_id: &str) {
        self.stop_all(session_id);
    }

    /// Start the `launch.json` entry `name`.
    pub async fn start(
        &self,
        session_id: &str,
        name: &str,
        cancel: &CancellationToken,
    ) -> Result<PreviewServer, String> {
        let f = self.launch_file(session_id)?;
        let cfg = f.get(name).cloned().ok_or_else(|| {
            let names: Vec<&str> = f.configurations.iter().map(|c| c.name.as_str()).collect();
            if names.is_empty() {
                format!("no dev server named `{name}`: {LAUNCH_FILE} has no entries")
            } else {
                format!(
                    "no dev server named `{name}` in {LAUNCH_FILE} (it has: {})",
                    names.join(", ")
                )
            }
        })?;
        self.start_config(session_id, cfg, cancel).await
    }

    /// Start `cfg` (or return it if it already runs) and wait until it listens, it fails,
    /// [`DevServers::ready_timeout`] passes (`Ok` with state `starting`), or `cancel`.
    pub async fn start_config(
        &self,
        session_id: &str,
        cfg: ServerConfig,
        cancel: &CancellationToken,
    ) -> Result<PreviewServer, String> {
        if let Some(s) = self.find(session_id, &cfg.name) {
            match s.snapshot().state {
                PreviewState::Running => return Ok(s.snapshot()),
                PreviewState::Starting => return self.wait_ready(&s, cancel).await,
                _ => {
                    lock(&self.servers).retain(|x| !Arc::ptr_eq(x, &s));
                }
            }
        }
        let root = self.root(session_id)?;
        if !cfg.has_command() {
            return self.attach(session_id, cfg).await;
        }
        let cwd = resolve_cwd(&root, cfg.cwd.as_deref())?;
        let argv = cfg.argv().unwrap_or_default();
        let Some((prog, args)) = argv.split_first() else {
            return Err("the entry has no command".into());
        };
        let program = resolve_program(prog, &cwd)?;
        let port = choose_port(&cfg)?;

        let mut env = Env::inherit()
            .with("BROWSER", "none")
            .with("FORCE_COLOR", "0");
        if let Some(p) = port {
            env = env.with("PORT", p.to_string());
        }
        for (k, v) in &cfg.env {
            env = env.with(k.clone(), v.clone());
        }
        let spec = PipeSpec::new(program.into_os_string())
            .args(args.iter().cloned())
            .cwd(&cwd)
            .env(env)
            .stdin(StdinMode::Null);
        let mut child = spawn_pipe(spec, None).map_err(|e| e.to_string())?;

        let url = cfg
            .url
            .clone()
            .or_else(|| port.map(|p| format!("http://localhost:{p}")));
        let info = PreviewServer {
            name: cfg.name.clone(),
            state: PreviewState::Starting,
            url,
            port,
            command: cfg.command_line(),
            pid: child.id(),
            exit_code: None,
            error: None,
        };
        let (tx, _) = watch::channel(PreviewState::Starting);
        let server = Arc::new(Server {
            session_id: session_id.to_string(),
            name: cfg.name.clone(),
            info: Mutex::new(info),
            logs: Mutex::new(TailBuffer::new(LOG_CAP)),
            cancel: self.shutdown.child_token(),
            state: tx,
        });
        lock(&self.servers).push(server.clone());
        self.changed(session_id);

        // Output → log ring.
        for out in [
            child
                .take_stdout()
                .map(|o| Box::new(o) as Box<dyn tokio::io::AsyncRead + Send + Unpin>),
            child
                .take_stderr()
                .map(|o| Box::new(o) as Box<dyn tokio::io::AsyncRead + Send + Unpin>),
        ]
        .into_iter()
        .flatten()
        {
            let s = server.clone();
            self.tasks.spawn(async move {
                let mut out = out;
                let mut buf = vec![0u8; 8192];
                loop {
                    match out.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => s.log(&buf[..n]),
                    }
                }
            });
        }

        // Readiness: the port accepts connections (or the tree starts listening).
        let probe = child.probe();
        {
            let s = server.clone();
            let this = self.notifier();
            self.tasks.spawn(async move {
                loop {
                    if s.cancel.is_cancelled() || s.snapshot().state != PreviewState::Starting {
                        return;
                    }
                    let known = s.snapshot().port;
                    let found = match known {
                        Some(p) => port_open(p).await.then_some(p),
                        None => {
                            let probe = probe.clone();
                            tokio::task::spawn_blocking(move || probe.listening_ports())
                                .await
                                .ok()
                                .and_then(|v| v.first().copied())
                        }
                    };
                    if let Some(p) = found {
                        let mut flipped = false;
                        s.set(|i| {
                            if i.state == PreviewState::Starting {
                                i.state = PreviewState::Running;
                                i.port = Some(p);
                                if i.url.is_none() {
                                    i.url = Some(format!("http://localhost:{p}"));
                                }
                                flipped = true;
                            }
                        });
                        if flipped {
                            this(&s.session_id);
                        }
                        return;
                    }
                    tokio::select! {
                        () = s.cancel.cancelled() => return,
                        () = tokio::time::sleep(POLL) => {}
                    }
                }
            });
        }

        // Exit watcher: owns the child; cancel kills the tree.
        {
            let s = server.clone();
            let this = self.notifier();
            self.tasks.spawn(async move {
                let outcome = child.wait_or_cancel(&s.cancel).await;
                let code = match &outcome {
                    Ok(RunOutcome::Exited(st)) => st.code(),
                    _ => None,
                };
                let stopped = matches!(outcome, Ok(RunOutcome::Cancelled));
                // Let the readers drain the last lines.
                tokio::time::sleep(Duration::from_millis(100)).await;
                let tail = tail_lines(&s.log_text(), 5);
                s.set(|i| {
                    i.exit_code = code;
                    i.pid = None;
                    if i.state == PreviewState::Starting && !stopped {
                        i.state = PreviewState::Failed;
                        let why = match code {
                            Some(c) => format!("exited with code {c} before it was listening"),
                            None => "ended before it was listening".to_string(),
                        };
                        i.error = Some(if tail.is_empty() {
                            why
                        } else {
                            format!("{why}:\n{tail}")
                        });
                    } else {
                        i.state = PreviewState::Exited;
                    }
                });
                if !stopped {
                    this(&s.session_id);
                }
            });
        }

        self.wait_ready(&server, cancel).await
    }

    async fn wait_ready(
        &self,
        s: &Arc<Server>,
        cancel: &CancellationToken,
    ) -> Result<PreviewServer, String> {
        let mut rx = s.state.subscribe();
        let deadline = tokio::time::sleep(self.ready_timeout);
        tokio::pin!(deadline);
        loop {
            let state = *rx.borrow_and_update();
            match state {
                PreviewState::Starting => {}
                PreviewState::Failed => {
                    let i = s.snapshot();
                    return Err(format!(
                        "`{}` didn't start: {}",
                        i.name,
                        i.error.unwrap_or_else(|| "it failed".into())
                    ));
                }
                _ => return Ok(s.snapshot()),
            }
            tokio::select! {
                r = rx.changed() => if r.is_err() { return Ok(s.snapshot()); },
                () = &mut deadline => return Ok(s.snapshot()),
                () = cancel.cancelled() => return Ok(s.snapshot()),
            }
        }
    }

    /// A `url`-only entry: check that something answers there.
    async fn attach(&self, session_id: &str, cfg: ServerConfig) -> Result<PreviewServer, String> {
        let url = cfg.url.clone().ok_or_else(|| {
            "the entry needs `runtimeExecutable` or `program` (a command), or `url` (a running server)"
                .to_string()
        })?;
        let port = url_port(&url);
        let up = match port {
            Some(p) if is_local_url(&url) => port_open(p).await,
            _ => true,
        };
        let (tx, _) = watch::channel(PreviewState::Running);
        let info = PreviewServer {
            name: cfg.name.clone(),
            state: if up {
                PreviewState::Running
            } else {
                PreviewState::Failed
            },
            url: Some(url.clone()),
            port,
            command: None,
            pid: None,
            exit_code: None,
            error: (!up).then(|| format!("nothing is listening at {url}")),
        };
        let server = Arc::new(Server {
            session_id: session_id.to_string(),
            name: cfg.name.clone(),
            info: Mutex::new(info.clone()),
            logs: Mutex::new(TailBuffer::new(0)),
            cancel: self.shutdown.child_token(),
            state: tx,
        });
        lock(&self.servers).push(server);
        self.changed(session_id);
        if up {
            Ok(info)
        } else {
            Err(format!("`{}`: nothing is listening at {url}", cfg.name))
        }
    }
}

impl Drop for DevServers {
    fn drop(&mut self) {
        for s in lock(&self.servers).drain(..) {
            s.cancel.cancel();
        }
    }
}

/// `cwd` inside the project folder.
fn resolve_cwd(root: &Path, cwd: Option<&str>) -> Result<PathBuf, String> {
    let root_c = std::fs::canonicalize(root).map_err(|e| format!("{}: {e}", root.display()))?;
    let Some(c) = cwd.filter(|c| !c.is_empty()) else {
        return Ok(root_c);
    };
    let p = Path::new(c);
    let p = if p.is_absolute() {
        p.to_path_buf()
    } else {
        root_c.join(p)
    };
    let canon = std::fs::canonicalize(&p).map_err(|e| format!("cwd {}: {e}", p.display()))?;
    if !canon.starts_with(&root_c) || !canon.is_dir() {
        return Err(format!("cwd `{c}` must be a folder inside the project"));
    }
    Ok(canon)
}

/// A bare name through `PATH` (with `PATHEXT` on Windows); a relative path from `cwd`.
fn resolve_program(prog: &str, cwd: &Path) -> Result<PathBuf, String> {
    let p = Path::new(prog);
    if p.components().count() > 1 || p.is_absolute() {
        let full = if p.is_absolute() {
            p.to_path_buf()
        } else {
            cwd.join(p)
        };
        return if full.exists() {
            Ok(full)
        } else {
            Err(format!("`{prog}` not found"))
        };
    }
    crate::shell::select::which(prog)
        .ok_or_else(|| format!("`{prog}` was not found on PATH; is it installed?"))
}

/// The port to pass as `PORT`, or `None` to detect it.
fn choose_port(cfg: &ServerConfig) -> Result<Option<u16>, String> {
    let auto = cfg.auto_port == Some(true);
    match cfg.port {
        Some(p) if port_free(p) => Ok(Some(p)),
        Some(_) if auto => free_port().map(Some),
        Some(p) => Err(format!(
            "port {p} is already in use. If the server is already running, add \"url\": \
             \"http://localhost:{p}\" to its entry instead of starting it again; otherwise \
             stop what holds the port or set \"autoPort\": true"
        )),
        None if auto => free_port().map(Some),
        None => Ok(None),
    }
}

fn port_free(p: u16) -> bool {
    TcpListener::bind(("127.0.0.1", p)).is_ok()
}

fn free_port() -> Result<u16, String> {
    TcpListener::bind(("127.0.0.1", 0))
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .map_err(|e| format!("couldn't pick a free port: {e}"))
}

async fn port_open(p: u16) -> bool {
    for addr in [
        SocketAddr::from(([127, 0, 0, 1], p)),
        SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, p)),
    ] {
        if let Ok(Ok(_)) = tokio::time::timeout(
            Duration::from_millis(300),
            tokio::net::TcpStream::connect(addr),
        )
        .await
        {
            return true;
        }
    }
    false
}

fn is_local_url(url: &str) -> bool {
    let rest = url.split("://").nth(1).unwrap_or(url);
    if rest.starts_with("[::1]") {
        return true;
    }
    let host = rest.split(['/', ':', '?', '#']).next().unwrap_or("");
    matches!(host, "localhost" | "127.0.0.1" | "0.0.0.0") || host.ends_with(".localhost")
}

fn url_port(url: &str) -> Option<u16> {
    let rest = url.split("://").nth(1)?;
    let hostport = rest.split(['/', '?', '#']).next()?;
    let port = if let Some(after) = hostport.strip_prefix('[') {
        after.split_once("]:").map(|(_, p)| p)
    } else {
        hostport.rsplit_once(':').map(|(_, p)| p)
    };
    match port {
        Some(p) => p.parse().ok(),
        None if url.starts_with("https://") => Some(443),
        None => Some(80),
    }
}
