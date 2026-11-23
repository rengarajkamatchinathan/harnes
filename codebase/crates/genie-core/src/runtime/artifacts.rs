//! Artifacts (D28, ticket 39, 06 §4.1): `artifact_create / artifact_update /
//! artifact_rewrite`, versions tied to messages, `artifact.versioned`.
//!
//! A version belongs to the assistant message whose tool call produced it, so each
//! branch (D24) has its own versions. That message is only stored after the step's tool
//! calls are joined (06 §1), so a call **stages** its version (keyed by the call id) and
//! [`ArtifactConversation`] commits it when the assistant message carrying that call is
//! appended, then publishes `artifact.versioned` through the call's event sink. The id
//! and version number are handed to the model at once.
//!
//! Storage is `genie-store` (`artifact`, `artifact_version`; content > 256 KiB as a blob)
//! or, without a store (tests, incognito runtimes), daemon memory.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use genie_protocol::common::SessionKind;
use genie_protocol::event::{ArtifactVersioned, SessionEvent};
use genie_providers::model::{Part, WireRecord};
use genie_tools::{
    Availability, EventSink, Tool, ToolCtx, ToolEnv, ToolError, ToolKind, ToolOutput,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::Defect;
use crate::ids::new_id;
use crate::runtime::conversation::{ConversationStore, SessionMeta, StoredMessage, UsageLine};

/// The bundled libraries artifacts may import (ticket 39); also in `feature.artifacts`.
pub const ARTIFACT_LIBRARIES: &str = "React + ReactDOM, Tailwind CSS (prebuilt utility classes), \
lucide-react, Recharts, d3, lodash, papaparse, mathjs, three.js, Mermaid, KaTeX";

/// v1 types (ticket 39).
pub const TYPES: &[&str] = &["document", "code", "svg", "mermaid", "html", "react"];

/// Largest artifact content accepted from the model.
pub const MAX_CONTENT: usize = 4 * 1024 * 1024;

pub const CREATE: &str = "artifact_create";
pub const UPDATE: &str = "artifact_update";
pub const REWRITE: &str = "artifact_rewrite";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactMeta {
    pub id: String,
    pub session_id: String,
    pub ty: String,
    pub title: String,
    pub created: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub n: u32,
    pub message_id: String,
    /// Inline content; `None` when stored as a blob.
    pub content: Option<String>,
    pub blob_id: Option<String>,
}

struct Staged {
    session_id: String,
    meta: ArtifactMeta,
    n: u32,
    content: String,
    sink: Arc<dyn EventSink>,
}

#[derive(Default)]
struct MemArtifacts {
    items: HashMap<String, (ArtifactMeta, Vec<Version>)>,
}

enum Backend {
    Store(Arc<genie_store::Store>),
    Memory(Mutex<MemArtifacts>),
}

fn sd(e: genie_store::StoreError) -> Defect {
    Defect::new(format!("store: {e}"))
}

fn now_ms() -> i64 {
    i64::try_from(crate::bus::now_ms()).unwrap_or(i64::MAX)
}

/// Artifacts of every session: staged versions, storage, path filtering.
pub struct ArtifactHub {
    backend: Backend,
    /// Artifacts of ephemeral (incognito) sessions: never written to the store.
    memory: Backend,
    ephemeral: OnceLock<crate::journal::Ephemeral>,
    conversation: OnceLock<Arc<dyn ConversationStore>>,
    staged: Mutex<HashMap<String, Vec<Staged>>>,
    enabled: AtomicBool,
}

impl ArtifactHub {
    /// Store-backed when `store` is set, else in memory.
    pub fn new(store: Option<Arc<genie_store::Store>>) -> Arc<Self> {
        Arc::new(ArtifactHub {
            backend: match store {
                Some(s) => Backend::Store(s),
                None => Backend::Memory(Mutex::new(MemArtifacts::default())),
            },
            memory: Backend::Memory(Mutex::new(MemArtifacts::default())),
            ephemeral: OnceLock::new(),
            conversation: OnceLock::new(),
            staged: Mutex::new(HashMap::new()),
            enabled: AtomicBool::new(true),
        })
    }

    /// Settings → Capabilities / admin policy switch (D28): hides the tools.
    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::SeqCst);
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    /// Decorate the runtime's conversation so appended assistant messages commit the
    /// versions their tool calls staged.
    pub fn wrap(self: &Arc<Self>, inner: Arc<dyn ConversationStore>) -> Arc<dyn ConversationStore> {
        let _ = self.conversation.set(inner.clone());
        Arc::new(ArtifactConversation {
            inner,
            hub: self.clone(),
        })
    }

    fn lock_staged(&self) -> std::sync::MutexGuard<'_, HashMap<String, Vec<Staged>>> {
        self.staged.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Which sessions keep their artifacts in memory only (incognito). Set once.
    pub fn set_ephemeral(&self, f: crate::journal::Ephemeral) {
        let _ = self.ephemeral.set(f);
    }

    fn for_session(&self, session: &str) -> &Backend {
        match self.ephemeral.get() {
            Some(f) if f(session) => &self.memory,
            _ => &self.backend,
        }
    }

    fn stored_meta(&self, id: &str) -> Result<Option<ArtifactMeta>, Defect> {
        if let Some(m) = Self::meta_in(&self.memory, id)? {
            return Ok(Some(m));
        }
        Self::meta_in(&self.backend, id)
    }

    fn meta_in(backend: &Backend, id: &str) -> Result<Option<ArtifactMeta>, Defect> {
        match backend {
            Backend::Store(s) => {
                let a = s.read(|c| genie_store::artifacts::get(c, id)).map_err(sd)?;
                Ok(a.map(|a| ArtifactMeta {
                    id: a.id,
                    session_id: a.session_id,
                    ty: a.ty,
                    title: a.title.unwrap_or_default(),
                    created: a.created,
                }))
            }
            Backend::Memory(m) => Ok(m
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .items
                .get(id)
                .map(|(a, _)| a.clone())),
        }
    }

    /// Every stored version of an artifact (all branches), by `n`.
    fn stored_versions(&self, id: &str) -> Result<Vec<Version>, Defect> {
        if Self::meta_in(&self.memory, id)?.is_some() {
            return Self::versions_in(&self.memory, id);
        }
        Self::versions_in(&self.backend, id)
    }

    fn versions_in(backend: &Backend, id: &str) -> Result<Vec<Version>, Defect> {
        match backend {
            Backend::Store(s) => {
                let v = s
                    .read(|c| genie_store::artifacts::versions(c, id))
                    .map_err(sd)?;
                Ok(v.into_iter()
                    .map(|v| Version {
                        n: u32::try_from(v.n).unwrap_or(0),
                        message_id: v.message_id,
                        content: v.content,
                        blob_id: v.blob_id,
                    })
                    .collect())
            }
            Backend::Memory(m) => Ok(m
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .items
                .get(id)
                .map(|(_, v)| v.clone())
                .unwrap_or_default()),
        }
    }

    fn session_artifacts(&self, session: &str) -> Result<Vec<ArtifactMeta>, Defect> {
        match self.for_session(session) {
            Backend::Store(s) => {
                let v = s
                    .read(|c| genie_store::artifacts::for_session(c, session))
                    .map_err(sd)?;
                Ok(v.into_iter()
                    .map(|a| ArtifactMeta {
                        id: a.id,
                        session_id: a.session_id,
                        ty: a.ty,
                        title: a.title.unwrap_or_default(),
                        created: a.created,
                    })
                    .collect())
            }
            Backend::Memory(m) => {
                let g = m.lock().unwrap_or_else(|e| e.into_inner());
                let mut v: Vec<ArtifactMeta> = g
                    .items
                    .values()
                    .filter(|(a, _)| a.session_id == session)
                    .map(|(a, _)| a.clone())
                    .collect();
                v.sort_by(|a, b| a.created.cmp(&b.created).then_with(|| a.id.cmp(&b.id)));
                Ok(v)
            }
        }
    }

    fn content_of(&self, v: &Version) -> Result<String, Defect> {
        if let Some(c) = &v.content {
            return Ok(c.clone());
        }
        match (&self.backend, &v.blob_id) {
            (Backend::Store(s), Some(b)) => s
                .read_blob(b)
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .map_err(sd),
            _ => Ok(String::new()),
        }
    }

    /// Message ids from the root to `leaf` (the active leaf when `None`).
    fn path_ids(&self, session: &str, leaf: Option<&str>) -> Result<HashSet<String>, Defect> {
        let Some(conv) = self.conversation.get() else {
            return Ok(HashSet::new());
        };
        let Some(leaf) = leaf else {
            return Ok(conv
                .path(session)?
                .iter()
                .map(|m| m.id().to_string())
                .collect());
        };
        let mut ids = HashSet::new();
        let mut cur = Some(leaf.to_string());
        while let Some(id) = cur {
            let Some(m) = conv.get(session, &id)? else {
                break;
            };
            if !ids.insert(id) {
                break;
            }
            cur = m.parent_id;
        }
        Ok(ids)
    }

    /// The artifact's metadata (staged artifacts included).
    pub fn meta(&self, id: &str) -> Result<Option<ArtifactMeta>, Defect> {
        let staged = self
            .lock_staged()
            .values()
            .flatten()
            .find(|s| s.meta.id == id)
            .map(|s| s.meta.clone());
        match staged {
            Some(m) => Ok(Some(m)),
            None => self.stored_meta(id),
        }
    }

    /// Latest content on the session's active path, staged versions first: `(n, text)`.
    fn current(&self, session: &str, id: &str) -> Result<Option<(u32, String)>, Defect> {
        let staged = self
            .lock_staged()
            .values()
            .flatten()
            .filter(|s| s.session_id == session && s.meta.id == id)
            .max_by_key(|s| s.n)
            .map(|s| (s.n, s.content.clone()));
        if staged.is_some() {
            return Ok(staged);
        }
        let path = self.path_ids(session, None)?;
        let Some(v) = self
            .stored_versions(id)?
            .into_iter()
            .filter(|v| path.contains(&v.message_id))
            .max_by_key(|v| v.n)
        else {
            return Ok(None);
        };
        let text = self.content_of(&v)?;
        Ok(Some((v.n, text)))
    }

    fn next_n(&self, id: &str) -> Result<u32, Defect> {
        let stored = self
            .stored_versions(id)?
            .iter()
            .map(|v| v.n)
            .max()
            .unwrap_or(0);
        let staged = self
            .lock_staged()
            .values()
            .flatten()
            .filter(|s| s.meta.id == id)
            .map(|s| s.n)
            .max()
            .unwrap_or(0);
        Ok(stored.max(staged) + 1)
    }

    fn stage(&self, ctx: &ToolCtx, meta: ArtifactMeta, content: String) -> Result<u32, Defect> {
        let n = self.next_n(&meta.id)?;
        self.lock_staged()
            .entry(ctx.call_id.clone())
            .or_default()
            .push(Staged {
                session_id: ctx.env.session_id.clone(),
                meta,
                n,
                content,
                sink: ctx.events.clone(),
            });
        Ok(n)
    }

    /// Commit versions staged by the tool calls of an appended assistant message.
    fn commit(&self, session: &str, m: &StoredMessage) {
        let calls: Vec<&str> = m
            .message
            .parts
            .iter()
            .filter_map(|p| match p {
                Part::ToolCall(c) => Some(c.id.0.as_str()),
                _ => None,
            })
            .collect();
        if calls.is_empty() {
            return;
        }
        let items: Vec<Staged> = {
            let mut g = self.lock_staged();
            calls
                .iter()
                .filter_map(|c| g.remove(*c))
                .flatten()
                .collect()
        };
        for s in items {
            if s.session_id != session {
                continue;
            }
            match self.persist(&s, m.id()) {
                Ok(n) => s
                    .sink
                    .publish(SessionEvent::ArtifactVersioned(ArtifactVersioned {
                        artifact_id: s.meta.id.clone(),
                        n,
                        message_id: m.id().to_string(),
                        artifact_type: s.meta.ty.clone(),
                        title: s.meta.title.clone(),
                    })),
                Err(e) => tracing::error!(incident_id = %e.incident_id, artifact = %s.meta.id,
                    "artifact version not stored"),
            }
        }
    }

    fn persist(&self, s: &Staged, message_id: &str) -> Result<u32, Defect> {
        match self.for_session(&s.meta.session_id) {
            Backend::Store(st) => {
                let a = genie_store::artifacts::Artifact {
                    id: s.meta.id.clone(),
                    session_id: s.meta.session_id.clone(),
                    ty: s.meta.ty.clone(),
                    title: Some(s.meta.title.clone()),
                    created: s.meta.created,
                };
                st.write(|tx| genie_store::artifacts::insert(tx, &a))
                    .map_err(sd)?;
                let v = st
                    .add_artifact_version(&s.meta.id, message_id, &s.content)
                    .map_err(sd)?;
                Ok(u32::try_from(v.n).unwrap_or(0))
            }
            Backend::Memory(mem) => {
                let mut g = mem.lock().unwrap_or_else(|e| e.into_inner());
                let entry = g
                    .items
                    .entry(s.meta.id.clone())
                    .or_insert_with(|| (s.meta.clone(), vec![]));
                let n = entry.1.iter().map(|v| v.n).max().unwrap_or(0) + 1;
                entry.1.push(Version {
                    n,
                    message_id: message_id.to_string(),
                    content: Some(s.content.clone()),
                    blob_id: None,
                });
                Ok(n)
            }
        }
    }

    /// `artifact.list`: artifacts with their version numbers on the path to `leaf`
    /// (the active leaf by default). Artifacts with no version on that path are left out.
    pub fn list(
        &self,
        session: &str,
        leaf: Option<&str>,
    ) -> Result<Vec<(ArtifactMeta, Vec<u32>)>, Defect> {
        let path = self.path_ids(session, leaf)?;
        let mut out = vec![];
        for a in self.session_artifacts(session)? {
            let ns: Vec<u32> = self
                .stored_versions(&a.id)?
                .into_iter()
                .filter(|v| path.contains(&v.message_id))
                .map(|v| v.n)
                .collect();
            if !ns.is_empty() {
                out.push((a, ns));
            }
        }
        Ok(out)
    }

    /// `artifact.get`: one version's metadata and inline content or blob id.
    pub fn get(&self, id: &str, n: u32) -> Result<Option<(ArtifactMeta, Version)>, Defect> {
        let Some(meta) = self.stored_meta(id)? else {
            return Ok(None);
        };
        Ok(self
            .stored_versions(id)?
            .into_iter()
            .find(|v| v.n == n)
            .map(|v| (meta, v)))
    }
}

