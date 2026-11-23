//! Image generation ports for `image_generate` (genie-tools `image`).
//!
//! - [`EngineImages`] is the [`ImageBackend`]: it reads `imageGeneration.enabled` (policy
//!   `false` wins, then Settings, default on) and `imageGeneration.model`
//!   (`<provider>/<model>`, default [`DEFAULT_MODEL`]), takes the provider's
//!   OpenAI-compatible API root and credentials from the [`ModelResolver`]
//!   ([`ModelResolver::provider_api`]) and calls `…/images/generations` with
//!   [`OpenAiImages`]. Works for OpenAI, Azure AI Foundry (the model is the deployment
//!   name) and OpenAI-compatible providers that serve the endpoint.
//! - [`BlobImages`] is the [`ImageStore`]: the bytes go to the store's blobs, which the
//!   desktop reads back with `blob.get`.

use std::sync::Arc;

use async_trait::async_trait;
use genie_providers::executor::Transport;
use genie_providers::images::{ImageRequest, OpenAiImages};
use genie_tools::ToolEnv;
use genie_tools::image::{ImageBackend, ImageBytes, ImageSpec, ImageStore};
use tokio_util::sync::CancellationToken;

use crate::config::ConfigHub;
use crate::runtime::ports::ModelResolver;

/// Used when `imageGeneration.model` is not set.
pub const DEFAULT_MODEL: &str = "openai/gpt-image-1";

pub const KEY_ENABLED: &str = "imageGeneration.enabled";
pub const KEY_MODEL: &str = "imageGeneration.model";

/// The image backend over the configured provider.
pub struct EngineImages {
    models: Arc<dyn ModelResolver>,
    config: Option<Arc<ConfigHub>>,
    transport: Arc<dyn Transport>,
}

impl EngineImages {
    pub fn new(
        models: Arc<dyn ModelResolver>,
        config: Option<Arc<ConfigHub>>,
        transport: Arc<dyn Transport>,
    ) -> Self {
        EngineImages {
            models,
            config,
            transport,
        }
    }

    /// `imageGeneration.enabled`: policy `false` wins, then Settings, default on.
    fn enabled(&self) -> Result<(), String> {
        let Some(hub) = &self.config else {
            return Ok(());
        };
        if hub.policy().bool(KEY_ENABLED) == Some(false) {
            return Err("image generation is disabled by your organization's policy".into());
        }
        if hub.value(None, KEY_ENABLED).and_then(|v| v.as_bool()) == Some(false) {
            return Err("image generation is turned off in Settings → Capabilities".into());
        }
        Ok(())
    }

    /// `(provider, wire model)` from `imageGeneration.model`.
    fn model(&self) -> (String, String) {
        let raw = self
            .config
            .as_ref()
            .and_then(|h| h.value(None, KEY_MODEL))
            .and_then(|v| v.as_str().map(str::trim).map(str::to_string))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_MODEL.to_string());
        split_model(&raw)
    }

    fn client(&self) -> Result<OpenAiImages, String> {
        self.enabled()?;
        let (provider, wire) = self.model();
        let api = self.models.provider_api(&provider).map_err(|e| {
            format!(
                "image generation needs the `{provider}` provider set up: {}",
                e.message
            )
        })?;
        Ok(OpenAiImages::new(
            self.transport.clone(),
            api.auth,
            format!("{}/images/generations", api.base),
            wire,
        )
        .with_headers(api.headers))
    }
}

/// `openai/gpt-image-1` → (`openai`, `gpt-image-1`); a bare model id means OpenAI.
fn split_model(raw: &str) -> (String, String) {
    match raw.split_once('/') {
        Some((p, m)) if !p.is_empty() && !m.is_empty() => (p.to_string(), m.to_string()),
        _ => ("openai".to_string(), raw.trim_matches('/').to_string()),
    }
}

#[async_trait]
impl ImageBackend for EngineImages {
    fn status(&self, _env: &ToolEnv) -> Result<(), String> {
        self.client().map(|_| ())
    }

    fn model_label(&self) -> String {
        let (p, m) = self.model();
        format!("{p}/{m}")
    }

    async fn generate(
        &self,
        spec: &ImageSpec,
        cancel: &CancellationToken,
    ) -> Result<Vec<ImageBytes>, String> {
        let client = self.client()?;
        let req = ImageRequest {
            prompt: spec.prompt.clone(),
            size: spec.size.clone(),
            quality: spec.quality.clone(),
            output_format: spec.output_format.clone(),
            background: spec.background.clone(),
            n: spec.n,
        };
        let out = client.generate(&req, cancel).await.map_err(|e| e.message)?;
        Ok(out
            .into_iter()
            .map(|i| ImageBytes {
                bytes: i.bytes,
                mime: i.mime,
                revised_prompt: i.revised_prompt,
            })
            .collect())
    }
}

/// Generated images as store blobs.
pub struct BlobImages(pub Arc<genie_store::Store>);

impl ImageStore for BlobImages {
    fn put(&self, _session_id: &str, bytes: &[u8], mime: &str) -> Result<String, String> {
        self.0
            .put_blob(bytes, Some(mime))
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::runtime::ports::{ProviderApi, ResolveError, ResolvedModel};
    use base64::Engine as _;
    use genie_protocol::common::SessionKind;
    use genie_providers::auth::NoAuth;
    use genie_providers::cassette::{CassetteTransport, RecordedResponse};

    struct Api(bool);
    impl ModelResolver for Api {
        fn resolve(&self, _m: &str) -> Result<ResolvedModel, ResolveError> {
            unreachable!()
        }
        fn provider_api(&self, provider: &str) -> Result<ProviderApi, ResolveError> {
            if !self.0 {
                return Err(ResolveError {
                    class: "no_provider_configured",
                    message: format!("no credential for {provider}"),
                    fix_link: None,
                });
            }
            Ok(ProviderApi {
                provider: provider.into(),
                base: "https://api.openai.com/v1".into(),
                auth: Arc::new(NoAuth),
                headers: vec![],
            })
        }
    }

    #[test]
    fn model_ids() {
        assert_eq!(
            split_model("azure/my-image-deploy"),
            ("azure".into(), "my-image-deploy".into())
        );
        assert_eq!(
            split_model("gpt-image-1"),
            ("openai".into(), "gpt-image-1".into())
        );
    }

    #[tokio::test]
    async fn generates_through_the_provider_api() {
        let png = b"\x89PNG\r\n\x1a\nxx";
        let body = serde_json::json!({"data": [{
            "b64_json": base64::engine::general_purpose::STANDARD.encode(png)
        }]})
        .to_string();
        let t = Arc::new(CassetteTransport::new(vec![RecordedResponse::json(
            200,
            &body,
            vec![],
        )]));
        let b = EngineImages::new(Arc::new(Api(true)), None, t.clone());
        let env = ToolEnv::new("s", SessionKind::Chat);
        assert!(b.status(&env).is_ok());
        assert_eq!(b.model_label(), DEFAULT_MODEL);
        let out = b
            .generate(
                &ImageSpec {
                    prompt: "a fox".into(),
                    n: 1,
                    ..Default::default()
                },
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(out[0].bytes, png);
        assert_eq!(out[0].mime, "image/png");
        assert_eq!(
            t.recorded()[0].url,
            "https://api.openai.com/v1/images/generations"
        );

        let off = EngineImages::new(Arc::new(Api(false)), None, t);
        let reason = off.status(&env).unwrap_err();
        assert!(reason.contains("no credential for openai"), "{reason}");
    }
}
