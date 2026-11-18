//! Skills and slash commands in the engine (06 §9, D37, D44) on top of `genie-skills`.
//!
//! - One [`SkillCatalog`] per workspace context (none = Chat / user scope), built with the
//!   standard roots (project roots only when the folder is trusted), the policy keys
//!   `skills.allowed / directoryOnly / disableUserUpload` and a shared
//!   `<genie home>/skills-state.json`. Trust/policy changes reset roots and policy;
//!   [`SkillsHub::refresh`] runs on a timer (fingerprint hot reload).
//! - The `<available_skills>` index joins the baseline ([`SkillsInstructions`]) and turns
//!   on the `feature.skills` prompt fragment (via the `skill` tool being available).
//! - The read-only `skill` / `skill_read` tools ([`SkillTool`], [`SkillReadTool`]) check
//!   the `Skill(<name>)` permission first; loaded skills are re-injected after compaction.
//! - `InputPart::Skill {id}` renders the skill (user invoker) into the user message.
//! - `/name args` user commands expand from `~/.genie/commands` and (trusted)
//!   `.genie/commands`; `@file` refs become `file_ref` parts; `` !`cmd` `` snippets are
//!   replaced by a note (the shell runner is not wired into the engine yet).

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use genie_config::EffectivePolicy;
use genie_protocol::api::mcp::{
    CommandInfo, CommandListResult, CommandRoute, CommandSource, SkillListResult,
};
use genie_protocol::common::SessionKind;
use genie_protocol::message::InputPart;
use genie_skills::command::{
    BUILTINS, BuiltinModes, CommandFile, CommandScope, INIT_PROMPT, builtin, command_dirs,
    is_builtin,
};
use genie_skills::tools::{self as st, SkillInput, SkillReadInput};
use genie_skills::{
    InstalledSkill, Invoker, JsonStateFile, SkillCatalog, SkillPolicy, SkillRoots, load_commands,
};
use genie_tools::{
    Availability, PermissionRequest, Tool, ToolCtx, ToolEnv, ToolError, ToolKind, ToolOutput,
};
use serde_json::Value;

use crate::config::{ConfigHub, project_root};
use crate::runtime::context::{InstructionSource, rank};
use crate::runtime::ports::{InstructionProvider, SessionSettings, SettingsSource};

type Catalog = Arc<Mutex<SkillCatalog>>;

/// See the module docs.
pub struct SkillsHub {
    home: PathBuf,
    hub: Arc<ConfigHub>,
    settings: Arc<dyn SettingsSource>,
    catalogs: Mutex<HashMap<Option<PathBuf>, Catalog>>,
    /// Skills loaded per session (id → rendered block), re-injected after compaction.
    loaded: Mutex<HashMap<String, BTreeMap<String, String>>>,
    /// Bumped whenever the `/` menu may have changed (`commands.changed`, 06 §9.1).
    changed: tokio::sync::watch::Sender<u64>,
    /// Fingerprint of the command folders seen by the last [`SkillsHub::refresh`].
    command_sig: Mutex<u64>,
}

/// Policy keys (08 §5.2) → [`SkillPolicy`].
pub fn skill_policy(p: &EffectivePolicy) -> SkillPolicy {
    let allowed = p.strings("skills.allowed");
    SkillPolicy {
        allowed: (p.get("skills.allowed").is_some()).then_some(allowed),
        directory_only: p.bool("skills.directoryOnly") == Some(true),
        disable_user_upload: p.bool("skills.disableUserUpload") == Some(true),
    }
}

fn kind(k: SessionKind) -> genie_skills::SessionKind {
    match k {
        SessionKind::Code => genie_skills::SessionKind::Code,
        _ => genie_skills::SessionKind::Chat,
    }
}

impl SkillsHub {
    /// `home`: the Genie home (`~/.genie`).
    pub fn new(
        home: PathBuf,
        hub: Arc<ConfigHub>,
        settings: Arc<dyn SettingsSource>,
    ) -> Arc<SkillsHub> {
        Arc::new(SkillsHub {
            home,
            hub,
            settings,
            catalogs: Mutex::new(HashMap::new()),
            loaded: Mutex::new(HashMap::new()),
            changed: tokio::sync::watch::channel(0).0,
            command_sig: Mutex::new(0),
        })
    }

