//! `preview_start`, `preview_stop`, `preview_logs` (ticket 47 phase 2): the agent's side of
//! [`DevServers`]. Code sessions only.
//!
//! `preview_start` runs a command, so it asks like `shell` (a `Bash(<command>)` /
//! `PowerShell(<command>)` resource with a Command preview: the same rules match). Entries
//! it is given inline are saved to `.genie/launch.json` first, so the user can restart them
//! from the Browser tab's **Dev servers** menu. Stopping and reading logs never ask.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use genie_protocol::api::browser::{PreviewServer, PreviewState};
use genie_protocol::ask::Preview;
use genie_protocol::common::SessionKind;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use super::{DevServers, LAUNCH_FILE};
use crate::registry::{parse_input, schema_of};
use crate::shell::{Dialect, permission_resource};
use crate::tool::{
    Availability, PermissionRequest, Tool, ToolCtx, ToolEnv, ToolError, ToolKind, ToolOutput,
};

pub const START: &str = "preview_start";
pub const STOP: &str = "preview_stop";
pub const LOGS: &str = "preview_logs";

/// The three tools over one [`DevServers`].
pub fn tools(servers: &Arc<DevServers>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(PreviewStart {
            servers: servers.clone(),
            schema: schema_of::<StartInput>(),
        }),
        Arc::new(PreviewStop {
            servers: servers.clone(),
            schema: schema_of::<StopInput>(),
        }),
        Arc::new(PreviewLogs {
            servers: servers.clone(),
            schema: schema_of::<LogsInput>(),
        }),
    ]
}

fn code_only(env: &ToolEnv) -> Availability {
    if env.kind == SessionKind::Code {
        Availability::Available
    } else {
        Availability::Unavailable(Cow::Borrowed("dev servers are a Code session feature"))
    }
}

fn describe(s: &PreviewServer) -> String {
    let state = match s.state {
        PreviewState::Starting => "starting",
        PreviewState::Running => "running",
        PreviewState::Exited => "exited",
        PreviewState::Failed => "failed",
        PreviewState::Unknown => "unknown",
    };
    let mut out = format!("`{}`: {state}", s.name);
    if let Some(u) = &s.url {
        out.push_str(&format!(" at {u}"));
    }
    if let Some(c) = &s.command {
        out.push_str(&format!(" ({c})"));
    }
    if let Some(code) = s.exit_code {
        out.push_str(&format!(", exit code {code}"));
    }
    out
}

// ---------------------------------------------------------------- preview_start

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct StartInput {
    /// The server's name. Alone, starts that entry of `.genie/launch.json`. With a command
    /// (or `url`), the entry is saved under this name first. Optional when the file has
    /// exactly one entry.
    #[serde(default)]
    name: Option<String>,
    /// The program that starts the server, e.g. `npm`, `bun`, `pnpm`, `node`, `python3`.
    #[serde(default)]
    runtime_executable: Option<String>,
    /// Its arguments, e.g. `["run", "dev"]`.
    #[serde(default)]
    runtime_args: Option<Vec<String>>,
    /// A script to run instead (`.js`/`.ts` run with node, `.py` with python3).
    #[serde(default)]
    program: Option<String>,
    /// Arguments after `program`.
    #[serde(default)]
    args: Option<Vec<String>>,
    /// The port the server listens on (passed as `PORT`). Omit to detect it.
    #[serde(default)]
    port: Option<u16>,
    /// Pick a free port if `port` is taken or missing (only for servers that honour `PORT`).
    #[serde(default)]
    auto_port: Option<bool>,
    /// Working directory relative to the project folder (e.g. `apps/web`).
    #[serde(default)]
    cwd: Option<String>,
    /// Extra environment variables.
    #[serde(default)]
    env: Option<BTreeMap<String, String>>,
    /// The URL to open. Without a command: attach to a server that is already running.
    #[serde(default)]
    url: Option<String>,
}

impl StartInput {
    fn inline(&self) -> bool {
        self.runtime_executable.is_some() || self.program.is_some() || self.url.is_some()
    }
}

pub struct PreviewStart {
    servers: Arc<DevServers>,
    schema: Value,
}

#[async_trait]
impl Tool for PreviewStart {
    fn name(&self) -> &str {
        START
    }

