//! Terminal tabs (D55, 02 §6.7): `terminal.open/attach/input/resize/close` over
//! `genie-process` PTY mode.
//!
//! * A terminal belongs to a session and opens in its folder (worktree included), with the
//!   D45 shell choice (`shell: "bash" | "powershell"` like the `shell` tool).
//! * `terminal.attach {offset}` replays from the client's last offset (or returns a VT
//!   checkpoint when it fell out of the 2 MiB ring) and then streams ephemeral
//!   `terminal.output {terminal_id, start, end, data_b64}` session events to **that
//!   connection** (like `browser.frame`, not through the bus, so a busy terminal never
//!   makes session subscribers resync). Re-attaching from the same connection replaces the
//!   previous stream; a lagging stream re-reads from the ring.
//! * `terminal.exited {terminal_id, code}` goes out on the workspace feed.
//! * Every open terminal holds an `Engine::terminal()` guard, so the daemon doesn't idle-exit
//!   under it. `terminal.close`, session delete and daemon exit kill the whole tree.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use genie_core::TaskScope;
use genie_core::engine::WorkGuard;
use genie_process::pipe::Env;
use genie_process::pty::{Pty, PtyEvent, PtySpec};
use genie_protocol::api::browser::{TerminalAttachResult, TerminalIdParams};
use genie_protocol::common::Empty;
use genie_protocol::event::{SessionEvent, SessionEventEnvelope, TerminalOutput};
use genie_protocol::methods::{self as m, Method};
use genie_protocol::workspace::TerminalExited;
use genie_protocol::{ErrorKind, Request, Response, RpcError};
use genie_tools::shell::select::{Probe, select};
use genie_tools::shell::{Requested, ShellKind};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::broadcast::error::RecvError;

use crate::server::{Conn, DaemonState};

struct Term {
    session_id: String,
    pty: Arc<Pty>,
    _guard: WorkGuard,
    /// Output streams by connection id.
    streams: Mutex<HashMap<u64, TaskScope>>,
}

impl Term {
    fn streams(&self) -> MutexGuard<'_, HashMap<u64, TaskScope>> {
        self.streams.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn stop_streams(&self) {
        for (_, s) in self.streams().drain() {
            s.cancel();
        }
    }
}

/// Open terminals of this daemon.
#[derive(Default)]
pub(crate) struct Terminals {
    map: Mutex<HashMap<String, Arc<Term>>>,
}

impl Terminals {
    fn map(&self) -> MutexGuard<'_, HashMap<String, Arc<Term>>> {
        self.map.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn get(&self, id: &str) -> Result<Arc<Term>, RpcError> {
        self.map()
            .get(id)
            .cloned()
            .ok_or_else(|| RpcError::new(ErrorKind::NotFound, "terminal not found"))
    }

    fn close(&self, id: &str) -> bool {
        let Some(t) = self.map().remove(id) else {
            return false;
        };
        t.stop_streams();
        t.pty.kill_tree();
        true
    }
}

impl Drop for Terminals {
    fn drop(&mut self) {
        for (_, t) in self.map().drain() {
            t.pty.kill_tree();
        }
    }
}

fn to_result<T: Serialize>(v: &T) -> Result<Value, RpcError> {
    serde_json::to_value(v).map_err(|e| RpcError::internal(e.to_string()))
}

fn params<M: Method>(r: &Request) -> Result<M::Params, RpcError> {
    let v = if r.params.is_null() {
        json!({})
    } else {
        r.params.clone()
    };
    serde_json::from_value(v).map_err(|e| RpcError::invalid_params(e.to_string()))
}

