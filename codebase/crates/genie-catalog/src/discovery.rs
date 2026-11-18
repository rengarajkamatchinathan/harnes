//! Live discovery parsers (03 §8.1 step 4). The HTTP calls go through the provider
//! Executor in `genie-core`; this module only turns response bodies into [`Discovered`].

use serde_json::Value;

use crate::entry::{Capabilities, Entry, Protocol};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Discovered {
    pub provider: String,
    pub wire_id: String,
    pub display_name: Option<String>,
    pub context_window: Option<u64>,
    pub protocol: Protocol,
}

/// Conservative defaults for a model the catalog doesn't know.
pub const DEFAULT_CONTEXT: u64 = 8_192;
pub const DEFAULT_MAX_OUTPUT: u32 = 4_096;

impl Discovered {
    pub fn to_entry(&self) -> Entry {
        Entry {
            id: format!("{}/{}", self.provider, self.wire_id),
            provider: self.provider.clone(),
            protocol: self.protocol,
            wire_model_id: self.wire_id.clone(),
            display_name: self
                .display_name
                .clone()
                .unwrap_or_else(|| self.wire_id.clone()),
            family: None,
            release_date: None,
            deprecated_at: None,
            context_window: self.context_window.unwrap_or(DEFAULT_CONTEXT),
            max_output: DEFAULT_MAX_OUTPUT,
            input_modalities: vec!["text".into()],
            output_modalities: vec!["text".into()],
            capabilities: Capabilities {
                tools: true,
                ..Default::default()
            },
            sampling_supported: vec![],
            api_version: None,
            quirks: vec![],
            pricing: None,
            small_model_for: vec![],
            limited_info: true,
        }
    }
}

fn arr<'a>(v: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    v.get(key).and_then(Value::as_array).into_iter().flatten()
}

/// OpenAI / OpenAI-compatible `GET /v1/models`: `{data:[{id}]}`.
pub fn openai_models(provider: &str, protocol: Protocol, body: &Value) -> Vec<Discovered> {
    arr(body, "data")
        .filter_map(|m| m.get("id").and_then(Value::as_str))
        .map(|id| Discovered {
            provider: provider.into(),
            wire_id: id.into(),
            display_name: None,
            context_window: None,
            protocol,
        })
        .collect()
}

/// Gemini `models.list`: `{models:[{name:"models/x", displayName, inputTokenLimit,
/// supportedGenerationMethods}]}`; only models that can `generateContent`.
pub fn gemini_models(provider: &str, body: &Value) -> Vec<Discovered> {
    arr(body, "models")
        .filter(|m| {
            arr(m, "supportedGenerationMethods").any(|g| g.as_str() == Some("generateContent"))
        })
        .filter_map(|m| {
            let name = m.get("name")?.as_str()?;
            Some(Discovered {
                provider: provider.into(),
                wire_id: name.trim_start_matches("models/").into(),
                display_name: m
                    .get("displayName")
                    .and_then(Value::as_str)
                    .map(String::from),
                context_window: m.get("inputTokenLimit").and_then(Value::as_u64),
                protocol: Protocol::Gemini,
            })
        })
        .collect()
}

/// Ollama `GET /api/tags`: `{models:[{name}]}`; context from `/api/show` via [`ollama_context`].
pub fn ollama_tags(provider: &str, body: &Value) -> Vec<Discovered> {
    arr(body, "models")
        .filter_map(|m| m.get("name").and_then(Value::as_str))
        .map(|n| Discovered {
            provider: provider.into(),
            wire_id: n.into(),
            display_name: None,
            context_window: None,
            protocol: Protocol::OpenaiChat,
        })
        .collect()
}

/// Ollama `POST /api/show`: `model_info` holds `<arch>.context_length`.
pub fn ollama_context(show: &Value) -> Option<u64> {
    show.get("model_info")?
        .as_object()?
        .iter()
        .find(|(k, _)| k.ends_with(".context_length"))
        .and_then(|(_, v)| v.as_u64())
}

