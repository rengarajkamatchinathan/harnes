//! Server → client requests surfaced as ports: elicitation and sampling (06 §7, D30).
//!
//! The turn loop wires these to asks (02 §7) later; this module gives it the parsed
//! requests, ready-made [`Ask`] builders, sensitive-field flags, the sampling policy and the
//! per-server token budget.

use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use genie_protocol::ask::{Ask, AskAnswer, AskKind, Decision, Preview, SamplingInfo};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

/// `elicitation/create` params.
#[derive(Debug, Clone, PartialEq)]
pub struct ElicitRequest {
    pub message: String,
    /// The form (a flat object schema of primitive properties per MCP 2025-06-18).
    pub requested_schema: Value,
    /// Properties that look like secrets (passwords, tokens, card numbers, …): the ask card
    /// flags them (08 §2.2).
    pub sensitive_fields: Vec<String>,
}

/// Words that mark a field as sensitive: long ones match inside a word (`apiKey`,
/// `password2`), short ones only as a whole word (so `shipping` is not a PIN).
const SENSITIVE_LONG: &[&str] = &[
    "password",
    "passwd",
    "passphrase",
    "secret",
    "token",
    "apikey",
    "credential",
    "privatekey",
    "cardnumber",
    "creditcard",
];
const SENSITIVE_SHORT: &[&str] = &[
    "otp", "2fa", "mfa", "pin", "ssn", "cvv", "cvc", "iban", "card",
];
const SENSITIVE_PHRASES: &[&str] = &["api key", "private key", "social security", "card number"];

fn looks_sensitive(text: &str) -> bool {
    let lower = text.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let joined = words.join(" ");
    words
        .iter()
        .any(|w| SENSITIVE_LONG.iter().any(|k| w.contains(k)) || SENSITIVE_SHORT.contains(w))
        || SENSITIVE_PHRASES.iter().any(|p| joined.contains(p))
}

impl ElicitRequest {
    pub fn from_params(params: &Value) -> ElicitRequest {
        let message = params
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let requested_schema = params
            .get("requestedSchema")
            .cloned()
            .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
        let mut sensitive_fields = Vec::new();
        if let Some(props) = requested_schema
            .get("properties")
            .and_then(Value::as_object)
        {
            for (name, p) in props {
                let hay = format!(
                    "{} {} {} {}",
                    name,
                    p.get("title").and_then(Value::as_str).unwrap_or_default(),
                    p.get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                    p.get("format").and_then(Value::as_str).unwrap_or_default(),
                );
                if looks_sensitive(&hay) {
                    sensitive_fields.push(name.clone());
                }
            }
        }
        ElicitRequest {
            message,
            requested_schema,
            sensitive_fields,
        }
    }

