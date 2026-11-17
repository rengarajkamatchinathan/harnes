//! Chat projects: instructions, knowledge files and retrieval (D32, SA-18, 04 §7, 06 §2).
//!
//! - **Files** (`project.file.add`): the blob's text is extracted (text, Markdown, code,
//!   PDF), counted (4 chars/token + 15 %), checked against the capacity cap, stored as a
//!   `project_file` row and chunked into `kchunk` rows in the same transaction. Embeddings
//!   are computed by a background job when an [`EmbeddingClient`] is configured, cached by
//!   content hash (re-uploads cost nothing).
//! - **Capacity cap (SA-18):** 10× the largest context window among the enabled models
//!   ([`ProjectsConfig::capacity_window`]), at most 20M tokens and 500 files per project.
//! - **Mode (D32):** full while the project's knowledge is ≤ 50 % of the *current*
//!   model's window, re-evaluated at every turn; retrieval above that.
//! - **Full mode:** `<project_knowledge>` with every file is a baseline source (rank 7;
//!   1 h cache TTL where the route supports it).
//! - **Retrieval mode:** the top chunks for the user's message are attached to that
//!   message as `<project_excerpts>` (never the cached baseline), and `project_search`
//!   is offered. Ranking: BM25 (any word) + cosine when vectors exist, fused with RRF.
//! - **Instructions:** `<project_instructions project="…">` (rank 6) when set.

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use genie_protocol::api::project::{IndexMode, IndexStatus};
use genie_providers::embedding::EmbeddingClient;
use genie_tools::{Availability, Tool, ToolCtx, ToolEnv, ToolError, ToolKind, ToolOutput};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::watch;

use crate::runtime::context::{InstructionSource, estimate_text};
use crate::runtime::extract;
use crate::runtime::memory_ctl::MemoryControl;
use crate::runtime::ports::{InstructionProvider, ModelResolver, SessionSettings};
use crate::runtime::retrieval::{self, line_range};
use crate::task::TaskScope;

pub const PROJECT_SEARCH: &str = "project_search";

#[derive(Debug, Clone)]
pub struct ProjectsConfig {
    /// The largest context window among the user's enabled models (SA-18 cap base).
    pub capacity_window: u64,
    /// Hard ceilings (SA-18).
    pub max_tokens: u64,
    pub max_files: usize,
    /// Window used for `project.index.status` (no session model to ask).
    pub status_window: u64,
    /// Excerpts pre-attached to a user message in retrieval mode, and their token budget.
    pub excerpts: usize,
    pub excerpt_tokens: u64,
}

