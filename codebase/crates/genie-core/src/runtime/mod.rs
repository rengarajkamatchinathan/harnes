//! The agent runtime (spec/06-agent-runtime.md §1–§4): the turn loop, context assembly
//! and prompt content, compaction, and the tool framework wiring.
//!
//! [`AgentRunner`] is the [`TurnRunner`] the engine runs per turn. It is built with
//! [`RuntimeBuilder`] from ports (01 §3 explicit wiring):
//!
//! | Port | Default | Real implementation |
//! |---|---|---|
//! | [`SettingsSource`] | — (required) | daemon session registry |
//! | [`ModelResolver`] | — (required) | [`models::CatalogResolver`] |
//! | [`ConversationStore`] | [`MemoryConversation`] | [`StoreConversation`] |
//! | [`ToolAuthorizer`] | [`DefaultAuthorizer`] | [`permissions::PermissionAuthorizer`] (06 §5) |
//! | [`InstructionProvider`] | [`BasicInstructions`] | + config, memory, skills, MCP |
//! | spill / memory | in-memory spill, none | [`bridge::BlobSpill`], [`bridge::StoreMemory`] |
//! | [`ToolProvider`]s | none | [`mcp::McpTools`] (06 §7) |
//!
//! Extra built-in tools (shell, web, browser …) are registered with
//! [`RuntimeBuilder::tools`] / [`RuntimeBuilder::chat_tool`] / [`RuntimeBuilder::code_tool`].
//!
//! Extension points left typed for other specs: code tools, shell, `tool_search` deferral,
//! web, browser, skills, subagents (`finish = stop` with children running), hooks
//! (`turn.end` continue), checkpoints, native compaction.

pub mod active;
pub mod bridge;
pub mod browser;
pub mod checkpoint;
pub mod code;
pub mod context;
pub mod conversation;
pub mod host;
pub mod mcp;
pub mod models;
pub mod permissions;
pub mod ports;
pub mod prompts;
pub mod skills;
mod turn;
pub mod wire;

use std::sync::Arc;

use futures::future::BoxFuture;
use genie_protocol::api::session::{ToolInfo, ToolSource};
use genie_tools::{
    MemoryPort, MemorySpill, OutputLimits, SpillStore, Tool, ToolKind, ToolRegistry,
};

pub use conversation::{ConversationStore, MemoryConversation, StoreConversation};
pub use ports::{
    AllowAllAuthorizer, AuthzDecision, AuthzRequest, BasicInstructions, DefaultAuthorizer,
    FixedSettings, InstructionProvider, ModelResolver, NoModels, ProvidedTool, ResolveError,
    ResolvedModel, SessionOverlay, SessionSettings, SettingsSource, ToolAuthorizer, ToolProvider,
};

use crate::runtime::active::ActiveTurns;

use crate::error::TurnError;
use crate::session::{TurnCtx, TurnRunner};

/// Tunables (06 §1, §2.4, §3).
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// SA-07: 50 steps per turn in Chat, 200 in Code.
    pub max_steps_chat: u32,
    pub max_steps_code: u32,
    /// `finish = length` → "continue in smaller pieces", at most this many in a row.
    pub max_continue: u32,
    /// `compaction.buffer`: tokens kept free; default max(15 % of window, 16k).
    pub compaction_buffer: Option<u64>,
    /// Verbatim tail kept by compaction (~15k tokens).
    pub compaction_tail_tokens: u64,
    /// Overflow retries shrink the tail to these fractions (85/70/50/35 %).
    pub overflow_tail: [f64; 4],
    /// One-shot title after the first reply (D27).
    pub titles: bool,
    pub output_limits: OutputLimits,
    /// Memory tools on unless the session is incognito (D31).
    pub memory: bool,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        RuntimeConfig {
            max_steps_chat: 50,
            max_steps_code: 200,
            max_continue: 2,
            compaction_buffer: None,
            compaction_tail_tokens: 15_000,
            overflow_tail: [0.85, 0.70, 0.50, 0.35],
            titles: true,
            output_limits: OutputLimits::default(),
            memory: true,
        }
    }
}

pub struct AgentRuntime {
    pub cfg: RuntimeConfig,
    pub settings: Arc<dyn SettingsSource>,
    pub models: Arc<dyn ModelResolver>,
    pub conversation: Arc<dyn ConversationStore>,
    pub authorizer: Arc<dyn ToolAuthorizer>,
    pub instructions: Arc<dyn InstructionProvider>,
    pub chat_tools: ToolRegistry,
    pub code_tools: ToolRegistry,
    pub spill: Option<Arc<dyn SpillStore>>,
    pub memory: Option<Arc<dyn MemoryPort>>,
    /// Dynamic tools asked for at each turn start (MCP).
    pub tool_providers: Vec<Arc<dyn ToolProvider>>,
    /// The running turn per session (MCP elicitation/sampling → asks).
    pub turns: Arc<ActiveTurns>,
    /// Skills and slash commands (06 §9).
    pub skills: Option<Arc<skills::SkillsHub>>,
    /// Per-turn taint cells; the web tools' [`genie_tools::web::TaintPort`].
    pub taints: Arc<host::TurnTaints>,
    /// The shell tool's runtime (Code): cwd per session, background jobs.
    pub shell: Option<Arc<genie_tools::shell::ShellRuntime>>,
    /// Provider-native web search policy (06 §8); `None`: never declared.
    pub web_search: Option<genie_tools::web::SearchPolicy>,
    /// Shadow-git checkpoints around mutating steps (Code, D49).
    pub checkpoints: Option<Arc<checkpoint::Checkpoints>>,
}

