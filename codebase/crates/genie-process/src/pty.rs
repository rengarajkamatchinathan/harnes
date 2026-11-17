//! PTY mode (D55, 06 §13.4): an interactive terminal for the Code tab.
//!
//! Written against `opencode-pty`'s documented design (KB ch.16), not copied:
//!
//! * the child runs on a pseudo-terminal from `portable-pty` (Unix PTY / Windows ConPTY),
//!   as a session leader on Unix (so its process group is its pid) and inside a Job Object
//!   on Windows, so [`Pty::kill_tree`] takes everything it started;
//! * one **reader** thread appends output to a bounded **replay ring** addressed by
//!   absolute byte offsets, feeds the same bytes to a VT parser (`vt100`) that keeps the
//!   authoritative screen, and fans the chunk out to subscribers — all under one lock, so
//!   [`Pty::attach`] can return "replay up to `end_offset`" and a live receiver that starts
//!   exactly there (no lost or duplicated bytes);
//! * one **writer** thread owns the PTY writer behind a bounded queue, so a child that
//!   stops reading its input never blocks a caller (the "Linux blocked-writer" fix):
//!   [`Pty::write`] fails with `WouldBlock` when the queue is full;
//! * one **wait** thread reaps the child, waits briefly for the reader to drain, then
//!   publishes [`PtyEvent::Exited`] (child exit and PTY EOF are separate signals).
//!
//! Reattach: [`Pty::attach`] with the client's last offset. When the offset fell out of
//! the ring (`truncated`), the client gets a **checkpoint** — escape codes that repaint the
//! current screen (cursor, attributes, modes) — instead of partial bytes, and live output
//! continues from `end_offset`. Clients reset their terminal (`ESC c`) before writing it.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::num::NonZeroU16;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::sync::{broadcast, watch};

use crate::pipe::Env;

/// Default replay window per terminal (KB ch.16: 2 MiB).
pub const REPLAY_CAPACITY: usize = 2 * 1024 * 1024;
/// Largest single input write (KB ch.16: 1 MiB).
pub const MAX_INPUT: usize = 1024 * 1024;
/// Queued input writes before [`Pty::write`] reports `WouldBlock`.
const WRITER_QUEUE: usize = 128;
/// Live chunks a subscriber may fall behind before it sees `Lagged` (it then re-reads
/// from the ring with [`Pty::read_from`]).
const SUBSCRIBER_QUEUE: usize = 1024;
/// Scrollback rows the VT screen keeps (the checkpoint shows the visible screen).
const VT_SCROLLBACK: usize = 1000;
const READ_CHUNK: usize = 8 * 1024;
/// How long the wait thread lets the reader drain after the child exits (descendants
/// that keep the PTY open must not hold `Exited` back).
const DRAIN: Duration = Duration::from_millis(300);

/// What to start.
#[derive(Debug, Clone)]
pub struct PtySpec {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    /// The child's environment; `TERM` defaults to `xterm-256color`.
    pub env: Env,
    pub cols: u16,
    pub rows: u16,
    pub replay_capacity: usize,
}

impl PtySpec {
    pub fn new(program: impl Into<OsString>) -> PtySpec {
        PtySpec {
            program: program.into(),
            args: vec![],
            cwd: None,
            env: Env::inherit(),
            cols: 100,
            rows: 30,
            replay_capacity: REPLAY_CAPACITY,
        }
    }

    pub fn args<I, S>(mut self, args: I) -> PtySpec
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> PtySpec {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn env(mut self, env: Env) -> PtySpec {
        self.env = env;
        self
    }

    pub fn size(mut self, cols: u16, rows: u16) -> PtySpec {
        self.cols = cols;
        self.rows = rows;
        self
    }

    pub fn replay_capacity(mut self, bytes: usize) -> PtySpec {
        self.replay_capacity = bytes.max(1);
        self
    }
}

/// Live output and exit, in order.
#[derive(Debug, Clone, PartialEq)]
pub enum PtyEvent {
    /// Bytes `[start, start + data.len())`.
    Output { start: u64, data: Arc<Vec<u8>> },
    /// The child exited (`None` = killed by a signal / unknown).
    Exited { code: Option<i32> },
}

impl PtyEvent {
    pub fn end(&self) -> Option<u64> {
        match self {
            PtyEvent::Output { start, data } => Some(start + data.len() as u64),
            PtyEvent::Exited { .. } => None,
        }
    }
}

/// A bounded byte ring addressed by absolute offsets: `head` is the oldest byte still
/// held, `end()` the offset after the newest.
#[derive(Debug)]
pub struct ReplayRing {
    buf: VecDeque<u8>,
    head: u64,
    cap: usize,
}

impl ReplayRing {
    pub fn new(cap: usize) -> ReplayRing {
        ReplayRing {
            buf: VecDeque::new(),
            head: 0,
            cap: cap.max(1),
        }
    }