impl Default for ProjectsConfig {
    fn default() -> Self {
        ProjectsConfig {
            capacity_window: 200_000,
            max_tokens: 20_000_000,
            max_files: 500,
            status_window: 200_000,
            excerpts: 5,
            excerpt_tokens: 4_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProjectError {
    #[error("{0} not found")]
    NotFound(String),
    #[error("{0}")]
    Invalid(String),
    /// Over the capacity cap (SA-18).
    #[error("{0}")]
    Capacity(String),
    #[error("store: {0}")]
    Store(String),
}

fn se(e: genie_store::StoreError) -> ProjectError {
    ProjectError::Store(e.to_string())
}

/// One retrieved chunk, ready to render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Excerpt {
    pub chunk_id: String,
    pub file: String,
    pub lines: (usize, usize),
    pub text: String,
    pub tokens: u64,
}

pub struct ProjectsHub {
    store: Arc<genie_store::Store>,
    models: Arc<dyn ModelResolver>,
    embedder: Option<Arc<dyn EmbeddingClient>>,
    scope: TaskScope,
    cfg: ProjectsConfig,
    /// Extracted text per file id (PDF extraction is not free).
    texts: Mutex<HashMap<String, Arc<String>>>,
    /// Running embedding jobs per project.
    jobs: watch::Sender<HashMap<String, usize>>,
    /// The mode decided at each session's last turn start (`project_search` availability).
    modes: Mutex<HashMap<String, IndexMode>>,
}

fn render_excerpts(ex: &[Excerpt]) -> String {
    let mut s = String::from("<project_excerpts>\n");
    for e in ex {
        let _ = writeln!(
            s,
            "<excerpt file=\"{}\" lines=\"{}-{}\">\n{}\n</excerpt>",
            e.file.replace('"', "&quot;"),
            e.lines.0,
            e.lines.1,
            e.text.trim_end()
        );
    }
    s.push_str("</project_excerpts>");
    s
}

impl ProjectsHub {
    pub fn new(
        store: Arc<genie_store::Store>,
        models: Arc<dyn ModelResolver>,
        embedder: Option<Arc<dyn EmbeddingClient>>,
        scope: TaskScope,
        cfg: ProjectsConfig,
    ) -> Arc<Self> {
        Arc::new(ProjectsHub {
            store,
            models,
            embedder,
            scope,
            cfg,
            texts: Mutex::new(HashMap::new()),
            jobs: watch::Sender::new(HashMap::new()),
            modes: Mutex::new(HashMap::new()),
        })
    }

    pub fn store(&self) -> &Arc<genie_store::Store> {
        &self.store
    }

    pub fn config(&self) -> &ProjectsConfig {
        &self.cfg
    }

    /// The per-project token cap (SA-18).
    pub fn cap_tokens(&self) -> u64 {
        self.cfg
            .capacity_window
            .saturating_mul(10)
            .min(self.cfg.max_tokens)
            .max(1)
    }

    fn project_tokens(&self, project_id: &str) -> Result<(u64, usize), ProjectError> {
        let files = self
            .store
            .read(|c| genie_store::projects::files(c, project_id))
            .map_err(se)?;
        let tokens = files
            .iter()
            .map(|f| u64::try_from(f.tokens.unwrap_or(0)).unwrap_or(0))
            .sum();
        Ok((tokens, files.len()))
    }

    /// `project.file.add`: extract, check the cap, store and chunk; embeddings follow in
    /// the background. Adding a file under a name the project already has is an update:
    /// identical content is a no-op, changed content replaces the file's text, chunks and
    /// embeddings in place (same file id) and re-indexes it.
    pub fn add_file(
        self: &Arc<Self>,
        project_id: &str,
        blob_id: &str,
        name: &str,
    ) -> Result<genie_store::projects::ProjectFile, ProjectError> {
        if self
            .store
            .read(|c| genie_store::projects::get(c, project_id))
            .map_err(se)?
            .is_none()
        {
            return Err(ProjectError::NotFound("project".into()));
        }
        let row = self
            .store
            .blob_row(blob_id)
            .map_err(se)?
            .ok_or_else(|| ProjectError::NotFound("blob".into()))?;
        let mime = row
            .mime
            .clone()
            .filter(|m| !m.is_empty() && m != "application/octet-stream")
            .unwrap_or_else(|| extract::guess_mime(name).to_string());
        let existing = self
            .store
            .read(|c| genie_store::projects::files(c, project_id))
            .map_err(se)?
            .into_iter()
            .find(|f| f.name == name);
        if let Some(f) = &existing
            && f.blob_id.as_deref() == Some(blob_id)
        {
            return Ok(f.clone());
        }
        let bytes = self.store.read_blob(blob_id).map_err(se)?;
        let text = extract::extract_text(&bytes, &mime, name).map_err(ProjectError::Invalid)?;
        let tokens = estimate_text(&text);
        let (mut have, mut files) = self.project_tokens(project_id)?;
        if let Some(f) = &existing {
            have = have.saturating_sub(u64::try_from(f.tokens.unwrap_or(0)).unwrap_or(0));
            files = files.saturating_sub(1);
        }
        if files >= self.cfg.max_files {
            return Err(ProjectError::Capacity(format!(
                "a project holds at most {} files",
                self.cfg.max_files
            )));
        }
        let cap = self.cap_tokens();
        if have + tokens > cap {
            return Err(ProjectError::Capacity(format!(
                "{name} (~{tokens} tokens) would take the project to {} of {cap} tokens; \
                 remove files or split the project",
                have + tokens
            )));
        }
        let spans = retrieval::chunk(&text, extract::text_kind(&mime, name));
        let tok_i64 = i64::try_from(tokens).unwrap_or(i64::MAX);
        let file = self
            .store
            .write(|tx| {
                let f = match &existing {
                    Some(old) => genie_store::projects::update_file(
                        tx,
                        &old.id,
                        Some(&mime),
                        blob_id,
                        Some(tok_i64),
                    )?,
                    None => genie_store::projects::add_file(
                        tx,
                        project_id,
                        name,
                        Some(&mime),
                        blob_id,
                        Some(tok_i64),
                    )?,
                };
                let chunks: Vec<genie_store::retrieval::NewChunk<'_>> = spans
                    .iter()
                    .enumerate()
                    .map(|(i, s)| genie_store::retrieval::NewChunk {
                        ordinal: i64::try_from(i).unwrap_or(0),
                        start_byte: i64::try_from(s.start).unwrap_or(0),
                        end_byte: i64::try_from(s.end).unwrap_or(0),
                        tokens: i64::try_from(s.tokens).unwrap_or(0),
                        text: &text[s.start..s.end],
                    })
                    .collect();
                genie_store::retrieval::replace_file_chunks(tx, project_id, &f.id, &chunks)?;
                tx.execute(
                    "UPDATE project SET updated = ?2 WHERE id = ?1",
                    (
                        project_id,
                        i64::try_from(crate::bus::now_ms()).unwrap_or(i64::MAX),
                    ),
                )?;
                Ok(f)
            })
            .map_err(se)?;
        self.texts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(file.id.clone(), Arc::new(text));
        self.schedule(project_id);
        Ok(file)
    }

    /// `project.file.remove` (chunks cascade).
    pub fn remove_file(&self, project_id: &str, file_id: &str) -> Result<(), ProjectError> {
        let f = self
            .store
            .read(|c| genie_store::projects::get_file(c, file_id))
            .map_err(se)?;
        match f {
            Some(f) if f.project_id == project_id => {
                self.store
                    .write(|tx| genie_store::projects::remove_file(tx, file_id))
                    .map_err(se)?;
                self.texts
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(file_id);
                Ok(())
            }
            _ => Err(ProjectError::NotFound("file".into())),
        }
    }

    /// Full while knowledge ≤ 50 % of `window` (D32).
    pub fn mode_for(tokens: u64, window: u64) -> IndexMode {
        if tokens <= window / 2 {
            IndexMode::Full
        } else {
            IndexMode::Retrieval
        }
    }

    /// `project.index.status`.
    pub fn status(&self, project_id: &str) -> Result<IndexStatus, ProjectError> {
        if self
            .store
            .read(|c| genie_store::projects::get(c, project_id))
            .map_err(se)?
            .is_none()
        {
            return Err(ProjectError::NotFound("project".into()));
        }
        let (tokens, files) = self.project_tokens(project_id)?;
        let cap = self.cap_tokens();
        Ok(IndexStatus {
            mode: Self::mode_for(tokens, self.cfg.status_window),
            files: u32::try_from(files).unwrap_or(u32::MAX),
            tokens,
            capacity_pct: (tokens as f64 / cap as f64 * 1000.0).round() / 10.0,
            indexing: self.indexing(project_id),
        })
    }

    pub fn indexing(&self, project_id: &str) -> bool {
        self.jobs.borrow().get(project_id).copied().unwrap_or(0) > 0
    }

    fn schedule(self: &Arc<Self>, project_id: &str) {
        if self.embedder.is_none() {
            return;
        }
        let pid = project_id.to_string();
        self.jobs
            .send_modify(|m| *m.entry(pid.clone()).or_default() += 1);
        let hub = self.clone();
        self.scope.spawn(async move {
            if let Err(e) = hub.embed_missing(&pid).await {
                tracing::warn!(project = %pid, error = %e, "embedding job failed; BM25 only");
            }
            hub.jobs.send_modify(|m| {
                if let Some(n) = m.get_mut(&pid) {
                    *n = n.saturating_sub(1);
                    if *n == 0 {
                        m.remove(&pid);
                    }
                }
            });
        });
    }

    /// Wait until no embedding job runs for the project.
    pub async fn wait_indexed(&self, project_id: &str) {
        let mut rx = self.jobs.subscribe();
        let _ = rx
            .wait_for(|m| m.get(project_id).copied().unwrap_or(0) == 0)
            .await;
    }

    /// Embed the project's chunks that have no vector for the configured model yet.
    pub async fn embed_missing(&self, project_id: &str) -> Result<usize, String> {
        let Some(emb) = &self.embedder else {
            return Ok(0);
        };
        let model = emb.model().to_string();
        let missing = self
            .store
            .read(|c| genie_store::retrieval::missing_embeddings(c, project_id, &model))
            .map_err(|e| e.to_string())?;
        let mut done = 0;
        for batch in missing.chunks(genie_providers::embedding::EMBED_BATCH) {
            let texts: Vec<String> = batch.iter().map(|(_, t)| t.clone()).collect();
            let vecs = emb
                .embed(&texts, self.scope.token())
                .await
                .map_err(|e| e.to_string())?;
            self.store
                .write(|tx| {
                    for ((hash, _), v) in batch.iter().zip(&vecs) {
                        genie_store::retrieval::put_embedding(tx, hash, &model, v)?;
                    }
                    Ok(())
                })
                .map_err(|e| e.to_string())?;
            done += vecs.len();
        }
        Ok(done)
    }

    fn file_text(&self, file: &genie_store::projects::ProjectFile) -> Option<Arc<String>> {
        if let Some(t) = self
            .texts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&file.id)
        {
            return Some(t.clone());
        }
        let blob = file.blob_id.as_deref()?;
        let bytes = self.store.read_blob(blob).ok()?;
        let mime = file.mime.clone().unwrap_or_default();
        let text = Arc::new(extract::extract_text(&bytes, &mime, &file.name).ok()?);
        self.texts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(file.id.clone(), text.clone());
        Some(text)
    }

    /// Hybrid search over a project's chunks (D32).
    pub async fn search(
        &self,
        project_id: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<Excerpt>, ProjectError> {
        let depth = u32::try_from(limit.saturating_mul(4).max(20)).unwrap_or(u32::MAX);
        let lexical = self
            .store
            .read(|c| genie_store::retrieval::bm25_any(c, project_id, query, depth))
            .map_err(se)?;
        let mut ranked = lexical.clone();
        if let Some(emb) = &self.embedder {
            match emb.embed(&[query.to_string()], self.scope.token()).await {
                Ok(mut v) if !v.is_empty() => {
                    let qv = v.swap_remove(0);
                    let model = emb.model().to_string();
                    let semantic = self
                        .store
                        .read(|c| {
                            genie_store::retrieval::vector_search(
                                c,
                                project_id,
                                &model,
                                &qv,
                                depth as usize,
                            )
                        })
                        .map_err(se)?;
                    if !semantic.is_empty() {
                        ranked = genie_store::retrieval::rrf(&[lexical, semantic], depth as usize);
                    }
                }
                Ok(_) => {}
                Err(e) => tracing::debug!(error = %e, "query embedding failed; BM25 only"),
            }
        }
        let files: HashMap<String, genie_store::projects::ProjectFile> = self
            .store
            .read(|c| genie_store::projects::files(c, project_id))
            .map_err(se)?
            .into_iter()
            .map(|f| (f.id.clone(), f))
            .collect();
        let mut out = vec![];
        for id in ranked.into_iter().take(limit) {
            let Some(c) = self
                .store
                .read(|conn| genie_store::retrieval::chunk(conn, &id))
                .map_err(se)?
            else {
                continue;
            };
            let Some(f) = files.get(&c.file_id) else {
                continue;
            };
            let start = usize::try_from(c.start_byte).unwrap_or(0);
            let end = usize::try_from(c.end_byte).unwrap_or(0);
            let lines = self
                .file_text(f)
                .map_or((1, 1), |t| line_range(&t, start, end));
            out.push(Excerpt {
                chunk_id: c.id,
                file: f.name.clone(),
                lines,
                text: c.text,
                tokens: u64::try_from(c.tokens).unwrap_or(0),
            });
        }
        Ok(out)
    }

    fn window(&self, settings: &SessionSettings) -> u64 {
        self.models
            .resolve(&settings.model)
            .map(|m| m.context_window())
            .unwrap_or(128_000)
    }

    /// The mode for this session's project and current model (cached for the tools).
    pub fn session_mode(&self, session_id: &str, settings: &SessionSettings) -> Option<IndexMode> {
        let project = settings.project_id.as_deref()?;
        let (tokens, files) = self.project_tokens(project).ok()?;
        let mode = if files == 0 {
            IndexMode::Full
        } else {
            Self::mode_for(tokens, self.window(settings))
        };
        self.modes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session_id.to_string(), mode);
        Some(mode)
    }

    /// The mode decided at the session's last turn start.
    pub fn cached_mode(&self, session_id: &str) -> Option<IndexMode> {
        self.modes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session_id)
            .copied()
    }

