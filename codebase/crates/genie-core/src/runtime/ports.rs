//! Ports the turn loop is wired with (01 §3: explicit wiring, no DI framework).
//!
//! - [`SettingsSource`]: per-session settings (the daemon's session registry).
//! - [`ModelResolver`]: catalog model id → route + client + prices (spec 03).
//! - [`ToolAuthorizer`]: the permission decision for one tool call. Rule evaluation is
//!   `genie-permissions` (06 §5, spec 08); [`DefaultAuthorizer`] asks for everything
//!   that is not read-only until it lands.
//! - [`InstructionProvider`]: baseline sources (06 §2.2); config, memory, skills and MCP
//!   add theirs when they land.

use std::path::PathBuf;
use std::sync::Arc;

use futures::future::BoxFuture;
use genie_protocol::api::session::ToolSource;
use genie_protocol::ask::{Ask, AskAnswer, AskKind, Preview, SaveSuggestion};
use genie_protocol::common::{Mode, SessionKind};
use genie_providers::executor::LlmClient;
use genie_providers::route::Route;
use genie_tools::{Tool, ToolKind};

use crate::error::BlockSource;

use crate::runtime::context::{EnvInfo, InstructionSource, utc_day};

/// What the turn loop needs to know about a session.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionSettings {
    pub kind: SessionKind,
    /// Catalog model id, e.g. `anthropic/claude-opus-5-5`.
    pub model: String,
    pub effort: Option<String>,
    pub mode: Mode,
    pub agent: Option<String>,
    pub project_id: Option<String>,
    pub incognito: bool,
    pub title: Option<String>,
    /// Code sessions: the workspace root.
    pub workspace: Option<PathBuf>,
    /// Session-only settings (CLI layer: `--allow`/`--deny`, `--settings`, `--add-dir`).
    pub overlay: Option<SessionOverlay>,
    /// Created by a headless client (`genie -p`): browser tools stay off unless enabled.
    pub headless: bool,
    /// The chat's web search toggle is off (06 §8): no native declaration, no local
    /// `web_search` tool.
    pub web_search_off: bool,
    /// The chat's provider code execution toggle is on (D35; off by default).
    pub code_execution: bool,
}

/// The session-only CLI layer (D23 layer 2), already read from `--settings <file>` and
/// merged with `--allow`/`--deny`/`--add-dir` by the daemon.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionOverlay {
    pub allow: Vec<String>,
    pub ask: Vec<String>,
    pub deny: Vec<String>,
    pub add_dirs: Vec<String>,
    /// `permissions.defaultMode` from the settings file, if any.
    pub default_mode: Option<Mode>,
    /// `--enable-browser` (SA-28c).
    pub enable_browser: bool,
}

pub trait SettingsSource: Send + Sync {
    fn settings(&self, session_id: &str) -> Option<SessionSettings>;
    /// A title was generated (D27); the registry stores it.
    fn set_title(&self, _session_id: &str, _title: &str) {}
}

/// Fixed settings for every session (tests, the CLI's single-session mode).
pub struct FixedSettings(pub SessionSettings);

impl SettingsSource for FixedSettings {
    fn settings(&self, _session_id: &str) -> Option<SessionSettings> {
        Some(self.0.clone())
    }
}

/// A model bound to its route (03 §3).
#[derive(Clone)]
pub struct ResolvedModel {
    pub route: Route,
    pub client: Arc<dyn LlmClient + Send + Sync>,
    pub display_name: String,
    pub provider_display_name: String,
    /// USD per token class (catalog, D27). `None`: cost unknown (reported as 0).
    pub pricing: Option<genie_catalog::entry::Pricing>,
    /// `capabilities.native_compaction` (D33). Native compaction is an extension point;
    /// the loop always produces our own summary today.
    pub native_compaction: bool,
}

impl ResolvedModel {
    pub fn key(&self) -> String {
        format!("{}/{}", self.route.provider, self.route.model.wire_id)
    }

    pub fn context_window(&self) -> u64 {
        self.route.model.context_window.unwrap_or(128_000)
    }
}

/// Why a model could not be resolved; becomes `TurnError::Provider { class }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveError {
    /// `no_provider_configured` | `model_not_found` | `invalid_request`.
    pub class: &'static str,
    pub message: String,
    pub fix_link: Option<String>,
}

pub trait ModelResolver: Send + Sync {
    fn resolve(&self, model: &str) -> Result<ResolvedModel, ResolveError>;
    /// The small model used for titles (D27). Defaults to the session model.
    fn small_model(&self, model: &str) -> Result<ResolvedModel, ResolveError> {
        self.resolve(model)
    }
}

/// No provider is set up: every turn fails with `no_provider_configured`.
pub struct NoModels;

impl ModelResolver for NoModels {
    fn resolve(&self, _model: &str) -> Result<ResolvedModel, ResolveError> {
        Err(ResolveError {
            class: "no_provider_configured",
            message: "no model provider is available".into(),
            fix_link: Some("genie://settings/providers".into()),
        })
    }
}