/// The runtime's conversation, committing staged artifact versions on append.
pub struct ArtifactConversation {
    inner: Arc<dyn ConversationStore>,
    hub: Arc<ArtifactHub>,
}

impl ConversationStore for ArtifactConversation {
    fn path(&self, session: &str) -> Result<Vec<StoredMessage>, Defect> {
        self.inner.path(session)
    }
    fn get(&self, session: &str, id: &str) -> Result<Option<StoredMessage>, Defect> {
        self.inner.get(session, id)
    }
    fn append(&self, session: &str, m: &StoredMessage) -> Result<(), Defect> {
        self.inner.append(session, m)?;
        if m.is_assistant() {
            self.hub.commit(session, m);
        }
        Ok(())
    }
    fn set_leaf(&self, session: &str, leaf: Option<&str>) -> Result<(), Defect> {
        self.inner.set_leaf(session, leaf)
    }
    fn meta(&self, session: &str) -> Result<SessionMeta, Defect> {
        self.inner.meta(session)
    }
    fn set_meta(&self, session: &str, meta: &SessionMeta) -> Result<(), Defect> {
        self.inner.set_meta(session, meta)
    }
    fn wire_records(&self, session: &str) -> Vec<WireRecord> {
        self.inner.wire_records(session)
    }
    fn add_wire_records(&self, session: &str, records: Vec<WireRecord>) {
        self.inner.add_wire_records(session, records);
    }
    fn record_usage(&self, session: &str, line: &UsageLine) -> Result<(), Defect> {
        self.inner.record_usage(session, line)
    }
}

