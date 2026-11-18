//! Pipe mode (06 §13.4, D45): spawn a program with piped stdio inside its own kill-tree.
//!
//! Semantics follow `opencode-pty`'s pipe processes (KB ch.16): the child gets exactly the
//! environment it is given, stdin is `/dev/null`/`NUL` unless the caller asks for a pipe,
//! and output is captured by the caller. Differences from the port source:
//!
//! * stdout and stderr are two pipes (std/tokio can't hand one pipe to both); an
//!   [`OutputCollector`] interleaves them at chunk granularity for "combined" output.
//! * a [`SpawnHook`] can rewrite `program/args` before spawning (the D44 sandbox hook).
//! * no tasks are spawned here: [`PipeChild::collect`] is a cancel-safe future the caller
//!   drives (01 §2.1 rule 4 keeps task ownership with the caller's `TaskTracker`).

use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::{ProcessTree, spawn_tree};

/// Variables that are always removed from an inherited environment: the daemon's own
/// internals (endpoint, token, channel overrides, log filter). Matched as prefixes.
pub const SCRUBBED_PREFIXES: &[&str] = &["GENIE_"];

/// The MCP SDK's default inherited variables for stdio servers (`getDefaultEnvironment`).
#[cfg(unix)]
pub const MINIMAL_ENV: &[&str] = &[
    "HOME", "LOGNAME", "PATH", "SHELL", "TERM", "USER", "LANG", "TMPDIR",
];
#[cfg(windows)]
pub const MINIMAL_ENV: &[&str] = &[
    "APPDATA",
    "HOMEDRIVE",
    "HOMEPATH",
    "LOCALAPPDATA",
    "PATH",
    "PATHEXT",
    "PROCESSOR_ARCHITECTURE",
    "PROGRAMDATA",
    "PROGRAMFILES",
    "SYSTEMDRIVE",
    "SYSTEMROOT",
    "TEMP",
    "TMP",
    "USERNAME",
    "USERPROFILE",
    "WINDIR",
    "COMSPEC",
];

/// Where the child's environment starts from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvBase {
    /// Nothing: the child gets exactly [`Env::set`] (opencode-pty semantics).
    Empty,
    /// The daemon's environment minus [`SCRUBBED_PREFIXES`] (shell commands).
    Inherit,
    /// Only these names from the daemon's environment (MCP stdio servers: [`MINIMAL_ENV`]).
    Allowlist(Vec<String>),
}

/// The child's environment: a base, then removals, then explicit values (which win).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Env {
    pub base: EnvBase,
    pub remove: Vec<String>,
    pub set: BTreeMap<String, String>,
}

impl Env {
    pub fn exact(set: BTreeMap<String, String>) -> Env {
        Env {
            base: EnvBase::Empty,
            remove: Vec::new(),
            set,
        }
    }

    pub fn inherit() -> Env {
        Env {
            base: EnvBase::Inherit,
            remove: Vec::new(),
            set: BTreeMap::new(),
        }
    }

    pub fn minimal() -> Env {
        Env {
            base: EnvBase::Allowlist(MINIMAL_ENV.iter().map(|s| (*s).to_string()).collect()),
            remove: Vec::new(),
            set: BTreeMap::new(),
        }
    }

    pub fn with(mut self, key: impl Into<String>, value: impl Into<String>) -> Env {
        self.set.insert(key.into(), value.into());
        self
    }