impl AgentRuntime {
    /// Resolve skill parts and `/commands` in a user message (06 §9).
    pub fn resolve_input(
        &self,
        session_id: &str,
        parts: &[genie_protocol::message::InputPart],
    ) -> Vec<genie_protocol::message::InputPart> {
        match &self.skills {
            Some(s) => s.resolve_input(session_id, parts),
            None => parts.to_vec(),
        }
    }

    /// The session's registry: the static tools of its kind plus provider tools.
    pub fn registry_for(&self, session_id: &str, settings: &SessionSettings) -> ToolRegistry {
        let mut r = match settings.kind {
            genie_protocol::common::SessionKind::Code => self.code_tools.clone(),
            _ => self.chat_tools.clone(),
        };
        for p in &self.tool_providers {
            for t in p.tools(session_id, settings) {
                r.register(t.tool);
            }
        }
        r
    }

    /// The taint a successful result of `tool` adds to the turn (SA-29): the provider's
    /// answer for its own tools, else every `External` tool (web fetch …) taints.
    pub fn taint_source(&self, tool: &str, kind: ToolKind) -> Option<String> {
        for p in &self.tool_providers {
            if let Some(t) = p.taint(tool) {
                return t;
            }
        }
        (kind == ToolKind::External).then(|| format!("the `{tool}` tool"))
    }

    /// `tools.list`: what the session's model sees, with each tool's source.
    pub fn tool_list(&self, session_id: &str) -> Option<Vec<ToolInfo>> {
        let settings = self.settings.settings(session_id)?;
        let env = turn::tool_env(self, session_id, &settings);
        let mut provided: std::collections::HashMap<String, (ToolSource, Option<String>)> =
            std::collections::HashMap::new();
        for p in &self.tool_providers {
            for t in p.tools(session_id, &settings) {
                provided.insert(t.tool.name().to_string(), (t.source, t.server.clone()));
            }
        }
        let reg = self.registry_for(session_id, &settings);
        Some(
            reg.specs(&env)
                .into_iter()
                .map(|s| {
                    let (source, server) = provided
                        .remove(&s.name)
                        .unwrap_or((ToolSource::Builtin, None));
                    ToolInfo {
                        name: s.name,
                        description: s.description,
                        source,
                        server,
                    }
                })
                .collect(),
        )
    }
}

/// Builds an [`AgentRuntime`].
pub struct RuntimeBuilder {
    rt: AgentRuntime,
}

impl RuntimeBuilder {
    pub fn new(settings: Arc<dyn SettingsSource>, models: Arc<dyn ModelResolver>) -> Self {
        let chat = genie_tools::chat::registry();
        RuntimeBuilder {
            rt: AgentRuntime {
                cfg: RuntimeConfig::default(),
                settings,
                models,
                conversation: Arc::new(MemoryConversation::new()),
                authorizer: Arc::new(DefaultAuthorizer),
                instructions: Arc::new(BasicInstructions::default()),
                code_tools: chat.clone(),
                chat_tools: chat,
                spill: Some(Arc::new(MemorySpill::new())),
                memory: None,
                tool_providers: Vec::new(),
                turns: Arc::new(ActiveTurns::new()),
                skills: None,
                taints: Arc::new(host::TurnTaints::default()),
                shell: None,
                web_search: None,
                checkpoints: None,
            },
        }
    }

    pub fn config(mut self, cfg: RuntimeConfig) -> Self {
        self.rt.cfg = cfg;
        self
    }

    pub fn conversation(mut self, c: Arc<dyn ConversationStore>) -> Self {
        self.rt.conversation = c;
        self
    }

    pub fn authorizer(mut self, a: Arc<dyn ToolAuthorizer>) -> Self {
        self.rt.authorizer = a;
        self
    }

    pub fn instructions(mut self, i: Arc<dyn InstructionProvider>) -> Self {
        self.rt.instructions = i;
        self
    }

    pub fn chat_tools(mut self, r: ToolRegistry) -> Self {
        self.rt.chat_tools = r;
        self
    }

    pub fn code_tools(mut self, r: ToolRegistry) -> Self {
        self.rt.code_tools = r;
        self
    }