// ------------------------------------------------------------------ tools

fn available(hub: &ArtifactHub, env: &ToolEnv) -> Availability {
    if !hub.enabled() {
        Availability::Unavailable(Cow::Borrowed("artifacts are off"))
    } else if env.kind != SessionKind::Chat {
        Availability::Unavailable(Cow::Borrowed("artifacts are a Chat feature"))
    } else {
        Availability::Available
    }
}

fn internal(e: &Defect) -> ToolError {
    ToolError::Failed {
        message: format!(
            "the artifact could not be saved (incident {})",
            e.incident_id
        ),
        may_have_run: false,
    }
}

fn check_size(content: &str) -> Result<(), ToolError> {
    if content.len() > MAX_CONTENT {
        return Err(ToolError::invalid_input(vec![format!(
            "content is {} bytes; artifacts are limited to {MAX_CONTENT} bytes",
            content.len()
        )]));
    }
    Ok(())
}

/// The artifact `id` of this session, or a model-readable error.
fn own(hub: &ArtifactHub, ctx: &ToolCtx, id: &str) -> Result<ArtifactMeta, ToolError> {
    match hub.meta(id).map_err(|e| internal(&e))? {
        Some(m) if m.session_id == ctx.env.session_id => Ok(m),
        _ => Err(ToolError::invalid_input(vec![format!(
            "no artifact `{id}` in this chat; create one with `{CREATE}`"
        )])),
    }
}