    /// Resolve against `parent` (normally `std::env::vars_os()`). Names compare
    /// case-insensitively on Windows.
    pub fn resolve(
        &self,
        parent: impl IntoIterator<Item = (OsString, OsString)>,
    ) -> BTreeMap<OsString, OsString> {
        let mut out = BTreeMap::new();
        let keep = |name: &str| -> bool {
            match &self.base {
                EnvBase::Empty => false,
                EnvBase::Inherit => !SCRUBBED_PREFIXES
                    .iter()
                    .any(|p| norm(name).starts_with(&norm(p))),
                EnvBase::Allowlist(names) => names.iter().any(|n| norm(n) == norm(name)),
            }
        };
        if self.base != EnvBase::Empty {
            for (k, v) in parent {
                let Some(name) = k.to_str() else { continue };
                if keep(name) && !self.remove.iter().any(|r| norm(r) == norm(name)) {
                    out.insert(k, v);
                }
            }
        }
        for (k, v) in &self.set {
            // Explicit values replace an inherited variable of the same (normalised) name.
            out.retain(|ek: &OsString, _| ek.to_str().is_none_or(|e| norm(e) != norm(k)));
            out.insert(OsString::from(k), OsString::from(v));
        }
        out
    }
}

fn norm(name: &str) -> String {
    if cfg!(windows) {
        name.to_ascii_uppercase()
    } else {
        name.to_string()
    }
}

/// What the child's stdin is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StdinMode {
    /// `/dev/null` / `NUL` (shell commands: no interactive input).
    #[default]
    Null,
    /// A pipe the caller writes (MCP stdio transport).
    Piped,
}

/// A program to run in pipe mode.
#[derive(Debug, Clone)]
pub struct PipeSpec {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    pub env: Env,
    pub stdin: StdinMode,
}

impl PipeSpec {
    pub fn new(program: impl Into<OsString>) -> PipeSpec {
        PipeSpec {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            env: Env::inherit(),
            stdin: StdinMode::Null,
        }
    }

    pub fn args<I, S>(mut self, args: I) -> PipeSpec
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> PipeSpec {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn env(mut self, env: Env) -> PipeSpec {
        self.env = env;
        self
    }

    pub fn stdin(mut self, mode: StdinMode) -> PipeSpec {
        self.stdin = mode;
        self
    }
}

/// Rewrites `program/args` (and may adjust env/cwd) just before spawning. The v2 sandbox
/// (D44) wraps commands in its launcher here (06 §13.4 "spawn hook").
pub trait SpawnHook: Send + Sync {
    fn wrap(&self, spec: &mut PipeSpec);
}

/// Spawning failed before the program ran: always safe to retry (06 §13.1 error split).
#[derive(Debug)]
pub struct SpawnError {
    pub program: OsString,
    pub source: io::Error,
}

impl std::fmt::Display for SpawnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "couldn't start {}: {} (safe to retry)",
            self.program.to_string_lossy(),
            self.source
        )
    }
}

impl std::error::Error for SpawnError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Spawn `spec` in pipe mode inside a new kill-tree (Job Object / process group).
pub fn spawn_pipe(
    mut spec: PipeSpec,
    hook: Option<&dyn SpawnHook>,
) -> Result<PipeChild, SpawnError> {
    if let Some(h) = hook {
        h.wrap(&mut spec);
    }
    let mut cmd = Command::new(&spec.program);
    cmd.args(&spec.args);
    if let Some(cwd) = &spec.cwd {
        cmd.current_dir(cwd);
    }
    cmd.env_clear();
    cmd.envs(spec.env.resolve(std::env::vars_os()));
    cmd.stdin(match spec.stdin {
        StdinMode::Null => Stdio::null(),
        StdinMode::Piped => Stdio::piped(),
    });
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let mut tree = spawn_tree(&mut cmd).map_err(|source| SpawnError {
        program: spec.program.clone(),
        source,
    })?;
    let child = tree.child_mut();
    Ok(PipeChild {
        stdin: child.stdin.take(),
        stdout: child.stdout.take(),
        stderr: child.stderr.take(),
        tree,
        exit: None,
    })
}

/// A pipe-mode child. Dropping it kills the whole tree.
pub struct PipeChild {
    tree: ProcessTree,
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
    exit: Option<ExitStatus>,
}