    /// Baseline sources: project instructions and (full mode) knowledge.
    pub fn sources(&self, session_id: &str, settings: &SessionSettings) -> Vec<InstructionSource> {
        let Some(pid) = settings.project_id.as_deref() else {
            return vec![];
        };
        let project = match self.store.read(|c| genie_store::projects::get(c, pid)) {
            Ok(Some(p)) => p,
            _ => return vec![],
        };
        if !settings.incognito
            && let Err(e) = self
                .store
                .write(|tx| genie_store::projects::link_session(tx, session_id, pid))
        {
            tracing::debug!(error = %e, "session not linked to its project");
        }
        let mut out = vec![];
        if let Some(text) = project
            .instructions
            .as_deref()
            .filter(|t| !t.trim().is_empty())
        {
            out.push(InstructionSource::project_instructions(&project.name, text));
        }
        if self.session_mode(session_id, settings) == Some(IndexMode::Full) {
            let files = self
                .store
                .read(|c| genie_store::projects::files(c, pid))
                .unwrap_or_default();
            let docs: Vec<(String, String)> = files
                .iter()
                .filter_map(|f| self.file_text(f).map(|t| (f.name.clone(), t.to_string())))
                .collect();
            if !docs.is_empty() {
                out.push(InstructionSource::project_knowledge(&docs));
            }
        }
        out
    }

