//! Image generation: the `image_generate` tool.
//!
//! The tool owns validation, authorisation and the per-turn budget; what it needs from
//! other subsystems are two ports implemented by `genie-core`:
//!
//! - [`ImageBackend`]: turns a prompt into image bytes (an OpenAI-compatible
//!   `/images/generations` client on the provider chosen in Settings → Capabilities), and
//!   says whether image generation is available at all (setting, policy, provider).
//! - [`ImageStore`]: keeps the bytes as a blob; the desktop renders the blob ids the tool
//!   returns inline in the tool row.
//!
//! The model gets back JSON with the blob ids, sizes and any revised prompt — never the
//! pixels — so the result is cheap to keep in context.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use genie_protocol::ask::Preview;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::registry::{parse_input, schema_of};
use crate::tool::{
    Availability, PermissionRequest, Tool, ToolCtx, ToolEnv, ToolError, ToolKind, ToolOutput,
};

pub const NAME: &str = "image_generate";
/// Calls per turn.
pub const DEFAULT_MAX_USES: u32 = 4;
/// Images per call.
pub const MAX_IMAGES: u32 = 4;
/// Prompt length cap (gpt-image accepts 32 000 characters; dall-e-3 4 000).
pub const MAX_PROMPT_CHARS: usize = 32_000;

const SIZES: &[&str] = &["auto", "1024x1024", "1536x1024", "1024x1536"];
const QUALITIES: &[&str] = &["auto", "low", "medium", "high"];
const FORMATS: &[&str] = &["png", "jpeg", "webp"];
const BACKGROUNDS: &[&str] = &["auto", "opaque", "transparent"];

/// One request to the backend (mirrors `genie_providers::images::ImageRequest`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageSpec {
    pub prompt: String,
    pub size: Option<String>,
    pub quality: Option<String>,
    pub output_format: Option<String>,
    pub background: Option<String>,
    pub n: u32,
}

/// One generated image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageBytes {
    pub bytes: Vec<u8>,
    pub mime: String,
    pub revised_prompt: Option<String>,
}

/// The image service (implemented by `genie-core` over the configured provider).
#[async_trait]
pub trait ImageBackend: Send + Sync {
    /// `Err(reason)` hides the tool: switched off, blocked by policy, no provider.
    fn status(&self, env: &ToolEnv) -> Result<(), String>;
    /// The model shown to the user and the model, e.g. `openai/gpt-image-1`.
    fn model_label(&self) -> String;
    async fn generate(
        &self,
        spec: &ImageSpec,
        cancel: &CancellationToken,
    ) -> Result<Vec<ImageBytes>, String>;
}

/// Blob storage for the generated images.
pub trait ImageStore: Send + Sync {
    /// Store `bytes`; returns the blob id.
    fn put(&self, session_id: &str, bytes: &[u8], mime: &str) -> Result<String, String>;
}

#[derive(Clone)]
pub struct ImageConfig {
    pub backend: Arc<dyn ImageBackend>,
    pub store: Arc<dyn ImageStore>,
    pub max_uses: u32,
}

