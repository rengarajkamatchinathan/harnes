//! Catalog → provider wiring (03 §8, 01 §2.1 rule 3: catalog and providers never see each
//! other; core converts).
//!
//! [`model_spec`] turns a catalog [`Entry`] into a [`ModelSpec`]; [`CatalogResolver`]
//! binds a catalog model id to a preset route with the credential from the credential
//! store or the environment (03 §10.1 precedence). Provider settings, Azure / Vertex /
//! custom endpoints and OAuth flows come with the provider settings UI (spec 03/08).

// `ProviderError` is a flat, inspectable struct (03 §7), as in `genie-providers`.
#![allow(clippy::result_large_err)]

use std::sync::{Arc, RwLock};

use genie_catalog::Catalog;
use genie_catalog::entry::{Effort, EffortWire, Entry, Protocol};
use genie_providers::auth::{ApiKeyHeader, BearerKey, NoAuth};
use genie_providers::compat::{
    self as pc, EffortMapping, InputModalities, ModelPatch, ModelSpec, PromptCacheCaps,
};
use genie_providers::executor::{Executor, LlmClient};
use genie_providers::model::{EffortLevel, ProtocolKind, ProviderId, SamplingParam};
use genie_providers::route::{Preset, PresetProvider, ProviderConfig};
use genie_providers::{Auth, Provider};
use genie_secrets::credentials::{self, CredentialStore};

use crate::runtime::ports::{ModelResolver, ProviderApi, ResolveError, ResolvedModel};

fn protocol(p: Protocol) -> Option<ProtocolKind> {
    Some(match p {
        Protocol::AnthropicMessages => ProtocolKind::AnthropicMessages,
        Protocol::OpenaiResponses => ProtocolKind::OpenaiResponses,
        Protocol::OpenaiChat => ProtocolKind::OpenaiChat,
        Protocol::Gemini => ProtocolKind::Gemini,
        Protocol::BedrockConverse => ProtocolKind::BedrockConverse,
        Protocol::Unknown => return None,
    })
}

fn level(e: Effort) -> EffortLevel {
    match e {
        Effort::Off => EffortLevel::Off,
        Effort::Low => EffortLevel::Low,
        Effort::Medium => EffortLevel::Medium,
        Effort::High => EffortLevel::High,
        Effort::Max => EffortLevel::Max,
    }
}

/// Parse a session's effort string (`low`, `medium` …).
pub fn parse_effort(s: &str) -> Option<EffortLevel> {
    Some(match s.trim().to_ascii_lowercase().as_str() {
        "off" | "none" => EffortLevel::Off,
        "low" => EffortLevel::Low,
        "medium" => EffortLevel::Medium,
        "high" => EffortLevel::High,
        "max" => EffortLevel::Max,
        _ => return None,
    })
}

