//! Agents (06 §11, D46, t18): definitions, the built-ins and how a definition narrows a
//! session.
//!
//! - **Definitions:** built-ins < `~/.genie/agents/<name>.md` < `<project>/.genie/agents/<name>.md`
//!   (project only when the workspace is trusted, D43); later wins by name. Claude-compatible
//!   frontmatter: `name, description, tools, disallowedTools, model, effort, permissionMode,
//!   maxSteps, context, skills, mcpServers`; the body replaces the base prompt.
//! - **Built-ins:** `general` (inherits everything, Genie's base prompt), `explore`
//!   (read-only tools + read-only shell, the small model), `plan` (read-only set). Hidden:
//!   `title`, `compaction` (internal one-shot prompts; never selectable).
//! - **Narrowing only:** `permissionMode` can only make the session's mode stricter, the tool
//!   lists only hide tools, read-only shell is enforced by [`AgentAuthorizer`] on top of the
//!   permission rules. Policy and deny rules stay engine-enforced.
//! - **Session start as an agent:** `session.create.agent` stores the name; every turn applies
//!   the definition with [`AgentCatalog::effective`].

use std::path::{Path, PathBuf};
use std::sync::Arc;

use genie_protocol::ask::{Ask, AskAnswer};
use genie_protocol::common::Mode;
use genie_skills::frontmatter;
use genie_tools::ToolRegistry;
use serde_json::{Map, Value};

use crate::config::{ConfigHub, project_root};
use crate::error::BlockSource;
use crate::runtime::context::InstructionSource;
use crate::runtime::ports::{
    AuthzDecision, AuthzRequest, InstructionProvider, ModelResolver, SessionSettings,
    SettingsSource, ToolAuthorizer,
};

const EXPLORE_BODY: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/prompts/agents/explore.md"
));
const PLAN_BODY: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/prompts/agents/plan.md"
));

/// Where a definition came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentSource {
    Builtin,
    User,
    Project,
}

/// `context: full | minimal` (D46): minimal keeps only the environment block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentContext {
    #[default]
    Full,
    Minimal,
}

/// One agent definition.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentDef {
    pub name: String,
    pub description: String,
    /// Allowlist of Genie tool names; `None` = inherit the session's tools.
    pub tools: Option<Vec<String>>,
    pub disallowed_tools: Vec<String>,
    /// Catalog model id; `None` = inherit.
    pub model: Option<String>,
    /// Use the catalog's small model for the inherited model (built-in `explore`).
    pub small_model: bool,
    pub effort: Option<String>,
    /// Can only narrow the session's mode.
    pub permission_mode: Option<Mode>,
    pub max_steps: Option<u32>,
    pub context: AgentContext,
    /// Skills to preload (D37). Recorded; not preloaded yet (README gaps).
    pub skills: Vec<String>,
    /// Subset of the configured MCP servers; `None` = all.
    pub mcp_servers: Option<Vec<String>>,
    /// Replaces the base prompt; `None` keeps Genie's base prompt.
    pub body: Option<String>,
    /// `shell` may only run read-only commands (built-in `explore` / `plan`).
    pub read_only_shell: bool,
    /// Internal agents (`title`, `compaction`): never selectable or listed.
    pub hidden: bool,
    pub source: AgentSource,
    pub path: Option<PathBuf>,
}

impl AgentDef {
    fn builtin(name: &str, description: &str) -> AgentDef {
        AgentDef {
            name: name.into(),
            description: description.into(),
            tools: None,
            disallowed_tools: Vec::new(),
            model: None,
            small_model: false,
            effort: None,
            permission_mode: None,
            max_steps: None,
            context: AgentContext::Full,
            skills: Vec::new(),
            mcp_servers: None,
            body: None,
            read_only_shell: false,
            hidden: false,
            source: AgentSource::Builtin,
            path: None,
        }
    }

