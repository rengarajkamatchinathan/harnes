//! The `shell` tool's execution: script + trailer, spawn, collect, background, render.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use genie_process::pipe::{
    OutputCaps, OutputCollector, OutputSummary, PipeChild, PipeSpec, RunOutcome, StdinMode,
    spawn_pipe,
};
use genie_protocol::ask::Preview;
use genie_protocol::common::{Mode, SessionKind};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use tokio::time::Instant;

use super::select::{Requested, ShellChoice, ShellKind};
use super::{Inner, JobEntry, JobFinished, JobStarted, NAME, lock, permission_resource};
use crate::output::line_count;
use crate::registry::{parse_input, schema_of};
use crate::tool::{
    Availability, PermissionRequest, SpillStore, Tool, ToolCtx, ToolEnv, ToolError, ToolKind,
    ToolOutput,
};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum ShellParam {
    Bash,
    Powershell,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct Input {
    /// The command to run.
    command: String,
    /// What the command does, in a few words (shown to the user).
    #[serde(default)]
    description: Option<String>,
    /// Milliseconds before a still-running command moves to the background (default
    /// 120000, max 600000).
    #[serde(default)]
    timeout_ms: Option<u64>,
    /// Start in the background right away; you will be notified when it finishes.
    #[serde(default)]
    run_in_background: Option<bool>,
    /// `bash` (Git Bash on Windows) or `powershell`. Default: the session's shell.
    #[serde(default)]
    shell: Option<ShellParam>,
}

pub(super) fn input_schema() -> Value {
    schema_of::<Input>()
}

pub struct ShellTool {
    inner: Arc<Inner>,
}

impl ShellTool {
    pub(super) fn new(inner: Arc<Inner>) -> Self {
        ShellTool { inner }
    }
}

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self, _env: &ToolEnv) -> Cow<'_, str> {
        let shell = match self.inner.choice(Requested::Default) {
            Ok(c) => c.kind.display_name(),
            Err(_) => "the system shell",
        };
        let os = std::env::consts::OS;
        Cow::Owned(format!(
            "Run a command in {shell} on {os}, in the session's working directory. `cd` \
             persists between calls; variables and other shell state don't. Output is \
             stdout and stderr combined; long output is cut to its head and tail with a ref \
             to read the rest. Commands still running after `timeout_ms` (default 2 min, \
             max 10 min) move to the background: you'll be notified when they finish, \
             don't poll. Use `run_in_background` for servers and watchers. Prefer the \
             dedicated read/search/edit tools over shell equivalents."
        ))
    }

    fn input_schema(&self) -> &Value {
        &self.inner.schema
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Mutating
    }

    fn availability(&self, env: &ToolEnv) -> Availability {
        if env.kind != SessionKind::Code {
            return Availability::Unavailable(Cow::Borrowed("shell is a code tool"));
        }
        if env.mode == Mode::Plan {
            return Availability::Unavailable(Cow::Borrowed("plan mode: no shell"));
        }
        Availability::Available
    }

    async fn execute(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let input: Input = parse_input(input, &self.inner.schema)?;
        let command = input.command.trim().to_string();
        if command.is_empty() {
            return Err(ToolError::invalid_input(vec!["`command` is empty".into()]));
        }
        let requested = match input.shell {
            None => Requested::Default,
            Some(ShellParam::Bash) => Requested::Bash,
            Some(ShellParam::Powershell) => Requested::PowerShell,
        };
        let choice = self.inner.choice(requested).map_err(ToolError::failed)?;
        let session = ctx.env.session_id.clone();
        let cwd = self
            .inner
            .cwd_for(&session)
            .ok_or_else(|| ToolError::failed("this session has no working directory"))?;

        ctx.authorize(PermissionRequest {
            resource: Some(permission_resource(choice.kind.dialect(), &command)),
            preview: Some(Preview::Command {
                command: command.clone(),
                cwd: Some(cwd.display().to_string()),
            }),
            save_suggestions: vec![],
        })
        .await?;
        if let Some(d) = &input.description {
            ctx.progress(d.clone(), None);
        }

        let cfg = &self.inner.cfg;
        let n = self.inner.next();
        let stem = self.inner.file_stem(n);
        make_private_dir(&cfg.temp_dir).map_err(|e| {
            ToolError::failed(format!("couldn't create {}: {e}", cfg.temp_dir.display()))
        })?;
        let files = SideFiles {
            out: cfg.temp_dir.join(format!("{stem}.out")),
            cwd: cfg.temp_dir.join(format!("{stem}.cwd")),
        };
        let spec = build_spec(&choice, &command, &cwd, &files.cwd, cfg);
        let mut child =
            spawn_pipe(spec, cfg.spawn_hook.as_deref()).map_err(|e| ToolError::Failed {
                message: e.to_string(),
                may_have_run: false,
            })?;
        let caps = OutputCaps {
            head_bytes: cfg.head_bytes,
            tail_bytes: cfg.tail_bytes,
            file: Some(files.out.clone()),
            file_cap: cfg.file_cap,
        };
        let mut out = match OutputCollector::new(caps).await {
            Ok(o) => o,
            Err(e) => {
                let _ = child.kill_tree();
                return Err(ToolError::Failed {
                    message: format!("couldn't open the output file: {e}"),
                    may_have_run: true,
                });
            }
        };

        let job = JobCtx {
            command: command.clone(),
            cwd: cwd.clone(),
            files,
            spill: ctx.env.services.spill.clone(),
            session_id: session.clone(),
            turn_id: ctx.turn_id.clone(),
            call_id: ctx.call_id.clone(),
        };
        if input.run_in_background == Some(true) {
            return self.background(child, out, job, None);
        }

        let timeout = input
            .timeout_ms
            .map_or(cfg.default_timeout, Duration::from_millis)
            .clamp(Duration::from_millis(100), cfg.max_timeout);
        let outcome = child
            .collect(&mut out, Some(Instant::now() + timeout), &ctx.cancel)
            .await;
        match outcome {
            Err(e) => {
                let _ = child.kill_tree();
                job.files.remove().await;
                Err(ToolError::Failed {
                    message: format!("lost the command's output: {e}"),
                    may_have_run: true,
                })
            }
            Ok(RunOutcome::Cancelled) => {
                job.files.remove().await;
                Err(ToolError::Cancelled)
            }
            Ok(RunOutcome::TimedOut) => {
                if cfg.jobs.is_some() {
                    self.background(child, out, job, Some(timeout))
                } else {
                    let _ = child.kill_tree();
                    let _ = child.wait().await;
                    job.files.remove().await;
                    Err(ToolError::Timeout {
                        after_s: timeout.as_secs(),
                    })
                }
            }
            Ok(RunOutcome::Exited(status)) => {
                let new_cwd = read_cwd(&job.files.cwd).await;
                let (mut text, _ref) = render(&out.summary(), &job, cfg).await;
                job.files.remove().await;
                if text.trim().is_empty() {
                    text = "(no output)".into();
                }
                if !status.success() {
                    match status.code() {
                        Some(c) => text.push_str(&format!("\n[exit code {c}]")),
                        None => text.push_str("\n[terminated by a signal]"),
                    }
                }
                if let Some(nc) = new_cwd
                    && nc != cwd
                    && nc.is_dir()
                {
                    text.push_str(&format!("\n[cwd is now {}]", nc.display()));
                    self.inner.set_cwd(&session, nc);
                }
                Ok(ToolOutput::Text(text))
            }
        }
    }
}