/// Catalog entry → the adapter's view of the model.
pub fn model_spec(e: &Entry) -> Option<ModelSpec> {
    let protocol = protocol(e.protocol)?;
    let mut s = ModelSpec::new(&e.provider, &e.wire_model_id, protocol);
    s.catalog_id.clone_from(&e.id);
    s.family.clone_from(&e.family);
    s.api_version.clone_from(&e.api_version);
    s.max_output = Some(e.max_output).filter(|m| *m > 0);
    s.context_window = Some(e.context_window).filter(|w| *w > 0);
    let has = |m: &str| e.input_modalities.iter().any(|x| x == m);
    s.input = InputModalities {
        image: e.capabilities.vision || has("image"),
        pdf: e.capabilities.pdf || has("pdf"),
    };
    s.sampling_supported = e
        .sampling_supported
        .iter()
        .map(|p| match p.as_str() {
            "temperature" => SamplingParam::Temperature,
            "top_p" => SamplingParam::TopP,
            "top_k" => SamplingParam::TopK,
            "stop" => SamplingParam::Stop,
            _ => SamplingParam::Unknown,
        })
        .collect();
    s.effort = e
        .effort_levels()
        .into_iter()
        .filter_map(|l| {
            let wire = match e.effort_wire(l)? {
                EffortWire::Effort(v) => pc::EffortWire::Effort(v),
                EffortWire::Budget(n) => pc::EffortWire::Budget(n),
                EffortWire::ThinkingLevel(v) => pc::EffortWire::ThinkingLevel(v),
                EffortWire::Off => pc::EffortWire::Off,
            };
            Some(EffortMapping {
                level: level(l),
                wire,
            })
        })
        .collect();
    let pc_caps = &e.capabilities.prompt_cache;
    s.prompt_cache = PromptCacheCaps {
        auto: pc_caps.auto,
        breakpoints: pc_caps.breakpoints,
        ttl_1h: pc_caps.ttl_1h,
    };
    s.web_search_native = e.capabilities.web_search_native;
    s.code_execution_native = e.capabilities.code_execution_native;
    s.patches = e
        .quirks
        .iter()
        .map(|q| ModelPatch {
            compat: q.compat.clone(),
            body: q.body.clone(),
            headers: q.headers.clone(),
        })
        .collect();
    Some(s)
}

/// Cost in USD of one usage line (D27, D34).
pub fn cost(p: Option<&genie_catalog::Pricing>, u: &genie_providers::model::Usage) -> f64 {
    p.map_or(0.0, |p| {
        genie_catalog::cost(
            p,
            &genie_catalog::Tokens {
                non_cached_input: u.non_cached_input,
                cache_read: u.cache_read,
                cache_write_5m: u.cache_write_5m,
                cache_write_1h: u.cache_write_1h,
                output: u.output_total,
                web_searches: 0,
            },
        )
    })
}

fn display_provider(p: &str) -> String {
    match p {
        "anthropic" => "Anthropic".into(),
        "openai" => "OpenAI".into(),
        "google" => "Google".into(),
        "ollama" => "Ollama".into(),
        "openrouter" => "OpenRouter".into(),
        other => other.to_string(),
    }
}

type EnvFn = dyn Fn(&str) -> Option<String> + Send + Sync;

/// Binds providers that need more than a preset + key: OAuth (Entra), Google ADC,
/// endpoints from Settings (Azure, Vertex, custom). Implemented by the daemon's provider
/// hub (03 §10); `None` falls back to the built-in preset + key path.
pub trait ProviderBinder: Send + Sync {
    fn bind(&self, provider: &str) -> Option<Result<ProviderConfig, ResolveError>>;
}

/// Resolves catalog ids with preset routes and stored/env credentials.
pub struct CatalogResolver {
    /// Shared so the catalog refresh, discovery and overrides apply without a restart.
    catalog: Arc<RwLock<Catalog>>,
    credentials: Arc<dyn CredentialStore>,
    env: Arc<EnvFn>,
    executor: Arc<Executor>,
    binder: Option<Arc<dyn ProviderBinder>>,
}

impl CatalogResolver {
    pub fn new(
        catalog: Catalog,
        credentials: Arc<dyn CredentialStore>,
        executor: Arc<Executor>,
    ) -> Self {
        CatalogResolver {
            catalog: Arc::new(RwLock::new(catalog)),
            credentials,
            env: Arc::new(|k| std::env::var(k).ok()),
            executor,
            binder: None,
        }
    }

    /// Read the model catalog from a shared, updatable copy.
    pub fn with_shared_catalog(mut self, catalog: Arc<RwLock<Catalog>>) -> Self {
        self.catalog = catalog;
        self
    }

    /// Bind OAuth / ADC / Settings-endpoint providers through `binder` first.
    pub fn with_binder(mut self, binder: Arc<dyn ProviderBinder>) -> Self {
        self.binder = Some(binder);
        self
    }

