//! Catalog entry schema (03 §8.2, t04). Decoded tolerantly: unknown fields are ignored,
//! missing ones default, and a bad entry is dropped with a diagnostic by [`crate::load`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    AnthropicMessages,
    OpenaiResponses,
    OpenaiChat,
    Gemini,
    BedrockConverse,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effort {
    Off,
    Low,
    Medium,
    High,
    Max,
}

/// How one effort level goes on the wire (mirrors `genie_providers::compat::EffortWire`;
/// `genie-core` converts, 01 §2.1 rule 3).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffortWire {
    Effort(String),
    Budget(u32),
    ThinkingLevel(String),
    Off,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Budget {
    pub min: u32,
    pub max: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Reasoning {
    /// Reasoning can be switched off.
    pub toggle: bool,
    /// Supported levels, a subset of Off/Low/Medium/High/Max.
    pub levels: Vec<Effort>,
    pub budget: Option<Budget>,
    pub adaptive: bool,
    /// Explicit per-level wire values; derived from `levels`/`budget` when absent.
    pub wire: BTreeMap<Effort, EffortWire>,
    pub default: Option<Effort>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PromptCache {
    pub auto: bool,
    pub breakpoints: u8,
    pub ttl_1h: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Embeddings {
    pub default: bool,
    pub dimensions: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Capabilities {
    pub tools: bool,
    pub parallel_tools: bool,
    pub vision: bool,
    pub pdf: bool,
    pub web_search_native: bool,
    pub code_execution_native: bool,
    pub native_compaction: bool,
    pub prompt_cache: PromptCache,
    pub reasoning: Option<Reasoning>,
    pub citations: bool,
    pub structured_output: bool,
    pub prefers_patch: bool,
    pub embeddings: Option<Embeddings>,
}

/// Prices in USD per `per` tokens (always "1M" today).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Pricing {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write_5m: f64,
    pub cache_write_1h: f64,
    pub web_search_per_call: Option<f64>,
    pub per: String,
}

impl Default for Pricing {
    fn default() -> Self {
        Self {
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write_5m: 0.0,
            cache_write_1h: 0.0,
            web_search_per_call: None,
            per: "1M".into(),
        }
    }
}

/// A data-driven quirk (mirrors `genie_providers::compat::ModelPatch`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Quirk {
    pub compat: serde_json::Value,
    pub body: Option<serde_json::Value>,
    pub headers: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub provider: String,
    pub protocol: Protocol,
    pub wire_model_id: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub family: Option<String>,
    #[serde(default)]
    pub release_date: Option<String>,
    #[serde(default)]
    pub deprecated_at: Option<String>,
    #[serde(default)]
    pub context_window: u64,
    #[serde(default)]
    pub max_output: u32,
    #[serde(default)]
    pub input_modalities: Vec<String>,
    #[serde(default)]
    pub output_modalities: Vec<String>,
    #[serde(default)]
    pub capabilities: Capabilities,
    #[serde(default)]
    pub sampling_supported: Vec<String>,
    #[serde(default)]
    pub api_version: Option<String>,
    #[serde(default)]
    pub quirks: Vec<Quirk>,
    #[serde(default)]
    pub pricing: Option<Pricing>,
    /// Tasks this model is the small model for ("title", "summary").
    #[serde(default)]
    pub small_model_for: Vec<String>,
    /// Discovered with no catalog entry: conservative defaults ("limited info" badge).
    #[serde(default)]
    pub limited_info: bool,
}

impl Entry {
    /// Wire value for an effort level: explicit table first, then derived defaults.
    pub fn effort_wire(&self, level: Effort) -> Option<EffortWire> {
        let r = self.capabilities.reasoning.as_ref()?;
        if !r.levels.contains(&level) {
            return None;
        }
        if let Some(w) = r.wire.get(&level) {
            return Some(w.clone());
        }
        if level == Effort::Off {
            return Some(EffortWire::Off);
        }
        let name = match level {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            _ => "max",
        };
        if let (Some(b), false) = (r.budget, r.adaptive) {
            let frac = match level {
                Effort::Low => 0.1,
                Effort::Medium => 0.35,
                Effort::High => 0.7,
                _ => 1.0,
            };
            let n = b.min as f64 + (b.max.saturating_sub(b.min)) as f64 * frac;
            return Some(EffortWire::Budget(n as u32));
        }
        Some(match self.protocol {
            Protocol::Gemini => EffortWire::ThinkingLevel(name.into()),
            _ => EffortWire::Effort(name.into()),
        })
    }

    /// Levels for the effort selector; empty → selector hidden (D27).
    pub fn effort_levels(&self) -> Vec<Effort> {
        let mut v = self
            .capabilities
            .reasoning
            .as_ref()
            .map(|r| r.levels.clone())
            .unwrap_or_default();
        v.sort();
        v.dedup();
        v
    }

    pub fn is_deprecated(&self) -> bool {
        self.deprecated_at.is_some()
    }
}