    /// Notified when skills or commands may have changed (enable state, uploads, trust,
    /// policy, files on disk); the daemon turns this into `commands.changed`.
    pub fn subscribe_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.changed.subscribe()
    }

    fn bump(&self) {
        self.changed.send_modify(|n| *n = n.wrapping_add(1));
    }

    /// Fingerprint of every command folder of every known root (paths, sizes, mtimes).
    fn commands_fingerprint(&self) -> u64 {
        use std::hash::Hasher;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        let mut roots: Vec<Option<PathBuf>> = self
            .catalogs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect();
        roots.sort();
        roots.dedup();
        if !roots.contains(&None) {
            roots.insert(0, None);
        }
        for root in roots {
            let trusted = root.as_deref().is_some_and(|r| self.hub.trusted(r));
            for (dir, _) in command_dirs(&self.home, root.as_deref(), trusted) {
                hash_dir(&dir, 0, &mut h);
            }
        }
        h.finish()
    }

    fn roots(&self, root: Option<&Path>) -> SkillRoots {
        let trusted = root.is_some_and(|r| self.hub.trusted(r));
        SkillRoots::standard(&self.home, root, trusted)
    }

    /// The catalog for a workspace root (`None`: user scope only).
    pub fn catalog(&self, root: Option<&Path>) -> Catalog {
        let key = root.map(Path::to_path_buf);
        let mut g = self.catalogs.lock().unwrap_or_else(|e| e.into_inner());
        g.entry(key)
            .or_insert_with(|| {
                Arc::new(Mutex::new(SkillCatalog::new(
                    self.roots(root),
                    skill_policy(&self.hub.policy()),
                    Arc::new(JsonStateFile::new(self.home.join("skills-state.json"))),
                )))
            })
            .clone()
    }

    fn all(&self) -> Vec<(Option<PathBuf>, Catalog)> {
        self.catalogs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Trust or policy changed: project roots appear/disappear, policy is re-applied.
    pub fn on_change(&self) {
        let policy = skill_policy(&self.hub.policy());
        for (root, c) in self.all() {
            let roots = self.roots(root.as_deref());
            let mut c = c.lock().unwrap_or_else(|e| e.into_inner());
            c.set_policy(policy.clone());
            c.set_roots(roots);
        }
        self.bump();
    }

    /// Hot reload (timer): rescan catalogs whose fingerprint changed; notify when skills or
    /// command files changed. Returns whether anything changed.
    pub fn refresh(&self) -> bool {
        let mut any = false;
        for (_, c) in self.all() {
            any |= c.lock().unwrap_or_else(|e| e.into_inner()).refresh();
        }
        let sig = self.commands_fingerprint();
        {
            let mut last = self.command_sig.lock().unwrap_or_else(|e| e.into_inner());
            if *last != sig {
                // The first fingerprint is a baseline, not a change.
                any |= *last != 0;
                *last = sig;
            }
        }
        if any {
            self.bump();
        }
        any
    }

    fn root_of(s: &SessionSettings) -> Option<PathBuf> {
        (s.kind == SessionKind::Code)
            .then(|| s.workspace.as_deref().map(project_root))
            .flatten()
    }

    fn session_catalog(&self, session_id: &str) -> Option<(Catalog, SessionSettings)> {
        let s = self.settings.settings(session_id)?;
        Some((self.catalog(Self::root_of(&s).as_deref()), s))
    }

    /// `<available_skills>` entries for the session.
    pub fn index(&self, s: &SessionSettings) -> Vec<(String, String)> {
        self.catalog(Self::root_of(s).as_deref())
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .index()
    }

    /// Load a skill for a session and remember it for re-injection after compaction.
    pub fn load(&self, session_id: &str, id: &str, invoker: Invoker) -> Result<String, String> {
        let (c, s) = self
            .session_catalog(session_id)
            .ok_or_else(|| "unknown session".to_string())?;
        let text = c
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .load(id, invoker, kind(s.kind))
            .map_err(|e| e.to_string())?
            .render();
        self.loaded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(session_id.to_string())
            .or_default()
            .insert(id.to_string(), text.clone());
        Ok(text)
    }

    /// Skills loaded in this session (rendered), for re-injection.
    pub fn loaded(&self, session_id: &str) -> Vec<(String, String)> {
        self.loaded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session_id)
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default()
    }

    /// `skill.list` (user scope).
    pub fn list(&self) -> SkillListResult {
        self.catalog(None)
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .list()
    }

    /// `skill.set_enabled`: the state file is shared, so every catalog follows.
    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), genie_skills::SkillError> {
        let mut res = Err(genie_skills::SkillError::NotFound(id.to_string()));
        let _ = self.catalog(None);
        for (_, c) in self.all() {
            let mut c = c.lock().unwrap_or_else(|e| e.into_inner());
            if c.get(id).is_some() {
                res = c.set_enabled(id, enabled);
            }
        }
        if res.is_ok() {
            self.bump();
        }
        res
    }

    /// `skill.upload`: install an archive into the user root; every catalog rescans.
    pub fn upload(&self, archive: &[u8]) -> Result<InstalledSkill, genie_skills::SkillError> {
        let r = self
            .catalog(None)
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .upload(archive, false)?;
        for (_, c) in self.all() {
            c.lock().unwrap_or_else(|e| e.into_inner()).rescan();
        }
        self.bump();
        Ok(r)
    }

    /// A private user-scope catalog for `directory.install` (the install awaits a
    /// download, so it can't hold a shared catalog's lock); call [`Self::rescan_all`] after.
    pub fn user_catalog(&self) -> SkillCatalog {
        SkillCatalog::new(
            self.roots(None),
            skill_policy(&self.hub.policy()),
            Arc::new(JsonStateFile::new(self.home.join("skills-state.json"))),
        )
    }

    pub fn rescan_all(&self) {
        for (_, c) in self.all() {
            c.lock().unwrap_or_else(|e| e.into_inner()).rescan();
        }
        self.bump();
    }

    /// User commands for the session's workspace (06 §9).
    pub fn commands(&self, s: &SessionSettings) -> Vec<CommandFile> {
        let root = Self::root_of(s);
        let trusted = root.as_deref().is_some_and(|r| self.hub.trusted(r));
        load_commands(&command_dirs(&self.home, root.as_deref(), trusted)).0
    }

    /// The `/` menu without MCP prompts (06 §9.1, D58): built-ins for `kind`, Markdown
    /// commands (project, then user), then enabled skills. A name that clashes with a
    /// built-in or an earlier row is skipped with a diagnostic. `workspace` matters for
    /// Code only (project commands and skills, trusted folders only).
    pub fn command_list(&self, kind: SessionKind, workspace: Option<&Path>) -> CommandListResult {
        let root = (kind == SessionKind::Code)
            .then(|| workspace.map(project_root))
            .flatten();
        let mut out = CommandListResult::default();
        for b in BUILTINS {
            if b.modes == BuiltinModes::CodeOnly && kind != SessionKind::Code {
                continue;
            }
            out.commands.push(CommandInfo {
                name: b.name.to_string(),
                aliases: b.aliases.iter().map(|a| a.to_string()).collect(),
                description: b.description.to_string(),
                args_hint: b.args_hint.map(str::to_string),
                source: CommandSource::Builtin,
                route: if b.prompt {
                    CommandRoute::Prompt
                } else {
                    CommandRoute::Client
                },
                immediate: b.immediate,
                server: None,
            });
        }
        let trusted = root.as_deref().is_some_and(|r| self.hub.trusted(r));
        let (mut files, diags) = load_commands(&command_dirs(&self.home, root.as_deref(), trusted));
        out.diagnostics.extend(
            diags
                .into_iter()
                .map(|d| format!("{}: {}", d.path.display(), d.message)),
        );
        // Project before user (06 §9.1 lookup order).
        files.sort_by_key(|c| (c.scope != CommandScope::Project, c.name.clone()));
        for c in files {
            out.commands.push(CommandInfo {
                name: c.name,
                aliases: vec![],
                description: c.description,
                args_hint: c.argument_hint,
                source: match c.scope {
                    CommandScope::Project => CommandSource::Project,
                    CommandScope::User => CommandSource::User,
                },
                route: CommandRoute::Prompt,
                immediate: false,
                server: None,
            });
        }
        let skills = self
            .catalog(root.as_deref())
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .user_invocable();
        for (id, description) in skills {
            if is_builtin(&id) || out.commands.iter().any(|c| c.name == id) {
                out.diagnostics.push(format!(
                    "skill {id}: hidden from the / menu, a command already uses that name"
                ));
                continue;
            }
            out.commands.push(CommandInfo {
                name: id,
                aliases: vec![],
                description,
                args_hint: None,
                source: CommandSource::Skill,
                route: CommandRoute::Prompt,
                immediate: false,
                server: None,
            });
        }
        out
    }

    /// A Markdown command named `name` exists for the session.
    pub fn has_command(&self, session_id: &str, name: &str) -> bool {
        self.settings
            .settings(session_id)
            .is_some_and(|s| self.commands(&s).iter().any(|c| c.name == name))
    }

    /// [`Self::command_list`] for an existing session.
    pub fn session_command_list(&self, session_id: &str) -> Option<CommandListResult> {
        let s = self.settings.settings(session_id)?;
        Some(self.command_list(s.kind, s.workspace.as_deref()))
    }

    /// Expand a Markdown command's text: body + `file_ref` parts; `` !`cmd` `` snippets are
    /// replaced by a note until the shell runner is wired in.
    fn push_expanded(cmd: &CommandFile, args: &str, out: &mut Vec<InputPart>) {
        let ex = cmd.expand(args);
        let body = if ex.shell.is_empty() {
            ex.text.clone()
        } else {
            ex.without_shell("[shell snippet not run: command snippets need the shell tool]")
        };
        out.push(InputPart::text(body));
        for f in ex.file_refs {
            out.push(InputPart::FileRef { path: f });
        }
    }

    /// Resolve `InputPart::Skill`, `InputPart::Command` and a leading `/command args` text
    /// in a user message. Returns the parts to send to the model. MCP prompts are expanded
    /// before this (async, [`crate::runtime::AgentRuntime::expand_mcp_commands`]); one
    /// still here is kept as its typed text.
    pub fn resolve_input(&self, session_id: &str, parts: &[InputPart]) -> Vec<InputPart> {
        let Some(s) = self.settings.settings(session_id) else {
            return parts.to_vec();
        };
        let mut out = Vec::new();
        for (i, p) in parts.iter().enumerate() {
            match p {
                InputPart::Skill { id } => match self.load(session_id, id, Invoker::User) {
                    Ok(text) => out.push(InputPart::text(text)),
                    Err(e) => out.push(InputPart::text(format!(
                        "[Skill {id} could not be loaded: {e}]"
                    ))),
                },
                InputPart::Command { name, args } => {
                    if let Some(b) = builtin(name) {
                        if b.name == "init" {
                            let mut text = INIT_PROMPT.to_string();
                            if !args.trim().is_empty() {
                                text.push_str("\n\n");
                                text.push_str(args.trim());
                            }
                            out.push(InputPart::text(text));
                        } else {
                            // `client` built-ins are rejected at `session.prompt`.
                            out.push(InputPart::text(
                                format!("/{name} {args}").trim_end().to_string(),
                            ));
                        }
                    } else if let Some(cmd) =
                        self.commands(&s).into_iter().find(|c| &c.name == name)
                    {
                        Self::push_expanded(&cmd, args, &mut out);
                    } else if !name.contains(':') {
                        match self.load(session_id, name, Invoker::User) {
                            Ok(text) => {
                                out.push(InputPart::text(text));
                                if !args.trim().is_empty() {
                                    out.push(InputPart::text(args.trim().to_string()));
                                }
                            }
                            Err(e) => out.push(InputPart::text(format!(
                                "[/{name} could not be loaded: {e}]{}",
                                if args.trim().is_empty() {
                                    String::new()
                                } else {
                                    format!("\n\n{}", args.trim())
                                }
                            ))),
                        }
                    } else {
                        out.push(InputPart::text(
                            format!("/{name} {args}").trim_end().to_string(),
                        ));
                    }
                }
                InputPart::Text { text } if i == 0 && text.starts_with('/') => {
                    let line = &text[1..];
                    let (name, args) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
                    match self.commands(&s).into_iter().find(|c| c.name == name) {
                        Some(cmd) => Self::push_expanded(&cmd, args, &mut out),
                        None => out.push(p.clone()),
                    }
                }
                _ => out.push(p.clone()),
            }
        }
        out
    }
}