    pub fn head(&self) -> u64 {
        self.head
    }

    pub fn end(&self) -> u64 {
        self.head + self.buf.len() as u64
    }

    pub fn append(&mut self, data: &[u8]) {
        if data.len() >= self.cap {
            let skip = data.len() - self.cap;
            self.head = self.end() + skip as u64;
            self.buf.clear();
            self.buf.extend(&data[skip..]);
            return;
        }
        let over = (self.buf.len() + data.len()).saturating_sub(self.cap);
        if over > 0 {
            self.buf.drain(..over);
            self.head += over as u64;
        }
        self.buf.extend(data);
    }

    /// Bytes from `offset` to the end. `truncated` when `offset` is older than the ring
    /// (the bytes returned then start at `head`); an offset past the end returns nothing.
    pub fn read_from(&self, offset: u64) -> (bool, u64, Vec<u8>) {
        let truncated = offset < self.head;
        let start = offset.clamp(self.head, self.end());
        let skip = usize::try_from(start - self.head).unwrap_or(usize::MAX);
        (
            truncated,
            start,
            self.buf.iter().skip(skip).copied().collect(),
        )
    }
}

/// The result of [`Pty::attach`].
pub struct Attached {
    pub truncated: bool,
    /// Escape codes that repaint the current screen; set when `truncated`.
    pub checkpoint: Option<Vec<u8>>,
    /// Bytes from the requested offset (empty when a checkpoint is given).
    pub replay: Vec<u8>,
    /// Offset the replay (or checkpoint) brings the client to; live output starts here.
    pub end_offset: u64,
    /// Already exited (no live output will follow).
    pub exited: Option<Option<i32>>,
    pub events: broadcast::Receiver<PtyEvent>,
}

struct State {
    ring: ReplayRing,
    vt: vt100::Parser,
    exited: Option<Option<i32>>,
}

struct Shared {
    state: Mutex<State>,
    tx: broadcast::Sender<PtyEvent>,
    exit_tx: watch::Sender<Option<Option<i32>>>,
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// A running terminal. Dropping it kills the tree.
pub struct Pty {
    shared: Arc<Shared>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    input: SyncSender<Vec<u8>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    pid: Option<u32>,
    killed: AtomicBool,
    #[cfg(windows)]
    _job: Option<crate::windows::Job>,
}

fn nz(v: u16) -> NonZeroU16 {
    NonZeroU16::new(v).unwrap_or(NonZeroU16::MIN)
}

fn size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        rows: rows.max(1),
        cols: cols.max(1),
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn other(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

impl Pty {
    /// Open a PTY of `spec.cols × spec.rows` and start the program on it. An error here
    /// means nothing ran.
    pub fn spawn(spec: PtySpec) -> io::Result<Pty> {
        let pair = native_pty_system()
            .openpty(size(spec.cols, spec.rows))
            .map_err(other)?;
        let mut cmd = CommandBuilder::new(&spec.program);
        cmd.args(&spec.args);
        if let Some(cwd) = &spec.cwd {
            cmd.cwd(cwd);
        }
        cmd.env_clear();
        let env = spec.env.resolve(std::env::vars_os());
        for (k, v) in &env {
            cmd.env(k, v);
        }
        if !env.keys().any(|k| k == "TERM") {
            cmd.env("TERM", "xterm-256color");
        }
        if !env.keys().any(|k| k == "COLORTERM") {
            cmd.env("COLORTERM", "truecolor");
        }
        let mut child = pair.slave.spawn_command(cmd).map_err(other)?;
        // The child holds its own copy of the slave; ours must close so EOF arrives.
        drop(pair.slave);
        let pid = child.process_id();
        #[cfg(windows)]
        let job = {
            let job = crate::windows::Job::new_kill_on_close().ok();
            if let (Some(j), Some(h)) = (&job, child.as_raw_handle()) {
                let _ = j.assign(h);
            }
            job
        };
        let killer = child.clone_killer();
        let mut reader = pair.master.try_clone_reader().map_err(other)?;
        let mut writer = pair.master.take_writer().map_err(other)?;

        let (tx, _) = broadcast::channel(SUBSCRIBER_QUEUE);
        let (exit_tx, _) = watch::channel(None);
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                ring: ReplayRing::new(spec.replay_capacity),
                vt: vt100::Parser::new(nz(spec.rows), nz(spec.cols), VT_SCROLLBACK),
                exited: None,
            }),
            tx,
            exit_tx,
        });