    /// Whether `tool` is offered to this agent (before the tool's own availability).
    pub fn allows_tool(&self, tool: &str) -> bool {
        if self.disallowed_tools.iter().any(|p| name_matches(p, tool)) {
            return false;
        }
        if let Some(rest) = tool.strip_prefix("mcp__")
            && let Some(servers) = &self.mcp_servers
        {
            let server = rest.split("__").next().unwrap_or_default();
            return servers.iter().any(|s| s == server);
        }
        match &self.tools {
            None => true,
            Some(list) => list.iter().any(|p| name_matches(p, tool)),
        }
    }

    /// Hide the tools this agent may not use.
    pub fn filter_registry(&self, reg: &mut ToolRegistry) {
        reg.retain(|name| self.allows_tool(name));
    }
}

fn name_matches(pattern: &str, name: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => pattern == name,
    }
}

/// The read-only tool set of the built-in `explore` / `plan` agents (t18).
const READ_ONLY_TOOLS: &[&str] = &[
    "read",
    "grep",
    "glob",
    "list",
    "shell",
    "tool_output_read",
    "skill",
    "skill_read",
];

/// The built-in agents (t18), hidden ones included.
pub fn builtins() -> Vec<AgentDef> {
    let general = AgentDef::builtin(
        "general",
        "General-purpose agent for multi-step research and independent sub-tasks. Inherits your tools and model.",
    );
    let mut explore = AgentDef::builtin(
        "explore",
        "Fast read-only codebase search: finds files, symbols and facts and reports them with paths.",
    );
    explore.tools = Some(READ_ONLY_TOOLS.iter().map(|s| (*s).to_string()).collect());
    explore.small_model = true;
    explore.read_only_shell = true;
    explore.body = Some(EXPLORE_BODY.trim_end().to_string());
    let mut plan = AgentDef::builtin(
        "plan",
        "Researches a task with read-only tools and returns a step-by-step plan.",
    );
    let mut plan_tools: Vec<String> = READ_ONLY_TOOLS.iter().map(|s| (*s).to_string()).collect();
    plan_tools.push("web_fetch".into());
    plan.tools = Some(plan_tools);
    plan.read_only_shell = true;
    plan.body = Some(PLAN_BODY.trim_end().to_string());
    let mut title = AgentDef::builtin("title", "Conversation titles (internal).");
    title.hidden = true;
    let mut compaction = AgentDef::builtin("compaction", "Context compaction (internal).");
    compaction.hidden = true;
    vec![general, explore, plan, title, compaction]
}

/// Claude Code tool names (imported agents) → Genie tool names.
fn genie_tool_name(t: &str) -> String {
    let mapped = match t {
        "Read" => "read",
        "Grep" => "grep",
        "Glob" => "glob",
        "LS" => "list",
        "Bash" => "shell",
        "PowerShell" => "shell",
        "Edit" | "MultiEdit" => "edit",
        "Write" => "write",
        "WebFetch" => "web_fetch",
        "WebSearch" => "web_search",
        "Task" => "task",
        "TodoWrite" => "todo",
        other => return other.to_string(),
    };
    mapped.to_string()
}

fn mode_rank(m: Mode) -> u8 {
    match m {
        Mode::Plan => 0,
        Mode::Default | Mode::Unknown => 1,
        Mode::AcceptEdits => 2,
        Mode::Bypass => 3,
    }
}

/// The stricter of the session's mode and the agent's (D46: narrow only).
pub fn narrow_mode(session: Mode, agent: Option<Mode>) -> Mode {
    match agent {
        Some(a) if mode_rank(a) < mode_rank(session) => a,
        _ => session,
    }
}

fn opt_string(fm: &Map<String, Value>, key: &str) -> Result<Option<String>, String> {
    match fm.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => {
            let s = s.trim();
            Ok((!s.is_empty() && s != "inherit").then(|| s.to_string()))
        }
        Some(other) => Err(format!("`{key}` must be a string, found `{other}`")),
    }
}