    fn description(&self, _env: &ToolEnv) -> Cow<'_, str> {
        Cow::Borrowed(
            "Start a dev server (web app, API) in the background and open it in the user's \
             Browser tab. Use this instead of `shell` for anything that keeps running and \
             listens on a port (`npm run dev`, `bun dev`, `vite`, `next dev`, `python -m \
             http.server`, `rails s` …). It returns once the port accepts connections, with \
             the URL; the server keeps running across turns until `preview_stop`.\n\
             \n\
             Servers are entries of `.genie/launch.json` (same format as `.claude/launch.json`). \
             Pass `name` alone to start an existing entry. To add one, pass `name` plus the \
             command (`runtimeExecutable` + `runtimeArgs`, or `program` + `args`) and `port` if \
             you know it, plus `cwd` when the app is in a subfolder; it is saved to the file \
             first. Read `package.json` scripts (or the framework config) to find the command \
             and port. `url` alone attaches to a server that is already running.\n\
             \n\
             After it starts, check the page with the `browser_*` tools and read \
             `preview_logs` when something looks wrong.",
        )
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Mutating
    }

    fn availability(&self, env: &ToolEnv) -> Availability {
        code_only(env)
    }

    async fn execute(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let input: StartInput = parse_input(input, &self.schema)?;
        let sid = ctx.env.session_id.clone();
        let file = self.servers.launch_file(&sid).map_err(ToolError::failed)?;

        let (cfg, save) = if input.inline() {
            let Some(name) = input.name.clone().filter(|n| !n.trim().is_empty()) else {
                return Err(ToolError::invalid_input(vec![
                    "`name` is required when you pass a command or `url`".into(),
                ]));
            };
            // Keep fields of an existing entry that this call doesn't mention.
            let mut cfg = file.get(&name).cloned().unwrap_or_default();
            cfg.name = name;
            if input.runtime_executable.is_some() || input.program.is_some() {
                cfg.runtime_executable = input.runtime_executable.clone();
                cfg.runtime_args = input.runtime_args.clone().unwrap_or_default();
                cfg.program = input.program.clone();
                cfg.args = input.args.clone().unwrap_or_default();
            }
            if input.port.is_some() {
                cfg.port = input.port;
            }
            if input.auto_port.is_some() {
                cfg.auto_port = input.auto_port;
            }
            if input.cwd.is_some() {
                cfg.cwd = input.cwd.clone();
            }
            if let Some(env) = &input.env {
                cfg.env = env.clone();
            }
            if input.url.is_some() {
                cfg.url = input.url.clone();
            }
            (cfg, true)
        } else {
            let name = match (&input.name, file.configurations.as_slice()) {
                (Some(n), _) => n.clone(),
                (None, [only]) => only.name.clone(),
                (None, []) => {
                    return Err(ToolError::failed(format!(
                        "{LAUNCH_FILE} has no dev servers yet. Pass `name` and the command \
                         (`runtimeExecutable` + `runtimeArgs`, or `program`), plus `port` if \
                         known, to add and start one."
                    )));
                }
                (None, many) => {
                    let names: Vec<&str> = many.iter().map(|c| c.name.as_str()).collect();
                    return Err(ToolError::invalid_input(vec![format!(
                        "pass `name`: {LAUNCH_FILE} has {}",
                        names.join(", ")
                    )]));
                }
            };
            let cfg = file.get(&name).cloned().ok_or_else(|| {
                ToolError::failed(format!(
                    "no entry `{name}` in {LAUNCH_FILE}; pass its command to add it"
                ))
            })?;
            (cfg, false)
        };

        if let Some(s) = self.servers.get(&sid, &cfg.name)
            && s.state == PreviewState::Running
            && !save
        {
            return Ok(ToolOutput::text(format!(
                "{} — already running. The user can see it in the Browser tab.",
                describe(&s)
            )));
        }

        if let Some(command) = cfg.command_line() {
            let root = self.servers.root(&sid).map_err(ToolError::failed)?;
            let cwd = match cfg.cwd.as_deref() {
                Some(c) if !c.is_empty() => root.join(c),
                _ => root,
            };
            let dialect = if cfg!(windows) {
                Dialect::PowerShell
            } else {
                Dialect::Posix
            };
            ctx.authorize(PermissionRequest {
                resource: Some(permission_resource(dialect, &command)),
                preview: Some(Preview::Command {
                    command,
                    cwd: Some(cwd.display().to_string()),
                }),
                save_suggestions: vec![],
            })
            .await?;
        }
        if save {
            self.servers
                .save_config(&sid, cfg.clone())
                .map_err(ToolError::failed)?;
        }
        ctx.progress(format!("Starting {}", cfg.name), None);
        let name = cfg.name.clone();
        let s = self
            .servers
            .start_config(&sid, cfg, &ctx.cancel)
            .await
            .map_err(|message| ToolError::Failed {
                message,
                may_have_run: true,
            })?;
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let logs = self.servers.logs(&sid, &name, 15).unwrap_or_default();
        let mut out = describe(&s);
        if let Some(pid) = s.pid {
            out.push_str(&format!(", pid {pid}"));
        }
        out.push('.');
        match s.state {
            PreviewState::Running => out.push_str(
                " It runs in the background until `preview_stop`; the user sees it in the \
                 Browser tab. Check the page with the `browser_*` tools.",
            ),
            _ => out.push_str(&format!(
                " It hasn't opened its port within {}s. It is still running: read \
                 `preview_logs`, or pass `port` if it listens somewhere else.",
                self.servers.ready_timeout.as_secs()
            )),
        }
        if save {
            out.push_str(&format!(" Saved to {LAUNCH_FILE}."));
        }
        if !logs.trim().is_empty() {
            out.push_str(&format!("\n\nOutput so far:\n{logs}"));
        }
        Ok(ToolOutput::text(out))
    }
}