        // Reader: ring + VT + fan-out under one lock.
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let sh = shared.clone();
        std::thread::Builder::new()
            .name("genie-pty-reader".into())
            .spawn(move || {
                let mut buf = vec![0u8; READ_CHUNK];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            let data = Arc::new(buf[..n].to_vec());
                            let mut st = sh.state();
                            let start = st.ring.end();
                            st.ring.append(&data);
                            st.vt.process(&data);
                            let _ = sh.tx.send(PtyEvent::Output { start, data });
                        }
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                        // EIO on Linux once every slave fd is closed.
                        Err(_) => break,
                    }
                }
                let _ = done_tx.send(());
            })?;

        // Writer: bounded queue so a child that stops reading never blocks callers.
        let (input, input_rx) = mpsc::sync_channel::<Vec<u8>>(WRITER_QUEUE);
        std::thread::Builder::new()
            .name("genie-pty-writer".into())
            .spawn(move || {
                while let Ok(chunk) = input_rx.recv() {
                    if writer
                        .write_all(&chunk)
                        .and_then(|()| writer.flush())
                        .is_err()
                    {
                        break;
                    }
                }
            })?;

        // Wait: reap, let the reader drain, then publish `Exited`.
        let sh = shared.clone();
        std::thread::Builder::new()
            .name("genie-pty-wait".into())
            .spawn(move || {
                let code = match child.wait() {
                    Ok(s) if s.signal().is_none() => i32::try_from(s.exit_code()).ok(),
                    _ => None,
                };
                let _ = done_rx.recv_timeout(DRAIN);
                let mut st = sh.state();
                st.exited = Some(code);
                let _ = sh.tx.send(PtyEvent::Exited { code });
                drop(st);
                let _ = sh.exit_tx.send(Some(code));
            })?;

        Ok(Pty {
            shared,
            master: Mutex::new(pair.master),
            input,
            killer: Mutex::new(killer),
            pid,
            killed: AtomicBool::new(false),
            #[cfg(windows)]
            _job: job,
        })
    }

    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// Replay from `offset` and subscribe to what follows, atomically.
    pub fn attach(&self, offset: u64) -> Attached {
        let st = self.shared.state();
        let events = self.shared.tx.subscribe();
        let end_offset = st.ring.end();
        let (truncated, _, bytes) = st.ring.read_from(offset);
        let (checkpoint, replay) = if truncated {
            (Some(st.vt.screen().state_formatted().into_bytes()), vec![])
        } else {
            (None, bytes)
        };
        Attached {
            truncated,
            checkpoint,
            replay,
            end_offset,
            exited: st.exited,
            events,
        }
    }

    /// Bytes from `offset` (for a subscriber that lagged): `(truncated, start, bytes)`.
    pub fn read_from(&self, offset: u64) -> (bool, u64, Vec<u8>) {
        self.shared.state().ring.read_from(offset)
    }

    pub fn end_offset(&self) -> u64 {
        self.shared.state().ring.end()
    }

    /// The visible screen as plain text (tests, diagnostics).
    pub fn screen_text(&self) -> String {
        self.shared.state().vt.screen().contents()
    }

    /// Queue input for the child. `WouldBlock` when the child has stopped reading and the
    /// queue is full; `BrokenPipe` after exit.
    pub fn write(&self, data: &[u8]) -> io::Result<()> {
        if data.len() > MAX_INPUT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("input is larger than {MAX_INPUT} bytes"),
            ));
        }
        if self.exited().is_some() {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "the terminal has exited",
            ));
        }
        match self.input.try_send(data.to_vec()) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "the terminal is not reading its input",
            )),
            Err(TrySendError::Disconnected(_)) => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "the terminal has exited",
            )),
        }
    }

    /// Resize the PTY and the VT screen together.
    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        if self.exited().is_some() {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "the terminal has exited",
            ));
        }
        let mut st = self.shared.state();
        self.master
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .resize(size(cols, rows))
            .map_err(other)?;
        st.vt.set_size(nz(rows), nz(cols));
        Ok(())
    }

    pub fn exited(&self) -> Option<Option<i32>> {
        self.shared.state().exited
    }

    /// Wait until the child exited (and the reader drained).
    pub async fn wait(&self) -> Option<i32> {
        let mut rx = self.shared.exit_tx.subscribe();
        loop {
            if let Some(code) = *rx.borrow_and_update() {
                return code;
            }
            if rx.changed().await.is_err() {
                return None;
            }
        }
    }

    /// Kill the child and everything it started. Idempotent.
    ///
    /// Unix: the shell is a session leader (its pgid is its pid); the foreground job's
    /// group (`tcgetpgrp`) and the shell's group get `SIGKILL`, and on Linux and macOS every
    /// process of the session too (`/proc` / `proc_listallpids`). Closing the master then hangs up anything left
    /// on the terminal. Windows: the Job Object is terminated.
    pub fn kill_tree(&self) {
        if self.killed.swap(true, Ordering::SeqCst) {
            return;
        }
        #[cfg(unix)]
        {
            let fg = self
                .master
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .process_group_leader();
            for pg in fg
                .into_iter()
                .chain(self.pid.and_then(|p| i32::try_from(p).ok()))
            {
                if pg > 1 {
                    // SAFETY: plain syscall; a negative pid targets a process group.
                    unsafe { libc::kill(-pg, libc::SIGKILL) };
                }
            }
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            if let Some(sid) = self.pid.and_then(|p| i32::try_from(p).ok()) {
                kill_session(sid);
            }
        }
        #[cfg(windows)]
        if let Some(j) = &self._job {
            let _ = j.terminate();
        }
        let _ = self.killer.lock().unwrap_or_else(|p| p.into_inner()).kill();
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        self.kill_tree();
    }
}

