//! `genie-catalog`: the model catalog (spec/03-providers-and-models.md §8, D27).
//!
//! Load order: bundled snapshot → cached last fetch (signature-verified) → background
//! refresh (ETag, Ed25519) → live discovery → user overrides → admin policy.
//!
//! Dependency rule (01 §2.1 rule 3): no internal dependencies. `genie-core` converts an
//! [`Entry`] into a `genie_providers::ModelSpec` and injects the HTTP [`refresh::FeedFetcher`].

pub mod discovery;
pub mod entry;
pub mod refresh;
pub mod signature;

use std::collections::BTreeMap;

use serde::Deserialize;

pub use entry::{Capabilities, Effort, EffortWire, Entry, Pricing, Protocol};

/// The snapshot compiled into every release (offline floor). Built from provider docs
/// only until the models.dev licence question is settled (03 §8.4).
pub const BUNDLED: &str = include_str!("../catalog/bundled.json");

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic(pub String);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Bundled,
    Cached,
    Fetched,
}

#[derive(Deserialize)]
struct Document {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    models: Vec<serde_json::Value>,
}

/// The merged catalog.
#[derive(Clone, Debug)]
pub struct Catalog {
    pub version: u32,
    pub source: Source,
    entries: BTreeMap<String, Entry>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Decode a catalog document, dropping bad entries with a diagnostic.
pub fn decode(text: &str, source: Source) -> Result<Catalog, String> {
    let doc: Document = serde_json::from_str(text).map_err(|e| format!("catalog document: {e}"))?;
    let mut cat = Catalog {
        version: doc.version,
        source,
        entries: BTreeMap::new(),
        diagnostics: vec![],
    };
    for (i, raw) in doc.models.into_iter().enumerate() {
        let id = raw
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string();
        match serde_json::from_value::<Entry>(raw) {
            Ok(e) if e.protocol == Protocol::Unknown => {
                cat.note(format!("model #{i} ({id}): unknown protocol, dropped"));
            }
            Ok(e) if e.id.is_empty() || e.wire_model_id.is_empty() || e.provider.is_empty() => {
                cat.note(format!(
                    "model #{i}: missing id/provider/wire_model_id, dropped"
                ));
            }
            Ok(e) => {
                if cat.entries.insert(e.id.clone(), e).is_some() {
                    cat.note(format!("model {id}: duplicate id, last one wins"));
                }
            }
            Err(err) => cat.note(format!("model #{i} ({id}): {err}, dropped")),
        }
    }
    Ok(cat)
}

/// Admin policy (D38): hidden providers/models.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Policy {
    pub hidden_providers: Vec<String>,
    pub hidden_models: Vec<String>,
}

impl Catalog {
    pub fn bundled() -> Self {
        decode(BUNDLED, Source::Bundled).unwrap_or_else(|e| Catalog {
            version: 0,
            source: Source::Bundled,
            entries: BTreeMap::new(),
            diagnostics: vec![Diagnostic(format!("bundled catalog unreadable: {e}"))],
        })
    }

    fn note(&mut self, s: String) {
        tracing::warn!(target: "genie_catalog", "{s}");
        self.diagnostics.push(Diagnostic(s));
    }

    pub fn get(&self, id: &str) -> Option<&Entry> {
        self.entries.get(id)
    }
    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.values()
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn find_wire(&self, provider: &str, wire: &str) -> Option<&Entry> {
        self.entries
            .values()
            .find(|e| e.provider == provider && e.wire_model_id == wire)
    }

    /// Merge live discovery: known models are kept as is; unknown ones get conservative
    /// defaults and `limited_info` (03 §8.1 step 4).
    pub fn merge_discovered(&mut self, found: &[discovery::Discovered]) {
        for d in found {
            if self.find_wire(&d.provider, &d.wire_id).is_some() {
                continue;
            }
            let e = d.to_entry();
            self.entries.entry(e.id.clone()).or_insert(e);
        }
    }