/// Hash `*.md` paths, sizes and mtimes under a command folder (depth-limited like the loader).
fn hash_dir(dir: &Path, depth: usize, h: &mut impl std::hash::Hasher) {
    use std::hash::Hash;
    if depth > 4 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.filter_map(Result::ok).collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let path = e.path();
        let Ok(md) = e.metadata() else { continue };
        if md.is_dir() {
            hash_dir(&path, depth + 1, h);
        } else if path.extension().is_some_and(|x| x == "md") {
            path.hash(h);
            md.len().hash(h);
            md.modified().ok().hash(h);
        }
    }
}

/// Adds `<available_skills>` to a base provider and re-injects loaded skills after
/// compaction.
pub struct SkillsInstructions {
    pub base: Arc<dyn InstructionProvider>,
    pub skills: Arc<SkillsHub>,
}

impl InstructionProvider for SkillsInstructions {
    fn sources(&self, session_id: &str, settings: &SessionSettings) -> Vec<InstructionSource> {
        let mut v = self.base.sources(session_id, settings);
        let idx = self.skills.index(settings);
        if !idx.is_empty() {
            v.push(InstructionSource::skills_index(&idx));
        }
        v
    }

    fn after_compaction(
        &self,
        session_id: &str,
        settings: &SessionSettings,
    ) -> Vec<InstructionSource> {
        let mut v = self.base.after_compaction(session_id, settings);
        for (id, text) in self.skills.loaded(session_id) {
            v.push(InstructionSource::new(
                format!("skill:{id}"),
                rank::SKILLS,
                text,
            ));
        }
        v
    }
}