struct SideFiles {
    out: PathBuf,
    cwd: PathBuf,
}

impl SideFiles {
    async fn remove(&self) {
        let _ = tokio::fs::remove_file(&self.out).await;
        let _ = tokio::fs::remove_file(&self.cwd).await;
    }
}

struct JobCtx {
    command: String,
    cwd: PathBuf,
    files: SideFiles,
    spill: Option<Arc<dyn SpillStore>>,
    session_id: String,
    turn_id: String,
    call_id: String,
}

impl ShellTool {
    /// Hand the running child to a background task (06 §13.1).
    fn background(
        &self,
        mut child: PipeChild,
        mut out: OutputCollector,
        job: JobCtx,
        after: Option<Duration>,
    ) -> Result<ToolOutput, ToolError> {
        let inner = self.inner.clone();
        let Some(port) = inner.cfg.jobs.clone() else {
            let _ = child.kill_tree();
            return Err(ToolError::Failed {
                message: "background commands are not available here".into(),
                may_have_run: true,
            });
        };
        let id = inner.job_id(inner.next());
        let cancel = inner.cfg.shutdown.child_token();
        lock(&inner.jobs).insert(
            id.clone(),
            JobEntry {
                session_id: job.session_id.clone(),
                cancel: cancel.clone(),
            },
        );
        port.started(&JobStarted {
            id: id.clone(),
            session_id: job.session_id.clone(),
            turn_id: job.turn_id.clone(),
            call_id: job.call_id.clone(),
            command: job.command.clone(),
            cwd: job.cwd.clone(),
            started: SystemTime::now(),
        });
        let so_far = out.summary();
        let mut msg = match after {
            Some(t) => format!(
                "The command is still running after {}s and moved to the background as job \
                 {id}.",
                t.as_secs()
            ),
            None => format!("Started in the background as job {id}."),
        };
        msg.push_str(" You'll be notified when it finishes; don't poll or sleep waiting for it.");
        if so_far.total_bytes > 0 {
            let tail = last_lines(&so_far.tail, 20);
            let shown = if tail.is_empty() {
                last_lines(&so_far.head, 20)
            } else {
                tail
            };
            msg.push_str(&format!("\nOutput so far (last lines):\n{shown}"));
        }
        let task_id = id.clone();
        inner.cfg.tasks.clone().spawn(async move {
            let outcome = child.collect(&mut out, None, &cancel).await;
            let (exit_code, killed) = match outcome {
                Ok(RunOutcome::Exited(s)) => (s.code(), false),
                Ok(RunOutcome::Cancelled | RunOutcome::TimedOut) => (None, true),
                Err(e) => {
                    tracing::warn!(job = %task_id, error = %e, "background command output lost");
                    let _ = child.kill_tree();
                    (None, false)
                }
            };
            let (output, output_ref) = render(&out.summary(), &job, &inner.cfg).await;
            job.files.remove().await;
            lock(&inner.jobs).remove(&task_id);
            port.finished(JobFinished {
                id: task_id,
                session_id: job.session_id,
                command: job.command,
                exit_code,
                killed,
                output,
                output_ref,
                ended: SystemTime::now(),
            });
        });
        Ok(ToolOutput::Text(msg))
    }
}

