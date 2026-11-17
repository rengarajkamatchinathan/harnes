//! Routes, endpoints and provider presets (03 §5).
//!
//! A preset is data: a route template + default `Compat` per protocol (D26).

use std::sync::Arc;

use crate::auth::Auth;
use crate::compat::{
    CacheFormat, Compat, MaxTokensField, ModelSpec, ReasoningField, SchemaSanitizer, ToolIdRule,
};
use crate::compile::RouteInfo;
use crate::error::ProviderError;
use crate::model::{AccountTag, ProtocolKind, ProviderId};
use crate::protocol::{ErasedProtocol, for_kind};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VersionPlacement {
    /// e.g. `anthropic-version`, value from the effective compat.
    Header(String),
    None,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    /// Scheme + host (+ optional base path), no trailing slash.
    pub base: String,
    /// Path template; `{model}` is replaced by the wire model id.
    pub path: String,
    pub query: Option<String>,
    pub version: VersionPlacement,
}

impl Endpoint {
    pub fn url(&self, model: &str) -> String {
        let path = self.path.replace("{model}", model);
        match &self.query {
            Some(q) => format!("{}{}?{}", self.base.trim_end_matches('/'), path, q),
            None => format!("{}{}", self.base.trim_end_matches('/'), path),
        }
    }
    pub fn host(&self) -> &str {
        let s = self.base.split("://").nth(1).unwrap_or(&self.base);
        s.split('/').next().unwrap_or(s)
    }
}

#[derive(Clone)]
pub struct Route {
    pub provider: ProviderId,
    pub protocol: Arc<dyn ErasedProtocol>,
    pub endpoint: Endpoint,
    pub auth: Arc<dyn Auth>,
    /// Route compat with the model's patches applied.
    pub compat: Compat,
    pub headers: Vec<(String, String)>,
    pub account: Option<AccountTag>,
    pub model: ModelSpec,
}

impl std::fmt::Debug for Route {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Route")
            .field("provider", &self.provider)
            .field("protocol", &self.protocol.kind())
            .field("endpoint", &self.endpoint)
            .field("model", &self.model.wire_id)
            .finish_non_exhaustive()
    }
}

impl Route {
    pub fn route_key(&self) -> String {
        format!(
            "{}|{}{}|{}",
            self.protocol.kind().as_str(),
            self.endpoint.host(),
            self.endpoint.path,
            self.model.wire_id
        )
    }
    pub fn info(&self) -> RouteInfo {
        RouteInfo {
            provider: self.provider.clone(),
            protocol: self.protocol.kind(),
            replay: self.protocol.replay(),
            account: self.account,
            compat: self.compat.clone(),
            route_key: self.route_key(),
        }
    }
}

/// Binds a model to a [`Route`]. Discovery and the catalog live above (genie-catalog).
pub trait Provider: Send + Sync {
    fn id(&self) -> &ProviderId;
    fn route(&self, model: &ModelSpec) -> Result<Route, ProviderError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    Anthropic,
    OpenAi,
    AzureFoundry,
    GeminiApi,
    Vertex,
    Ollama,
    OpenRouter,
    /// vLLM and any other OpenAI-compatible server.
    OpenAiCompatible,
}

pub const OLLAMA_DEFAULT_BASE: &str = "http://127.0.0.1:11434";

/// What the user configured for one provider in Settings.
#[derive(Clone)]
pub struct ProviderConfig {
    pub id: ProviderId,
    pub preset: Preset,
    /// Required for Azure and custom endpoints; overrides the preset default otherwise.
    pub base_url: Option<String>,
    /// Vertex project / location.
    pub project: Option<String>,
    pub location: Option<String>,
    pub auth: Arc<dyn Auth>,
    pub account: Option<AccountTag>,
    /// Partial `Compat` JSON from settings (custom endpoints).
    pub compat_overlay: Option<serde_json::Value>,
    pub headers: Vec<(String, String)>,
}

pub struct PresetProvider(pub ProviderConfig);

fn anthropic_compat() -> Compat {
    Compat {
        anthropic_version: Some("2023-06-01".into()),
        max_cache_breakpoints: 4,
        tool_id_rule: ToolIdRule::AnthropicCharset,
        mid_conversation_system: false,
        ..Compat::default()
    }
}

