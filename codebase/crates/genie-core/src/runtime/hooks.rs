//! Hooks (06 §12, D48, t20). Code sessions only; Chat never fires them.
//!
//! - **Config:** `hooks: [{id?, event, match: {tool, resource, path, agent}, run: [argv],
//!   timeoutMs, failClosed}]` in user, project and local settings, plus managed hooks from
//!   policy `hooks.managed`. Policy `hooks.enabled: false` turns all hooks off;
//!   `hooks.managedOnly` keeps only the managed ones. **Project hooks** are code from the
//!   repository (08 §2.3): they run only when the workspace is trusted **and** the hook's
//!   definition hash is approved (D43, `trust.pending_definitions` kind `hook`); local
//!   hooks need trust. Unapproved hooks are skipped, never run.
//! - **Run:** argv through `genie-process` pipe mode (own kill-tree, no shell parsing), the
//!   session cwd, a minimal environment plus `GENIE_*` variables (never provider
//!   credentials), the envelope `{genieHook:1, event, session, cwd, agent, mode, data}` on
//!   stdin, one JSON object `{decision?, message?, updatedInput?, addContext?}` on stdout.
//!   Non-zero exit, timeout (kill-tree) or bad JSON = hook error: published, ignored unless
//!   `failClosed` (then the action is denied / blocked).
//! - **Combining:** matching hooks run in order; the most restrictive decision wins
//!   (deny/block > ask > continue > allow); `updatedInput` composes (later on top of
//!   earlier) and the rewritten input goes through the tool's own permission check again.
//!   Deny rules and policy are evaluated by the permission gate and a hook can never turn
//!   a deny into an allow.
//! - **Events:** every run is a durable `hook.ran` and an audit `hook.ran` record.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use genie_audit::{Attrs, AuditEvent, Auditor};
use genie_config::Definition;
use genie_protocol::api::config::DefinitionKind;
use genie_protocol::ask::{Ask, AskKind, Preview};
use genie_protocol::common::{ConfigScope, Mode, SessionKind};
use genie_protocol::event::{ErrorInfo, HookRan, SessionEvent};
use genie_tools::{PermissionRequest, Tool, ToolCtx, ToolError as TError, ToolOutput};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

use crate::config::{ConfigHub, project_root};
use crate::error::BlockSource;
use crate::runtime::bridge::Started;
use crate::runtime::ports::{AuthzDecision, AuthzRequest, SessionSettings};
use crate::session::TurnCtx;

/// Default per-hook timeout (the spec leaves it open; Claude Code also uses 60 s).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
/// `turn.end` may ask to continue at most this often per turn (D48).
pub const MAX_TURN_END_CONTINUES: u32 = 3;
/// Captured stdout/stderr per hook.
const MAX_OUTPUT: usize = 256 * 1024;

/// The hook events (06 §12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HookEvent {
    ToolBefore,
    ToolAfter,
    PromptSubmit,
    TurnEnd,
    SubagentEnd,
    SessionStart,
    SessionEnd,
    CompactBefore,
    CompactAfter,
    PermissionRequest,
    Notification,
}

impl HookEvent {
    pub const ALL: [HookEvent; 11] = [
        HookEvent::ToolBefore,
        HookEvent::ToolAfter,
        HookEvent::PromptSubmit,
        HookEvent::TurnEnd,
        HookEvent::SubagentEnd,
        HookEvent::SessionStart,
        HookEvent::SessionEnd,
        HookEvent::CompactBefore,
        HookEvent::CompactAfter,
        HookEvent::PermissionRequest,
        HookEvent::Notification,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            HookEvent::ToolBefore => "tool.before",
            HookEvent::ToolAfter => "tool.after",
            HookEvent::PromptSubmit => "prompt.submit",
            HookEvent::TurnEnd => "turn.end",
            HookEvent::SubagentEnd => "subagent.end",
            HookEvent::SessionStart => "session.start",
            HookEvent::SessionEnd => "session.end",
            HookEvent::CompactBefore => "compact.before",
            HookEvent::CompactAfter => "compact.after",
            HookEvent::PermissionRequest => "permission.request",
            HookEvent::Notification => "notification",
        }
    }

    pub fn parse(s: &str) -> Option<HookEvent> {
        Self::ALL.into_iter().find(|e| e.as_str() == s)
    }

    /// What a fail-closed error turns into.
    fn closed_decision(self) -> Option<HookDecision> {
        match self {
            HookEvent::ToolBefore | HookEvent::PermissionRequest | HookEvent::ToolAfter => {
                Some(HookDecision::Deny)
            }
            HookEvent::PromptSubmit => Some(HookDecision::Block),
            _ => None,
        }
    }
}

