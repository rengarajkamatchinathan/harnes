//! Error taxonomy (01 §7). Errors are typed by layer; each type says who sees it.
//!
//! | Layer    | Type                    | Model sees                         |
//! |----------|-------------------------|------------------------------------|
//! | Tool     | [`ToolError`]           | yes (Declined: only user feedback) |
//! | Engine   | [`TurnError`]           | final step only (max steps)        |
//! | Protocol | `genie_protocol::RpcError` | no                              |
//! | Internal | [`Defect`]              | **never**                          |
//!
//! Every error has a stable `code` (`GENIE-…`) for docs and support.

use std::fmt;

use genie_protocol::{ErrorKind, RpcError};
use serde::{Deserialize, Serialize};

use crate::ids::new_id;

/// Max issues reported for invalid tool input.
pub const MAX_INPUT_ISSUES: usize = 5;
/// Provider error bodies are cut to this many bytes before logging (01 §7 rule 2).
pub const ERROR_EXCERPT_BYTES: usize = 2 * 1024;

/// A tool call failed. Model-visible as tool result text, with the per-variant rules below.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolError {
    /// Bad JSON or schema errors (at most [`MAX_INPUT_ISSUES`]). The model decides whether to retry.
    #[error("invalid input: {}", issues.join("; "))]
    InvalidInput { issues: Vec<String> },
    /// The tool ran (or may have) and failed. Never auto-retried when `may_have_run` (D45).
    #[error("{message}")]
    Failed { message: String, may_have_run: bool },
    /// The user declined. The model sees **only** the user's message, as feedback (D20, D25).
    #[error("declined")]
    Declined { message: Option<String> },
    /// A policy or deny rule blocked the call.
    #[error("blocked by {origin} rule {rule}")]
    Blocked {
        rule: String,
        /// `source` on the wire (01 §7); renamed here because thiserror reserves `source`.
        #[serde(rename = "source")]
        origin: BlockSource,
    },
    #[error("timed out after {after_s}s")]
    Timeout { after_s: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockSource {
    Policy,
    Rule,
}

impl fmt::Display for BlockSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            BlockSource::Policy => "policy",
            BlockSource::Rule => "rule",
        })
    }
}

impl ToolError {
    pub fn invalid_input(mut issues: Vec<String>) -> Self {
        issues.truncate(MAX_INPUT_ISSUES);
        ToolError::InvalidInput { issues }
    }

    pub fn code(&self) -> &'static str {
        match self {
            ToolError::InvalidInput { .. } => "GENIE-TOOL-INPUT",
            ToolError::Failed { .. } => "GENIE-TOOL-FAILED",
            ToolError::Declined { .. } => "GENIE-TOOL-DECLINED",
            ToolError::Blocked { .. } => "GENIE-TOOL-BLOCKED",
            ToolError::Timeout { .. } => "GENIE-TOOL-TIMEOUT",
        }
    }

    /// The text placed in the tool result for the model.
    pub fn model_text(&self) -> String {
        match self {
            ToolError::InvalidInput { issues } => {
                format!("Invalid tool input:\n- {}", issues.join("\n- "))
            }
            ToolError::Failed { message, .. } => format!("Error: {message}"),
            // Only the user's own words; never any output (D20, D25).
            ToolError::Declined { message: Some(m) } => {
                format!("The user declined this tool call and said: {m}")
            }
            ToolError::Declined { message: None } => "The user declined this tool call.".into(),
            ToolError::Blocked { rule, origin } => format!("Blocked by {origin} {rule}."),
            ToolError::Timeout { after_s } => format!("The tool timed out after {after_s}s."),
        }
    }

    /// Whether the engine itself may retry the call.
    pub fn auto_retry_allowed(&self) -> bool {
        matches!(
            self,
            ToolError::Failed {
                may_have_run: false,
                ..
            }
        )
    }
}

/// A turn failed at the engine layer.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum TurnError {
    /// Max steps reached: the final step gets "answer without tools" (t18).
    #[error("max steps reached")]
    MaxSteps,
    #[error("compaction failed twice")]
    CompactionFailed,
    /// Provider failure; `class` per 03 §7 (`rate_limited`, `context_overflow`, …).
    #[error("provider error: {class}")]
    Provider {
        class: String,
        fix_link: Option<String>,
    },
    /// The turn's token was cancelled.
    #[error("cancelled")]
    Cancelled,
    #[error(transparent)]
    Internal(#[from] Defect),
}

impl TurnError {
    pub fn code(&self) -> &'static str {
        match self {
            TurnError::MaxSteps => "GENIE-TURN-MAXSTEPS",
            TurnError::CompactionFailed => "GENIE-TURN-COMPACT",
            TurnError::Provider { .. } => "GENIE-PROV",
            TurnError::Cancelled => "GENIE-TURN-CANCELLED",
            TurnError::Internal(_) => "GENIE-INTERNAL",
        }
    }
}

/// An internal defect: a panic or a broken invariant. **Never** shown to the model (t02).
/// The user sees "Something went wrong" + **Copy diagnostics** with the incident id.
#[derive(Clone, PartialEq, thiserror::Error)]
#[error("internal error (incident {incident_id})")]
pub struct Defect {
    pub incident_id: String,
    /// Logged only. Never serialized to clients or the model.
    detail: String,
}