    /// The bundled catalog over live HTTP (rustls), with credentials from the environment
    /// until provider settings and the keychain land (spec 03 §10, spec 08).
    pub fn live() -> Result<Self, genie_providers::ProviderError> {
        let exec = Executor::new(
            Arc::new(genie_providers::executor::ReqwestTransport::new()?),
            genie_providers::RetryPolicy::default(),
        );
        Ok(Self::new(
            Catalog::bundled(),
            Arc::new(credentials::MemoryStore::default()),
            Arc::new(exec),
        ))
    }

    /// Replace the environment lookup (tests).
    pub fn with_env(mut self, env: Arc<EnvFn>) -> Self {
        self.env = env;
        self
    }

    fn entry(&self, model: &str) -> Option<Entry> {
        let cat = self.catalog.read().unwrap_or_else(|p| p.into_inner());
        cat.get(model)
            .or_else(|| {
                let (p, w) = model.split_once('/')?;
                cat.find_wire(p, w)
            })
            .cloned()
    }

    fn bind(&self, e: &Entry) -> Result<ResolvedModel, ResolveError> {
        let spec = model_spec(e).ok_or_else(|| ResolveError {
            class: "invalid_request",
            message: format!("{} uses a protocol this build can't speak", e.id),
            fix_link: None,
        })?;
        if let Some(cfg) = self.binder.as_ref().and_then(|b| b.bind(&e.provider)) {
            return self.finish(e, &spec, PresetProvider(cfg?));
        }
        let preset = match e.provider.as_str() {
            "anthropic" => Preset::Anthropic,
            "openai" => Preset::OpenAi,
            "google" | "gemini" => Preset::GeminiApi,
            "ollama" => Preset::Ollama,
            "openrouter" => Preset::OpenRouter,
            other => {
                return Err(ResolveError {
                    class: "no_provider_configured",
                    message: format!("provider {other} needs to be set up in Settings"),
                    fix_link: Some("genie://settings/providers".into()),
                });
            }
        };
        let auth: Arc<dyn Auth> = if preset == Preset::Ollama {
            Arc::new(NoAuth)
        } else {
            let env = self.env.clone();
            let cred =
                credentials::resolve(self.credentials.as_ref(), &e.provider, &move |k| env(k))
                    .ok_or_else(|| ResolveError {
                        class: "no_provider_configured",
                        message: format!("no credential for {}", e.provider),
                        fix_link: Some("genie://settings/providers".into()),
                    })?;
            match preset {
                Preset::Anthropic => Arc::new(ApiKeyHeader {
                    header: http::HeaderName::from_static("x-api-key"),
                    key: cred.secret,
                }),
                Preset::GeminiApi => Arc::new(ApiKeyHeader {
                    header: http::HeaderName::from_static("x-goog-api-key"),
                    key: cred.secret,
                }),
                _ => Arc::new(BearerKey(cred.secret)),
            }
        };
        let provider = PresetProvider(ProviderConfig {
            id: ProviderId::new(e.provider.clone()),
            preset,
            base_url: None,
            project: None,
            location: None,
            auth,
            account: None,
            compat_overlay: None,
            headers: vec![],
        });
        self.finish(e, &spec, provider)
    }

    fn finish(
        &self,
        e: &Entry,
        spec: &ModelSpec,
        provider: PresetProvider,
    ) -> Result<ResolvedModel, ResolveError> {
        let route = provider.route(spec).map_err(|err| ResolveError {
            class: "invalid_request",
            message: err.message,
            fix_link: None,
        })?;
        let client: Arc<dyn LlmClient + Send + Sync> = self.executor.clone();
        Ok(ResolvedModel {
            route,
            client,
            display_name: e.display_name.clone(),
            provider_display_name: display_provider(&e.provider),
            pricing: e.pricing.clone(),
            native_compaction: e.capabilities.native_compaction,
        })
    }
}