/// Parse one agent file (`name` defaults to the file stem).
pub fn parse_agent(
    text: &str,
    stem: &str,
    source: AgentSource,
    path: Option<PathBuf>,
) -> Result<AgentDef, String> {
    let (fm, body) = frontmatter::parse_document(text).map_err(|e| e.to_string())?;
    let name = opt_string(&fm, "name")?.unwrap_or_else(|| stem.to_string());
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
    {
        return Err(format!(
            "agent name `{name}` must be 1–64 characters of a-z, 0-9, `-` or `_`"
        ));
    }
    let description = opt_string(&fm, "description")?.unwrap_or_default();
    let tools = if fm.contains_key("tools") {
        let list = frontmatter::string_list(fm.get("tools"))?;
        Some(dedupe(list.iter().map(|t| genie_tool_name(t)).collect()))
    } else {
        None
    };
    let disallowed_tools = dedupe(
        frontmatter::string_list(fm.get("disallowedTools"))?
            .iter()
            .map(|t| genie_tool_name(t))
            .collect(),
    );
    let permission_mode = match opt_string(&fm, "permissionMode")? {
        None => None,
        Some(m) => match genie_config::ModeSetting::parse(&m) {
            // Bypass can never narrow anything: treated as inherit.
            Some(genie_config::ModeSetting::Bypass) => None,
            Some(ms) => Some(ms.to_mode()),
            None => return Err(format!("unknown permissionMode `{m}`")),
        },
    };
    let max_steps = match fm.get("maxSteps") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            v.as_u64()
                .filter(|n| *n >= 1)
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(|| format!("`maxSteps` must be a positive integer, found `{v}`"))?,
        ),
    };
    let context = match opt_string(&fm, "context")?.as_deref() {
        None | Some("full") => AgentContext::Full,
        Some("minimal") => AgentContext::Minimal,
        Some(o) => return Err(format!("`context` must be full or minimal, found `{o}`")),
    };
    let mcp_servers = if fm.contains_key("mcpServers") {
        Some(frontmatter::string_list(fm.get("mcpServers"))?)
    } else {
        None
    };
    let body = body.trim();
    Ok(AgentDef {
        name,
        description,
        tools,
        disallowed_tools,
        model: opt_string(&fm, "model")?,
        small_model: false,
        effort: opt_string(&fm, "effort")?,
        permission_mode,
        max_steps,
        context,
        skills: frontmatter::string_list(fm.get("skills"))?,
        mcp_servers,
        body: (!body.is_empty()).then(|| body.to_string()),
        read_only_shell: false,
        hidden: false,
        source,
        path,
    })
}