/// `skill` (06 §4.1, §9): load a skill's instructions.
pub struct SkillTool {
    hub: Arc<SkillsHub>,
    schema: Value,
}

/// `skill_read`: read a file in a skill's folder.
pub struct SkillReadTool {
    hub: Arc<SkillsHub>,
    schema: Value,
}

impl SkillTool {
    pub fn new(hub: Arc<SkillsHub>) -> SkillTool {
        SkillTool {
            hub,
            schema: st::skill_schema(),
        }
    }
}

impl SkillReadTool {
    pub fn new(hub: Arc<SkillsHub>) -> SkillReadTool {
        SkillReadTool {
            hub,
            schema: st::skill_read_schema(),
        }
    }
}

fn available(hub: &SkillsHub, env: &ToolEnv) -> Availability {
    let has = hub
        .settings
        .settings(&env.session_id)
        .is_some_and(|s| !hub.index(&s).is_empty());
    if has {
        Availability::Available
    } else {
        Availability::Unavailable(Cow::Borrowed("no skills"))
    }
}

async fn check(ctx: &ToolCtx, name: &str) -> Result<(), ToolError> {
    // The `skill.use` check: `Skill(<name>)` rules (deny / ask) apply.
    ctx.authorize(PermissionRequest {
        resource: Some(name.to_string()),
        preview: None,
        save_suggestions: Vec::new(),
    })
    .await
}

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &str {
        st::SKILL_TOOL
    }
    fn description(&self, _env: &ToolEnv) -> Cow<'_, str> {
        Cow::Borrowed(st::SKILL_DESCRIPTION)
    }
    fn input_schema(&self) -> &Value {
        &self.schema
    }
    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }
    fn availability(&self, env: &ToolEnv) -> Availability {
        available(&self.hub, env)
    }
    fn authorizes(&self) -> bool {
        true
    }
    async fn execute(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let i: SkillInput = genie_tools::parse_input(input, &self.schema)?;
        check(&ctx, &i.name).await?;
        self.hub
            .load(&ctx.env.session_id, &i.name, Invoker::Model)
            .map(ToolOutput::Text)
            .map_err(ToolError::failed)
    }
}

#[async_trait]
impl Tool for SkillReadTool {
    fn name(&self) -> &str {
        st::SKILL_READ_TOOL
    }
    fn description(&self, _env: &ToolEnv) -> Cow<'_, str> {
        Cow::Borrowed(st::SKILL_READ_DESCRIPTION)
    }
    fn input_schema(&self) -> &Value {
        &self.schema
    }
    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }
    fn availability(&self, env: &ToolEnv) -> Availability {
        available(&self.hub, env)
    }
    fn authorizes(&self) -> bool {
        true
    }
    async fn execute(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let i: SkillReadInput = genie_tools::parse_input(input, &self.schema)?;
        check(&ctx, &i.name).await?;
        let (c, _) = self
            .hub
            .session_catalog(&ctx.env.session_id)
            .ok_or_else(|| ToolError::failed("unknown session"))?;
        let c = c.lock().unwrap_or_else(|e| e.into_inner());
        st::run_skill_read(&c, &i)
            .map(ToolOutput::Text)
            .map_err(|e| ToolError::failed(e.to_string()))
    }
}
