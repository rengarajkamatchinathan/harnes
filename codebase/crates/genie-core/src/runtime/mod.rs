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
//! | [`ToolAuthorizer`] | [`DefaultAuthorizer`] | `genie-permissions` (06 §5, spec 08) |
//! | [`InstructionProvider`] | [`BasicInstructions`] | + config, memory, skills, MCP |
//! | spill / memory | in-memory spill, none | [`bridge::BlobSpill`], [`bridge::StoreMemory`] |
//!
//! Extension points left typed for other specs: code tools, shell, MCP (`mcp__*`, deferred
//! behind `tool_search`), web, browser, skills, subagents (`finish = stop` with children
//! running), hooks (`turn.end` continue), checkpoints, native compaction.

pub mod bridge;
pub mod context;
pub mod conversation;
pub mod models;
pub mod ports;
pub mod prompts;
mod turn;
pub mod wire;

use std::sync::Arc;

use futures::future::BoxFuture;
use genie_tools::{MemoryPort, MemorySpill, OutputLimits, SpillStore, ToolRegistry};

pub use conversation::{ConversationStore, MemoryConversation, StoreConversation};
pub use ports::{
    AllowAllAuthorizer, AuthzDecision, AuthzRequest, BasicInstructions, DefaultAuthorizer,
    FixedSettings, InstructionProvider, ModelResolver, NoModels, ResolveError, ResolvedModel,
    SessionSettings, SettingsSource, ToolAuthorizer,
};

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

    pub fn build(self) -> AgentRunner {
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