/// Where a hook is configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookScope {
    Managed,
    User,
    Project,
    Local,
}

impl HookScope {
    fn as_str(self) -> &'static str {
        match self {
            HookScope::Managed => "managed",
            HookScope::User => "user",
            HookScope::Project => "project",
            HookScope::Local => "local",
        }
    }
}

/// One string or a list (`match.tool: "shell"` or `["edit", "write"]`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    fn items(&self) -> Vec<&str> {
        match self {
            // `Edit|Write` (imported Claude matchers) is a list too.
            OneOrMany::One(s) => s.split('|').map(str::trim).collect(),
            OneOrMany::Many(v) => v.iter().map(String::as_str).collect(),
        }
    }
}

/// Structured `match` (no regex strings).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookMatch {
    #[serde(default)]
    pub tool: Option<OneOrMany>,
    /// Glob on the call's resource (shell command, URL, path).
    #[serde(default)]
    pub resource: Option<String>,
    /// Glob on the file path the call touches.
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub agent: Option<OneOrMany>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawHook {
    #[serde(default)]
    id: Option<String>,
    event: String,
    #[serde(default, rename = "match")]
    matcher: HookMatch,
    run: Vec<String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    fail_closed: bool,
}

/// One configured hook.
#[derive(Debug, Clone, PartialEq)]
pub struct HookSpec {
    /// `id` from the config, else `<scope>:<event>#<n>`. Also the D43 definition name.
    pub id: String,
    pub event: HookEvent,
    pub matcher: HookMatch,
    pub run: Vec<String>,
    pub timeout: Duration,
    pub fail_closed: bool,
    pub scope: HookScope,
    /// The config entry as written (definition-hash input).
    pub raw: Value,
}

/// Parse a `hooks` array. Bad entries are skipped with a problem message.
pub fn parse_hooks(v: Option<&Value>, scope: HookScope) -> (Vec<HookSpec>, Vec<String>) {
    let mut out = Vec::new();
    let mut problems = Vec::new();
    let Some(v) = v else {
        return (out, problems);
    };
    let Some(items) = v.as_array() else {
        problems.push(format!("{} hooks: expected an array", scope.as_str()));
        return (out, problems);
    };
    for (i, item) in items.iter().enumerate() {
        let raw: RawHook = match serde_json::from_value(item.clone()) {
            Ok(r) => r,
            Err(e) => {
                problems.push(format!("{} hooks[{i}]: {e}", scope.as_str()));
                continue;
            }
        };
        let Some(event) = HookEvent::parse(&raw.event) else {
            problems.push(format!(
                "{} hooks[{i}]: unknown event `{}`",
                scope.as_str(),
                raw.event
            ));
            continue;
        };
        if raw.run.first().is_none_or(|p| p.trim().is_empty()) {
            problems.push(format!("{} hooks[{i}]: `run` needs a program", scope.as_str()));
            continue;
        }
        out.push(HookSpec {
            id: raw
                .id
                .clone()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| format!("{}:{}#{i}", scope.as_str(), event.as_str())),
            event,
            matcher: raw.matcher,
            run: raw.run,
            timeout: raw
                .timeout_ms
                .map_or(DEFAULT_TIMEOUT, |ms| Duration::from_millis(ms.max(1))),
            fail_closed: raw.fail_closed,
            scope,
            raw: item.clone(),
        });
    }
    for p in &problems {
        tracing::warn!(problem = %p, "hook skipped");
    }
    (out, problems)
}

/// Where hooks come from.
pub trait HookSource: Send + Sync {
    /// The hooks that may run for this session, in order (already gated by policy,
    /// trust and approvals).
    fn hooks(&self, settings: &SessionSettings) -> Vec<HookSpec>;
}

/// A fixed list (tests, embedders).
pub struct StaticHooks(pub Vec<HookSpec>);

impl HookSource for StaticHooks {
    fn hooks(&self, _s: &SessionSettings) -> Vec<HookSpec> {
        self.0.clone()
    }
}

/// Settings + policy + trust (see the module docs).
pub struct ConfigHooks {
    pub hub: Arc<ConfigHub>,
}

/// The project-scope hooks of `root` as D43 definitions (`trust.pending_definitions`).
pub fn project_hook_definitions(hub: &ConfigHub, root: &Path) -> Vec<Definition> {
    let v = hub.layer_value(Some(root), ConfigScope::Project, "hooks");
    parse_hooks(v.as_ref(), HookScope::Project)
        .0
        .into_iter()
        .map(|h| Definition {
            kind: DefinitionKind::Hook,
            name: h.id,
            value: h.raw,
        })
        .collect()
}

