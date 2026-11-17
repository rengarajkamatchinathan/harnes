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

use genie_protocol::ask::{Ask, AskKind, Preview, SaveSuggestion};
use genie_protocol::common::{Mode, SessionKind};
use genie_providers::executor::LlmClient;
use genie_providers::route::Route;
use genie_tools::ToolKind;

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
    pub tool: String,
    pub kind: ToolKind,
    pub resource: Option<String>,
    pub preview: Option<Preview>,
    pub save_suggestions: Vec<SaveSuggestion>,
    pub mode: Mode,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AuthzDecision {
    Allow,
    Deny {
        reason: String,
    },
    /// Ask the user; the loop sends it with `TurnCtx::ask_with`.
    Ask(Box<Ask>),
}

/// The permission decision for a tool call (06 §5). Swapped for the `genie-permissions`
/// evaluator when it lands.
pub trait ToolAuthorizer: Send + Sync {
    fn authorize(&self, req: &AuthzRequest) -> AuthzDecision;
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