fn make_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// POSIX single-quoting.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// PowerShell single-quoting.
fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// The script: the user's command plus the cwd trailer.
pub(crate) fn script(kind: ShellKind, command: &str, cwd_file: &Path) -> String {
    match kind {
        ShellKind::Pwsh | ShellKind::WindowsPowerShell => format!(
            "$ProgressPreference = 'SilentlyContinue'\n\
             $__genie_ok = $true\n\
             try {{\n{command}\n$__genie_ok = $?\n}} finally {{\n\
             try {{ [System.IO.File]::WriteAllText({file}, (Get-Location).ProviderPath) }} catch {{}}\n\
             }}\n\
             if ($LASTEXITCODE) {{ exit $LASTEXITCODE }} elseif (-not $__genie_ok) {{ exit 1 }}\n",
            file = ps_quote(&cwd_file.to_string_lossy()),
        ),
        ShellKind::Bash | ShellKind::Zsh | ShellKind::Sh | ShellKind::GitBash => {
            let pwd = if kind == ShellKind::GitBash {
                "pwd -W"
            } else {
                "pwd"
            };
            let file = cwd_file.to_string_lossy().replace('\\', "/");
            format!(
                "__genie_cwd={}\ntrap '{pwd} > \"$__genie_cwd\" 2>/dev/null' EXIT\n{command}\n",
                sh_quote(&file)
            )
        }
    }
}

fn build_spec(
    choice: &ShellChoice,
    command: &str,
    cwd: &Path,
    cwd_file: &Path,
    cfg: &super::ShellConfig,
) -> PipeSpec {
    let s = script(choice.kind, command, cwd_file);
    let args: Vec<String> = match choice.kind {
        ShellKind::Zsh => vec!["+o".into(), "nomatch".into(), "-c".into(), s],
        ShellKind::Bash | ShellKind::Sh | ShellKind::GitBash => vec!["-c".into(), s],
        ShellKind::Pwsh | ShellKind::WindowsPowerShell => {
            let mut a: Vec<String> = vec![
                "-NoLogo".into(),
                "-NoProfile".into(),
                "-NonInteractive".into(),
            ];
            if cfg!(windows) {
                a.extend(["-ExecutionPolicy".into(), "Bypass".into()]);
            }
            a.extend(["-EncodedCommand".into(), encode_ps(&s)]);
            a
        }
    };
    PipeSpec::new(choice.program.clone())
        .args(args)
        .cwd(cwd)
        .env(cfg.env.clone())
        .stdin(StdinMode::Null)
}