    /// The durable ask for this request (kind `elicitation`).
    pub fn to_ask(
        &self,
        server: &str,
        id: impl Into<String>,
        session_id: impl Into<String>,
        created_at: impl Into<String>,
    ) -> Ask {
        let mut a = Ask::new(id, session_id, AskKind::Elicitation, created_at);
        a.resource = Some(server.to_string());
        a.preview = Some(Preview::Text {
            text: format!("{server} asks: {}", self.message),
        });
        a.form = Some(self.requested_schema.clone());
        a.sensitive_fields = self.sensitive_fields.clone();
        a
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElicitAction {
    Accept,
    Decline,
    Cancel,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ElicitResponse {
    pub action: ElicitAction,
    pub content: Option<Value>,
}

impl ElicitResponse {
    pub fn decline() -> ElicitResponse {
        ElicitResponse {
            action: ElicitAction::Decline,
            content: None,
        }
    }

    pub fn cancel() -> ElicitResponse {
        ElicitResponse {
            action: ElicitAction::Cancel,
            content: None,
        }
    }

    pub fn accept(content: Value) -> ElicitResponse {
        ElicitResponse {
            action: ElicitAction::Accept,
            content: Some(content),
        }
    }

    /// Map an ask answer: `submit` → accept with the form data, `reject` → decline,
    /// anything else → cancel.
    pub fn from_answer(a: &AskAnswer) -> ElicitResponse {
        match a.decision {
            Decision::Submit | Decision::Once | Decision::Always => {
                ElicitResponse::accept(a.data.clone().unwrap_or_else(|| json!({})))
            }
            Decision::Reject => ElicitResponse::decline(),
            _ => ElicitResponse::cancel(),
        }
    }

    pub fn to_result(&self) -> Value {
        let action = match self.action {
            ElicitAction::Accept => "accept",
            ElicitAction::Decline => "decline",
            ElicitAction::Cancel => "cancel",
        };
        let mut v = json!({ "action": action });
        if let (ElicitAction::Accept, Some(c)) = (self.action, &self.content) {
            v["content"] = c.clone();
        }
        v
    }
}

/// `sampling/createMessage` params.
#[derive(Debug, Clone, PartialEq)]
pub struct SamplingRequest {
    pub messages: Vec<Value>,
    pub system_prompt: Option<String>,
    pub max_tokens: u64,
    pub temperature: Option<f64>,
    pub stop_sequences: Vec<String>,
    pub model_preferences: Option<Value>,
    pub include_context: Option<String>,
    pub raw: Value,
}

impl SamplingRequest {
    pub fn from_params(params: &Value) -> Result<SamplingRequest, String> {
        let messages = params
            .get("messages")
            .and_then(Value::as_array)
            .cloned()
            .ok_or("sampling request without `messages`")?;
        let max_tokens = params
            .get("maxTokens")
            .and_then(Value::as_u64)
            .ok_or("sampling request without `maxTokens`")?;
        Ok(SamplingRequest {
            messages,
            system_prompt: params
                .get("systemPrompt")
                .and_then(Value::as_str)
                .map(String::from),
            max_tokens,
            temperature: params.get("temperature").and_then(Value::as_f64),
            stop_sequences: params
                .get("stopSequences")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|s| s.as_str().map(String::from))
                .collect(),
            model_preferences: params.get("modelPreferences").cloned(),
            include_context: params
                .get("includeContext")
                .and_then(Value::as_str)
                .map(String::from),
            raw: params.clone(),
        })
    }

    /// Text shown on the ask card: system prompt and message texts, capped.
    pub fn preview(&self, max_chars: usize) -> String {
        let mut s = String::new();
        if let Some(sp) = &self.system_prompt {
            s.push_str("[system] ");
            s.push_str(sp);
            s.push('\n');
        }
        for m in &self.messages {
            let role = m.get("role").and_then(Value::as_str).unwrap_or("user");
            let text = match m.get("content") {
                Some(Value::Array(items)) => {
                    items.iter().map(content_text).collect::<Vec<_>>().join(" ")
                }
                Some(c) => content_text(c),
                None => String::new(),
            };
            s.push_str(&format!("[{role}] {text}\n"));
        }
        if s.chars().count() > max_chars {
            let mut t: String = s.chars().take(max_chars).collect();
            t.push('…');
            t
        } else {
            s
        }
    }

    /// The durable ask (kind `sampling`): request text, model, estimated cost, budget left.
    #[allow(clippy::too_many_arguments)]
    pub fn to_ask(
        &self,
        server: &str,
        model: &str,
        est_cost_usd: f64,
        budget_left_tokens: u64,
        id: impl Into<String>,
        session_id: impl Into<String>,
        created_at: impl Into<String>,
    ) -> Ask {
        let mut a = Ask::new(id, session_id, AskKind::Sampling, created_at);
        a.resource = Some(server.to_string());
        a.sampling = Some(SamplingInfo {
            server: server.to_string(),
            messages_preview: self.preview(4000),
            model: model.to_string(),
            est_cost_usd,
            budget_left_tokens,
        });
        a
    }
}

fn content_text(c: &Value) -> String {
    match c.get("type").and_then(Value::as_str) {
        Some("text") => c
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        Some(t) => format!("<{t}>"),
        None => String::new(),
    }
}

/// The completion produced for a sampling request.
#[derive(Debug, Clone, PartialEq)]
pub struct SamplingResponse {
    pub model: String,
    /// MCP content block, e.g. `{"type": "text", "text": "…"}`.
    pub content: Value,
    pub stop_reason: Option<String>,
    /// Input + output tokens, charged to the server's budget and to usage.
    pub tokens_used: u64,
}

impl SamplingResponse {
    pub fn to_result(&self) -> Value {
        let mut v = json!({
            "role": "assistant",
            "content": self.content,
            "model": self.model,
        });
        if let Some(r) = &self.stop_reason {
            v["stopReason"] = r.clone().into();
        }
        v
    }
}

/// Why sampling was refused (the server gets a JSON-RPC error with code -1, as in the MCP
/// spec's "user rejected" example).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SamplingRefusal {
    Rejected(String),
    PolicyBanned,
    BudgetExhausted { left: u64, requested: u64 },
    Unavailable,
}

impl std::fmt::Display for SamplingRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SamplingRefusal::Rejected(m) => write!(f, "user rejected sampling request: {m}"),
            SamplingRefusal::PolicyBanned => f.write_str("sampling is disabled by policy"),
            SamplingRefusal::BudgetExhausted { left, requested } => write!(
                f,
                "sampling token budget exhausted for this server ({left} left, {requested} requested)"
            ),
            SamplingRefusal::Unavailable => f.write_str("sampling is not available"),
        }
    }
}