/// How [`PipeChild::collect`] returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    /// The child exited and its output was drained.
    Exited(ExitStatus),
    /// The deadline passed; the child is **still running** (the shell tool moves it to the
    /// background, 06 §13.1). Call `collect` again or [`PipeChild::kill_tree`].
    TimedOut,
    /// The token was cancelled; the tree was killed and reaped.
    Cancelled,
}

/// After the direct child exits, descendants may still hold the pipes open. Keep draining
/// until EOF or this much silence, then stop (opencode-pty "descendants holding the pipe").
const EXIT_DRAIN_IDLE: Duration = Duration::from_millis(300);

impl PipeChild {
    pub fn id(&self) -> Option<u32> {
        self.tree.id()
    }

    /// See [`crate::ProcessTree::probe`].
    pub fn probe(&self) -> crate::ports::TreeProbe {
        self.tree.probe()
    }

    pub fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.stdin.take()
    }

    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.stdout.take()
    }

    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.stderr.take()
    }

    /// Write `data` to stdin and close it.
    pub async fn write_stdin_and_close(&mut self, data: &[u8]) -> io::Result<()> {
        if let Some(mut s) = self.stdin.take() {
            s.write_all(data).await?;
            s.shutdown().await?;
        }
        Ok(())
    }

    /// Kill the child and everything it started. Idempotent.
    pub fn kill_tree(&mut self) -> io::Result<()> {
        self.tree.kill_tree()
    }

    /// Wait for the direct child (without reading output).
    pub async fn wait(&mut self) -> io::Result<ExitStatus> {
        if let Some(s) = self.exit {
            return Ok(s);
        }
        let s = self.tree.wait().await?;
        self.exit = Some(s);
        Ok(s)
    }

    /// Wait for exit, or kill the tree on cancel.
    pub async fn wait_or_cancel(&mut self, token: &CancellationToken) -> io::Result<RunOutcome> {
        Ok(match self.tree.wait_or_cancel(token).await? {
            crate::WaitOutcome::Exited(s) => {
                self.exit = Some(s);
                RunOutcome::Exited(s)
            }
            crate::WaitOutcome::Cancelled => RunOutcome::Cancelled,
        })
    }

    /// Read stdout and stderr (whichever are still owned) into `out` until the child exits
    /// and the pipes are drained, `deadline` passes, or `cancel` fires. Cancel-safe and
    /// resumable: all state lives in `self` and `out`.
    pub async fn collect(
        &mut self,
        out: &mut OutputCollector,
        deadline: Option<Instant>,
        cancel: &CancellationToken,
    ) -> io::Result<RunOutcome> {
        let mut buf_o = vec![0u8; 16 * 1024];
        let mut buf_e = vec![0u8; 16 * 1024];
        let far = Instant::now() + Duration::from_secs(365 * 24 * 3600);
        loop {
            if self.stdout.is_none() && self.stderr.is_none() {
                break;
            }
            let idle = if self.exit.is_some() {
                Instant::now() + EXIT_DRAIN_IDLE
            } else {
                far
            };
            tokio::select! {
                biased;
                () = cancel.cancelled() => return self.cancelled().await,
                () = tokio::time::sleep_until(deadline.unwrap_or(far)) => return Ok(RunOutcome::TimedOut),
                n = read_opt(&mut self.stdout, &mut buf_o) => match n? {
                    0 => self.stdout = None,
                    n => out.push(Stream::Stdout, &buf_o[..n]).await?,
                },
                n = read_opt(&mut self.stderr, &mut buf_e) => match n? {
                    0 => self.stderr = None,
                    n => out.push(Stream::Stderr, &buf_e[..n]).await?,
                },
                s = self.tree.wait(), if self.exit.is_none() => { self.exit = Some(s?); }
                () = tokio::time::sleep_until(idle), if self.exit.is_some() => break,
            }
        }
        let status = match self.exit {
            Some(s) => s,
            None => tokio::select! {
                biased;
                () = cancel.cancelled() => return self.cancelled().await,
                () = tokio::time::sleep_until(deadline.unwrap_or(far)) => return Ok(RunOutcome::TimedOut),
                s = self.tree.wait() => s?,
            },
        };
        self.exit = Some(status);
        out.finish().await?;
        Ok(RunOutcome::Exited(status))
    }

    async fn cancelled(&mut self) -> io::Result<RunOutcome> {
        self.tree.kill_tree()?;
        if self.exit.is_none() {
            let _ = self.tree.wait().await;
        }
        self.stdout = None;
        self.stderr = None;
        Ok(RunOutcome::Cancelled)
    }
}