#[derive(Deserialize)]
struct CreateInput {
    #[serde(rename = "type")]
    ty: String,
    title: String,
    content: String,
    #[serde(default)]
    language: Option<String>,
}

#[derive(Deserialize)]
struct UpdateInput {
    id: String,
    old_str: String,
    new_str: String,
}

#[derive(Deserialize)]
struct RewriteInput {
    id: String,
    content: String,
}

pub struct ArtifactCreate {
    hub: Arc<ArtifactHub>,
    schema: Value,
}

pub struct ArtifactUpdate {
    hub: Arc<ArtifactHub>,
    schema: Value,
}

pub struct ArtifactRewrite {
    hub: Arc<ArtifactHub>,
    schema: Value,
}

/// The three tools over one hub.
pub fn tools(hub: &Arc<ArtifactHub>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(ArtifactCreate {
            hub: hub.clone(),
            schema: json!({
                "type": "object",
                "properties": {
                    "type": {"type": "string", "enum": TYPES,
                        "description": "document (Markdown), code, svg, mermaid, html (single file) or react (one component, default export)."},
                    "title": {"type": "string", "description": "Short title shown on the artifact card."},
                    "content": {"type": "string", "description": "The full content."},
                    "language": {"type": "string", "description": "For `code`: the language, e.g. python."}
                },
                "required": ["type", "title", "content"],
                "additionalProperties": false
            }),
        }),
        Arc::new(ArtifactUpdate {
            hub: hub.clone(),
            schema: json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "The artifact id (art_…)."},
                    "old_str": {"type": "string", "description": "Exact text to replace; must occur exactly once."},
                    "new_str": {"type": "string", "description": "Replacement text."}
                },
                "required": ["id", "old_str", "new_str"],
                "additionalProperties": false
            }),
        }),
        Arc::new(ArtifactRewrite {
            hub: hub.clone(),
            schema: json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "The artifact id (art_…)."},
                    "content": {"type": "string", "description": "The complete new content."}
                },
                "required": ["id", "content"],
                "additionalProperties": false
            }),
        }),
    ]
}

