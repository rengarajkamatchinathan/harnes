//! The `git` CLI as a child process through `genie-process` (04 §10 "Tooling", D45):
//! own kill-tree, inherited environment minus `GENIE_*` and any `GIT_DIR`-style
//! overrides, no prompts, C locale.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use genie_process::pipe::{Env, PipeSpec, StdinMode, spawn_pipe};
use tokio::io::AsyncReadExt;

/// Variables that would redirect git away from the repository we name.
const GIT_OVERRIDES: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_CEILING_DIRECTORIES",
];

/// One git invocation's output.
#[derive(Debug, Clone)]
pub struct GitOut {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

impl GitOut {
    pub fn ok(&self) -> bool {
        self.code == Some(0)
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }
}

/// A git runner bound to a working directory, with optional extra environment
/// (`GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE` for the shadow repository).
#[derive(Debug, Clone)]
pub struct Git {
    pub program: OsString,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    pub timeout: Duration,
}

impl Git {
    pub fn new(cwd: impl Into<PathBuf>) -> Git {
        Git {
            program: OsString::from("git"),
            cwd: cwd.into(),
            env: Vec::new(),
            timeout: Duration::from_secs(120),
        }
    }

    pub fn with_env(mut self, k: impl Into<String>, v: impl Into<String>) -> Git {
        self.env.push((k.into(), v.into()));
        self
    }

    /// Run `git args…` (stdin from `input` when given). `Err` only when git couldn't be
    /// run at all or timed out.
    pub async fn run<S: AsRef<str>>(
        &self,
        args: &[S],
        input: Option<&[u8]>,
    ) -> Result<GitOut, String> {
        let mut env = Env::inherit()
            .with("GIT_TERMINAL_PROMPT", "0")
            .with("GIT_OPTIONAL_LOCKS", "0")
            .with("LC_ALL", "C")
            .with("GIT_PAGER", "cat")
            .with("GIT_CONFIG_NOSYSTEM", "1");
        env.remove = GIT_OVERRIDES.iter().map(|s| (*s).to_string()).collect();
        for (k, v) in &self.env {
            env = env.with(k.clone(), v.clone());
        }
        let spec = PipeSpec::new(self.program.clone())
            .args(args.iter().map(|a| a.as_ref().to_string()))
            .cwd(self.cwd.clone())
            .env(env)
            .stdin(if input.is_some() {
                StdinMode::Piped
            } else {
                StdinMode::Null
            });
        let mut child = spawn_pipe(spec, None).map_err(|e| format!("can't run git: {e}"))?;
        let mut so = child.take_stdout();
        let mut se = child.take_stderr();
        let fut = async {
            let mut out = Vec::new();
            let mut err = Vec::new();
            let read_out = async {
                if let Some(s) = so.as_mut() {
                    let _ = s.read_to_end(&mut out).await;
                }
            };
            let read_err = async {
                if let Some(s) = se.as_mut() {
                    let _ = s.read_to_end(&mut err).await;
                }
            };
            let write_in = async {
                if let Some(i) = input {
                    let _ = child.write_stdin_and_close(i).await;
                }
            };
            tokio::join!(read_out, read_err, write_in);
            let status = child.wait().await.map_err(|e| e.to_string())?;
            Ok::<GitOut, String>(GitOut {
                code: status.code(),
                stdout: out,
                stderr: String::from_utf8_lossy(&err).trim().to_string(),
            })
        };
        match tokio::time::timeout(self.timeout, fut).await {
            Ok(r) => r,
            Err(_) => Err(format!(
                "git {} timed out",
                args.first().map(|a| a.as_ref()).unwrap_or("")
            )),
        }
    }

    /// Run and require success; returns trimmed stdout.
    pub async fn ok<S: AsRef<str>>(&self, args: &[S]) -> Result<String, String> {
        let o = self.run(args, None).await?;
        if o.ok() {
            Ok(o.text().trim_end().to_string())
        } else {
            Err(git_error(args, &o))
        }
    }

    /// Run with stdin and require success.
    pub async fn ok_with<S: AsRef<str>>(&self, args: &[S], input: &[u8]) -> Result<String, String> {
        let o = self.run(args, Some(input)).await?;
        if o.ok() {
            Ok(o.text().trim_end().to_string())
        } else {
            Err(git_error(args, &o))
        }
    }
}

fn git_error<S: AsRef<str>>(args: &[S], o: &GitOut) -> String {
    let cmd: Vec<&str> = args.iter().map(AsRef::as_ref).take(3).collect();
    let msg = if o.stderr.is_empty() {
        o.text().trim().to_string()
    } else {
        o.stderr.clone()
    };
    format!("git {} failed: {msg}", cmd.join(" "))
}

/// Facts about the git repository around `dir`, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoInfo {
    /// The working tree root (`--show-toplevel`).
    pub top: PathBuf,
    /// This worktree's git dir (`.git`, or `.git/worktrees/<name>`).
    pub git_dir: PathBuf,
    /// The shared git dir (objects, refs).
    pub common_dir: PathBuf,
}

/// `None` when `dir` is not inside a git working tree (or git is missing).
pub async fn repo_info(dir: &Path) -> Option<RepoInfo> {
    let g = Git::new(dir);
    let out = g
        .run(
            &[
                "rev-parse",
                "--path-format=absolute",
                "--show-toplevel",
                "--git-dir",
                "--git-common-dir",
            ],
            None,
        )
        .await
        .ok()?;
    if !out.ok() {
        return None;
    }
    let text = out.text();
    let mut l = text.lines();
    Some(RepoInfo {
        top: PathBuf::from(l.next()?),
        git_dir: PathBuf::from(l.next()?),
        common_dir: PathBuf::from(l.next()?),
    })
}

/// Split NUL-separated output.
pub fn nul_split(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

/// Whether `git` can be run.
pub async fn available() -> bool {
    Git::new(std::env::temp_dir())
        .run(&["--version"], None)
        .await
        .is_ok_and(|o| o.ok())
}