async fn read_opt<R: AsyncRead + Unpin>(r: &mut Option<R>, buf: &mut [u8]) -> io::Result<usize> {
    match r {
        Some(r) => r.read(buf).await,
        None => std::future::pending().await,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// Output caps (06 §13.1: combined output to a file; the model gets head + tail).
#[derive(Debug, Clone)]
pub struct OutputCaps {
    /// Bytes kept from the start of the combined output.
    pub head_bytes: usize,
    /// Bytes kept from the end of the combined output.
    pub tail_bytes: usize,
    /// Spill file for the full combined output (truncated on open).
    pub file: Option<PathBuf>,
    /// The spill file stops growing past this many bytes.
    pub file_cap: u64,
}

impl Default for OutputCaps {
    fn default() -> OutputCaps {
        OutputCaps {
            head_bytes: 16 * 1024,
            tail_bytes: 16 * 1024,
            file: None,
            file_cap: 64 * 1024 * 1024,
        }
    }
}

/// Collects combined stdout+stderr: a bounded head, a bounded tail, counts, and an
/// optional spill file.
pub struct OutputCollector {
    caps: OutputCaps,
    head: Vec<u8>,
    tail: TailBuffer,
    total: u64,
    stderr_total: u64,
    newlines: u64,
    file: Option<tokio::fs::File>,
    file_written: u64,
    file_truncated: bool,
}

impl OutputCollector {
    /// Opens (and truncates) the spill file if one is configured.
    pub async fn new(caps: OutputCaps) -> io::Result<OutputCollector> {
        let file = match &caps.file {
            Some(p) => Some(tokio::fs::File::create(p).await?),
            None => None,
        };
        Ok(OutputCollector {
            tail: TailBuffer::new(caps.tail_bytes),
            caps,
            head: Vec::new(),
            total: 0,
            stderr_total: 0,
            newlines: 0,
            file,
            file_written: 0,
            file_truncated: false,
        })
    }

    pub async fn push(&mut self, stream: Stream, bytes: &[u8]) -> io::Result<()> {
        self.total += bytes.len() as u64;
        if stream == Stream::Stderr {
            self.stderr_total += bytes.len() as u64;
        }
        self.newlines += bytes.iter().filter(|b| **b == b'\n').count() as u64;
        let room = self.caps.head_bytes.saturating_sub(self.head.len());
        let (h, rest) = bytes.split_at(room.min(bytes.len()));
        self.head.extend_from_slice(h);
        self.tail.push(rest);
        if let Some(f) = &mut self.file {
            let room = self.caps.file_cap.saturating_sub(self.file_written);
            let n = usize::try_from(room).unwrap_or(usize::MAX).min(bytes.len());
            if n < bytes.len() {
                self.file_truncated = true;
            }
            if n > 0 {
                f.write_all(&bytes[..n]).await?;
                self.file_written += n as u64;
            }
        }
        Ok(())
    }

    async fn finish(&mut self) -> io::Result<()> {
        if let Some(f) = &mut self.file {
            f.flush().await?;
        }
        Ok(())
    }

    /// Snapshot of what was captured so far.
    pub fn summary(&self) -> OutputSummary {
        let tail = self.tail.to_vec();
        let omitted = self.total - self.head.len() as u64 - tail.len() as u64;
        OutputSummary {
            head: String::from_utf8_lossy(&self.head).into_owned(),
            tail: String::from_utf8_lossy(&tail).into_owned(),
            omitted_bytes: omitted,
            total_bytes: self.total,
            stderr_bytes: self.stderr_total,
            lines: self.newlines,
            file: self.caps.file.clone(),
            file_truncated: self.file_truncated,
        }
    }
}

/// Head + tail of the combined output. When `omitted_bytes == 0`, `head + tail` is the
/// whole output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputSummary {
    pub head: String,
    pub tail: String,
    pub omitted_bytes: u64,
    pub total_bytes: u64,
    pub stderr_bytes: u64,
    pub lines: u64,
    pub file: Option<PathBuf>,
    pub file_truncated: bool,
}

impl OutputSummary {
    /// `head`, an omission marker if anything was dropped, then `tail`.
    pub fn text(&self) -> String {
        if self.omitted_bytes == 0 {
            format!("{}{}", self.head, self.tail)
        } else {
            let note = match &self.file {
                Some(p) => format!(" (full output: {})", p.display()),
                None => String::new(),
            };
            format!(
                "{}\n… {} bytes omitted{} …\n{}",
                self.head, self.omitted_bytes, note, self.tail
            )
        }
    }
}

/// A byte ring that keeps the last `cap` bytes.
#[derive(Debug, Clone)]
pub struct TailBuffer {
    cap: usize,
    buf: VecDeque<u8>,
}

impl TailBuffer {
    pub fn new(cap: usize) -> TailBuffer {
        TailBuffer {
            cap,
            buf: VecDeque::new(),
        }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        if self.cap == 0 {
            return;
        }
        let bytes = if bytes.len() > self.cap {
            &bytes[bytes.len() - self.cap..]
        } else {
            bytes
        };
        let overflow = (self.buf.len() + bytes.len()).saturating_sub(self.cap);
        self.buf.drain(..overflow);
        self.buf.extend(bytes);
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn to_vec(&self) -> Vec<u8> {
        self.buf.iter().copied().collect()
    }
}

/// Result of [`run`].
#[derive(Debug, Clone)]
pub struct RunResult {
    pub outcome: RunOutcome,
    pub output: OutputSummary,
}

/// Spawn, capture combined output and wait. On timeout the tree is **killed** (callers that
/// background long commands drive [`PipeChild::collect`] themselves).
pub async fn run(
    spec: PipeSpec,
    hook: Option<&dyn SpawnHook>,
    caps: OutputCaps,
    timeout: Option<Duration>,
    cancel: &CancellationToken,
) -> Result<RunResult, RunError> {
    let mut child = spawn_pipe(spec, hook).map_err(RunError::Spawn)?;
    let mut out = OutputCollector::new(caps).await.map_err(RunError::Io)?;
    let deadline = timeout.map(|t| Instant::now() + t);
    let outcome = child
        .collect(&mut out, deadline, cancel)
        .await
        .map_err(RunError::Io)?;
    if outcome == RunOutcome::TimedOut {
        child.kill_tree().map_err(RunError::Io)?;
        let _ = child.wait().await;
    }
    Ok(RunResult {
        outcome,
        output: out.summary(),
    })
}

/// [`run`] failed. `Spawn` never ran the program (safe to retry); `Io` may have.
#[derive(Debug)]
pub enum RunError {
    Spawn(SpawnError),
    Io(io::Error),
}

impl RunError {
    /// 06 §13.1: "couldn't start → safe to retry" vs "may have run → never auto-retry".
    pub fn safe_to_retry(&self) -> bool {
        matches!(self, RunError::Spawn(_))
    }
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::Spawn(e) => e.fmt(f),
            RunError::Io(e) => write!(f, "command may have run: {e}"),
        }
    }
}

impl std::error::Error for RunError {}