impl HookSource for ConfigHooks {
    fn hooks(&self, s: &SessionSettings) -> Vec<HookSpec> {
        let policy = self.hub.policy();
        if policy.bool("hooks.enabled") == Some(false) {
            return Vec::new();
        }
        let mut out = parse_hooks(policy.get("hooks.managed"), HookScope::Managed).0;
        if policy.bool("hooks.managedOnly") == Some(true) {
            return out;
        }
        let root = s.workspace.as_deref().map(project_root);
        let layer = |scope| self.hub.layer_value(root.as_deref(), scope, "hooks");
        out.extend(parse_hooks(layer(ConfigScope::User).as_ref(), HookScope::User).0);
        if let Some(r) = &root
            && self.hub.trusted(r)
        {
            for h in parse_hooks(layer(ConfigScope::Project).as_ref(), HookScope::Project).0 {
                let def = Definition {
                    kind: DefinitionKind::Hook,
                    name: h.id.clone(),
                    value: h.raw.clone(),
                };
                if self.hub.is_approved(r, &def) {
                    out.push(h);
                } else {
                    tracing::info!(hook = %h.id, root = %r.display(), "project hook not approved; skipped");
                }
            }
            out.extend(parse_hooks(layer(ConfigScope::Local).as_ref(), HookScope::Local).0);
        }
        out
    }
}

/// A hook's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookDecision {
    Allow,
    Ask,
    Deny,
    Block,
    Continue,
}

impl HookDecision {
    fn rank(self) -> u8 {
        match self {
            HookDecision::Deny | HookDecision::Block => 4,
            HookDecision::Ask => 3,
            HookDecision::Continue => 2,
            HookDecision::Allow => 1,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            HookDecision::Allow => "allow",
            HookDecision::Ask => "ask",
            HookDecision::Deny => "deny",
            HookDecision::Block => "block",
            HookDecision::Continue => "continue",
        }
    }

    fn denies(self) -> bool {
        matches!(self, HookDecision::Deny | HookDecision::Block)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HookReply {
    #[serde(default)]
    decision: Option<HookDecision>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    updated_input: Option<Value>,
    #[serde(default)]
    add_context: Option<String>,
}

/// What the call being hooked is about (for `match`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HookTarget {
    pub tool: Option<String>,
    pub resource: Option<String>,
    pub path: Option<String>,
}

impl HookTarget {
    /// Resource and path from a tool's input: `command`, `url`, `path`, `file_path`.
    pub fn from_tool(tool: &str, input: &Value) -> HookTarget {
        let s = |k: &str| input.get(k).and_then(Value::as_str).map(String::from);
        let path = s("path").or_else(|| s("file_path"));
        HookTarget {
            tool: Some(tool.to_string()),
            resource: s("command").or_else(|| s("url")).or_else(|| path.clone()),
            path,
        }
    }
}

/// Claude tool names in imported matchers → Genie names.
fn tool_alias(t: &str) -> &str {
    match t {
        "Bash" | "PowerShell" => "shell",
        "Read" => "read",
        "Edit" | "MultiEdit" => "edit",
        "Write" => "write",
        "Grep" => "grep",
        "Glob" => "glob",
        "WebFetch" => "web_fetch",
        "WebSearch" => "web_search",
        "Task" => "task",
        other => other,
    }
}

/// `*` matches any run of characters (including `/`), `?` one character.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

impl HookMatch {
    pub fn matches(&self, t: &HookTarget, agent: &str) -> bool {
        if let Some(tools) = &self.tool {
            let Some(name) = &t.tool else { return false };
            if !tools
                .items()
                .iter()
                .any(|p| *p == "*" || glob_match(tool_alias(p), name))
            {
                return false;
            }
        }
        if let Some(g) = &self.resource
            && !t.resource.as_deref().is_some_and(|r| glob_match(g, r))
        {
            return false;
        }
        if let Some(g) = &self.path
            && !t.path.as_deref().is_some_and(|p| glob_match(g, p))
        {
            return false;
        }
        if let Some(a) = &self.agent
            && !a.items().contains(&agent)
        {
            return false;
        }
        true
    }
}

/// The combined result of the hooks of one event.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HookOutcome {
    pub decision: Option<HookDecision>,
    /// The message of the hook whose decision won.
    pub message: Option<String>,
    /// The hook whose decision won.
    pub hook_id: Option<String>,
    /// The composed input (tool.before).
    pub updated_input: Option<Value>,
    pub context: Vec<String>,
    pub ran: Vec<HookRan>,
}

impl HookOutcome {
    pub fn denies(&self) -> bool {
        self.decision.is_some_and(HookDecision::denies)
    }