/// `terminal.*` requests; `None` for anything else.
pub(crate) fn dispatch(
    state: &Arc<DaemonState>,
    conn: &Arc<Conn>,
    r: &Request,
) -> Option<Response> {
    let name = r.method.as_str();
    if !name.starts_with("terminal.") {
        return None;
    }
    let res = match name {
        n if n == m::TerminalOpen::NAME => params::<m::TerminalOpen>(r).and_then(|p| open(state, p)),
        n if n == m::TerminalAttach::NAME => {
            params::<m::TerminalAttach>(r).and_then(|p| attach(state, conn, &p.terminal_id, p.offset))
        }
        n if n == m::TerminalInput::NAME => params::<m::TerminalInput>(r).and_then(|p| {
            let t = state.terminals().get(&p.terminal_id)?;
            let data = B64
                .decode(p.data_b64.as_bytes())
                .map_err(|e| RpcError::invalid_params(format!("data_b64: {e}")))?;
            t.pty.write(&data).map_err(io_err)?;
            to_result(&Empty {})
        }),
        n if n == m::TerminalResize::NAME => params::<m::TerminalResize>(r).and_then(|p| {
            let t = state.terminals().get(&p.terminal_id)?;
            t.pty.resize(p.cols, p.rows).map_err(io_err)?;
            to_result(&Empty {})
        }),
        n if n == m::TerminalClose::NAME => params::<m::TerminalClose>(r).and_then(|p| {
            if !state.terminals().close(&p.terminal_id) {
                return Err(RpcError::new(ErrorKind::NotFound, "terminal not found"));
            }
            to_result(&Empty {})
        }),
        other => Err(RpcError::method_not_found(other)),
    };
    Some(match res {
        Ok(v) => Response::ok(r.id.clone(), v),
        Err(e) => Response::err(r.id.clone(), e),
    })
}

fn io_err(e: std::io::Error) -> RpcError {
    let kind = match e.kind() {
        std::io::ErrorKind::WouldBlock => ErrorKind::Busy,
        std::io::ErrorKind::InvalidInput => ErrorKind::InvalidParams,
        _ => ErrorKind::InvalidState,
    };
    RpcError::new(kind, e.to_string())
}

/// The folder a terminal opens in: the session's folder, or `cwd` inside it.
fn folder(state: &DaemonState, sid: &str, cwd: Option<&str>) -> Result<PathBuf, RpcError> {
    if !state.registry().exists(sid) {
        return Err(RpcError::new(ErrorKind::NotFound, "session not found"));
    }
    let ws = state.registry().extra(sid).workspace.ok_or_else(|| {
        RpcError::new(
            ErrorKind::InvalidState,
            "terminals open in a Code session's folder; this session has none",
        )
    })?;
    let Some(cwd) = cwd.filter(|c| !c.is_empty()) else {
        return Ok(ws);
    };
    let p = Path::new(cwd);
    let p = if p.is_absolute() {
        p.to_path_buf()
    } else {
        ws.join(p)
    };
    let canon = std::fs::canonicalize(&p)
        .map_err(|e| RpcError::invalid_params(format!("cwd {}: {e}", p.display())))?;
    let root = std::fs::canonicalize(&ws).unwrap_or(ws);
    if !canon.starts_with(&root) || !canon.is_dir() {
        return Err(RpcError::invalid_params(
            "cwd must be a folder inside the session's folder",
        ));
    }
    Ok(canon)
}

/// The D45 shell as an interactive program.
fn shell_spec(shell: Option<&str>) -> Result<PtySpec, RpcError> {
    let requested = match shell.map(str::to_ascii_lowercase).as_deref() {
        None | Some("") | Some("default") => Requested::Default,
        Some("bash") => Requested::Bash,
        Some("powershell" | "pwsh") => Requested::PowerShell,
        Some(other) => {
            return Err(RpcError::invalid_params(format!(
                "unknown shell `{other}` (bash or powershell)"
            )));
        }
    };
    let choice = select(&Probe::system(), requested)
        .map_err(|e| RpcError::new(ErrorKind::InvalidState, e))?;
    let args: &[&str] = match choice.kind {
        // A login shell, like a terminal app opens.
        ShellKind::Bash | ShellKind::Zsh | ShellKind::Sh | ShellKind::GitBash => &["-l"],
        ShellKind::Pwsh | ShellKind::WindowsPowerShell => &["-NoLogo"],
    };
    Ok(PtySpec::new(choice.program.as_os_str()).args(args.iter().copied()))
}

