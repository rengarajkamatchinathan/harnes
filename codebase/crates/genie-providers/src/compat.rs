//! Quirk flags ([`Compat`]) and the per-model data the adapters read ([`ModelSpec`]).
//!
//! Protocols read `Compat` and `ModelSpec`; they never switch on model names. Name-based
//! rules are catalog [`ModelPatch`]es so the conformance suite can test them (research).

use serde::{Deserialize, Serialize};

use crate::model::{EffortLevel, ProtocolKind, ProviderId, SamplingParam};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaxTokensField {
    #[default]
    MaxTokens,
    MaxCompletionTokens,
}

/// Where an OpenAI-Chat dialect puts reasoning.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningField {
    /// DeepSeek / vLLM `reasoning_content`.
    ReasoningContent,
    /// Ollama `reasoning`.
    Reasoning,
    /// OpenRouter `reasoning_details` (replayed verbatim).
    ReasoningDetails,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolIdRule {
    /// Send canonical ids unchanged.
    #[default]
    Keep,
    /// `[a-zA-Z0-9_-]` only.
    AnthropicCharset,
    /// At most 40 characters.
    Max40,
    /// Exactly 9 alphanumerics (Mistral).
    Mistral9,
    /// Don't send ids (older Gemini).
    Omit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemaSanitizer {
    Gemini,
    /// Root must be an object schema.
    ObjectRoot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheFormat {
    /// `cache_control` on content parts (Anthropic-style, also some Chat dialects).
    Anthropic,
}

/// Per-route quirk flags (D18). Routes set defaults; [`ModelPatch`]es overlay them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Compat {
    /// "2026-10-21" | "2023-06-01" | "vertex-2023-10-16". The model's `api_version` wins.
    pub anthropic_version: Option<String>,
    /// Vertex `streamRawPredict`: `anthropic_version` in the body, model in the URL.
    pub version_in_body: bool,
    /// False on Anthropic `2026-10-21`.
    pub sampling_params_allowed: bool,
    pub max_tokens_field: MaxTokensField,
    pub reasoning_field: Option<ReasoningField>,
    pub require_finish_reason: bool,
    pub require_assistant_after_tool: bool,
    pub stream_usage: bool,
    pub cache_format: Option<CacheFormat>,
    pub max_cache_breakpoints: u8,
    pub tool_id_rule: ToolIdRule,
    pub schema_sanitizer: Option<SchemaSanitizer>,
    /// Azure deployment: fall back to Chat on "Model not supported".
    pub prefer_responses: bool,
    /// Effort / instruction changes as a mid-conversation system message.
    pub mid_conversation_system: bool,
    /// Chat dialects without multi-system support: baseline as a first user `<context>` message.
    pub baseline_as_user: bool,
    /// False on Foundry (no `anthropic-ratelimit-*` headers).
    pub rate_limit_headers: bool,
}

impl Default for Compat {
    fn default() -> Self {
        Self {
            anthropic_version: None,
            version_in_body: false,
            sampling_params_allowed: true,
            max_tokens_field: MaxTokensField::MaxTokens,
            reasoning_field: None,
            require_finish_reason: false,
            require_assistant_after_tool: false,
            stream_usage: true,
            cache_format: None,
            max_cache_breakpoints: 0,
            tool_id_rule: ToolIdRule::Keep,
            schema_sanitizer: None,
            prefer_responses: false,
            mid_conversation_system: false,
            baseline_as_user: false,
            rate_limit_headers: true,
        }
    }
}

impl Compat {
    /// Apply a JSON overlay (a partial `Compat`). Unknown keys are ignored.
    pub fn overlay(&mut self, patch: &serde_json::Value) {
        let Ok(mut base) = serde_json::to_value(&*self) else {
            return;
        };
        if let (Some(b), Some(p)) = (base.as_object_mut(), patch.as_object()) {
            for (k, v) in p {
                if b.contains_key(k) {
                    b.insert(k.clone(), v.clone());
                }
            }
        }
        if let Ok(c) = serde_json::from_value(base) {
            *self = c;
        }
    }
}

/// How one effort level is expressed on the wire (catalog `reasoning` table, D27).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffortWire {
    /// Anthropic `output_config.effort` / OpenAI `reasoning.effort` / Chat `reasoning_effort`.
    Effort(String),
    /// Anthropic `thinking.budget_tokens` / Gemini `thinkingBudget`.
    Budget(u32),
    /// Gemini `thinkingLevel`.
    ThinkingLevel(String),
    /// Thinking disabled.
    Off,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffortMapping {
    pub level: EffortLevel,
    pub wire: EffortWire,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PromptCacheCaps {
    pub auto: bool,
    pub breakpoints: u8,
    pub ttl_1h: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct InputModalities {
    pub image: bool,
    pub pdf: bool,
}

/// A data-driven per-model patch (catalog `quirks`): a compat overlay plus a body merge patch.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelPatch {
    /// Partial `Compat` JSON.
    pub compat: serde_json::Value,
    /// RFC 7396 merge patch applied to the request body.
    pub body: Option<serde_json::Value>,
    pub headers: Vec<(String, String)>,
}

/// What an adapter needs to know about the model it talks to. `genie-core` builds it from
/// a `genie-catalog` entry (01 §2.1 rule 3 keeps the two crates apart).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelSpec {
    pub provider: ProviderId,
    pub catalog_id: String,
    /// Model id on the wire; the deployment name on Azure.
    pub wire_id: String,
    pub protocol: ProtocolKind,
    #[serde(default)]
    pub family: Option<String>,
    #[serde(default)]
    pub api_version: Option<String>,
    #[serde(default)]
    pub max_output: Option<u32>,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub input: InputModalities,
    #[serde(default)]
    pub sampling_supported: Vec<SamplingParam>,
    #[serde(default)]
    pub effort: Vec<EffortMapping>,
    #[serde(default)]
    pub prompt_cache: PromptCacheCaps,
    #[serde(default)]
    pub web_search_native: bool,
    #[serde(default)]
    pub code_execution_native: bool,
    #[serde(default)]
    pub patches: Vec<ModelPatch>,
}

impl ModelSpec {
    pub fn new(provider: &str, wire_id: &str, protocol: ProtocolKind) -> Self {
        Self {
            provider: ProviderId::new(provider),
            catalog_id: wire_id.to_string(),
            wire_id: wire_id.to_string(),
            protocol,
            family: None,
            api_version: None,
            max_output: None,
            context_window: None,
            input: InputModalities::default(),
            sampling_supported: vec![],
            effort: vec![],
            prompt_cache: PromptCacheCaps::default(),
            web_search_native: false,
            code_execution_native: false,
            patches: vec![],
        }
    }

    pub fn effort_wire(&self, level: EffortLevel) -> Option<&EffortWire> {
        self.effort
            .iter()
            .find(|m| m.level == level)
            .map(|m| &m.wire)
    }

    pub fn supports_sampling(&self, p: SamplingParam) -> bool {
        self.sampling_supported.contains(&p)
    }

    /// The route's compat with this model's patches applied in order.
    pub fn effective_compat(&self, route: &Compat) -> Compat {
        let mut c = route.clone();
        for p in &self.patches {
            if !p.compat.is_null() {
                c.overlay(&p.compat);
            }
        }
        if let Some(v) = &self.api_version
            && self.protocol == ProtocolKind::AnthropicMessages
        {
            c.anthropic_version = Some(v.clone());
        }
        c
    }
}

/// RFC 7396 JSON merge patch.
pub fn merge_patch(target: &mut serde_json::Value, patch: &serde_json::Value) {
    if let Some(p) = patch.as_object() {
        if !target.is_object() {
            *target = serde_json::Value::Object(Default::default());
        }
        if let Some(t) = target.as_object_mut() {
            for (k, v) in p {
                if v.is_null() {
                    t.remove(k);
                } else {
                    merge_patch(t.entry(k.clone()).or_insert(serde_json::Value::Null), v);
                }
            }
        }
    } else {
        *target = patch.clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn patches_overlay_compat_and_api_version() {
        let mut m = ModelSpec::new("anthropic", "claude-x", ProtocolKind::AnthropicMessages);
        m.api_version = Some("2026-10-21".into());
        m.patches.push(ModelPatch {
            compat: json!({"sampling_params_allowed": false, "nonsense": 1}),
            ..Default::default()
        });
        let c = m.effective_compat(&Compat::default());
        assert!(!c.sampling_params_allowed);
        assert_eq!(c.anthropic_version.as_deref(), Some("2026-10-21"));
    }

    #[test]
    fn merge_patch_rfc7396() {
        let mut t = json!({"a": 1, "b": {"c": 2, "d": 3}});
        merge_patch(&mut t, &json!({"a": null, "b": {"c": 5}, "e": [1]}));
        assert_eq!(t, json!({"b": {"c": 5, "d": 3}, "e": [1]}));
    }
}