macro_rules! common {
    ($name:expr) => {
        fn name(&self) -> &str {
            $name
        }
        fn input_schema(&self) -> &Value {
            &self.schema
        }
        // Local and sandboxed: always allowed (D28), so no permission ask.
        fn kind(&self) -> ToolKind {
            ToolKind::Mutating
        }
        fn authorizes(&self) -> bool {
            false
        }
        fn availability(&self, env: &ToolEnv) -> Availability {
            available(&self.hub, env)
        }
    };
}

#[async_trait]
impl Tool for ArtifactCreate {
    common!(CREATE);

    fn description(&self, _env: &ToolEnv) -> Cow<'_, str> {
        Cow::Owned(format!(
            "Create an artifact: self-contained content shown in a side panel (documents, code \
             files, SVG, Mermaid diagrams, HTML pages, React components). Artifacts have no \
             network access; only these libraries can be imported: {ARTIFACT_LIBRARIES}. Returns \
             the artifact id; change it later with `{UPDATE}` or `{REWRITE}`."
        ))
    }

    async fn execute(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let i: CreateInput = genie_tools::parse_input(input, &self.schema)?;
        if !TYPES.contains(&i.ty.as_str()) {
            return Err(ToolError::invalid_input(vec![format!(
                "type must be one of {}",
                TYPES.join(", ")
            )]));
        }
        if i.title.trim().is_empty() {
            return Err(ToolError::invalid_input(vec!["title is empty".into()]));
        }
        check_size(&i.content)?;
        let meta = ArtifactMeta {
            id: new_id("art"),
            session_id: ctx.env.session_id.clone(),
            ty: i.ty.clone(),
            title: i.title.trim().to_string(),
            created: now_ms(),
        };
        let id = meta.id.clone();
        let n = self
            .hub
            .stage(&ctx, meta, i.content)
            .map_err(|e| internal(&e))?;
        let lang = i
            .language
            .filter(|l| !l.trim().is_empty())
            .map(|l| format!(", {l}"))
            .unwrap_or_default();
        Ok(ToolOutput::Text(format!(
            "Created artifact {id} \"{}\" ({}{lang}), version {n}. It is shown to the user in the artifact panel.",
            i.title.trim(),
            i.ty
        )))
    }
}

