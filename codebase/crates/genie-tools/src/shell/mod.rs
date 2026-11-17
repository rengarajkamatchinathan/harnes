//! The `shell` tool (spec/06-agent-runtime.md §13.1–13.3, D45).
//!
//! - Every call runs in a **hidden shell process** ([`select`]: zsh/bash on Unix, pwsh /
//!   Windows PowerShell / Git Bash on Windows) in the session's cwd, through
//!   `genie-process` pipe mode (own kill-tree, stdin `/dev/null`, combined output to a file).
//! - **`cd` persists:** the script ends with a trailer (an `EXIT` trap / `finally`) that
//!   writes the final directory to a side file; the next call starts there. Nothing else
//!   (variables, functions, aliases) carries over.
//! - **Timeout → background:** after 2 min (the model may ask for up to 10) the still
//!   running command becomes a background job: [`BackgroundJobs::started`] (the engine
//!   writes the `background_job` row) and, when it ends, [`BackgroundJobs::finished`] (the
//!   engine records it and queues the synthetic `background_done` inbox message).
//!   `run_in_background: true` starts there.
//! - **Kill:** the call's cancel token (stop / interrupt) kills a foreground command's
//!   whole tree; [`ShellRuntime::end_session`], [`ShellRuntime::kill_job`] and the runtime's
//!   shutdown token kill background jobs.
//! - **Output:** head + tail of the combined stdout+stderr; the full text goes to the
//!   [`SpillStore`](crate::SpillStore) with a ref the model can page.
//! - **Errors:** couldn't start → `Failed { may_have_run: false }` (safe to retry); anything
//!   after the spawn → `may_have_run: true` (never auto-retried).
//! - **Permissions:** the tool asks with resource `Bash(<command>)` / `PowerShell(<command>)`
//!   and a `Command` preview carrying the cwd; [`authz_from_request`] turns that back into
//!   the dialect, command, cwd and [`scan`]ned parts for `genie_permissions::Action::Shell`.

pub mod scan;
pub mod select;
mod tool;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, SystemTime};

use genie_process::pipe::{Env, SpawnHook};
use genie_protocol::ask::Preview;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::tool::Tool;

pub use scan::{Dialect, ScanError, scan};
pub use select::{Requested, ShellChoice, ShellKind};
pub use tool::ShellTool;

pub const NAME: &str = "shell";
/// 06 §13.1: 2 min, then the command moves to the background.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
/// The longest timeout the model may request.
pub const MAX_TIMEOUT: Duration = Duration::from_secs(600);

/// Where a session's first shell call starts (workspace root or worktree).
pub trait SessionDirs: Send + Sync {
    fn initial_cwd(&self, session_id: &str) -> Option<PathBuf>;
}

/// Every session starts in the same directory (tests, single-workspace hosts).
pub struct FixedDir(pub PathBuf);

impl SessionDirs for FixedDir {
    fn initial_cwd(&self, _session_id: &str) -> Option<PathBuf> {
        Some(self.0.clone())
    }
}

/// A command moved to the background (04 §3 `background_job` row).
#[derive(Debug, Clone, PartialEq)]
pub struct JobStarted {
    pub id: String,
    pub session_id: String,
    pub turn_id: String,
    pub call_id: String,
    pub command: String,
    pub cwd: PathBuf,
    pub started: SystemTime,
}

/// A background command ended.
#[derive(Debug, Clone, PartialEq)]
pub struct JobFinished {
    pub id: String,
    pub session_id: String,
    pub command: String,
    /// `None` when killed by a signal or by Genie.
    pub exit_code: Option<i32>,
    /// Killed by `kill_job`, session end or shutdown.
    pub killed: bool,
    /// Head + tail of the output, as the model would see it.
    pub output: String,
    /// Spill ref of the full output (`background_job.output_blob`).
    pub output_ref: Option<String>,
    pub ended: SystemTime,
}

impl JobFinished {
    /// The `{code}` of the `background_done` prompt (06 §2.6.7).
    pub fn code_text(&self) -> String {
        match (self.killed, self.exit_code) {
            (true, _) => "killed".into(),
            (false, Some(c)) => c.to_string(),
            (false, None) => "signal".into(),
        }
    }
}

/// Background-job port: implemented by `genie-core` on the store (`background_job` rows)
/// and the session inbox (synthetic `background_done` message, 06 §13.1).
pub trait BackgroundJobs: Send + Sync {
    fn started(&self, job: &JobStarted);
    fn finished(&self, job: JobFinished);
}

