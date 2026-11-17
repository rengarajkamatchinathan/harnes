//! The canonical model (03 §2; research/provider-layer.md "Type sketch").
//!
//! These types are what the engine stores and what every adapter normalises from and to.
//! They are serde-serialisable so the store can persist them; enums we *read* from disk
//! carry an `Unknown`/`#[serde(other)]` arm where a later version may add variants.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

macro_rules! id_type {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);
        impl $name {
            pub fn new(s: impl Into<String>) -> Self { Self(s.into()) }
            pub fn as_str(&self) -> &str { &self.0 }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.0) }
        }
        impl From<&str> for $name { fn from(s: &str) -> Self { Self(s.to_string()) } }
    };
}

id_type!(
    /// "anthropic", "openai", "azure-foundry", "google", "ollama", …
    ProviderId
);
id_type!(MessageId);
id_type!(
    /// Canonical tool-call id. Wire ids are derived per destination (03 §4 step 5).
    ToolCallId
);
id_type!(
    /// Content-addressed blob in the store (04); resolved to bytes before an adapter runs.
    BlobId
);

/// The wire protocol a route speaks (D26). Bedrock is v2 and stays unused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolKind {
    AnthropicMessages,
    OpenaiResponses,
    OpenaiChat,
    Gemini,
    /// v2 (03 §12); no adapter yet.
    BedrockConverse,
}

impl ProtocolKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AnthropicMessages => "anthropic_messages",
            Self::OpenaiResponses => "openai_responses",
            Self::OpenaiChat => "openai_chat",
            Self::Gemini => "gemini",
            Self::BedrockConverse => "bedrock_converse",
        }
    }
}

/// Per-protocol rule deciding whether an [`Opaque`] payload is replayed to a destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayScope {
    /// Anthropic: send everything within the protocol family; account-bound blocks need
    /// the same account.
    ProtocolFamilySameAccount,
    /// OpenAI Responses: same model family.
    ModelFamily,
    /// Gemini, OpenAI Chat (conservative default).
    SameModel,
    Never,
}

/// `AccountTag = first 16 bytes of SHA-256(provider_id ‖ credential_id ‖ tenant_or_project ‖ endpoint_host)`
/// (03 §2, SA-13). Fields are joined with a 0x00 separator so `("ab","c")` and `("a","bc")`
/// hash differently.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AccountTag(#[serde(with = "hex16")] pub [u8; 16]);

impl AccountTag {
    pub fn derive(
        provider: &ProviderId,
        credential_id: &str,
        tenant_or_project: &str,
        endpoint_host: &str,
    ) -> Self {
        let mut h = Sha256::new();
        for (i, f) in [
            provider.as_str(),
            credential_id,
            tenant_or_project,
            endpoint_host,
        ]
        .iter()
        .enumerate()
        {
            if i > 0 {
                h.update([0u8]);
            }
            h.update(f.as_bytes());
        }
        let d = h.finalize();
        let mut out = [0u8; 16];
        out.copy_from_slice(&d[..16]);
        Self(out)
    }
}

mod hex16 {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};
    pub fn serialize<S: Serializer>(v: &[u8; 16], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(v))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 16], D::Error> {
        let s = String::deserialize(d)?;
        let v = hex::decode(&s).map_err(D::Error::custom)?;
        v.try_into()
            .map_err(|_| D::Error::custom("account tag must be 16 bytes"))
    }
}

/// Who produced a part, and therefore who may receive its opaque payload back.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    pub provider: ProviderId,
    /// Wire model id actually served (a server-side fallback may differ from the request).
    pub model: String,
    pub protocol: ProtocolKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<AccountTag>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub id: MessageId,
    pub role: Role,
    pub parts: Vec<Part>,
    /// Set on assistant messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
}