    /// `addContext` texts as one block for the model.
    pub fn context_text(&self, event: HookEvent) -> Option<String> {
        if self.context.is_empty() {
            return None;
        }
        Some(format!(
            "<hook_context event=\"{}\">\n{}\n</hook_context>",
            event.as_str(),
            self.context.join("\n")
        ))
    }
}

/// Who/where the hooks run for.
#[derive(Debug, Clone)]
pub struct HookCtx {
    pub session_id: String,
    pub cwd: PathBuf,
    pub agent: String,
    pub mode: Mode,
}

impl HookCtx {
    pub fn for_session(session_id: &str, s: &SessionSettings) -> HookCtx {
        HookCtx {
            session_id: session_id.to_string(),
            cwd: s
                .workspace
                .clone()
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_default(),
            agent: s.agent.clone().unwrap_or_else(|| "main".into()),
            mode: s.mode,
        }
    }
}

fn mode_str(m: Mode) -> &'static str {
    match m {
        Mode::AcceptEdits => "accept-edits",
        Mode::Plan => "plan",
        Mode::Bypass => "bypass",
        Mode::Default | Mode::Unknown => "default",
    }
}

/// Shallow merge: object keys of `update` replace those of `base`; anything else replaces.
fn compose(base: &Value, update: Value) -> Value {
    match (base, update) {
        (Value::Object(b), Value::Object(u)) => {
            let mut m: Map<String, Value> = b.clone();
            m.extend(u);
            Value::Object(m)
        }
        (_, u) => u,
    }
}

/// Runs hooks (see the module docs).
pub struct HooksHub {
    source: Arc<dyn HookSource>,
    audit: Option<Arc<Auditor>>,
}

impl HooksHub {
    pub fn new(source: Arc<dyn HookSource>) -> Arc<HooksHub> {
        Arc::new(HooksHub {
            source,
            audit: None,
        })
    }

    pub fn with_audit(source: Arc<dyn HookSource>, audit: Option<Arc<Auditor>>) -> Arc<HooksHub> {
        Arc::new(HooksHub { source, audit })
    }

    /// The hooks for a session; none outside Code (06 §12, t20).
    pub fn hooks_for(&self, s: &SessionSettings) -> Vec<HookSpec> {
        if s.kind != SessionKind::Code {
            return Vec::new();
        }
        self.source.hooks(s)
    }

    /// `session.end` (read-only) in the background: the session is going away, so runs are
    /// only logged and audited.
    pub fn session_end(
        self: &Arc<Self>,
        session_id: &str,
        settings: &SessionSettings,
        scope: &crate::task::TaskScope,
    ) {
        let specs = self.hooks_for(settings);
        if !specs.iter().any(|h| h.event == HookEvent::SessionEnd) {
            return;
        }
        let ctx = HookCtx::for_session(session_id, settings);
        let hub = self.clone();
        let token = scope.token().clone();
        drop(scope.spawn(async move {
            let publish = |_r: HookRan| {};
            hub.run(
                &specs,
                &ctx,
                HookEvent::SessionEnd,
                json!({ "reason": "deleted" }),
                &HookTarget::default(),
                &token,
                &publish,
            )
            .await;
        }));
    }