/// Shell tool configuration.
pub struct ShellConfig {
    pub dirs: Arc<dyn SessionDirs>,
    /// Without it, a timeout kills the command (`ToolError::Timeout`) and
    /// `run_in_background` is refused.
    pub jobs: Option<Arc<dyn BackgroundJobs>>,
    /// Background jobs run on this tracker (01 §2.1 rule 4).
    pub tasks: TaskTracker,
    /// Parent of every background job's token (daemon shutdown kills them).
    pub shutdown: CancellationToken,
    /// Output and cwd side files (created 0700).
    pub temp_dir: PathBuf,
    pub default_timeout: Duration,
    pub max_timeout: Duration,
    /// Child environment: inherited minus `GENIE_*`, pagers off.
    pub env: Env,
    /// Fixed shell instead of [`select`] (settings, tests).
    pub shell: Option<ShellChoice>,
    pub spawn_hook: Option<Arc<dyn SpawnHook>>,
    /// Bytes kept from the start / end of the output for the model.
    pub head_bytes: usize,
    pub tail_bytes: usize,
    /// Lines kept from the start / end of the output for the model.
    pub head_lines: usize,
    pub tail_lines: usize,
    /// The output file stops growing here.
    pub file_cap: u64,
}

impl ShellConfig {
    pub fn new(
        dirs: Arc<dyn SessionDirs>,
        tasks: TaskTracker,
        shutdown: CancellationToken,
    ) -> Self {
        ShellConfig {
            dirs,
            jobs: None,
            tasks,
            shutdown,
            temp_dir: std::env::temp_dir().join(format!("genie-shell-{}", std::process::id())),
            default_timeout: DEFAULT_TIMEOUT,
            max_timeout: MAX_TIMEOUT,
            env: Env::inherit()
                .with("PAGER", "cat")
                .with("GIT_PAGER", "cat")
                .with("GIT_TERMINAL_PROMPT", "0"),
            shell: None,
            spawn_hook: None,
            head_bytes: 20 * 1024,
            tail_bytes: 20 * 1024,
            head_lines: 400,
            tail_lines: 400,
            file_cap: 64 * 1024 * 1024,
        }
    }

    pub fn with_jobs(mut self, jobs: Arc<dyn BackgroundJobs>) -> Self {
        self.jobs = Some(jobs);
        self
    }
}

struct JobEntry {
    session_id: String,
    cancel: CancellationToken,
}