#[async_trait]
impl Tool for ArtifactUpdate {
    common!(UPDATE);

    fn description(&self, _env: &ToolEnv) -> Cow<'_, str> {
        Cow::Borrowed(
            "Edit an existing artifact by replacing one exact, unique piece of its current text. \
             Creates a new version. Prefer this over rewriting for small changes.",
        )
    }

    async fn execute(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let i: UpdateInput = genie_tools::parse_input(input, &self.schema)?;
        let meta = own(&self.hub, &ctx, &i.id)?;
        let Some((_, current)) = self
            .hub
            .current(&ctx.env.session_id, &i.id)
            .map_err(|e| internal(&e))?
        else {
            return Err(ToolError::invalid_input(vec![format!(
                "artifact `{}` has no version in this branch of the chat",
                i.id
            )]));
        };
        if i.old_str.is_empty() {
            return Err(ToolError::invalid_input(vec!["old_str is empty".into()]));
        }
        let count = current.matches(&i.old_str).count();
        if count != 1 {
            return Err(ToolError::invalid_input(vec![if count == 0 {
                "old_str was not found in the artifact; copy it exactly from the latest version"
                    .to_string()
            } else {
                format!("old_str occurs {count} times; include more context so it is unique")
            }]));
        }
        let next = current.replacen(&i.old_str, &i.new_str, 1);
        check_size(&next)?;
        let n = self
            .hub
            .stage(&ctx, meta.clone(), next)
            .map_err(|e| internal(&e))?;
        Ok(ToolOutput::Text(format!(
            "Updated artifact {} \"{}\" to version {n}.",
            meta.id, meta.title
        )))
    }
}

#[async_trait]
impl Tool for ArtifactRewrite {
    common!(REWRITE);

    fn description(&self, _env: &ToolEnv) -> Cow<'_, str> {
        Cow::Borrowed(
            "Replace the whole content of an existing artifact (a new version). Use for large \
             changes; use `artifact_update` for small edits.",
        )
    }

    async fn execute(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let i: RewriteInput = genie_tools::parse_input(input, &self.schema)?;
        let meta = own(&self.hub, &ctx, &i.id)?;
        check_size(&i.content)?;
        let n = self
            .hub
            .stage(&ctx, meta.clone(), i.content)
            .map_err(|e| internal(&e))?;
        Ok(ToolOutput::Text(format!(
            "Rewrote artifact {} \"{}\" as version {n}.",
            meta.id, meta.title
        )))
    }
}