    /// `<project_excerpts>` for a user message in retrieval mode (06 §2.1: attached to
    /// the message, not the cached baseline).
    pub async fn excerpts_for(
        &self,
        session_id: &str,
        settings: &SessionSettings,
        query: &str,
    ) -> Option<String> {
        let pid = settings.project_id.as_deref()?;
        if query.trim().is_empty() || self.cached_mode(session_id) != Some(IndexMode::Retrieval) {
            return None;
        }
        let hits = match self.search(pid, query, self.cfg.excerpts * 2).await {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!(error = %e, "project retrieval failed");
                return None;
            }
        };
        let mut used = 0u64;
        let mut picked = vec![];
        for h in hits {
            if picked.len() >= self.cfg.excerpts || used + h.tokens > self.cfg.excerpt_tokens {
                break;
            }
            used += h.tokens;
            picked.push(h);
        }
        (!picked.is_empty()).then(|| render_excerpts(&picked))
    }
}

/// The embedding provider (03 §9, SA-14) from the environment: OpenAI
/// `text-embedding-3-small` when `OPENAI_API_KEY` is set (`OPENAI_BASE_URL` overrides the
/// endpoint). `None`: retrieval ranks with BM25 alone.
pub fn embedder_from_env() -> Option<Arc<dyn EmbeddingClient>> {
    let key = std::env::var("OPENAI_API_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty())?;
    let base = std::env::var("OPENAI_BASE_URL")
        .ok()
        .filter(|u| !u.trim().is_empty())
        .unwrap_or_else(|| "https://api.openai.com/v1".into());
    let transport = match genie_providers::executor::ReqwestTransport::new() {
        Ok(t) => Arc::new(t),
        Err(e) => {
            tracing::warn!(error = %e, "no HTTP client for embeddings");
            return None;
        }
    };
    Some(Arc::new(genie_providers::embedding::OpenAiEmbeddings::new(
        transport,
        Arc::new(genie_providers::auth::BearerKey(
            genie_secrets::SecretString::from(key),
        )),
        format!("{}/embeddings", base.trim_end_matches('/')),
        "openai",
        "text-embedding-3-small",
    )))
}

/// Adds project instructions/knowledge and memory to a base provider (06 §2.1 items
/// 5–8).
pub struct ContextSources {
    pub base: Arc<dyn InstructionProvider>,
    pub projects: Option<Arc<ProjectsHub>>,
    pub memory: Option<Arc<MemoryControl>>,
}

impl InstructionProvider for ContextSources {
    fn sources(&self, session_id: &str, settings: &SessionSettings) -> Vec<InstructionSource> {
        let mut v = self.base.sources(session_id, settings);
        if let Some(m) = &self.memory {
            v.extend(m.sources(settings));
        }
        if let Some(p) = &self.projects {
            v.extend(p.sources(session_id, settings));
        }
        v
    }

    fn after_compaction(
        &self,
        session_id: &str,
        settings: &SessionSettings,
    ) -> Vec<InstructionSource> {
        self.base.after_compaction(session_id, settings)
    }
}

// ------------------------------------------------------------------ project_search

#[derive(Deserialize)]
struct SearchInput {
    query: String,
    #[serde(default)]
    limit: Option<u32>,
}

/// `project_search {query, limit?}`: retrieval mode only (06 §4.1).
pub struct ProjectSearch {
    hub: Arc<ProjectsHub>,
    schema: Value,
}

impl ProjectSearch {
    pub fn new(hub: Arc<ProjectsHub>) -> Self {
        ProjectSearch {
            hub,
            schema: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "What to look for in the project's files."},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 20, "description": "Max excerpts (default 8)."}
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        }
    }
}