/// `-EncodedCommand`: base64 of UTF-16LE (no quoting issues on any OS).
fn encode_ps(script: &str) -> String {
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64(&bytes)
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(char::from(T[(n >> 18) as usize & 63]));
        out.push(char::from(T[(n >> 12) as usize & 63]));
        out.push(if chunk.len() > 1 {
            char::from(T[(n >> 6) as usize & 63])
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            char::from(T[n as usize & 63])
        } else {
            '='
        });
    }
    out
}

async fn read_cwd(file: &Path) -> Option<PathBuf> {
    let s = tokio::fs::read_to_string(file).await.ok()?;
    let s = s.trim_end_matches(['\n', '\r']);
    if s.is_empty() {
        return None;
    }
    Some(PathBuf::from(s))
}

fn last_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.trim_end_matches('\n').lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// Head + tail for the model; the full output is spilled when it doesn't fit.
async fn render(
    summary: &OutputSummary,
    job: &JobCtx,
    cfg: &super::ShellConfig,
) -> (String, Option<String>) {
    let full = if summary.omitted_bytes == 0 {
        format!("{}{}", summary.head, summary.tail)
    } else {
        match tokio::fs::read(&job.files.out).await {
            Ok(b) => String::from_utf8_lossy(&b).into_owned(),
            Err(_) => return (summary.text(), None),
        }
    };
    let total_lines = line_count(&full);
    let fits = summary.omitted_bytes == 0
        && full.len() <= cfg.head_bytes + cfg.tail_bytes
        && total_lines <= cfg.head_lines + cfg.tail_lines;
    if fits {
        return (full, None);
    }
    let lines: Vec<&str> = full.split_inclusive('\n').collect();
    // Head: up to head_lines / head_bytes.
    let mut head_n = 0;
    let mut head_b = 0;
    for l in &lines {
        if head_n >= cfg.head_lines || head_b + l.len() > cfg.head_bytes {
            break;
        }
        head_n += 1;
        head_b += l.len();
    }
    let mut tail_n = 0;
    let mut tail_b = 0;
    for l in lines[head_n..].iter().rev() {
        if tail_n >= cfg.tail_lines || tail_b + l.len() > cfg.tail_bytes {
            break;
        }
        tail_n += 1;
        tail_b += l.len();
    }
    let head: String = lines[..head_n].concat();
    let tail: String = lines[lines.len() - tail_n..].concat();
    let first_omitted = head_n + 1;
    let last_omitted = lines.len() - tail_n;
    let spill_ref = job
        .spill
        .as_ref()
        .and_then(|s| s.put(&job.session_id, &full).ok());
    let capped = if summary.file_truncated {
        " (the output file was capped)"
    } else {
        ""
    };
    let marker = match &spill_ref {
        Some(r) => format!(
            "[… lines {first_omitted}–{last_omitted} of {total_lines} omitted; full output: \
             {r}{capped}]"
        ),
        None => {
            format!("[… lines {first_omitted}–{last_omitted} of {total_lines} omitted{capped}]")
        }
    };
    let mut text = head.trim_end_matches('\n').to_string();
    if head_n == 0 {
        // A single huge line: show its start.
        let mut cut = cfg.head_bytes.min(full.len());
        while !full.is_char_boundary(cut) {
            cut -= 1;
        }
        text = full[..cut].to_string();
    }
    text.push_str(&format!("\n{marker}\n"));
    text.push_str(tail.trim_end_matches('\n'));
    (text, spill_ref)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc4648() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        // `ls` as UTF-16LE.
        assert_eq!(encode_ps("ls"), "bABzAA==");
    }

    #[test]
    fn scripts_have_a_cwd_trailer() {
        let s = script(ShellKind::Zsh, "cd /tmp", Path::new("/t/it's.cwd"));
        assert!(s.contains("__genie_cwd='/t/it'\\''s.cwd'"), "{s}");
        assert!(s.contains("trap 'pwd > \"$__genie_cwd\" 2>/dev/null' EXIT"));
        assert!(s.ends_with("cd /tmp\n"));
        let s = script(ShellKind::GitBash, "ls", Path::new(r"C:\t\a.cwd"));
        assert!(s.contains("'C:/t/a.cwd'") && s.contains("pwd -W"), "{s}");
        let s = script(ShellKind::Pwsh, "Get-Item x", Path::new(r"C:\t\o'k.cwd"));
        assert!(s.contains(r"WriteAllText('C:\t\o''k.cwd'"), "{s}");
        assert!(s.contains("try {\nGet-Item x\n"));
    }
}
