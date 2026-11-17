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
use genie_protocol::api::mcp::SkillListResult;
use genie_protocol::common::SessionKind;
use genie_protocol::message::InputPart;
use genie_skills::command::{CommandFile, command_dirs};
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
        })
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
    }

    /// Hot reload (timer): rescan catalogs whose fingerprint changed.
    pub fn refresh(&self) {
        for (_, c) in self.all() {
            c.lock().unwrap_or_else(|e| e.into_inner()).refresh();
        }
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
    }

    /// User commands for the session's workspace (06 §9).
    pub fn commands(&self, s: &SessionSettings) -> Vec<CommandFile> {
        let root = Self::root_of(s);
        let trusted = root.as_deref().is_some_and(|r| self.hub.trusted(r));
        load_commands(&command_dirs(&self.home, root.as_deref(), trusted)).0
    }

    /// Resolve `InputPart::Skill` and `/command args` in a user message. Returns the parts
    /// to send to the model.
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
                InputPart::Text { text } if i == 0 && text.starts_with('/') => {
                    let line = &text[1..];
                    let (name, args) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
                    match self.commands(&s).into_iter().find(|c| c.name == name) {
                        Some(cmd) => {
                            let ex = cmd.expand(args);
                            let body = if ex.shell.is_empty() {
                                ex.text.clone()
                            } else {
                                ex.without_shell(
                                    "[shell snippet not run: command snippets need the shell tool]",
                                )
                            };
                            out.push(InputPart::text(body));
                            for f in ex.file_refs {
                                out.push(InputPart::FileRef { path: f });
                            }
                        }
                        None => out.push(p.clone()),
                    }
                }
                _ => out.push(p.clone()),
            }
        }
        out
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