impl PresetProvider {
    fn template(&self, protocol: ProtocolKind) -> Result<(Endpoint, Compat), ProviderError> {
        let c = &self.0;
        let unsupported = || {
            ProviderError::invalid_request(format!(
                "{:?} does not serve the {} protocol",
                c.preset,
                protocol.as_str()
            ))
        };
        let need_base = || {
            c.base_url
                .clone()
                .ok_or_else(|| ProviderError::invalid_request(format!("{} needs a base URL", c.id)))
        };
        let base_or = |d: &str| c.base_url.clone().unwrap_or_else(|| d.to_string());
        let ep = |base: String, path: &str| Endpoint {
            base,
            path: path.into(),
            query: None,
            version: VersionPlacement::None,
        };
        use ProtocolKind as P;
        Ok(match (c.preset, protocol) {
            (Preset::Anthropic, P::AnthropicMessages) => (
                Endpoint {
                    version: VersionPlacement::Header("anthropic-version".into()),
                    ..ep(base_or("https://api.anthropic.com"), "/v1/messages")
                },
                anthropic_compat(),
            ),
            (Preset::OpenAi, P::OpenaiResponses) => (
                ep(base_or("https://api.openai.com"), "/v1/responses"),
                Compat::default(),
            ),
            (Preset::OpenAi, P::OpenaiChat) => (
                ep(base_or("https://api.openai.com"), "/v1/chat/completions"),
                Compat {
                    max_tokens_field: MaxTokensField::MaxCompletionTokens,
                    ..Compat::default()
                },
            ),
            (Preset::AzureFoundry, P::OpenaiResponses) => (
                ep(need_base()?, "/openai/v1/responses"),
                Compat {
                    prefer_responses: true,
                    rate_limit_headers: false,
                    ..Compat::default()
                },
            ),
            (Preset::AzureFoundry, P::OpenaiChat) => (
                ep(need_base()?, "/openai/v1/chat/completions"),
                Compat {
                    max_tokens_field: MaxTokensField::MaxCompletionTokens,
                    rate_limit_headers: false,
                    ..Compat::default()
                },
            ),
            (Preset::AzureFoundry, P::AnthropicMessages) => (
                Endpoint {
                    version: VersionPlacement::Header("anthropic-version".into()),
                    ..ep(need_base()?, "/anthropic/v1/messages")
                },
                Compat {
                    rate_limit_headers: false,
                    ..anthropic_compat()
                },
            ),
            (Preset::GeminiApi, P::Gemini) => (
                Endpoint {
                    query: Some("alt=sse".into()),
                    ..ep(
                        base_or("https://generativelanguage.googleapis.com"),
                        "/v1beta/models/{model}:streamGenerateContent",
                    )
                },
                Compat {
                    schema_sanitizer: Some(SchemaSanitizer::Gemini),
                    ..Compat::default()
                },
            ),
            (Preset::Vertex, P::Gemini | P::AnthropicMessages) => {
                let project = c
                    .project
                    .clone()
                    .ok_or_else(|| ProviderError::invalid_request("Vertex needs a project"))?;
                let location = c.location.clone().unwrap_or_else(|| "global".into());
                let host = if location == "global" {
                    "https://aiplatform.googleapis.com".to_string()
                } else {
                    format!("https://{location}-aiplatform.googleapis.com")
                };
                let base = c.base_url.clone().unwrap_or(host);
                if protocol == P::Gemini {
                    (
                        Endpoint {
                            query: Some("alt=sse".into()),
                            ..ep(
                                base,
                                &format!(
                                    "/v1/projects/{project}/locations/{location}/publishers/google/models/{{model}}:streamGenerateContent"
                                ),
                            )
                        },
                        Compat {
                            schema_sanitizer: Some(SchemaSanitizer::Gemini),
                            ..Compat::default()
                        },
                    )
                } else {
                    (
                        ep(
                            base,
                            &format!(
                                "/v1/projects/{project}/locations/{location}/publishers/anthropic/models/{{model}}:streamRawPredict"
                            ),
                        ),
                        Compat {
                            anthropic_version: Some("vertex-2023-10-16".into()),
                            version_in_body: true,
                            ..anthropic_compat()
                        },
                    )
                }
            }
            (Preset::Ollama, P::OpenaiChat) => (
                ep(base_or(OLLAMA_DEFAULT_BASE), "/v1/chat/completions"),
                Compat {
                    reasoning_field: Some(ReasoningField::Reasoning),
                    baseline_as_user: false,
                    ..Compat::default()
                },
            ),
            (Preset::OpenRouter, P::OpenaiChat) => (
                ep(base_or("https://openrouter.ai/api"), "/v1/chat/completions"),
                Compat {
                    reasoning_field: Some(ReasoningField::ReasoningDetails),
                    cache_format: Some(CacheFormat::Anthropic),
                    max_cache_breakpoints: 4,
                    ..Compat::default()
                },
            ),
            (Preset::OpenAiCompatible, P::OpenaiChat) => (
                ep(need_base()?, "/v1/chat/completions"),
                Compat {
                    reasoning_field: Some(ReasoningField::ReasoningContent),
                    ..Compat::default()
                },
            ),
            _ => return Err(unsupported()),
        })
    }
}