    /// Run the matching hooks of `event` in order. `input` is the tool input for
    /// `tool.before` (composed with each `updatedInput`); `publish` gets each `hook.ran`.
    #[allow(clippy::too_many_arguments)]
    pub async fn run(
        &self,
        specs: &[HookSpec],
        ctx: &HookCtx,
        event: HookEvent,
        mut data: Value,
        target: &HookTarget,
        cancel: &CancellationToken,
        publish: &(dyn Fn(HookRan) + Send + Sync),
    ) -> HookOutcome {
        let mut out = HookOutcome::default();
        let mut input = data.get("input").cloned();
        for h in specs
            .iter()
            .filter(|h| h.event == event && h.matcher.matches(target, &ctx.agent))
        {
            if cancel.is_cancelled() {
                break;
            }
            if let (Some(i), Some(obj)) = (&input, data.as_object_mut()) {
                obj.insert("input".into(), i.clone());
            }
            let envelope = json!({
                "genieHook": 1,
                "event": event.as_str(),
                "session": ctx.session_id,
                "cwd": ctx.cwd.display().to_string(),
                "agent": ctx.agent,
                "mode": mode_str(ctx.mode),
                "data": data,
            });
            let started = Instant::now();
            let res = run_one(h, &envelope, ctx, target, cancel).await;
            let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            let (reply, error) = match res {
                Ok(r) => (r, None),
                Err(e) => {
                    let mut r = HookReply::default();
                    if h.fail_closed {
                        r.decision = event.closed_decision();
                        r.message = Some(format!("hook {} failed: {e}", h.id));
                    }
                    (r, Some(e))
                }
            };
            if let Some(e) = &error {
                tracing::warn!(hook = %h.id, event = event.as_str(), error = %e, "hook error");
            }
            let ran = HookRan {
                event: event.as_str().into(),
                hook_id: h.id.clone(),
                decision: reply.decision.map(|d| d.as_str().to_string()),
                duration_ms,
                error: error.clone(),
            };
            if let Some(a) = &self.audit {
                let mut attrs = Attrs::new()
                    .with("event", event.as_str())
                    .with("hook_id", h.id.as_str())
                    .with("scope", h.scope.as_str())
                    .with("duration_ms", duration_ms);
                if let Some(d) = reply.decision {
                    attrs = attrs.with("decision", d.as_str());
                }
                if let Some(e) = &error {
                    attrs = attrs.with("error", e.as_str());
                }
                a.record(Some(&ctx.session_id), AuditEvent::HookRan, attrs);
            }
            publish(ran.clone());
            out.ran.push(ran);
            if let Some(d) = reply.decision
                && out.decision.is_none_or(|cur| d.rank() > cur.rank())
            {
                out.decision = Some(d);
                out.message.clone_from(&reply.message);
                out.hook_id = Some(h.id.clone());
            }
            if let Some(u) = reply.updated_input
                && event == HookEvent::ToolBefore
            {
                let next = compose(input.as_ref().unwrap_or(&Value::Null), u);
                input = Some(next.clone());
                out.updated_input = Some(next);
            }
            if let Some(c) = reply.add_context.filter(|c| !c.trim().is_empty()) {
                out.context.push(c);
            }
        }
        out
    }
}

/// `{path}`, `{tool}`, `{session}` placeholders in argv (t20 example: `cargo fmt -- {path}`).
fn expand_arg(a: &str, ctx: &HookCtx, t: &HookTarget) -> String {
    a.replace("{path}", t.path.as_deref().unwrap_or(""))
        .replace("{tool}", t.tool.as_deref().unwrap_or(""))
        .replace("{session}", &ctx.session_id)
}

async fn read_capped<R: tokio::io::AsyncRead + Unpin>(r: Option<R>) -> Vec<u8> {
    let Some(mut r) = r else { return Vec::new() };
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match r.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if out.len() < MAX_OUTPUT {
                    let take = n.min(MAX_OUTPUT - out.len());
                    out.extend_from_slice(&buf[..take]);
                }
            }
        }
    }
    out
}

enum Finished {
    Done(std::io::Result<std::process::ExitStatus>, Vec<u8>, Vec<u8>),
    TimedOut,
    Cancelled,
}

/// Run one hook process.
async fn run_one(
    h: &HookSpec,
    envelope: &Value,
    ctx: &HookCtx,
    target: &HookTarget,
    cancel: &CancellationToken,
) -> Result<HookReply, String> {
    use genie_process::pipe::{Env, PipeSpec, StdinMode, spawn_pipe};
    let argv: Vec<String> = h.run.iter().map(|a| expand_arg(a, ctx, target)).collect();
    let env = Env::minimal()
        .with("GENIE_HOOK", "1")
        .with("GENIE_HOOK_EVENT", envelope["event"].as_str().unwrap_or(""))
        .with("GENIE_HOOK_ID", h.id.as_str())
        .with("GENIE_SESSION_ID", ctx.session_id.as_str())
        .with("GENIE_PROJECT_DIR", ctx.cwd.display().to_string());
    let mut spec = PipeSpec::new(&argv[0])
        .args(&argv[1..])
        .env(env)
        .stdin(StdinMode::Piped);
    if ctx.cwd.is_dir() {
        spec = spec.cwd(&ctx.cwd);
    }
    let mut child = spawn_pipe(spec, None).map_err(|e| e.to_string())?;
    let mut payload = serde_json::to_vec(envelope).map_err(|e| e.to_string())?;
    payload.push(b'\n');
    let stdin = child.take_stdin();
    let stdout = child.take_stdout();
    let stderr = child.take_stderr();
    let finished = {
        let work = async {
            let write = async {
                if let Some(mut s) = stdin {
                    // A hook that never reads stdin is fine (broken pipe ignored).
                    let _ = s.write_all(&payload).await;
                    let _ = s.shutdown().await;
                }
            };
            let ((), out, err) = tokio::join!(write, read_capped(stdout), read_capped(stderr));
            let status = child.wait().await;
            Finished::Done(status, out, err)
        };
        tokio::select! {
            f = work => f,
            () = tokio::time::sleep(h.timeout) => Finished::TimedOut,
            () = cancel.cancelled() => Finished::Cancelled,
        }
    };
    let (status, out, err) = match finished {
        Finished::Done(s, o, e) => (s, o, e),
        Finished::TimedOut => {
            let _ = child.kill_tree();
            let _ = child.wait().await;
            return Err(format!("timed out after {} ms", h.timeout.as_millis()));
        }
        Finished::Cancelled => {
            let _ = child.kill_tree();
            let _ = child.wait().await;
            return Err("cancelled".into());
        }
    };
    let status = status.map_err(|e| e.to_string())?;
    if !status.success() {
        let tail: String = String::from_utf8_lossy(&err)
            .trim()
            .chars()
            .rev()
            .take(500)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let code = status
            .code()
            .map_or_else(|| "a signal".to_string(), |c| format!("code {c}"));
        return Err(if tail.is_empty() {
            format!("exited with {code}")
        } else {
            format!("exited with {code}: {tail}")
        });
    }
    let text = String::from_utf8_lossy(&out);
    let text = text.trim();
    if text.is_empty() {
        return Ok(HookReply::default());
    }
    serde_json::from_str::<HookReply>(text).map_err(|e| format!("invalid JSON on stdout: {e}"))
}