/// One authorisation request (input of [`ToolAuthorizer`]).
#[derive(Debug, Clone, PartialEq)]
pub struct AuthzRequest {
    pub session_id: String,
    pub turn_id: String,
    pub tool: String,
    pub kind: ToolKind,
    pub resource: Option<String>,
    pub preview: Option<Preview>,
    pub save_suggestions: Vec<SaveSuggestion>,
    pub mode: Mode,
    /// Untrusted content seen earlier in this turn (SA-29): "This step follows content
    /// from <source>". Set from tool results (MCP, web fetch).
    pub taint: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AuthzDecision {
    Allow,
    Deny {
        /// The rule text (or the engine limit) that blocked the call.
        rule: String,
        source: BlockSource,
        /// User-facing text, e.g. "Blocked by Acme policy (Bash(rm:*))" (08 §5.3).
        message: String,
    },
    /// Ask the user; the loop sends it with `TurnCtx::ask_with`.
    Ask(Box<Ask>),
}

/// The permission decision for a tool call (06 §5). The daemon uses
/// [`crate::runtime::permissions::PermissionAuthorizer`] (`genie-permissions` rules).
pub trait ToolAuthorizer: Send + Sync {
    fn authorize(&self, req: &AuthzRequest) -> AuthzDecision;

    /// The ask for `req` was published with `ask_id`.
    fn asked(&self, _req: &AuthzRequest, _ask_id: &str, _ask: &Ask) {}

    /// The ask was answered (or cancelled). For an `always`, remember and persist the
    /// rule and return the ids of other pending asks of this session that the new rule
    /// now allows; the caller releases them (02 §7 step 6).
    fn answered(&self, _req: &AuthzRequest, _ask_id: &str, _answer: &AskAnswer) -> Vec<String> {
        Vec::new()
    }
}

/// The rules an `always` answer saves: **every** save suggestion (a compound command needs
/// one rule per sub-command that asked, or the same command asks again next time), in the
/// scope the user picked (default: each suggestion's own scope). Empty when the ask had no
/// suggestion.
pub fn saved_rules(ask: &Ask, answer: &AskAnswer) -> Vec<SaveSuggestion> {
    let picked = answer
        .scope
        .filter(|s| *s != genie_protocol::common::RuleScope::Unknown);
    ask.save_suggestions
        .iter()
        .map(|s| SaveSuggestion {
            rule: s.rule.clone(),
            scope: picked.unwrap_or(s.scope),
        })
        .collect()
}

/// A tool contributed at turn start by a dynamic source (MCP servers, extensions).
#[derive(Clone)]
pub struct ProvidedTool {
    pub tool: Arc<dyn Tool>,
    pub source: ToolSource,
    /// The MCP server, for `source: mcp`.
    pub server: Option<String>,
}

/// Dynamic tools (06 §4, §7): asked for the session's tools at every turn start, so
/// `list_changed` reloads show up on the next turn.
pub trait ToolProvider: Send + Sync {
    fn tools(&self, session_id: &str, settings: &SessionSettings) -> Vec<ProvidedTool>;

    /// Wait for slow sources before the first model call (MCP: 2 s grace).
    fn ready(&self, _settings: &SessionSettings) -> BoxFuture<'static, ()> {
        Box::pin(async {})
    }

    /// Whether a successful result of `tool` taints the turn (SA-29). `None`: not one of
    /// this provider's tools; `Some(None)`: trusted output; `Some(Some(source))`: tainted.
    fn taint(&self, _tool: &str) -> Option<Option<String>> {
        None
    }
}

/// Until rules exist: read-only tools run; everything else asks once per call.
pub struct DefaultAuthorizer;

impl ToolAuthorizer for DefaultAuthorizer {
    fn authorize(&self, req: &AuthzRequest) -> AuthzDecision {
        if req.kind == ToolKind::ReadOnly {
            return AuthzDecision::Allow;
        }
        let mut ask = Ask::new("", req.session_id.clone(), AskKind::Permission, "");
        ask.tool = Some(req.tool.clone());
        ask.resource.clone_from(&req.resource);
        ask.preview.clone_from(&req.preview);
        ask.save_suggestions.clone_from(&req.save_suggestions);
        AuthzDecision::Ask(Box::new(ask))
    }
}

/// Allows every call (tests, `--dangerously-skip-permissions`-style harnesses).
pub struct AllowAllAuthorizer;

impl ToolAuthorizer for AllowAllAuthorizer {
    fn authorize(&self, _req: &AuthzRequest) -> AuthzDecision {
        AuthzDecision::Allow
    }
}

/// Baseline sources for a session (06 §2.2).
pub trait InstructionProvider: Send + Sync {
    fn sources(&self, session_id: &str, settings: &SessionSettings) -> Vec<InstructionSource>;

    /// Extra sources for the baseline rebuilt by compaction (06 §3: loaded skills …).
    fn after_compaction(
        &self,
        _session_id: &str,
        _settings: &SessionSettings,
    ) -> Vec<InstructionSource> {
        Vec::new()
    }
}

/// Environment + date. Org/custom instructions, skills, MCP guidance, memory, project
/// instructions/knowledge and instruction files are added by their subsystems.
pub struct BasicInstructions {
    pub app_version: String,
    /// Fixed day for golden tests; `None` = today (UTC).
    pub day: Option<String>,
}

impl Default for BasicInstructions {
    fn default() -> Self {
        BasicInstructions {
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            day: None,
        }
    }
}

impl InstructionProvider for BasicInstructions {
    fn sources(&self, _session_id: &str, s: &SessionSettings) -> Vec<InstructionSource> {
        let code = s.kind == SessionKind::Code;
        let env = EnvInfo {
            cwd: s.workspace.as_ref().map(|p| p.display().to_string()),
            os: std::env::consts::OS.to_string(),
            shell: None,
            git: code.then(|| {
                s.workspace
                    .as_ref()
                    .is_some_and(|w| w.join(".git").exists())
            }),
            branch: None,
            app_version: self.app_version.clone(),
        };
        let day = self
            .day
            .clone()
            .unwrap_or_else(|| utc_day(crate::bus::now_ms()));
        vec![InstructionSource::env(&env), InstructionSource::date(&day)]
    }
}