pub(crate) struct Inner {
    cfg: ShellConfig,
    cwds: Mutex<HashMap<String, PathBuf>>,
    jobs: Mutex<HashMap<String, JobEntry>>,
    default_choice: OnceLock<Result<ShellChoice, String>>,
    seq: AtomicU64,
    epoch: u64,
    schema: Value,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Inner {
    fn choice(&self, requested: Requested) -> Result<ShellChoice, String> {
        if let Some(fixed) = &self.cfg.shell
            && requested == Requested::Default
        {
            return Ok(fixed.clone());
        }
        if requested == Requested::Default {
            return self
                .default_choice
                .get_or_init(|| select::select(&select::Probe::system(), Requested::Default))
                .clone();
        }
        select::select(&select::Probe::system(), requested)
    }

    fn next(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn file_stem(&self, n: u64) -> String {
        format!("{:x}-{n}", self.epoch)
    }

    fn job_id(&self, n: u64) -> String {
        format!("bg_{:x}{n}", self.epoch)
    }

    /// The directory a call runs in; resets to the initial cwd if it disappeared.
    fn cwd_for(&self, session_id: &str) -> Option<PathBuf> {
        let mut map = lock(&self.cwds);
        if let Some(p) = map.get(session_id)
            && p.is_dir()
        {
            return Some(p.clone());
        }
        let init = self.cfg.dirs.initial_cwd(session_id)?;
        map.insert(session_id.to_string(), init.clone());
        Some(init)
    }

    fn set_cwd(&self, session_id: &str, cwd: PathBuf) {
        lock(&self.cwds).insert(session_id.to_string(), cwd);
    }
}

/// Owns the per-session cwd and the background jobs; hands out the `shell` tool.
#[derive(Clone)]
pub struct ShellRuntime {
    inner: Arc<Inner>,
}

impl ShellRuntime {
    pub fn new(cfg: ShellConfig) -> Self {
        let epoch = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        ShellRuntime {
            inner: Arc::new(Inner {
                cfg,
                cwds: Mutex::new(HashMap::new()),
                jobs: Mutex::new(HashMap::new()),
                default_choice: OnceLock::new(),
                seq: AtomicU64::new(0),
                epoch,
                schema: tool::input_schema(),
            }),
        }
    }

    /// `[shell]`, for the code registry.
    pub fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![Arc::new(ShellTool::new(self.inner.clone()))]
    }

    /// The session's current shell directory (after `cd`s).
    pub fn cwd(&self, session_id: &str) -> Option<PathBuf> {
        self.inner.cwd_for(session_id)
    }

    /// Move the session's shell (e.g. after a worktree switch).
    pub fn set_cwd(&self, session_id: &str, cwd: PathBuf) {
        self.inner.set_cwd(session_id, cwd);
    }

    /// Ids of the session's running background jobs.
    pub fn running_jobs(&self, session_id: &str) -> Vec<String> {
        let mut v: Vec<String> = lock(&self.inner.jobs)
            .iter()
            .filter(|(_, j)| j.session_id == session_id)
            .map(|(id, _)| id.clone())
            .collect();
        v.sort();
        v
    }

    /// Kill one background job's tree. `false` if it isn't running.
    pub fn kill_job(&self, id: &str) -> bool {
        match lock(&self.inner.jobs).get(id) {
            Some(j) => {
                j.cancel.cancel();
                true
            }
            None => false,
        }
    }

    /// Session end / eviction: kill its background jobs and forget its cwd.
    pub fn end_session(&self, session_id: &str) {
        for j in lock(&self.inner.jobs).values() {
            if j.session_id == session_id {
                j.cancel.cancel();
            }
        }
        lock(&self.inner.cwds).remove(session_id);
    }
}

/// The shell tools for `cfg` (drops the runtime handle; keep a [`ShellRuntime`] to call
/// `end_session` / `kill_job`).
pub fn tools(cfg: ShellConfig) -> Vec<Arc<dyn Tool>> {
    ShellRuntime::new(cfg).tools()
}

/// The `resource` of a shell permission request: `Bash(<command>)` / `PowerShell(<command>)`.
pub fn permission_resource(dialect: Dialect, command: &str) -> String {
    format!("{}({command})", dialect.rule_tool())
}

/// A shell permission request decoded for `genie_permissions::Action::Shell`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellAuthz {
    pub dialect: Dialect,
    pub command: String,
    /// Scanner output; `None` = unparseable (→ ask).
    pub parts: Option<Vec<String>>,
    pub cwd: Option<String>,
}

/// Decode the `resource` + `preview` the shell tool sends (see [`permission_resource`]).
pub fn authz_from_request(resource: Option<&str>, preview: Option<&Preview>) -> Option<ShellAuthz> {
    let r = resource?;
    let (dialect, rest) = if let Some(rest) = r.strip_prefix("Bash(") {
        (Dialect::Posix, rest)
    } else {
        (Dialect::PowerShell, r.strip_prefix("PowerShell(")?)
    };
    let command = rest.strip_suffix(')')?.to_string();
    let cwd = match preview {
        Some(Preview::Command { cwd, .. }) => cwd.clone(),
        _ => None,
    };
    Some(ShellAuthz {
        parts: scan(dialect, &command).ok(),
        dialect,
        command,
        cwd,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authz_round_trip() {
        let res = permission_resource(Dialect::Posix, "ls && rm x");
        assert_eq!(res, "Bash(ls && rm x)");
        let a = authz_from_request(
            Some(&res),
            Some(&Preview::Command {
                command: "ls && rm x".into(),
                cwd: Some("/w".into()),
            }),
        )
        .unwrap();
        assert_eq!(a.dialect, Dialect::Posix);
        assert_eq!(a.parts, Some(vec!["ls".to_string(), "rm x".to_string()]));
        assert_eq!(a.cwd.as_deref(), Some("/w"));
        let a = authz_from_request(Some("PowerShell(Get-Item (x)"), None).unwrap();
        assert_eq!(a.command, "Get-Item (x");
        assert_eq!(a.parts, None, "unparseable → ask");
        assert!(authz_from_request(Some("example.com"), None).is_none());
    }
}