impl Provider for PresetProvider {
    fn id(&self) -> &ProviderId {
        &self.0.id
    }

    fn route(&self, model: &ModelSpec) -> Result<Route, ProviderError> {
        let protocol = for_kind(model.protocol).ok_or_else(|| {
            ProviderError::invalid_request(format!(
                "{} is not supported in v1",
                model.protocol.as_str()
            ))
        })?;
        let (endpoint, mut compat) = self.template(model.protocol)?;
        if let Some(o) = &self.0.compat_overlay {
            compat.overlay(o);
        }
        let compat = model.effective_compat(&compat);
        let mut headers = self.0.headers.clone();
        for p in &model.patches {
            headers.extend(p.headers.iter().cloned());
        }
        Ok(Route {
            provider: self.0.id.clone(),
            protocol,
            endpoint,
            auth: self.0.auth.clone(),
            compat,
            headers,
            account: self.0.account,
            model: model.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::NoAuth;

    fn cfg(preset: Preset) -> ProviderConfig {
        ProviderConfig {
            id: ProviderId::new("p"),
            preset,
            base_url: None,
            project: None,
            location: None,
            auth: Arc::new(NoAuth),
            account: None,
            compat_overlay: None,
            headers: vec![],
        }
    }

    #[test]
    fn urls_per_preset() {
        let r = PresetProvider(cfg(Preset::Anthropic))
            .route(&ModelSpec::new(
                "p",
                "claude-x",
                ProtocolKind::AnthropicMessages,
            ))
            .unwrap();
        assert_eq!(
            r.endpoint.url("claude-x"),
            "https://api.anthropic.com/v1/messages"
        );

        let r = PresetProvider(cfg(Preset::GeminiApi))
            .route(&ModelSpec::new("p", "gemini-x", ProtocolKind::Gemini))
            .unwrap();
        assert_eq!(
            r.endpoint.url("gemini-x"),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-x:streamGenerateContent?alt=sse"
        );

        let mut c = cfg(Preset::Vertex);
        c.project = Some("proj".into());
        c.location = Some("us-east5".into());
        let r = PresetProvider(c)
            .route(&ModelSpec::new(
                "p",
                "claude-x",
                ProtocolKind::AnthropicMessages,
            ))
            .unwrap();
        assert_eq!(
            r.endpoint.url("claude-x"),
            "https://us-east5-aiplatform.googleapis.com/v1/projects/proj/locations/us-east5/publishers/anthropic/models/claude-x:streamRawPredict"
        );
        assert!(r.compat.version_in_body);

        let r = PresetProvider(cfg(Preset::Ollama))
            .route(&ModelSpec::new("p", "llama", ProtocolKind::OpenaiChat))
            .unwrap();
        assert_eq!(
            r.endpoint.url("llama"),
            "http://127.0.0.1:11434/v1/chat/completions"
        );
    }

    #[test]
    fn azure_needs_base_and_rejects_gemini() {
        let p = PresetProvider(cfg(Preset::AzureFoundry));
        assert!(
            p.route(&ModelSpec::new("p", "dep", ProtocolKind::OpenaiResponses))
                .is_err()
        );
        let mut c = cfg(Preset::AzureFoundry);
        c.base_url = Some("https://res.services.ai.azure.com".into());
        let p = PresetProvider(c);
        let r = p
            .route(&ModelSpec::new("p", "dep", ProtocolKind::OpenaiResponses))
            .unwrap();
        assert_eq!(
            r.endpoint.url("dep"),
            "https://res.services.ai.azure.com/openai/v1/responses"
        );
        assert!(r.compat.prefer_responses);
        assert!(
            p.route(&ModelSpec::new("p", "g", ProtocolKind::Gemini))
                .is_err()
        );
        assert!(
            p.route(&ModelSpec::new("p", "b", ProtocolKind::BedrockConverse))
                .is_err()
        );
    }

    #[test]
    fn route_key_identifies_protocol_host_and_model() {
        let r = PresetProvider(cfg(Preset::OpenAi))
            .route(&ModelSpec::new("p", "gpt", ProtocolKind::OpenaiResponses))
            .unwrap();
        assert_eq!(
            r.route_key(),
            "openai_responses|api.openai.com/v1/responses|gpt"
        );
    }
}