    /// Apply a user override `models.<id>` (RFC 7396 merge patch over the entry JSON).
    /// An override for an unknown id creates a custom entry if it is complete.
    pub fn apply_override(&mut self, id: &str, patch: &serde_json::Value) -> Result<(), String> {
        let mut v = match self.entries.get(id) {
            Some(e) => serde_json::to_value(e).map_err(|e| e.to_string())?,
            None => serde_json::json!({"id": id}),
        };
        merge_patch(&mut v, patch);
        if let Some(o) = v.as_object_mut() {
            o.insert("id".into(), serde_json::json!(id));
        }
        let e: Entry = serde_json::from_value(v).map_err(|e| format!("override for {id}: {e}"))?;
        self.entries.insert(id.to_string(), e);
        Ok(())
    }

    pub fn apply_policy(&mut self, p: &Policy) {
        self.entries.retain(|id, e| {
            !p.hidden_providers.contains(&e.provider) && !p.hidden_models.contains(id)
        });
    }

    /// Small model for titles / compaction summaries: same provider, cheapest flagged
    /// model; `None` → caller falls back to the primary at lowest effort (t04).
    pub fn small_model(&self, provider: &str, task: &str) -> Option<&Entry> {
        self.entries
            .values()
            .filter(|e| e.provider == provider && !e.is_deprecated())
            .filter(|e| e.small_model_for.iter().any(|t| t == task))
            .min_by(|a, b| price_key(a).total_cmp(&price_key(b)))
    }

    /// Default embedding model per provider (SA-14).
    pub fn default_embedding(&self, provider: &str) -> Option<&Entry> {
        self.entries.values().find(|e| {
            e.provider == provider
                && e.capabilities
                    .embeddings
                    .as_ref()
                    .is_some_and(|x| x.default)
        })
    }
}

fn price_key(e: &Entry) -> f64 {
    e.pricing
        .as_ref()
        .map(|p| p.input + p.output)
        .unwrap_or(f64::MAX)
}

/// RFC 7396 JSON merge patch.
pub fn merge_patch(target: &mut serde_json::Value, patch: &serde_json::Value) {
    match (target.as_object_mut(), patch.as_object()) {
        (Some(t), Some(p)) => {
            for (k, v) in p {
                if v.is_null() {
                    t.remove(k);
                } else {
                    merge_patch(t.entry(k.clone()).or_insert(serde_json::Value::Null), v);
                }
            }
        }
        (None, Some(_)) => {
            *target = serde_json::Value::Object(Default::default());
            merge_patch(target, patch);
        }
        _ => *target = patch.clone(),
    }
}

/// Token counts for costing (mirrors `genie_providers::Usage`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tokens {
    pub non_cached_input: u64,
    pub cache_read: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
    pub output: u64,
    pub web_searches: u64,
}