impl Message {
    pub fn new(id: impl Into<String>, role: Role, parts: Vec<Part>) -> Self {
        Self {
            id: MessageId::new(id),
            role,
            parts,
            origin: None,
        }
    }
    pub fn user_text(id: impl Into<String>, text: impl Into<String>) -> Self {
        Self::new(id, Role::User, vec![Part::text(text)])
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Part {
    Text(TextPart),
    Media(MediaPart),
    ToolCall(ToolCall),
    ToolResult(ToolResult),
    Reasoning(ReasoningPart),
    Compaction(CompactionPart),
    EffortMarker(EffortMarker),
    Unknown(UnknownPart),
}

impl Part {
    pub fn text(t: impl Into<String>) -> Self {
        Part::Text(TextPart {
            text: t.into(),
            ..Default::default()
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TextPart {
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub citations: Vec<Citation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache: Option<CacheHint>,
    /// e.g. a Gemini `thoughtSignature` on the final text part.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opaque: Option<Opaque>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Citation {
    pub kind: CitationKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cited_text: Option<String>,
    pub source: CitationSource,
    /// Byte span in the owning text part, when the provider gives one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<(usize, usize)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opaque: Option<Opaque>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CitationKind {
    CharRange,
    PageRange,
    ContentBlock,
    SearchResult,
    WebUrl,
    Annotation,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CitationSource {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document_index: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MediaPart {
    pub mime: String,
    pub source: MediaSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    /// Text extracted by the daemon (PDF → text fallback, 03 §4 step 9).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extracted_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache: Option<CacheHint>,
}

impl MediaPart {
    pub fn is_image(&self) -> bool {
        self.mime.starts_with("image/")
    }
    pub fn is_pdf(&self) -> bool {
        self.mime == "application/pdf"
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MediaSource {
    /// Stored blob; the engine resolves it to [`MediaSource::Base64`] before compile.
    Blob {
        id: BlobId,
    },
    /// Inline bytes, base64 (standard alphabet).
    Base64 {
        data: String,
    },
    Url {
        url: String,
    },
    ProviderFile {
        id: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: ToolCallId,
    pub name: String,
    pub input: serde_json::Value,
    /// Server tools (web search, code execution, …).
    #[serde(default)]
    pub provider_executed: bool,
    /// e.g. a Gemini `thoughtSignature` on a `functionCall`, or the raw server-tool block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opaque: Option<Opaque>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: ToolCallId,
    pub content: ToolOutput,
    #[serde(default)]
    pub is_error: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache: Option<CacheHint>,
    /// Raw provider block for provider-executed results (e.g. `web_search_tool_result`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opaque: Option<Opaque>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ToolOutput {
    Text(String),
    Json(serde_json::Value),
    Rich(Vec<Part>),
}

impl ToolOutput {
    /// Plain-text rendering for protocols whose tool results are strings.
    pub fn to_text(&self) -> String {
        match self {
            ToolOutput::Text(t) => t.clone(),
            ToolOutput::Json(v) => v.to_string(),
            ToolOutput::Rich(parts) => parts
                .iter()
                .filter_map(|p| match p {
                    Part::Text(t) => Some(t.text.clone()),
                    Part::Media(m) => Some(format!("[{} attachment]", m.mime)),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ReasoningPart {
    /// Readable text or summary; may be empty ("omitted" display).
    pub text: String,
    #[serde(default)]
    pub redacted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opaque: Option<Opaque>,
}

/// Provider checkpoint (opaque) xor our own summary text (D33).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CompactionPart {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opaque: Option<Opaque>,
}

/// "Effort changed here": a mid-conversation system message where the route supports it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EffortMarker {
    pub effort: EffortLevel,
}

/// A provider block with no canonical meaning (e.g. Anthropic `fallback`), replayed only in scope.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UnknownPart {
    pub origin: Origin,
    pub raw: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Opaque {
    pub origin: Origin,
    pub data: OpaqueData,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OpaqueData {
    AnthropicSignature {
        signature: String,
    },
    AnthropicRedacted {
        data: String,
    },
    ResponsesReasoning {
        item_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encrypted_content: Option<String>,
        #[serde(default)]
        summary: Vec<String>,
    },
    GeminiThoughtSignature {
        signature: String,
    },
    BedrockReasoning {
        #[serde(default)]
        signature: Option<String>,
        #[serde(default)]
        redacted: Option<String>,
    },
    /// OpenRouter / Kimi `reasoning_details`.
    ChatReasoningDetails {
        value: serde_json::Value,
    },
    /// A whole provider block kept verbatim (server tool calls/results, compaction).
    Raw {
        value: serde_json::Value,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheHint {
    pub ttl: CacheTtl,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CacheTtl {
    #[default]
    #[serde(rename = "5m")]
    FiveMinutes,
    #[serde(rename = "1h")]
    OneHour,
}

// ---------- request ----------

/// The user-facing effort scale (D27). Each model supports a subset; the wire value comes
/// from the catalog's per-(protocol, model) table in [`crate::compat::ModelSpec::effort`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffortLevel {
    Off,
    Low,
    Medium,
    High,
    Max,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache: Option<CacheHint>,
}

/// Provider-executed tools (D36, D35).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ServerTools {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_search: Option<WebSearchConfig>,
    #[serde(default)]
    pub code_execution: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WebSearchConfig {
    #[serde(default)]
    pub allowed_domains: Vec<String>,
    #[serde(default)]
    pub blocked_domains: Vec<String>,
    #[serde(default)]
    pub max_uses: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ToolChoice {
    #[default]
    Auto,
    Required,
    None,
    Tool {
        name: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SamplingParam {
    Temperature,
    TopP,
    TopK,
    Stop,
    #[serde(other)]
    Unknown,
}

/// Advanced-panel sampling settings; sent only if the model lists them (D27).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Sampling {
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub top_k: Option<u32>,
    pub stop: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CachePolicy {
    /// Up to 4 breakpoints: tools, system block 1, system block 2, conversation tail (D32).
    #[default]
    Auto,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timeouts {
    /// Time to response headers.
    pub headers: std::time::Duration,
    /// Max gap between body chunks.
    pub chunk_gap: std::time::Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            headers: std::time::Duration::from_secs(5 * 60),
            chunk_gap: std::time::Duration::from_secs(15 * 60),
        }
    }
}

/// A request in canonical form; [`crate::compile::compile`] turns it into a [`crate::CompiledRequest`].
#[derive(Clone, Debug)]
pub struct LlmRequest {
    pub model: crate::compat::ModelSpec,
    /// System blocks: [0] base prompt, [1] context baseline (06 §2.1, SA-41).
    pub system: Vec<TextPart>,
    /// The active branch path root → leaf (D24).
    pub messages: Vec<Message>,
    /// Wire records persisted from earlier turns (reused byte-for-byte, 03 §4 step 2).
    pub wire_records: Vec<WireRecord>,
    pub tools: Vec<ToolDef>,
    pub server_tools: ServerTools,
    pub tool_choice: ToolChoice,
    pub effort: Option<EffortLevel>,
    pub max_output_tokens: Option<u32>,
    pub sampling: Sampling,
    pub cache: CachePolicy,
    /// True when the baseline (system block 2) holds project knowledge → 1 h TTL where supported.
    pub baseline_long_ttl: bool,
    /// Root session id; OpenAI `prompt_cache_key` (D32, D47).
    pub prompt_cache_key: Option<String>,
    /// Opt-in OpenAI reasoning summaries.
    pub reasoning_summary: bool,
    pub include_raw: bool,
    pub timeouts: Timeouts,
}

impl LlmRequest {
    pub fn new(model: crate::compat::ModelSpec, messages: Vec<Message>) -> Self {
        Self {
            model,
            system: vec![],
            messages,
            wire_records: vec![],
            tools: vec![],
            server_tools: ServerTools::default(),
            tool_choice: ToolChoice::Auto,
            effort: None,
            max_output_tokens: None,
            sampling: Sampling::default(),
            cache: CachePolicy::Auto,
            baseline_long_ttl: false,
            prompt_cache_key: None,
            reasoning_summary: false,
            include_raw: false,
            timeouts: Timeouts::default(),
        }
    }
}

/// The exact wire JSON a message was sent as, persisted with the session (D18).
///
/// `items` is a JSON array: one canonical message may become several wire items (Responses
/// input items, Chat `tool` messages). It is replayed byte-for-byte on the same route.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WireRecord {
    pub message_id: MessageId,
    /// Identifies protocol + endpoint + model; reuse requires an exact match.
    pub route_key: String,
    pub items: String,
}

// ---------- usage ----------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    Refusal,
    Paused,
    Error,
    #[default]
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FinishDetails {
    pub raw: Option<String>,
    pub category: Option<String>,
    pub explanation: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// = non_cached + cache_read + cache_write_5m + cache_write_1h
    pub input_total: u64,
    pub non_cached_input: u64,
    pub cache_read: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
    pub output_total: u64,
    /// ≤ output_total
    pub reasoning: Option<u64>,
    /// Latest call only; not summed.
    pub context_tokens: Option<u64>,
}

impl Usage {
    /// Recompute `input_total` and `context_tokens` from the parts.
    pub fn finalize(mut self) -> Self {
        self.input_total =
            self.non_cached_input + self.cache_read + self.cache_write_5m + self.cache_write_1h;
        self.context_tokens = Some(self.input_total + self.output_total);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_tag_changes_with_credential_and_is_stable() {
        let p = ProviderId::new("anthropic");
        let a = AccountTag::derive(&p, "cred1", "", "api.anthropic.com");
        let b = AccountTag::derive(&p, "cred1", "", "api.anthropic.com");
        let c = AccountTag::derive(&p, "cred2", "", "api.anthropic.com");
        let d = AccountTag::derive(&p, "cred", "1", "api.anthropic.com");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, d);
        let json = serde_json::to_string(&a).unwrap();
        assert_eq!(json.len(), 34);
        assert_eq!(serde_json::from_str::<AccountTag>(&json).unwrap(), a);
    }

    #[test]
    fn parts_round_trip() {
        let m = Message::new(
            "m1",
            Role::Assistant,
            vec![
                Part::Reasoning(ReasoningPart {
                    text: "hm".into(),
                    redacted: false,
                    opaque: None,
                }),
                Part::ToolCall(ToolCall {
                    id: ToolCallId::new("t1"),
                    name: "read".into(),
                    input: serde_json::json!({"path": "a"}),
                    provider_executed: false,
                    opaque: None,
                }),
            ],
        );
        let s = serde_json::to_string(&m).unwrap();
        assert_eq!(serde_json::from_str::<Message>(&s).unwrap(), m);
    }
}