/// Per-server sampling token budget (D30). `limit = None` means unlimited.
#[derive(Debug, Default)]
pub struct SamplingBudget {
    limit: Option<u64>,
    used: AtomicU64,
}

impl SamplingBudget {
    pub fn new(limit: Option<u64>) -> SamplingBudget {
        SamplingBudget {
            limit,
            used: AtomicU64::new(0),
        }
    }

    pub fn used(&self) -> u64 {
        self.used.load(Ordering::Relaxed)
    }

    pub fn left(&self) -> u64 {
        self.limit
            .map_or(u64::MAX, |l| l.saturating_sub(self.used()))
    }

    /// Refuse a request whose `maxTokens` doesn't fit what is left.
    pub fn check(&self, requested: u64) -> Result<(), SamplingRefusal> {
        let left = self.left();
        if left == 0 || requested > left {
            return Err(SamplingRefusal::BudgetExhausted { left, requested });
        }
        Ok(())
    }

    pub fn charge(&self, tokens: u64) {
        self.used.fetch_add(tokens, Ordering::Relaxed);
    }
}

/// Handles requests a server makes of Genie. The defaults decline everything, so a
/// server never gets input or a completion unless the engine wires a handler.
#[async_trait]
pub trait ClientHandler: Send + Sync {
    /// Show the form as an ask (pause + notification when no client, D22).
    async fn elicit(
        &self,
        _server: &str,
        _req: ElicitRequest,
        _cancel: CancellationToken,
    ) -> ElicitResponse {
        ElicitResponse::decline()
    }

    /// Ask for approval (always by default; "always for this server" is off by default),
    /// then run the chat's current model and report the tokens used. Policy and budget are
    /// checked by the client before this is called.
    async fn sample(
        &self,
        _server: &str,
        _req: SamplingRequest,
        _budget_left_tokens: u64,
        _cancel: CancellationToken,
    ) -> Result<SamplingResponse, SamplingRefusal> {
        Err(SamplingRefusal::Unavailable)
    }
}

/// Declines every elicitation and sampling request.
pub struct DeclineAll;

impl ClientHandler for DeclineAll {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elicitation_flags_sensitive_fields_and_builds_ask() {
        let r = ElicitRequest::from_params(&json!({
            "message": "Log in",
            "requestedSchema": {"type": "object", "properties": {
                "user": {"type": "string"},
                "pw": {"type": "string", "title": "Password"},
                "api_key": {"type": "string"},
                "shipping": {"type": "string", "title": "Shipping address"}
            }}
        }));
        assert_eq!(
            r.sensitive_fields,
            vec!["api_key".to_string(), "pw".to_string()]
        );
        let ask = r.to_ask("srv", "a1", "s1", "2026-11-17T00:00:00Z");
        assert_eq!(ask.kind, AskKind::Elicitation);
        assert_eq!(ask.sensitive_fields.len(), 2);
        assert!(ask.form.is_some());
        let ans = AskAnswer {
            decision: Decision::Submit,
            scope: None,
            message: None,
            data: Some(json!({"user": "u"})),
        };
        assert_eq!(
            ElicitResponse::from_answer(&ans).to_result(),
            json!({"action": "accept", "content": {"user": "u"}})
        );
        assert_eq!(
            ElicitResponse::from_answer(&AskAnswer::decision(Decision::Reject)).to_result(),
            json!({"action": "decline"})
        );
    }

    #[test]
    fn sampling_parse_preview_and_budget() {
        let r = SamplingRequest::from_params(&json!({
            "messages": [{"role": "user", "content": {"type": "text", "text": "hi"}}],
            "systemPrompt": "be brief",
            "maxTokens": 100
        }))
        .unwrap();
        assert!(r.preview(100).contains("[user] hi"));
        let ask = r.to_ask("srv", "m", 0.01, 500, "a", "s", "t");
        assert_eq!(ask.sampling.unwrap().budget_left_tokens, 500);
        assert!(SamplingRequest::from_params(&json!({"messages": []})).is_err());

        let b = SamplingBudget::new(Some(150));
        assert!(b.check(100).is_ok());
        b.charge(100);
        assert_eq!(b.left(), 50);
        assert!(matches!(
            b.check(100),
            Err(SamplingRefusal::BudgetExhausted { left: 50, .. })
        ));
        assert!(SamplingBudget::new(None).check(u64::MAX).is_ok());
    }
}