impl ModelResolver for CatalogResolver {
    fn resolve(&self, model: &str) -> Result<ResolvedModel, ResolveError> {
        let e = self.entry(model).ok_or_else(|| ResolveError {
            class: "model_not_found",
            message: format!("{model} is not in the model catalog"),
            fix_link: Some("genie://settings/models".into()),
        })?;
        self.bind(&e)
    }

    fn small_model(&self, model: &str) -> Result<ResolvedModel, ResolveError> {
        let provider = self
            .entry(model)
            .map(|e| e.provider.clone())
            .unwrap_or_default();
        let small = self
            .catalog
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .small_model(&provider, "title")
            .cloned();
        match small {
            Some(e) => self.bind(&e).or_else(|_| self.resolve(model)),
            None => self.resolve(model),
        }
    }

    /// Bind any OpenAI-protocol model of `provider` and keep its API root and auth.
    fn provider_api(&self, provider: &str) -> Result<ProviderApi, ResolveError> {
        let entries: Vec<Entry> = self
            .catalog
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .entries()
            .filter(|e| {
                e.provider == provider
                    && matches!(e.protocol, Protocol::OpenaiResponses | Protocol::OpenaiChat)
            })
            .cloned()
            .collect();
        let mut last = ResolveError {
            class: "no_provider_configured",
            message: format!("{provider} has no OpenAI-compatible model in the catalog"),
            fix_link: Some("genie://settings/providers".into()),
        };
        for e in &entries {
            match self.bind(e) {
                Ok(m) => {
                    let url = m.route.endpoint.url(&m.route.model.wire_id);
                    if let Some(base) = ProviderApi::base_of(&url) {
                        return Ok(ProviderApi {
                            provider: provider.to_string(),
                            base,
                            auth: m.route.auth.clone(),
                            headers: m.route.headers.clone(),
                        });
                    }
                }
                // A missing credential is the same for every model of the provider.
                Err(err) if err.class == "no_provider_configured" => return Err(err),
                Err(err) => last = err,
            }
        }
        Err(last)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use genie_providers::retry::RetryPolicy;

    fn resolver(env: &'static [(&'static str, &'static str)]) -> CatalogResolver {
        let ex = Arc::new(Executor::new(
            Arc::new(genie_providers::cassette::CassetteTransport::default()),
            RetryPolicy::default(),
        ));
        CatalogResolver::new(
            Catalog::bundled(),
            Arc::new(credentials::MemoryStore::default()),
            ex,
        )
        .with_env(Arc::new(move |k| {
            env.iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| (*v).to_string())
        }))
    }

    #[test]
    fn bundled_entries_convert_and_resolve_with_env_keys() {
        let r = resolver(&[("ANTHROPIC_API_KEY", "sk-test")]);
        let m = r.resolve("anthropic/claude-opus-5-5").unwrap();
        assert_eq!(m.route.model.protocol, ProtocolKind::AnthropicMessages);
        assert!(m.route.model.context_window.is_some());
        assert_eq!(m.provider_display_name, "Anthropic");
        let e = r.resolve("openai/gpt-5.1").err().unwrap();
        assert_eq!(e.class, "no_provider_configured");
        assert_eq!(r.resolve("nope/x").err().unwrap().class, "model_not_found");
    }

    #[test]
    fn provider_api_for_image_and_other_rest_calls() {
        let r = resolver(&[("OPENAI_API_KEY", "sk-test")]);
        let api = r.provider_api("openai").ok().unwrap();
        assert_eq!(api.base, "https://api.openai.com/v1");
        let r = resolver(&[]);
        assert_eq!(
            r.provider_api("openai").err().unwrap().class,
            "no_provider_configured"
        );
        assert!(r.provider_api("anthropic").is_err());
        assert_eq!(
            ProviderApi::base_of("https://res.services.ai.azure.com/openai/v1/chat/completions"),
            Some("https://res.services.ai.azure.com/openai/v1".into())
        );
        assert_eq!(ProviderApi::base_of("https://x/v1/messages"), None);
    }
}