fn dedupe(v: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(v.len());
    for s in v {
        if !s.is_empty() && !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

/// A definition file that could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentProblem {
    pub path: PathBuf,
    pub message: String,
}

/// Agent definitions per workspace (see the module docs). Files are read on each lookup
/// (they are small and few), so edits apply on the next turn.
pub struct AgentCatalog {
    user_dir: Option<PathBuf>,
    hub: Option<Arc<ConfigHub>>,
}

impl AgentCatalog {
    /// `user_dir`: `~/.genie` (definitions in `agents/`). `hub`: workspace trust for
    /// project definitions; without it project definitions are never loaded.
    pub fn new(user_dir: Option<PathBuf>, hub: Option<Arc<ConfigHub>>) -> AgentCatalog {
        AgentCatalog { user_dir, hub }
    }

    /// Built-ins only.
    pub fn builtin_only() -> AgentCatalog {
        AgentCatalog::new(None, None)
    }

    fn project_dir(&self, workspace: Option<&Path>) -> Option<PathBuf> {
        let root = project_root(workspace?);
        let hub = self.hub.as_ref()?;
        hub.trusted(&root)
            .then(|| root.join(".genie").join("agents"))
    }

    /// All definitions (hidden ones included) and the files that failed to parse.
    pub fn scan(&self, workspace: Option<&Path>) -> (Vec<AgentDef>, Vec<AgentProblem>) {
        let mut defs = builtins();
        let mut problems = Vec::new();
        let dirs = [
            (
                self.user_dir.as_ref().map(|d| d.join("agents")),
                AgentSource::User,
            ),
            (self.project_dir(workspace), AgentSource::Project),
        ];
        for (dir, source) in dirs {
            let Some(dir) = dir else { continue };
            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            let mut files: Vec<PathBuf> = rd
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "md") && p.is_file())
                .collect();
            files.sort();
            for path in files {
                let stem = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let parsed = std::fs::read_to_string(&path)
                    .map_err(|e| e.to_string())
                    .and_then(|t| parse_agent(&t, &stem, source, Some(path.clone())));
                match parsed {
                    Ok(d) => {
                        if let Some(b) = defs.iter().find(|x| x.name == d.name && x.hidden) {
                            problems.push(AgentProblem {
                                path: path.clone(),
                                message: format!("`{}` is an internal agent name", b.name),
                            });
                            continue;
                        }
                        defs.retain(|x| x.name != d.name);
                        defs.push(d);
                    }
                    Err(message) => {
                        tracing::warn!(path = %path.display(), %message, "agent definition skipped");
                        problems.push(AgentProblem { path, message });
                    }
                }
            }
        }
        (defs, problems)
    }

    /// Selectable agents (not hidden), sorted by name.
    pub fn list(&self, workspace: Option<&Path>) -> Vec<AgentDef> {
        let mut v: Vec<AgentDef> = self
            .scan(workspace)
            .0
            .into_iter()
            .filter(|d| !d.hidden)
            .collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    /// A selectable agent by name.
    pub fn get(&self, name: &str, workspace: Option<&Path>) -> Option<AgentDef> {
        self.scan(workspace)
            .0
            .into_iter()
            .find(|d| d.name == name && !d.hidden)
    }

    /// The definition a session runs as (`session.create.agent` or a subagent).
    pub fn for_session(&self, s: &SessionSettings) -> Option<AgentDef> {
        let name = s.agent.as_deref().filter(|n| !n.is_empty())?;
        let d = self.get(name, s.workspace.as_deref());
        if d.is_none() {
            tracing::warn!(agent = name, "unknown agent; running as the main agent");
        }
        d
    }

    /// Apply `def` to `s` (model, effort, narrowed mode).
    pub fn apply(def: &AgentDef, s: &mut SessionSettings, models: &dyn ModelResolver) {
        if let Some(m) = &def.model {
            s.model.clone_from(m);
        } else if def.small_model
            && let Ok(small) = models.small_model(&s.model)
            && !small.route.model.catalog_id.is_empty()
        {
            s.model = small.route.model.catalog_id;
        }
        if let Some(e) = &def.effort {
            s.effort = Some(e.clone());
        }
        s.mode = narrow_mode(s.mode, def.permission_mode);
    }

    /// The settings a turn runs with, and the agent definition if any.
    pub fn effective(
        &self,
        mut s: SessionSettings,
        models: &dyn ModelResolver,
    ) -> (SessionSettings, Option<AgentDef>) {
        let def = self.for_session(&s);
        if let Some(d) = &def {
            Self::apply(d, &mut s, models);
        }
        (s, def)
    }
}

/// The settings a turn of `sid` runs with (06 §11). A subagent's settings were derived
/// when it was created (`task.model` may override the agent's model), so only its
/// definition is looked up.
pub(crate) fn turn_settings(
    rt: &crate::runtime::AgentRuntime,
    sid: &str,
    s: SessionSettings,
) -> (SessionSettings, Option<AgentDef>) {
    let Some(cat) = &rt.agents else {
        return (s, None);
    };
    if rt.subagents.as_ref().is_some_and(|h| h.is_child(sid)) {
        let d = cat.for_session(&s);
        return (s, d);
    }
    cat.effective(s, rt.models.as_ref())
}

/// `context: minimal` keeps only the environment and date sources (t18).
pub struct AgentInstructions {
    pub base: Arc<dyn InstructionProvider>,
    pub agents: Arc<AgentCatalog>,
}

fn minimal(agents: &AgentCatalog, s: &SessionSettings) -> bool {
    agents
        .for_session(s)
        .is_some_and(|d| d.context == AgentContext::Minimal)
}