// ---------------------------------------------------------------- preview_stop

#[derive(Debug, Deserialize, JsonSchema)]
struct StopInput {
    /// The server to stop. Omit to stop all of this session's dev servers.
    #[serde(default)]
    name: Option<String>,
}

pub struct PreviewStop {
    servers: Arc<DevServers>,
    schema: Value,
}

#[async_trait]
impl Tool for PreviewStop {
    fn name(&self) -> &str {
        STOP
    }

    fn description(&self, _env: &ToolEnv) -> Cow<'_, str> {
        Cow::Borrowed(
            "Stop a dev server started with `preview_start` (and everything it started). Omit \
             `name` to stop all of them. Restart one after changing its config or when it \
             doesn't pick up a change by itself.",
        )
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Mutating
    }

    fn authorizes(&self) -> bool {
        false
    }

    fn availability(&self, env: &ToolEnv) -> Availability {
        code_only(env)
    }

    async fn execute(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let input: StopInput = parse_input(input, &self.schema)?;
        let sid = &ctx.env.session_id;
        Ok(ToolOutput::text(match input.name {
            Some(n) => {
                if self.servers.stop(sid, &n) {
                    format!("Stopped `{n}`.")
                } else {
                    return Err(ToolError::failed(format!(
                        "no dev server named `{n}` is running"
                    )));
                }
            }
            None => match self.servers.stop_all(sid) {
                0 => "No dev servers were running.".to_string(),
                1 => "Stopped 1 dev server.".to_string(),
                n => format!("Stopped {n} dev servers."),
            },
        }))
    }
}

// ---------------------------------------------------------------- preview_logs

#[derive(Debug, Deserialize, JsonSchema)]
struct LogsInput {
    /// The server. Optional when only one was started.
    #[serde(default)]
    name: Option<String>,
    /// How many of the last lines to show. Default 100, maximum 1000.
    #[serde(default)]
    lines: Option<u32>,
}

pub struct PreviewLogs {
    servers: Arc<DevServers>,
    schema: Value,
}

#[async_trait]
impl Tool for PreviewLogs {
    fn name(&self) -> &str {
        LOGS
    }

    fn description(&self, _env: &ToolEnv) -> Cow<'_, str> {
        Cow::Borrowed(
            "Read a dev server's recent output (stdout and stderr) and its state: compile \
             errors, stack traces, request logs. Use it after `preview_start` when the page is \
             blank or broken, and after edits the server reloads.",
        )
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }

    fn availability(&self, env: &ToolEnv) -> Availability {
        code_only(env)
    }

    async fn execute(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let input: LogsInput = parse_input(input, &self.schema)?;
        let sid = &ctx.env.session_id;
        let list = self.servers.list(sid);
        let name = match (input.name, list.as_slice()) {
            (Some(n), _) => n,
            (None, [only]) => only.name.clone(),
            (None, []) => {
                return Ok(ToolOutput::text(
                    "No dev servers were started in this session (`preview_start`).",
                ));
            }
            (None, many) => {
                let lines: Vec<String> = many.iter().map(describe).collect();
                return Err(ToolError::invalid_input(vec![format!(
                    "pass `name`; this session's servers: {}",
                    lines.join("; ")
                )]));
            }
        };
        let Some(s) = self.servers.get(sid, &name) else {
            return Err(ToolError::failed(format!("no dev server named `{name}`")));
        };
        let n = input.lines.unwrap_or(100).clamp(1, 1000) as usize;
        let text = self.servers.logs(sid, &name, n).unwrap_or_default();
        let mut out = describe(&s);
        if let Some(e) = &s.error {
            out.push_str(&format!("\nError: {e}"));
        }
        out.push_str("\n\n");
        out.push_str(if text.trim().is_empty() {
            "(no output yet)"
        } else {
            &text
        });
        Ok(ToolOutput::text(out))
    }
}