fn open(
    state: &Arc<DaemonState>,
    p: genie_protocol::api::browser::TerminalOpenParams,
) -> Result<Value, RpcError> {
    let dir = folder(state, &p.session_id, p.cwd.as_deref())?;
    let spec = shell_spec(p.shell.as_deref())?
        .cwd(dir)
        .env(Env::inherit())
        .size(p.cols.max(2), p.rows.max(1));
    let pty = Pty::spawn(spec).map_err(|e| {
        RpcError::new(ErrorKind::InvalidState, format!("couldn't start the shell: {e}"))
    })?;
    let id = genie_core::ids::new_id("term");
    let term = Arc::new(Term {
        session_id: p.session_id.clone(),
        pty: Arc::new(pty),
        _guard: state.engine().terminal(),
        streams: Mutex::new(HashMap::new()),
    });
    state.terminals().map().insert(id.clone(), term.clone());
    // Exit watcher: `terminal.exited` on the workspace feed, then forget the terminal.
    let st = state.clone();
    let tid = id.clone();
    state.root_scope().child("terminal").spawn_cancellable(async move {
        let code = term.pty.wait().await;
        st.workspace_send(
            m::TerminalExited::NAME,
            &TerminalExited {
                terminal_id: tid.clone(),
                code,
            },
        );
        // Let the streams flush the last output before they are stopped.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let mut map = st.terminals().map();
        if map.get(&tid).is_some_and(|t| Arc::ptr_eq(t, &term)) {
            map.remove(&tid);
        }
        drop(map);
        term.stop_streams();
    });
    to_result(&TerminalIdParams { terminal_id: id })
}

fn attach(
    state: &Arc<DaemonState>,
    conn: &Arc<Conn>,
    tid: &str,
    offset: u64,
) -> Result<Value, RpcError> {
    let t = state.terminals().get(tid)?;
    let att = t.pty.attach(offset);
    let result = TerminalAttachResult {
        truncated: att.truncated,
        checkpoint_b64: att.checkpoint.as_deref().map(|c| B64.encode(c)),
        replay_b64: B64.encode(&att.replay),
        end_offset: att.end_offset,
    };
    if att.exited.is_none() {
        let scope = conn.scope().child("terminal-stream");
        if let Some(old) = t.streams().insert(conn.id(), scope.clone()) {
            old.cancel();
        }
        let conn = conn.clone();
        let term = t.clone();
        let tid = tid.to_string();
        let mut rx = att.events;
        let mut next = att.end_offset;
        scope.spawn_cancellable(async move {
            loop {
                match rx.recv().await {
                    Ok(PtyEvent::Output { start, data }) => {
                        // Skip anything the replay already covered.
                        let end = start + data.len() as u64;
                        if end <= next {
                            continue;
                        }
                        let skip = usize::try_from(next.saturating_sub(start)).unwrap_or(0);
                        send(&conn, &term.session_id, &tid, next.max(start), &data[skip..]);
                        next = end;
                    }
                    Ok(PtyEvent::Exited { .. }) | Err(RecvError::Closed) => break,
                    Err(RecvError::Lagged(_)) => {
                        let (_, start, bytes) = term.pty.read_from(next);
                        if !bytes.is_empty() {
                            send(&conn, &term.session_id, &tid, start, &bytes);
                            next = start + bytes.len() as u64;
                        }
                    }
                }
            }
        });
    }
    to_result(&result)
}

fn send(conn: &Conn, session_id: &str, tid: &str, start: u64, data: &[u8]) {
    conn.notify_event(&SessionEventEnvelope {
        session_id: session_id.to_string(),
        seq: None,
        ref_seq: None,
        ts: genie_protocol::rfc3339_ms(genie_core::bus::now_ms()),
        turn_id: None,
        step: None,
        subagent_of: None,
        event: SessionEvent::TerminalOutput(TerminalOutput {
            terminal_id: tid.to_string(),
            start,
            end: start + data.len() as u64,
            data_b64: B64.encode(data),
        }),
    });
}

/// `session.delete`: close the session's terminals.
pub(crate) fn session_deleted(state: &DaemonState, sid: &str) {
    let ids: Vec<String> = state
        .terminals()
        .map()
        .iter()
        .filter(|(_, t)| t.session_id == sid)
        .map(|(k, _)| k.clone())
        .collect();
    for id in ids {
        state.terminals().close(&id);
    }
}