impl ImageConfig {
    pub fn new(backend: Arc<dyn ImageBackend>, store: Arc<dyn ImageStore>) -> Self {
        ImageConfig {
            backend,
            store,
            max_uses: DEFAULT_MAX_USES,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Input {
    /// A detailed description of the image: subject, composition, style, lighting,
    /// colours, and any text that must appear (quoted exactly).
    prompt: String,
    /// `1024x1024` (square), `1536x1024` (landscape), `1024x1536` (portrait) or `auto`.
    #[serde(default)]
    size: Option<String>,
    /// `low`, `medium`, `high` or `auto`. Higher is slower and costs more.
    #[serde(default)]
    quality: Option<String>,
    /// `png` (default), `jpeg` or `webp`.
    #[serde(default)]
    output_format: Option<String>,
    /// `transparent` (png/webp only), `opaque` or `auto`.
    #[serde(default)]
    background: Option<String>,
    /// How many images to make, 1–4 (default 1).
    #[serde(default)]
    n: Option<u32>,
}

/// The `image_generate` tool.
pub struct ImageGenerate {
    cfg: ImageConfig,
    schema: Value,
    /// session → (turn, uses in that turn).
    uses: Mutex<HashMap<String, (String, u32)>>,
}

impl ImageGenerate {
    pub fn new(cfg: ImageConfig) -> Self {
        ImageGenerate {
            cfg,
            schema: schema_of::<Input>(),
            uses: Mutex::new(HashMap::new()),
        }
    }

    /// Count one use; `false` once the turn's budget is spent.
    fn take_use(&self, session: &str, turn: &str) -> bool {
        let mut m = self
            .uses
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let e = m
            .entry(session.to_string())
            .or_insert_with(|| (turn.to_string(), 0));
        if e.0 != turn {
            *e = (turn.to_string(), 0);
        }
        if e.1 >= self.cfg.max_uses {
            return false;
        }
        e.1 += 1;
        true
    }
}

/// `v` lower-cased if it is one of `allowed`; an issue otherwise.
fn one_of(
    field: &str,
    v: Option<String>,
    allowed: &[&str],
    issues: &mut Vec<String>,
) -> Option<String> {
    let v = v?.trim().to_ascii_lowercase();
    if v.is_empty() {
        return None;
    }
    if allowed.contains(&v.as_str()) {
        Some(v)
    } else {
        issues.push(format!("`{field}` must be one of {}", allowed.join(", ")));
        None
    }
}

fn validate(input: Input) -> Result<ImageSpec, ToolError> {
    let mut issues = vec![];
    let prompt = input.prompt.trim().to_string();
    if prompt.is_empty() {
        issues.push("`prompt` is empty".into());
    } else if prompt.chars().count() > MAX_PROMPT_CHARS {
        issues.push(format!(
            "`prompt` is longer than {MAX_PROMPT_CHARS} characters"
        ));
    }
    let n = input.n.unwrap_or(1);
    if !(1..=MAX_IMAGES).contains(&n) {
        issues.push(format!("`n` must be between 1 and {MAX_IMAGES}"));
    }
    let size = one_of("size", input.size, SIZES, &mut issues);
    let quality = one_of("quality", input.quality, QUALITIES, &mut issues);
    let format = input.output_format.map(|f| {
        if f.eq_ignore_ascii_case("jpg") {
            "jpeg".into()
        } else {
            f
        }
    });
    let output_format = one_of("output_format", format, FORMATS, &mut issues);
    let background = one_of("background", input.background, BACKGROUNDS, &mut issues);
    if background.as_deref() == Some("transparent") && output_format.as_deref() == Some("jpeg") {
        issues.push("a transparent background needs `output_format` png or webp".into());
    }
    if !issues.is_empty() {
        return Err(ToolError::invalid_input(issues));
    }
    Ok(ImageSpec {
        prompt,
        size,
        quality,
        output_format,
        background,
        n,
    })
}

#[async_trait]
impl Tool for ImageGenerate {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self, _env: &ToolEnv) -> Cow<'_, str> {
        Cow::Owned(format!(
            "Generate images from a text description with {}. Use it when the user asks you \
             to create, draw, render or design a picture, illustration, logo, icon or photo; \
             don't use it for charts or diagrams you can draw as code. Write a detailed \
             prompt (subject, composition, style, lighting, colours, exact text in quotes). \
             The images are shown to the user in the chat; you get back their ids, not the \
             pixels, so don't describe what they look like beyond what you asked for. At \
             most {} calls per turn and {MAX_IMAGES} images per call.",
            self.cfg.backend.model_label(),
            self.cfg.max_uses
        ))
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn kind(&self) -> ToolKind {
        ToolKind::External
    }

    fn availability(&self, env: &ToolEnv) -> Availability {
        match self.cfg.backend.status(env) {
            Ok(()) => Availability::Available,
            Err(reason) => Availability::Unavailable(Cow::Owned(reason)),
        }
    }

    async fn execute(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let input: Input = parse_input(input, &self.schema)?;
        let spec = validate(input)?;
        if let Err(reason) = self.cfg.backend.status(&ctx.env) {
            return Err(ToolError::failed(reason));
        }
        ctx.authorize(PermissionRequest {
            resource: None,
            preview: Some(Preview::Text {
                text: spec.prompt.clone(),
            }),
            save_suggestions: vec![],
        })
        .await?;
        if !self.take_use(&ctx.env.session_id, &ctx.turn_id) {
            return Err(ToolError::failed(format!(
                "{NAME} can be used {} times per turn; show the user what you have",
                self.cfg.max_uses
            )));
        }
        ctx.progress(
            if spec.n == 1 {
                "Generating an image".to_string()
            } else {
                format!("Generating {} images", spec.n)
            },
            None,
        );
        let images = tokio::select! {
            biased;
            () = ctx.cancel.cancelled() => return Err(ToolError::Cancelled),
            r = self.cfg.backend.generate(&spec, &ctx.cancel) => r.map_err(|e| ToolError::Failed {
                message: format!("image generation failed: {e}"),
                may_have_run: true,
            })?,
        };
        if images.is_empty() {
            return Err(ToolError::Failed {
                message: "image generation returned no images".into(),
                may_have_run: true,
            });
        }
        let mut out = Vec::with_capacity(images.len());
        for img in &images {
            let blob_id = self
                .cfg
                .store
                .put(&ctx.env.session_id, &img.bytes, &img.mime)
                .map_err(|e| ToolError::Failed {
                    message: format!("couldn't save the generated image: {e}"),
                    may_have_run: true,
                })?;
            let mut v = json!({
                "blob_id": blob_id,
                "mime": img.mime,
                "bytes": img.bytes.len(),
            });
            if let Some(p) = &img.revised_prompt {
                v["revised_prompt"] = json!(p);
            }
            out.push(v);
        }
        Ok(ToolOutput::Json(json!({
            "model": self.cfg.backend.model_label(),
            "images": out,
            "note": "The images are shown to the user above your reply.",
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::{AllowAll, NullSink};
    use genie_protocol::common::SessionKind;

    struct Fake {
        on: bool,
        seen: Mutex<Vec<ImageSpec>>,
    }

    #[async_trait]
    impl ImageBackend for Fake {
        fn status(&self, _env: &ToolEnv) -> Result<(), String> {
            if self.on {
                Ok(())
            } else {
                Err("image generation is turned off".into())
            }
        }
        fn model_label(&self) -> String {
            "openai/gpt-image-1".into()
        }
        async fn generate(
            &self,
            spec: &ImageSpec,
            _cancel: &CancellationToken,
        ) -> Result<Vec<ImageBytes>, String> {
            self.seen.lock().unwrap().push(spec.clone());
            Ok((0..spec.n)
                .map(|i| ImageBytes {
                    bytes: vec![i as u8; 3],
                    mime: "image/png".into(),
                    revised_prompt: (i == 0).then(|| "revised".into()),
                })
                .collect())
        }
    }

    #[derive(Default)]
    struct Blobs(Mutex<Vec<Vec<u8>>>);
    impl ImageStore for Blobs {
        fn put(&self, _s: &str, bytes: &[u8], _m: &str) -> Result<String, String> {
            let mut v = self.0.lock().unwrap();
            v.push(bytes.to_vec());
            Ok(format!("blob_{}", v.len()))
        }
    }

    fn setup(on: bool) -> (ImageGenerate, Arc<Fake>, Arc<Blobs>) {
        let fake = Arc::new(Fake {
            on,
            seen: Mutex::default(),
        });
        let blobs = Arc::new(Blobs::default());
        let mut cfg = ImageConfig::new(fake.clone(), blobs.clone());
        cfg.max_uses = 2;
        (ImageGenerate::new(cfg), fake, blobs)
    }

    fn ctx(turn: &str) -> ToolCtx {
        ToolCtx {
            env: Arc::new(ToolEnv::new("s", SessionKind::Chat)),
            turn_id: turn.into(),
            call_id: "call".into(),
            tool_name: NAME.into(),
            cancel: CancellationToken::new(),
            detach: CancellationToken::new(),
            permissions: Arc::new(AllowAll),
            events: Arc::new(NullSink),
        }
    }

    #[tokio::test]
    async fn generates_stores_and_reports_blob_ids() {
        let (t, fake, blobs) = setup(true);
        let out = t
            .execute(
                json!({"prompt": " a fox ", "size": "1536X1024", "outputFormat": "webp", "n": 2}),
                ctx("t1"),
            )
            .await
            .unwrap();
        let ToolOutput::Json(v) = out else { panic!() };
        assert_eq!(v["model"], "openai/gpt-image-1");
        assert_eq!(v["images"][0]["blob_id"], "blob_1");
        assert_eq!(v["images"][0]["revised_prompt"], "revised");
        assert_eq!(v["images"][1]["blob_id"], "blob_2");
        assert!(v["images"][1].get("revised_prompt").is_none());
        assert_eq!(blobs.0.lock().unwrap().len(), 2);
        let spec = fake.seen.lock().unwrap()[0].clone();
        assert_eq!(spec.prompt, "a fox");
        assert_eq!(spec.size.as_deref(), Some("1536x1024"));
        assert_eq!(spec.output_format.as_deref(), Some("webp"));
    }

    #[tokio::test]
    async fn validates_input() {
        let (t, _, _) = setup(true);
        let e = t
            .execute(
                json!({"prompt": "", "n": 9, "size": "999x1", "background": "transparent", "output_format": "jpeg"}),
                ctx("t1"),
            )
            .await
            .unwrap_err();
        let ToolError::InvalidInput { issues } = e else {
            panic!("{e:?}")
        };
        assert_eq!(issues.len(), 4, "{issues:?}");
        assert!(issues[0].contains("prompt"));
    }

    #[tokio::test]
    async fn per_turn_budget_and_availability() {
        let (t, _, _) = setup(true);
        for _ in 0..2 {
            t.execute(json!({"prompt": "x"}), ctx("t1")).await.unwrap();
        }
        let e = t.execute(json!({"prompt": "x"}), ctx("t1")).await;
        assert!(matches!(e, Err(ToolError::Failed { .. })));
        t.execute(json!({"prompt": "x"}), ctx("t2")).await.unwrap();

        let (off, _, _) = setup(false);
        let env = ToolEnv::new("s", SessionKind::Chat);
        assert!(!off.availability(&env).is_available());
        assert!(
            off.execute(json!({"prompt": "x"}), ctx("t1"))
                .await
                .is_err()
        );
    }
}