impl Defect {
    /// Create and log a defect.
    pub fn new(detail: impl Into<String>) -> Self {
        let d = Defect {
            incident_id: new_id("inc"),
            detail: detail.into(),
        };
        tracing::error!(incident_id = %d.incident_id, detail = %d.detail, "internal defect");
        d
    }

    pub fn from_panic(payload: &(dyn std::any::Any + Send)) -> Self {
        let msg = payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "panic".to_string());
        Defect::new(format!("panic: {msg}"))
    }

    pub fn detail(&self) -> &str {
        &self.detail
    }

    pub fn to_rpc(&self) -> RpcError {
        RpcError::internal(self.incident_id.clone())
    }
}

impl fmt::Debug for Defect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Defect")
            .field("incident_id", &self.incident_id)
            .finish_non_exhaustive()
    }
}

/// Errors from session operations, mapped onto protocol kinds.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CoreError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid state: {0}")]
    InvalidState(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("cancelled")]
    Cancelled,
    #[error(transparent)]
    Internal(#[from] Defect),
}

impl CoreError {
    pub fn to_rpc(&self) -> RpcError {
        match self {
            CoreError::NotFound(m) => RpcError::new(ErrorKind::NotFound, m.clone()),
            CoreError::InvalidState(m) => RpcError::new(ErrorKind::InvalidState, m.clone()),
            CoreError::Conflict(m) => RpcError::new(ErrorKind::Conflict, m.clone()),
            CoreError::Cancelled => RpcError::new(ErrorKind::Cancelled, "cancelled"),
            CoreError::Internal(d) => d.to_rpc(),
        }
    }
}

/// Cut a provider error body to [`ERROR_EXCERPT_BYTES`] and scrub credentials before
/// logging (01 §7 rule 2, t34).
pub fn scrub_excerpt(body: &str) -> String {
    let mut end = body.len().min(ERROR_EXCERPT_BYTES);
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    let cut = &body[..end];
    let mut out = scrub(cut);
    if end < body.len() {
        out.push('…');
    }
    out
}

/// Token-level scrubber: redacts known key prefixes, the word after `Bearer`/`Basic`, and
/// the value after a secret-looking key (`api_key=…`, `"token": "…"`).
fn scrub(text: &str) -> String {
    const PREFIXES: [&str; 8] = [
        "sk-",
        "sk_",
        "AIza",
        "xoxb-",
        "xoxp-",
        "ghp_",
        "github_pat_",
        "AKIA",
    ];
    const SECRET_KEYS: [&str; 6] = ["key", "token", "secret", "password", "authorization", "sig"];
    let is_sep = |c: char| c.is_whitespace() || "\"',{}[]()=:".contains(c);

    let mut out = String::with_capacity(text.len());
    let mut after_scheme = false; // previous word was Bearer/Basic
    let mut key_pending = false; // previous word looked like a secret key
    let mut saw_assign = false; // a `=` or `:` since that key
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        let sep = is_sep(c);
        let len = rest
            .find(|ch: char| is_sep(ch) != sep)
            .unwrap_or(rest.len());
        let (run, tail) = rest.split_at(len);
        rest = tail;
        if sep {
            saw_assign |= run.contains('=') || run.contains(':');
            out.push_str(run);
            continue;
        }
        let redact = after_scheme
            || (key_pending && saw_assign)
            || (run.len() >= 12 && PREFIXES.iter().any(|p| run.starts_with(p)));
        after_scheme = run.eq_ignore_ascii_case("bearer") || run.eq_ignore_ascii_case("basic");
        let lower = run.to_ascii_lowercase();
        key_pending = !redact && SECRET_KEYS.iter().any(|k| lower.contains(k));
        saw_assign = false;
        out.push_str(if redact { "[redacted]" } else { run });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declined_shows_only_user_message() {
        let e = ToolError::Declined {
            message: Some("use the other file".into()),
        };
        assert_eq!(
            e.model_text(),
            "The user declined this tool call and said: use the other file"
        );
        assert!(!e.auto_retry_allowed());
        let failed = ToolError::Failed {
            message: "x".into(),
            may_have_run: true,
        };
        assert!(!failed.auto_retry_allowed());
    }

    #[test]
    fn invalid_input_caps_issues() {
        let e = ToolError::invalid_input((0..9).map(|i| i.to_string()).collect());
        match e {
            ToolError::InvalidInput { issues } => assert_eq!(issues.len(), MAX_INPUT_ISSUES),
            _ => unreachable!(),
        }
    }

    #[test]
    fn defect_debug_and_rpc_hide_detail() {
        let d = Defect::new("db invariant broken: row 7 secret stuff");
        assert!(!format!("{d:?}").contains("secret"));
        let rpc = d.to_rpc();
        assert_eq!(rpc.kind(), ErrorKind::InternalError);
        assert!(!serde_json::to_string(&rpc).unwrap().contains("secret"));
    }

    #[test]
    fn scrub_redacts_keys_and_cuts() {
        let body = r#"{"error":"bad key sk-ant-REALLYSECRET123","auth":"Bearer abcdef123456"} api_key=xyz"#;
        let s = scrub_excerpt(body);
        assert!(!s.contains("REALLYSECRET"), "{s}");
        assert!(!s.contains("abcdef123456"), "{s}");
        assert!(!s.contains("xyz"), "{s}");
        let long = "é".repeat(4000);
        let cut = scrub_excerpt(&long);
        assert!(cut.len() <= ERROR_EXCERPT_BYTES + '…'.len_utf8());
    }
}