#[async_trait]
impl Tool for ProjectSearch {
    fn name(&self) -> &str {
        PROJECT_SEARCH
    }
    fn description(&self, _env: &ToolEnv) -> Cow<'_, str> {
        Cow::Borrowed(
            "Search the files of this chat's project. Returns excerpts with file names and \
             line ranges; cite the file you use.",
        )
    }
    fn input_schema(&self) -> &Value {
        &self.schema
    }
    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }
    fn availability(&self, env: &ToolEnv) -> Availability {
        if env.features.project_id.is_none() {
            Availability::Unavailable(Cow::Borrowed("not in a project"))
        } else if self.hub.cached_mode(&env.session_id) != Some(IndexMode::Retrieval) {
            Availability::Unavailable(Cow::Borrowed("project knowledge is in full mode"))
        } else {
            Availability::Available
        }
    }
    async fn execute(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let i: SearchInput = genie_tools::parse_input(input, &self.schema)?;
        let pid = ctx
            .env
            .features
            .project_id
            .clone()
            .ok_or_else(|| ToolError::failed("this chat is not in a project"))?;
        let limit = i.limit.unwrap_or(8).clamp(1, 20) as usize;
        let hits = self
            .hub
            .search(&pid, &i.query, limit)
            .await
            .map_err(|e| ToolError::failed(e.to_string()))?;
        if hits.is_empty() {
            return Ok(ToolOutput::text(format!(
                "No project files match \"{}\".",
                i.query
            )));
        }
        Ok(ToolOutput::Text(render_excerpts(&hits)))
    }
}