impl InstructionProvider for AgentInstructions {
    fn sources(&self, session_id: &str, s: &SessionSettings) -> Vec<InstructionSource> {
        let mut v = self.base.sources(session_id, s);
        if minimal(&self.agents, s) {
            v.retain(|x| x.key == "env" || x.key == "date");
        }
        v
    }

    fn after_compaction(&self, session_id: &str, s: &SessionSettings) -> Vec<InstructionSource> {
        if minimal(&self.agents, s) {
            return Vec::new();
        }
        self.base.after_compaction(session_id, s)
    }
}

/// Enforces read-only shell for agents that have it (`explore`, `plan`) on top of the
/// permission rules: a non-read-only (or unparseable) command is blocked, everything else
/// goes to the wrapped authorizer (deny rules and policy still apply first).
pub struct AgentAuthorizer {
    pub inner: Arc<dyn ToolAuthorizer>,
    pub agents: Arc<AgentCatalog>,
    pub settings: Arc<dyn SettingsSource>,
}

fn read_only_command(req: &AuthzRequest) -> bool {
    use genie_permissions::{BuiltinReadOnly, ReadOnlyClassifier, ShellDialect};
    let Some(a) = genie_tools::shell::authz_from_request(req.resource.as_deref(), req.preview.as_ref())
    else {
        return false;
    };
    let dialect = match a.dialect {
        genie_tools::shell::Dialect::Posix => ShellDialect::Posix,
        genie_tools::shell::Dialect::PowerShell => ShellDialect::PowerShell,
    };
    match a.parts {
        Some(parts) if !parts.is_empty() => parts
            .iter()
            .all(|p| BuiltinReadOnly.is_read_only(dialect, p)),
        _ => false,
    }
}

impl ToolAuthorizer for AgentAuthorizer {
    fn authorize(&self, req: &AuthzRequest) -> AuthzDecision {
        if req.tool == "shell"
            && let Some(s) = self.settings.settings(&req.session_id)
            && let Some(def) = self.agents.for_session(&s)
            && def.read_only_shell
            && !read_only_command(req)
        {
            let name = def.name;
            return AuthzDecision::Deny {
                rule: format!("agent:{name}"),
                source: BlockSource::Rule,
                message: format!(
                    "The {name} agent may only run read-only shell commands (blocked by agent {name})"
                ),
            };
        }
        self.inner.authorize(req)
    }

    fn asked(&self, req: &AuthzRequest, ask_id: &str, ask: &Ask) {
        self.inner.asked(req, ask_id, ask);
    }

