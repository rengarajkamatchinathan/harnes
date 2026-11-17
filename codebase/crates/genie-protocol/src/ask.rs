//! Asks: every decision the user must make shares one durable shape (02 §7).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::common::{ClientRef, RuleScope};
pub use crate::handshake::AskKind;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Ask {
    pub id: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    pub kind: AskKind,
    /// Labels prompts raised by a subagent (D47).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent: Option<SubagentRef>,
    /// RFC 3339.
    pub created_at: String,

    // permission
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<Preview>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub save_suggestions: Vec<SaveSuggestion>,
    /// MCP hints; informational only, never trusted for decisions (t13).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Annotations>,

    // elicitation (MCP) / question (ask_user)
    /// JSON Schema of the form to fill.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub form: Option<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sensitive_fields: Vec<String>,

    // sampling (MCP)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampling: Option<SamplingInfo>,

    // browser_confirm
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser: Option<BrowserConfirm>,
}

impl Ask {
    /// A bare ask of `kind`; callers fill the kind-specific fields.
    pub fn new(
        id: impl Into<String>,
        session_id: impl Into<String>,
        kind: AskKind,
        created_at: impl Into<String>,
    ) -> Ask {
        Ask {
            id: id.into(),
            session_id: session_id.into(),
            turn_id: None,
            kind,
            subagent: None,
            created_at: created_at.into(),
            tool: None,
            resource: None,
            preview: None,
            save_suggestions: Vec::new(),
            annotations: None,
            form: None,
            sensitive_fields: Vec::new(),
            sampling: None,
            browser: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SubagentRef {
    pub session_id: String,
    pub agent: String,
}

/// What the ask card shows for a permission ask.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Preview {
    Diff {
        path: String,
        unified: String,
    },
    Command {
        command: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
    },
    Url {
        url: String,
    },
    McpArgs {
        server: String,
        tool: String,
        args: Value,
    },
    Text {
        text: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SaveSuggestion {
    pub rule: String,
    pub scope: RuleScope,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Annotations {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only_hint: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destructive_hint: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SamplingInfo {
    pub server: String,
    pub messages_preview: String,
    pub model: String,
    pub est_cost_usd: f64,
    pub budget_left_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BrowserConfirm {
    pub origin: String,
    pub action: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screenshot_blob: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Once,
    Always,
    Reject,
    Submit,
    Cancel,
    #[serde(other)]
    Unknown,
}

/// The answer to an ask: the result of the `permission.ask` request, and the body of
/// `permission.respond` (minus `ask_id`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AskAnswer {
    pub decision: Decision,
    /// Where to save an `always` rule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<RuleScope>,
    /// `reject` with a message sends it to the model as feedback (02 §7 step 5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Elicitation / question form values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl AskAnswer {
    pub fn decision(d: Decision) -> AskAnswer {
        AskAnswer {
            decision: d,
            scope: None,
            message: None,
            data: None,
        }
    }
}

/// Who resolved an ask (`permission.resolved.by`): a client, or an automatic source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ResolvedBy {
    Client(ClientRef),
    Auto(AutoResolver),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AutoResolver {
    Rule,
    Hook,
    Policy,
    /// The turn ended (interrupt, failure) while the ask was open: resolved as `cancel`.
    System,
    #[serde(other)]
    Unknown,
}