// ------------------------------------------------------------------ turn integration

/// The hooks of one turn (Code sessions with hooks configured).
pub struct TurnHooks {
    hub: Arc<HooksHub>,
    turn: Arc<TurnCtx>,
    specs: Vec<HookSpec>,
    ctx: HookCtx,
}

impl TurnHooks {
    /// `None` outside Code or when no hook is configured.
    pub fn new(
        hub: &Arc<HooksHub>,
        turn: &Arc<TurnCtx>,
        settings: &SessionSettings,
    ) -> Option<Arc<TurnHooks>> {
        let specs = hub.hooks_for(settings);
        if specs.is_empty() {
            return None;
        }
        Some(Arc::new(TurnHooks {
            hub: hub.clone(),
            turn: turn.clone(),
            specs,
            ctx: HookCtx::for_session(&turn.session_id, settings),
        }))
    }

    pub fn has(&self, event: HookEvent) -> bool {
        self.specs.iter().any(|h| h.event == event)
    }

    pub async fn run(&self, event: HookEvent, data: Value, target: &HookTarget) -> HookOutcome {
        if !self.has(event) {
            return HookOutcome::default();
        }
        let turn = self.turn.clone();
        let publish = move |r: HookRan| {
            if let Err(e) = turn.publish(SessionEvent::HookRan(r)) {
                tracing::error!(error = %e, "publish hook.ran failed");
            }
        };
        self.hub
            .run(
                &self.specs,
                &self.ctx,
                event,
                data,
                target,
                self.turn.token(),
                &publish,
            )
            .await
    }

    /// `prompt.submit`: `Err(message)` blocks the prompt; `Ok(context)` adds context.
    pub async fn prompt_submit(&self, prompt: &str) -> Result<Option<String>, String> {
        let o = self
            .run(
                HookEvent::PromptSubmit,
                json!({ "prompt": prompt }),
                &HookTarget::default(),
            )
            .await;
        if o.denies() {
            return Err(o
                .message
                .unwrap_or_else(|| "A hook blocked this prompt.".into()));
        }
        Ok(o.context_text(HookEvent::PromptSubmit))
    }

    /// `session.start` (first turn of a session): extra context.
    pub async fn session_start(&self) -> Option<String> {
        self.run(
            HookEvent::SessionStart,
            json!({ "source": "startup" }),
            &HookTarget::default(),
        )
        .await
        .context_text(HookEvent::SessionStart)
    }

    /// `turn.end`: `Some(instruction)` when a hook asks to continue (bounded by the caller).
    pub async fn turn_end(&self, continues: u32) -> Option<String> {
        let o = self
            .run(
                HookEvent::TurnEnd,
                json!({ "turn_id": self.turn.turn_id, "continues": continues }),
                &HookTarget::default(),
            )
            .await;
        (o.decision == Some(HookDecision::Continue)).then(|| {
            o.message
                .unwrap_or_else(|| "Continue the task.".into())
        })
    }

    /// `compact.before` / `compact.after` (read-only).
    pub async fn compact(&self, before: bool, trigger: &str) {
        let ev = if before {
            HookEvent::CompactBefore
        } else {
            HookEvent::CompactAfter
        };
        self.run(ev, json!({ "trigger": trigger }), &HookTarget::default())
            .await;
    }