    fn answered(&self, req: &AuthzRequest, ask_id: &str, answer: &AskAnswer) -> Vec<String> {
        self.inner.answered(req, ask_id, answer)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn parses_claude_style_frontmatter() {
        let d = parse_agent(
            "---\nname: reviewer\ndescription: Reviews diffs.\ntools: Read, Grep, Bash\n\
             disallowedTools: [edit]\nmodel: inherit\neffort: high\npermissionMode: acceptEdits\n\
             maxSteps: 40\ncontext: minimal\nmcpServers: [github]\n---\nYou review code.\n",
            "x",
            AgentSource::User,
            None,
        )
        .unwrap();
        assert_eq!(d.name, "reviewer");
        assert_eq!(
            d.tools,
            Some(vec!["read".into(), "grep".into(), "shell".into()])
        );
        assert_eq!(d.disallowed_tools, vec!["edit".to_string()]);
        assert_eq!(d.model, None);
        assert_eq!(d.effort.as_deref(), Some("high"));
        assert_eq!(d.permission_mode, Some(Mode::AcceptEdits));
        assert_eq!(d.max_steps, Some(40));
        assert_eq!(d.context, AgentContext::Minimal);
        assert_eq!(d.body.as_deref(), Some("You review code."));
        assert!(d.allows_tool("read"));
        assert!(!d.allows_tool("edit"));
        assert!(!d.allows_tool("web_fetch"));
        assert!(d.allows_tool("mcp__github__issues"));
        assert!(!d.allows_tool("mcp__slack__post"));
    }

    #[test]
    fn rejects_bad_definitions() {
        assert!(parse_agent("---\nname: Bad Name\n---\n", "x", AgentSource::User, None).is_err());
        assert!(parse_agent("---\nmaxSteps: 0\n---\n", "x", AgentSource::User, None).is_err());
        assert!(
            parse_agent("---\npermissionMode: wild\n---\n", "x", AgentSource::User, None).is_err()
        );
        // Name from the file stem, bypass is "inherit" (it can never narrow).
        let d = parse_agent("---\npermissionMode: bypass\n---\n", "helper", AgentSource::User, None)
            .unwrap();
        assert_eq!(d.name, "helper");
        assert_eq!(d.permission_mode, None);
    }

    #[test]
    fn modes_only_narrow() {
        assert_eq!(narrow_mode(Mode::Bypass, Some(Mode::Plan)), Mode::Plan);
        assert_eq!(narrow_mode(Mode::Default, Some(Mode::AcceptEdits)), Mode::Default);
        assert_eq!(narrow_mode(Mode::AcceptEdits, Some(Mode::Default)), Mode::Default);
        assert_eq!(narrow_mode(Mode::Plan, None), Mode::Plan);
    }

    #[test]
    fn user_files_override_builtins_and_project_needs_trust() {
        let d = tempfile::tempdir().unwrap();
        let user = d.path().join("home/.genie");
        std::fs::create_dir_all(user.join("agents")).unwrap();
        std::fs::write(
            user.join("agents/explore.md"),
            "---\ndescription: mine\n---\nCustom explore.",
        )
        .unwrap();
        std::fs::write(user.join("agents/title.md"), "---\n---\nnope").unwrap();
        std::fs::write(user.join("agents/broken.md"), "---\nmaxSteps: -1\n---\n").unwrap();
        let proj = d.path().join("work/proj");
        std::fs::create_dir_all(proj.join(".git")).unwrap();
        std::fs::create_dir_all(proj.join(".genie/agents")).unwrap();
        std::fs::write(
            proj.join(".genie/agents/repo-bot.md"),
            "---\ndescription: from the repo\n---\nRepo bot.",
        )
        .unwrap();
        let hub = Arc::new(
            ConfigHub::new(
                user.clone(),
                user.join("trust.json"),
                Some(d.path().join("home").display().to_string()),
            )
            .unwrap(),
        );
        let cat = AgentCatalog::new(Some(user), Some(hub.clone()));
        let (defs, problems) = cat.scan(Some(&proj));
        assert_eq!(problems.len(), 2, "{problems:?}");
        let explore = defs.iter().find(|d| d.name == "explore").unwrap();
        assert_eq!(explore.source, AgentSource::User);
        assert_eq!(explore.body.as_deref(), Some("Custom explore."));
        assert!(cat.get("repo-bot", Some(&proj)).is_none(), "untrusted");
        assert!(cat.get("title", None).is_none(), "hidden");
        hub.trust_set(
            &proj.display().to_string(),
            genie_protocol::api::config::TrustDecision::Trust,
        )
        .unwrap();
        assert_eq!(
            cat.get("repo-bot", Some(&proj)).unwrap().source,
            AgentSource::Project
        );
        let names: Vec<String> = cat.list(Some(&proj)).into_iter().map(|d| d.name).collect();
        assert_eq!(names, ["explore", "general", "plan", "repo-bot"]);
    }

    #[test]
    fn builtin_explore_is_read_only() {
        let e = builtins().into_iter().find(|d| d.name == "explore").unwrap();
        assert!(e.allows_tool("grep"));
        assert!(e.allows_tool("shell"));
        assert!(!e.allows_tool("edit"));
        assert!(!e.allows_tool("task"));
        assert!(e.read_only_shell && e.small_model);
        let g = builtins().into_iter().find(|d| d.name == "general").unwrap();
        assert!(g.allows_tool("task") && g.body.is_none());
    }
}