/// Azure Foundry deployment entered by hand (no Entra management API): deployment name +
/// base model. Known base models inherit their catalog entry with the deployment as wire id.
pub fn azure_deployment(
    catalog: &crate::Catalog,
    provider: &str,
    deployment: &str,
    base_model: &str,
    protocol: Protocol,
) -> Entry {
    let exact = catalog
        .entries()
        .find(|e| e.wire_model_id == base_model || e.id == base_model);
    match exact.or_else(|| family_sibling(catalog, base_model)) {
        Some(base) => {
            let mut e = base.clone();
            let inherited = e.wire_model_id != base_model;
            e.id = format!("{provider}/{deployment}");
            e.provider = provider.into();
            e.wire_model_id = deployment.into();
            e.protocol = protocol;
            if inherited {
                // A newer base model the catalog doesn't list yet: borrow its closest
                // sibling's limits rather than the 8k "unknown model" default.
                e.display_name = format!("{deployment} ({base_model})");
                e.pricing = None;
                e.limited_info = true;
            }
            e
        }
        None => Discovered {
            provider: provider.into(),
            wire_id: deployment.into(),
            display_name: Some(format!("{deployment} ({base_model})")),
            context_window: None,
            protocol,
        }
        .to_entry(),
    }
}

/// The catalog entry closest to an unlisted base model of the same family: `gpt-5.2` →
/// the highest listed `gpt-5.x` (not a `-mini`/`-nano` variant). `None` when no entry
/// shares the family prefix (everything up to the last `.`).
fn family_sibling<'a>(catalog: &'a crate::Catalog, base_model: &str) -> Option<&'a Entry> {
    let (family, _) = base_model.rsplit_once('.')?;
    let prefix = format!("{family}.");
    let suffix_of = |id: &str| id.strip_prefix(&prefix).map(str::to_owned);
    catalog
        .entries()
        .filter_map(|e| suffix_of(&e.wire_model_id).map(|v| (v, e)))
        .filter(|(v, _)| v.chars().all(|c| c.is_ascii_digit()))
        .max_by_key(|(v, _)| v.parse::<u32>().unwrap_or(0))
        .map(|(_, e)| e)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parsers() {
        let o = openai_models(
            "openai",
            Protocol::OpenaiResponses,
            &json!({"object":"list","data":[{"id":"gpt-x","object":"model"}]}),
        );
        assert_eq!(o[0].wire_id, "gpt-x");
        let g = gemini_models(
            "google",
            &json!({"models":[
                {"name":"models/gemini-x","displayName":"Gemini X","inputTokenLimit":1048576,"supportedGenerationMethods":["generateContent","countTokens"]},
                {"name":"models/embed","supportedGenerationMethods":["embedContent"]}
            ]}),
        );
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].wire_id, "gemini-x");
        assert_eq!(g[0].context_window, Some(1048576));
        let t = ollama_tags("ollama", &json!({"models":[{"name":"llama3.2:3b"}]}));
        assert_eq!(t[0].wire_id, "llama3.2:3b");
        assert_eq!(
            ollama_context(&json!({"model_info":{"llama.context_length":131072}})),
            Some(131072)
        );
        assert!(openai_models("x", Protocol::OpenaiChat, &json!({"weird": true})).is_empty());
    }

    #[test]
    fn azure_deployment_inherits_base_model() {
        let c = crate::Catalog::bundled();
        let base = c
            .entries()
            .find(|e| e.provider == "openai")
            .unwrap()
            .wire_model_id
            .clone();
        let e = azure_deployment(&c, "azure", "my-dep", &base, Protocol::OpenaiResponses);
        assert_eq!(e.wire_model_id, "my-dep");
        assert!(!e.limited_info);
        let u = azure_deployment(&c, "azure", "grok-dep", "grok-9", Protocol::OpenaiChat);
        assert!(u.limited_info);
        assert_eq!(u.context_window, DEFAULT_CONTEXT);
    }

    #[test]
    fn azure_deployment_of_newer_model_borrows_family_limits() {
        let c = crate::Catalog::bundled();
        let e = azure_deployment(
            &c,
            "azure",
            "gpt-5.2-atg",
            "gpt-5.2",
            Protocol::OpenaiResponses,
        );
        assert_eq!(e.wire_model_id, "gpt-5.2-atg");
        assert_eq!(e.display_name, "gpt-5.2-atg (gpt-5.2)");
        assert!(e.limited_info);
        assert!(e.pricing.is_none());
        assert!(
            e.context_window > DEFAULT_CONTEXT,
            "inherits a gpt-5.x window, got {}",
            e.context_window
        );
    }
}