    /// Per-call state for `tool.before` / `permission.request`.
    pub fn call(self: &Arc<Self>, tool: &str) -> Arc<CallHooks> {
        Arc::new(CallHooks {
            th: self.clone(),
            tool: tool.to_string(),
            verdict: Mutex::new(None),
        })
    }
}

/// What `tool.before` decided for one call.
#[derive(Debug, Clone)]
struct Verdict {
    decision: HookDecision,
    message: Option<String>,
    hook_id: String,
}

/// Hook state of one tool call, shared by the call wrapper and the permission gate.
pub struct CallHooks {
    th: Arc<TurnHooks>,
    tool: String,
    verdict: Mutex<Option<Verdict>>,
}

impl CallHooks {
    fn verdict(&self) -> Option<Verdict> {
        self.verdict
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Apply the `tool.before` verdict to the rule decision. A deny (rule or policy)
    /// always stands; `ask` turns an allow into an ask; `allow` answers an ask for this
    /// call only.
    pub(crate) fn adjust(&self, d: AuthzDecision, req: &AuthzRequest) -> AuthzDecision {
        let Some(v) = self.verdict() else { return d };
        match (v.decision, d) {
            (_, d @ AuthzDecision::Deny { .. }) => d,
            (HookDecision::Deny | HookDecision::Block, _) => AuthzDecision::Deny {
                rule: format!("hook:{}", v.hook_id),
                source: BlockSource::Rule,
                message: v
                    .message
                    .unwrap_or_else(|| format!("Denied by hook {}", v.hook_id)),
            },
            (HookDecision::Ask, AuthzDecision::Allow) => {
                let mut ask = Ask::new("", req.session_id.clone(), AskKind::Permission, "");
                ask.tool = Some(req.tool.clone());
                ask.resource.clone_from(&req.resource);
                ask.preview = req.preview.clone().or_else(|| {
                    v.message.clone().map(|text| Preview::Text { text })
                });
                ask.save_suggestions.clone_from(&req.save_suggestions);
                AuthzDecision::Ask(Box::new(ask))
            }
            (HookDecision::Allow, AuthzDecision::Ask(_)) => AuthzDecision::Allow,
            (_, d) => d,
        }
    }

    /// Before an ask is shown: `permission.request` hooks may answer it (`Some(Ok)` =
    /// allowed, `Some(Err)` = declined); `notification` hooks are told (read-only).
    pub(crate) async fn before_ask(&self, req: &AuthzRequest) -> Option<Result<(), TError>> {
        let th = &self.th;
        if th.has(HookEvent::Notification) {
            let th2 = th.clone();
            let msg = format!(
                "Genie needs your approval for {}{}",
                req.tool,
                req.resource
                    .as_deref()
                    .map(|r| format!(" ({r})"))
                    .unwrap_or_default()
            );
            // Read-only and fire-and-forget: the ask is shown without waiting for it.
            drop(th.turn.scope().child("hook").spawn(async move {
                th2.run(
                    HookEvent::Notification,
                    json!({ "message": msg, "kind": "permission" }),
                    &HookTarget::default(),
                )
                .await;
            }));
        }
        if !th.has(HookEvent::PermissionRequest) {
            return None;
        }
        let target = HookTarget {
            tool: Some(self.tool.clone()),
            resource: req.resource.clone(),
            path: None,
        };
        let o = th
            .run(
                HookEvent::PermissionRequest,
                json!({ "tool": self.tool, "resource": req.resource, "preview": req.preview }),
                &target,
            )
            .await;
        match o.decision {
            Some(d) if d.denies() => Some(Err(TError::Declined { message: o.message })),
            Some(HookDecision::Allow) => Some(Ok(())),
            _ => None,
        }
    }
}

/// Run one tool call with `tool.before` / `tool.after` around it.
pub(crate) async fn run_tool(
    tool: Arc<dyn Tool>,
    input: Value,
    ctx: ToolCtx,
    hooks: Option<Arc<CallHooks>>,
    started: Arc<Started>,
) -> Result<ToolOutput, TError> {
    let Some(ch) = hooks else {
        return tool.execute(input, ctx).await;
    };
    let name = tool.name().to_string();
    let mut input = input;
    if ch.th.has(HookEvent::ToolBefore) {
        let target = HookTarget::from_tool(&name, &input);
        let o = ch
            .th
            .run(
                HookEvent::ToolBefore,
                json!({ "tool": name, "input": input }),
                &target,
            )
            .await;
        if let Some(u) = o.updated_input.clone() {
            input = u;
        }
        if let Some(d) = o.decision {
            let hook_id = o.hook_id.clone().unwrap_or_default();
            if d.denies() {
                let message = format!(
                    "Denied by hook {hook_id}{}",
                    o.message
                        .as_deref()
                        .map(|m| format!(": {m}"))
                        .unwrap_or_default()
                );
                started.set_blocked(ErrorInfo {
                    code: "hook_denied".into(),
                    message,
                    link: None,
                });
                return Err(TError::Blocked {
                    rule: format!("hook:{hook_id}"),
                    origin: genie_tools::BlockSource::Rule,
                });
            }
            *ch.verdict.lock().unwrap_or_else(|e| e.into_inner()) = Some(Verdict {
                decision: d,
                message: o.message.clone(),
                hook_id,
            });
            if d == HookDecision::Ask && !tool.authorizes() {
                // The tool never asks by itself: ask here (rules and policy still apply).
                let target = HookTarget::from_tool(&name, &input);
                ctx.authorize(PermissionRequest {
                    resource: target.resource,
                    preview: o.message.map(|text| Preview::Text { text }),
                    save_suggestions: Vec::new(),
                })
                .await?;
            }
        }
    }
    let res = tool.execute(input.clone(), ctx).await;
    if !ch.th.has(HookEvent::ToolAfter) || matches!(res, Err(TError::Cancelled)) {
        return res;
    }
    let (output, is_error) = match &res {
        Ok(o) => (o.to_text(), false),
        Err(e) => (e.to_string(), true),
    };
    let o = ch
        .th
        .run(
            HookEvent::ToolAfter,
            json!({ "tool": name, "input": input, "output": output, "is_error": is_error }),
            &HookTarget::from_tool(&name, &input),
        )
        .await;
    if o.denies() {
        return Err(TError::Failed {
            message: format!(
                "A hook marked this call as failed{}",
                o.message
                    .as_deref()
                    .map(|m| format!(": {m}"))
                    .unwrap_or_default()
            ),
            may_have_run: true,
        });
    }
    match (res, o.context_text(HookEvent::ToolAfter)) {
        (Ok(out), Some(ctx_text)) => Ok(ToolOutput::Text(format!(
            "{}\n\n{ctx_text}",
            out.to_text()
        ))),
        (r, _) => r,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn parse_and_ids() {
        let v = json!([
            {"event": "tool.before", "match": {"tool": "shell", "resource": "git push*"},
             "run": ["./check.sh"], "timeoutMs": 10000, "failClosed": true},
            {"id": "fmt", "event": "tool.after", "match": {"tool": ["edit", "write"], "path": "**/*.rs"},
             "run": ["cargo", "fmt", "--", "{path}"]},
            {"event": "nope", "run": ["x"]},
            {"event": "turn.end", "run": []},
            {"event": "turn.end", "run": ["x"], "bogus": 1}
        ]);
        let (h, problems) = parse_hooks(Some(&v), HookScope::Project);
        assert_eq!(h.len(), 2);
        assert_eq!(problems.len(), 3);
        assert_eq!(h[0].id, "project:tool.before#0");
        assert_eq!(h[0].timeout, Duration::from_secs(10));
        assert!(h[0].fail_closed);
        assert_eq!(h[1].id, "fmt");
        assert_eq!(h[1].timeout, DEFAULT_TIMEOUT);
    }

    #[test]
    fn matching() {
        let m: HookMatch = serde_json::from_value(
            json!({"tool": "Bash", "resource": "git push*"}),
        )
        .unwrap();
        let t = HookTarget::from_tool("shell", &json!({"command": "git push origin"}));
        assert!(m.matches(&t, "main"));
        let t2 = HookTarget::from_tool("shell", &json!({"command": "git status"}));
        assert!(!m.matches(&t2, "main"));
        let m: HookMatch =
            serde_json::from_value(json!({"tool": "Edit|Write", "path": "*.rs", "agent": "main"}))
                .unwrap();
        let t = HookTarget::from_tool("write", &json!({"path": "src/a.rs"}));
        assert!(m.matches(&t, "main"));
        assert!(!m.matches(&t, "explore"));
        assert!(HookMatch::default().matches(&HookTarget::default(), "x"));
        assert!(glob_match("**/*.rs", "a/b/c.rs"));
        assert!(!glob_match("*.rs", "a.rsx"));
    }

    #[test]
    fn decisions_rank_and_compose() {
        assert!(HookDecision::Deny.rank() > HookDecision::Ask.rank());
        assert!(HookDecision::Ask.rank() > HookDecision::Allow.rank());
        assert_eq!(
            compose(&json!({"a": 1, "b": 2}), json!({"b": 3})),
            json!({"a": 1, "b": 3})
        );
    }
}