/// Cost in USD = usage × catalog prices, including cache lines and search fees (D27, D36).
/// `None` when the entry has no pricing.
pub fn cost(p: &Pricing, t: &Tokens) -> f64 {
    let per = match p.per.as_str() {
        "1K" => 1_000.0,
        _ => 1_000_000.0,
    };
    (t.non_cached_input as f64 * p.input
        + t.cache_read as f64 * p.cache_read
        + t.cache_write_5m as f64 * p.cache_write_5m
        + t.cache_write_1h as f64 * p.cache_write_1h
        + t.output as f64 * p.output)
        / per
        + t.web_searches as f64 * p.web_search_per_call.unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn bundled_snapshot_decodes_cleanly() {
        let c = Catalog::bundled();
        assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
        assert!(c.len() >= 8);
        for e in c.entries() {
            assert!(e.context_window > 0 && e.max_output > 0, "{}", e.id);
            assert!(e.pricing.is_some(), "{}", e.id);
        }
    }

    #[test]
    fn bad_entries_are_dropped_not_the_catalog() {
        let doc = json!({"version": 3, "models": [
            {"id": "ok", "provider": "p", "protocol": "openai_chat", "wire_model_id": "ok", "future_field": 1},
            {"id": "bad-proto", "provider": "p", "protocol": "smoke_signals", "wire_model_id": "x"},
            {"id": "bad-type", "provider": "p", "protocol": "gemini", "wire_model_id": "x", "context_window": "big"},
            {"provider": "p", "protocol": "gemini", "wire_model_id": "x"}
        ]});
        let c = decode(&doc.to_string(), Source::Fetched).unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(c.version, 3);
        assert_eq!(c.diagnostics.len(), 3);
        assert!(decode("not json", Source::Fetched).is_err());
    }

    #[test]
    fn overrides_policy_and_discovery() {
        let mut c = Catalog::bundled();
        let id = c
            .entries()
            .find(|e| e.provider == "openai")
            .map(|e| e.id.clone())
            .unwrap();
        c.apply_override(
            &id,
            &json!({"context_window": 1234, "pricing": {"input": 9.0}}),
        )
        .unwrap();
        let e = c.get(&id).unwrap();
        assert_eq!(e.context_window, 1234);
        assert_eq!(e.pricing.as_ref().unwrap().input, 9.0);
        assert!(
            e.pricing.as_ref().unwrap().output > 0.0,
            "merge patch keeps other prices"
        );

        c.apply_override("my-vllm", &json!({"provider": "vllm", "protocol": "openai_chat", "wire_model_id": "llama", "context_window": 8192})).unwrap();
        assert_eq!(c.get("my-vllm").unwrap().context_window, 8192);
        assert!(
            c.apply_override("broken", &json!({"provider": "x"}))
                .is_err()
        );

        c.merge_discovered(&[discovery::Discovered {
            provider: "ollama".into(),
            wire_id: "qwen3:8b".into(),
            display_name: None,
            context_window: Some(40960),
            protocol: Protocol::OpenaiChat,
        }]);
        let d = c.get("ollama/qwen3:8b").unwrap();
        assert!(d.limited_info);
        assert_eq!(d.context_window, 40960);

        c.apply_policy(&Policy {
            hidden_providers: vec!["ollama".into()],
            hidden_models: vec!["my-vllm".into()],
        });
        assert!(c.get("ollama/qwen3:8b").is_none() && c.get("my-vllm").is_none());
    }

    #[test]
    fn effort_mapping_per_protocol() {
        let c = Catalog::bundled();
        let claude = c
            .entries()
            .find(|e| e.protocol == Protocol::AnthropicMessages)
            .unwrap();
        assert!(!claude.effort_levels().is_empty());
        assert_eq!(
            claude.effort_wire(Effort::High),
            Some(EffortWire::Effort("high".into()))
        );
        let gemini = c.get("google/gemini-3-pro").unwrap();
        assert!(matches!(
            gemini.effort_wire(Effort::High),
            Some(EffortWire::ThinkingLevel(_))
        ));
        let mut budget = gemini.clone();
        if let Some(r) = budget.capabilities.reasoning.as_mut() {
            r.wire.clear();
            r.budget = Some(entry::Budget {
                min: 1000,
                max: 11000,
            });
            r.adaptive = false;
        }
        assert_eq!(
            budget.effort_wire(Effort::Low),
            Some(EffortWire::Budget(2000))
        );
        assert_eq!(
            budget.effort_wire(Effort::Off),
            None,
            "Off only where listed"
        );
    }

    #[test]
    fn small_model_and_embeddings() {
        let c = Catalog::bundled();
        let s = c.small_model("anthropic", "title").unwrap();
        assert!(s.small_model_for.contains(&"title".to_string()));
        assert!(c.small_model("nobody", "title").is_none());
        assert!(c.default_embedding("openai").is_some());
        assert!(c.default_embedding("anthropic").is_none());
    }

    #[test]
    fn cost_includes_cache_lines_and_search() {
        let p = Pricing {
            input: 3.0,
            output: 15.0,
            cache_read: 0.3,
            cache_write_5m: 3.75,
            cache_write_1h: 6.0,
            web_search_per_call: Some(0.01),
            per: "1M".into(),
        };
        let t = Tokens {
            non_cached_input: 1_000_000,
            cache_read: 1_000_000,
            cache_write_5m: 0,
            cache_write_1h: 1_000_000,
            output: 100_000,
            web_searches: 2,
        };
        let c = cost(&p, &t);
        assert!((c - (3.0 + 0.3 + 6.0 + 1.5 + 0.02)).abs() < 1e-9, "{c}");
    }
}