    /// Register extra built-in tools for both Chat and Code (each tool's
    /// `availability` decides where it shows).
    pub fn tools(mut self, tools: impl IntoIterator<Item = Arc<dyn Tool>>) -> Self {
        for t in tools {
            self.rt.chat_tools.register(t.clone());
            self.rt.code_tools.register(t);
        }
        self
    }

    pub fn chat_tool(mut self, t: Arc<dyn Tool>) -> Self {
        self.rt.chat_tools.register(t);
        self
    }

    pub fn code_tool(mut self, t: Arc<dyn Tool>) -> Self {
        self.rt.code_tools.register(t);
        self
    }

    /// Add a dynamic tool source (MCP).
    pub fn tool_provider(mut self, p: Arc<dyn ToolProvider>) -> Self {
        self.rt.tool_providers.push(p);
        self
    }

    /// Skills (06 §9): `<available_skills>` in the baseline, the `skill` / `skill_read`
    /// tools for Chat and Code, skill parts and user commands in prompts.
    pub fn skills(mut self, hub: Arc<skills::SkillsHub>) -> Self {
        self.rt.skills = Some(hub);
        self
    }

    /// The per-turn taint cells (for ports built before the runtime, e.g. the browser host).
    pub fn taints(&self) -> Arc<host::TurnTaints> {
        self.rt.taints.clone()
    }

    /// The `shell` tool (Code registry only, 06 §13).
    pub fn shell(mut self, shell: Arc<genie_tools::shell::ShellRuntime>) -> Self {
        for t in shell.tools() {
            self.rt.code_tools.register(t);
        }
        self.rt.shell = Some(shell);
        self
    }

    /// `web_fetch` (and a local `web_search` when `cfg.search` is set) for Chat and Code
    /// (06 §8). Results taint the turn through [`AgentRuntime::taints`].
    pub fn web(self, mut cfg: genie_tools::web::WebConfig) -> Self {
        if cfg.taint.is_none() {
            cfg.taint = Some(self.rt.taints.clone());
        }
        self.tools(genie_tools::web::tools(cfg))
    }

    /// The 18 `browser_*` tools for Chat and Code (06 §10), hidden in headless sessions
    /// unless enabled.
    pub fn browser(self, b: &Arc<genie_browser::Browser>) -> Self {
        let settings = self.rt.settings.clone();
        let tools: Vec<Arc<dyn Tool>> = genie_browser::tools(b)
            .into_iter()
            .map(|t| {
                Arc::new(browser::HeadlessGated {
                    inner: t,
                    settings: settings.clone(),
                }) as Arc<dyn Tool>
            })
            .collect();
        self.tools(tools)
    }

    /// Declare provider-native web search when the model supports it (06 §8).
    pub fn web_search(mut self, policy: Option<genie_tools::web::SearchPolicy>) -> Self {
        self.rt.web_search = policy;
        self
    }

    /// Share the active-turn index (the MCP handler resolves asks through it).
    pub fn turns(mut self, t: Arc<ActiveTurns>) -> Self {
        self.rt.turns = t;
        self
    }

    pub fn spill(mut self, s: Option<Arc<dyn SpillStore>>) -> Self {
        self.rt.spill = s;
        self
    }

    pub fn memory(mut self, m: Option<Arc<dyn MemoryPort>>) -> Self {
        self.rt.memory = m;
        self
    }

    /// Wire the store: messages, memory and blob spill (spec 04).
    pub fn store(self, store: Arc<genie_store::Store>) -> Self {
        self.conversation(Arc::new(StoreConversation::new(store.clone())))
            .memory(Some(Arc::new(bridge::StoreMemory::new(store.clone()))))
            .spill(Some(Arc::new(bridge::BlobSpill::new(store))))
    }

    pub fn build(mut self) -> AgentRunner {
        if let Some(hub) = self.rt.skills.clone() {
            self.rt.instructions = Arc::new(skills::SkillsInstructions {
                base: self.rt.instructions.clone(),
                skills: hub.clone(),
            });
            let tools: [Arc<dyn Tool>; 2] = [
                Arc::new(skills::SkillTool::new(hub.clone())),
                Arc::new(skills::SkillReadTool::new(hub)),
            ];
            for t in tools {
                self.rt.chat_tools.register(t.clone());
                self.rt.code_tools.register(t);
            }
        }
        AgentRunner(Arc::new(self.rt))
    }
}

/// The real [`TurnRunner`] (06 §1).
#[derive(Clone)]
pub struct AgentRunner(Arc<AgentRuntime>);

impl AgentRunner {
    pub fn runtime(&self) -> &Arc<AgentRuntime> {
        &self.0
    }

    pub fn conversation(&self) -> Arc<dyn ConversationStore> {
        self.0.conversation.clone()
    }
}

impl TurnRunner for AgentRunner {
    fn run(&self, turn: TurnCtx) -> BoxFuture<'static, Result<(), TurnError>> {
        let rt = self.0.clone();
        Box::pin(async move { turn::run(rt, Arc::new(turn)).await })
    }
}