/// SIGKILL every process whose session id is `sid`: background jobs of an interactive
/// shell live in their own process groups, so killing groups alone would miss them.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn kill_session(sid: i32) {
    for pid in all_pids() {
        // SAFETY: plain syscalls.
        if pid > 1 && unsafe { libc::getsid(pid) } == sid {
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    }
}

#[cfg(target_os = "linux")]
fn all_pids() -> Vec<i32> {
    let Ok(rd) = std::fs::read_dir("/proc") else {
        return vec![];
    };
    rd.flatten()
        .filter_map(|e| e.file_name().to_str().and_then(|s| s.parse::<i32>().ok()))
        .collect()
}

#[cfg(target_os = "macos")]
fn all_pids() -> Vec<i32> {
    let mut buf = vec![0i32; 16_384];
    let bytes = libc::c_int::try_from(buf.len() * std::mem::size_of::<i32>()).unwrap_or(0);
    // SAFETY: the buffer is valid for `bytes` bytes; the call writes at most that many.
    let n = unsafe { libc::proc_listallpids(buf.as_mut_ptr().cast(), bytes) };
    buf.truncate(usize::try_from(n).unwrap_or(0));
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_keeps_the_newest_bytes_with_absolute_offsets() {
        let mut r = ReplayRing::new(8);
        r.append(b"abcdef");
        assert_eq!((r.head(), r.end()), (0, 6));
        r.append(b"ghij");
        assert_eq!((r.head(), r.end()), (2, 10));
        assert_eq!(r.read_from(4), (false, 4, b"efghij".to_vec()));
        assert_eq!(r.read_from(0), (true, 2, b"cdefghij".to_vec()));
        assert_eq!(r.read_from(99), (false, 10, vec![]));
        r.append(b"0123456789AB");
        assert_eq!((r.head(), r.end()), (14, 22));
        assert_eq!(r.read_from(14).2, b"456789AB".to_vec());
    }
}
