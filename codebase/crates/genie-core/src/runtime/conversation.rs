//! The message tree the turn loop reads and appends to (D24, 04 §3.2).
//!
//! [`ConversationStore`] is the engine's port; [`MemoryConversation`] serves tests and
//! incognito sessions, [`StoreConversation`] projects messages and parts into
//! `genie-store` (canonical parts are the part `body`; FTS indexes their `text`).

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};

use genie_providers::model::{Message, MessageId, Part, Role, Usage, WireRecord};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Defect;
use crate::runtime::context::FrozenBaseline;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MsgKind {
    Normal,
    /// Our summary (or a provider checkpoint) replacing everything before it (D33).
    Compaction,
    /// A frozen `<system-update>` (06 §2.2, §2.6.6).
    SystemUpdate,
    /// Engine-written user-role text (continue/max-steps notes, resume notes).
    Synthetic,
}

/// A message on the tree with its engine metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredMessage {
    pub message: Message,
    pub parent_id: Option<String>,
    pub kind: MsgKind,
    pub turn_id: Option<String>,
    pub created_ms: u64,
    /// Catalog model id for assistant messages.
    pub model: Option<String>,
    /// Assistant messages: the usage and cost of the step that produced them.
    pub usage: Option<Usage>,
    pub cost_usd: Option<f64>,
}

impl StoredMessage {
    pub fn new(message: Message, kind: MsgKind) -> Self {
        StoredMessage {
            message,
            parent_id: None,
            kind,
            turn_id: None,
            created_ms: crate::bus::now_ms(),
            model: None,
            usage: None,
            cost_usd: None,
        }
    }

    pub fn id(&self) -> &str {
        &self.message.id.0
    }

    pub fn is_assistant(&self) -> bool {
        self.message.role == Role::Assistant
    }
}

/// Per-session runtime state that lives next to the tree.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMeta {
    pub baseline: Option<FrozenBaseline>,
    /// compaction message id → first kept message id (`None`: nothing kept verbatim).
    #[serde(default)]
    pub compactions: BTreeMap<String, Option<String>>,
    /// Last measured context size: `(provider/model, tokens, path length at measurement)`.
    #[serde(default)]
    pub measured: Option<(String, u64, usize)>,
    /// Session usage totals for `usage.updated`.
    #[serde(default)]
    pub totals: UsageTotals,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageTotals {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub reasoning: u64,
    pub cost_usd: f64,
}

impl Eq for UsageTotals {}

/// One usage line (06 §1, D34): per step, compaction, title.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageLine {
    /// `chat` | `compaction` | `title`.
    pub kind: &'static str,
    pub provider: String,
    pub model: String,
    pub project_id: Option<String>,
    pub usage: Usage,
    pub cost_usd: f64,
    /// The assistant message the usage belongs to (chat steps); shown per message.
    pub message_id: Option<String>,
}

pub trait ConversationStore: Send + Sync {
    /// Root → active leaf.
    fn path(&self, session: &str) -> Result<Vec<StoredMessage>, Defect>;
    fn get(&self, session: &str, id: &str) -> Result<Option<StoredMessage>, Defect>;
    /// Append under `m.parent_id` and make it the active leaf.
    fn append(&self, session: &str, m: &StoredMessage) -> Result<(), Defect>;
    /// Move the active leaf (regenerate, branch switch). `None` = empty path.
    fn set_leaf(&self, session: &str, leaf: Option<&str>) -> Result<(), Defect>;
    fn meta(&self, session: &str) -> Result<SessionMeta, Defect>;
    fn set_meta(&self, session: &str, meta: &SessionMeta) -> Result<(), Defect>;
    /// Wire records (D18) for byte-exact replay.
    fn wire_records(&self, session: &str) -> Vec<WireRecord>;
    fn add_wire_records(&self, session: &str, records: Vec<WireRecord>);
    fn record_usage(&self, session: &str, line: &UsageLine) -> Result<(), Defect>;
}

// ------------------------------------------------------------------ memory

#[derive(Default)]
struct MemSession {
    messages: HashMap<String, StoredMessage>,
    leaf: Option<String>,
    meta: SessionMeta,
    wire: Vec<WireRecord>,
    usage: Vec<UsageLine>,
}

/// In-memory conversation (tests, incognito).
#[derive(Default)]
pub struct MemoryConversation {
    inner: Mutex<HashMap<String, MemSession>>,
}

impl MemoryConversation {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, MemSession>> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Usage lines recorded for `session` (tests).
    pub fn usage_lines(&self, session: &str) -> Vec<UsageLine> {
        self.lock()
            .get(session)
            .map(|s| s.usage.clone())
            .unwrap_or_default()
    }

    /// Every message ever appended to `session`, in no particular order (tests).
    pub fn all(&self, session: &str) -> Vec<StoredMessage> {
        self.lock()
            .get(session)
            .map(|s| s.messages.values().cloned().collect())
            .unwrap_or_default()
    }
}

impl ConversationStore for MemoryConversation {
    fn path(&self, session: &str) -> Result<Vec<StoredMessage>, Defect> {
        let g = self.lock();
        let Some(s) = g.get(session) else {
            return Ok(vec![]);
        };
        let mut out = vec![];
        let mut cur = s.leaf.clone();
        while let Some(id) = cur {
            let Some(m) = s.messages.get(&id) else {
                return Err(Defect::new(format!("dangling message {id}")));
            };
            cur = m.parent_id.clone();
            out.push(m.clone());
        }
        out.reverse();
        Ok(out)
    }

    fn get(&self, session: &str, id: &str) -> Result<Option<StoredMessage>, Defect> {
        Ok(self
            .lock()
            .get(session)
            .and_then(|s| s.messages.get(id).cloned()))
    }

    fn append(&self, session: &str, m: &StoredMessage) -> Result<(), Defect> {
        let mut g = self.lock();
        let s = g.entry(session.to_string()).or_default();
        s.messages.insert(m.id().to_string(), m.clone());
        s.leaf = Some(m.id().to_string());
        Ok(())
    }

    fn set_leaf(&self, session: &str, leaf: Option<&str>) -> Result<(), Defect> {
        self.lock().entry(session.to_string()).or_default().leaf = leaf.map(String::from);
        Ok(())
    }

    fn meta(&self, session: &str) -> Result<SessionMeta, Defect> {
        Ok(self
            .lock()
            .get(session)
            .map(|s| s.meta.clone())
            .unwrap_or_default())
    }

    fn set_meta(&self, session: &str, meta: &SessionMeta) -> Result<(), Defect> {
        self.lock().entry(session.to_string()).or_default().meta = meta.clone();
        Ok(())
    }

    fn wire_records(&self, session: &str) -> Vec<WireRecord> {
        self.lock()
            .get(session)
            .map(|s| s.wire.clone())
            .unwrap_or_default()
    }

    fn add_wire_records(&self, session: &str, records: Vec<WireRecord>) {
        let mut g = self.lock();
        let s = g.entry(session.to_string()).or_default();
        for r in records {
            s.wire
                .retain(|w| !(w.message_id == r.message_id && w.route_key == r.route_key));
            s.wire.push(r);
        }
    }

    fn record_usage(&self, session: &str, line: &UsageLine) -> Result<(), Defect> {
        let mut g = self.lock();
        let s = g.entry(session.to_string()).or_default();
        if let Some(m) = line
            .message_id
            .as_ref()
            .and_then(|id| s.messages.get_mut(id))
        {
            m.usage = Some(line.usage);
            m.cost_usd = Some(line.cost_usd);
        }
        s.usage.push(line.clone());
        Ok(())
    }
}

// ------------------------------------------------------------------ store

/// Messages and parts in `genie-store` (spec 04 §3).
///
/// Wire records are cached in memory for now: the `wire_record` table stores request
/// and response blobs per step, which needs the request bytes the executor builds
/// (gap: persisted byte-exact replay across daemon restarts).
pub struct StoreConversation {
    store: Arc<genie_store::Store>,
    wire: Mutex<HashMap<String, Vec<WireRecord>>>,
}

fn sd(e: genie_store::StoreError) -> Defect {
    Defect::new(format!("store: {e}"))
}

fn role_str(r: Role) -> &'static str {
    match r {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn parse_role(s: &str) -> Role {
    match s {
        "system" => Role::System,
        "assistant" => Role::Assistant,
        "tool" => Role::Tool,
        _ => Role::User,
    }
}

fn part_kind(p: &Part) -> &'static str {
    match p {
        Part::Text(_) => "text",
        Part::Media(_) => "media",
        Part::ToolCall(_) => "tool_call",
        Part::ToolResult(_) => "tool_result",
        Part::Reasoning(_) => "reasoning",
        Part::Compaction(_) => "compaction",
        Part::EffortMarker(_) => "effort_marker",
        Part::Unknown(_) => "unknown",
    }
}

const META_PREFIX: &str = "genie.runtime.session/";

impl StoreConversation {
    pub fn new(store: Arc<genie_store::Store>) -> Self {
        StoreConversation {
            store,
            wire: Mutex::new(HashMap::new()),
        }
    }

    fn load(
        conn: &genie_store::Connection,
        row: genie_store::messages::Message,
    ) -> genie_store::Result<StoredMessage> {
        let parts = genie_store::messages::parts(conn, &row.id)?;
        let mut canonical = Vec::with_capacity(parts.len());
        for p in parts {
            match serde_json::from_value::<Part>(p.body.clone()) {
                Ok(c) => canonical.push(c),
                Err(e) => tracing::warn!(part = %p.id, error = %e, "unreadable part skipped"),
            }
        }
        let role = parse_role(&row.role);
        let kind = match (row.kind, role) {
            (genie_store::messages::MessageKind::Compaction, _) => MsgKind::Compaction,
            (genie_store::messages::MessageKind::Synthetic, Role::System) => MsgKind::SystemUpdate,
            (genie_store::messages::MessageKind::Synthetic, _) => MsgKind::Synthetic,
            _ => MsgKind::Normal,
        };
        let origin = row
            .origin
            .as_ref()
            .and_then(|o| serde_json::from_value(o.clone()).ok());
        let turn_id = row
            .error
            .as_ref()
            .and_then(|e| e.get("turn_id"))
            .and_then(Value::as_str)
            .map(String::from);
        Ok(StoredMessage {
            message: Message {
                id: MessageId::new(row.id),
                role,
                parts: canonical,
                origin,
            },
            parent_id: row.parent_id,
            kind,
            turn_id,
            created_ms: u64::try_from(row.created).unwrap_or(0),
            model: row.model,
            usage: row
                .usage
                .as_ref()
                .and_then(|u| serde_json::from_value(u.clone()).ok()),
            cost_usd: row.cost_usd,
        })
    }
}

impl ConversationStore for StoreConversation {
    fn path(&self, session: &str) -> Result<Vec<StoredMessage>, Defect> {
        self.store
            .read(|c| {
                genie_store::messages::active_path(c, session)?
                    .into_iter()
                    .map(|m| Self::load(c, m))
                    .collect()
            })
            .map_err(sd)
    }

    fn get(&self, session: &str, id: &str) -> Result<Option<StoredMessage>, Defect> {
        self.store
            .read(|c| match genie_store::messages::get(c, id)? {
                Some(m) if m.session_id == session => Self::load(c, m).map(Some),
                _ => Ok(None),
            })
            .map_err(sd)
    }

    fn append(&self, session: &str, m: &StoredMessage) -> Result<(), Defect> {
        use genie_store::messages as sm;
        self.store
            .write(|tx| {
                genie_store::sessions::ensure(tx, session)?;
                let mut row =
                    sm::Message::new(session, m.parent_id.as_deref(), role_str(m.message.role));
                row.id = m.id().to_string();
                row.created = i64::try_from(m.created_ms).unwrap_or(i64::MAX);
                row.model.clone_from(&m.model);
                row.provider = m.message.origin.as_ref().map(|o| o.provider.0.clone());
                row.origin = m
                    .message
                    .origin
                    .as_ref()
                    .and_then(|o| serde_json::to_value(o).ok());
                // The `error` column carries engine metadata until it has its own column.
                row.error = m
                    .turn_id
                    .as_ref()
                    .map(|t| serde_json::json!({"turn_id": t}));
                row.kind = match m.kind {
                    MsgKind::Normal => sm::MessageKind::Normal,
                    MsgKind::Compaction => sm::MessageKind::Compaction,
                    MsgKind::SystemUpdate | MsgKind::Synthetic => sm::MessageKind::Synthetic,
                };
                sm::insert(tx, &row)?;
                for (i, p) in m.message.parts.iter().enumerate() {
                    let body = serde_json::to_value(p)?;
                    let mut part =
                        sm::Part::new(&row.id, i64::try_from(i).unwrap_or(0), part_kind(p), body);
                    match p {
                        Part::ToolCall(c) => part.call_id = Some(c.id.0.clone()),
                        Part::ToolResult(r) => part.call_id = Some(r.call_id.0.clone()),
                        // Attachments keep their blob alive (refcount, 04 §4).
                        Part::Media(genie_providers::model::MediaPart {
                            source: genie_providers::model::MediaSource::Blob { id },
                            ..
                        }) => part.blob_id = Some(id.0.clone()),
                        _ => {}
                    }
                    sm::insert_part(tx, &part)?;
                }
                genie_store::sessions::set_active_leaf(tx, session, Some(&row.id))
            })
            .map_err(sd)
    }

    fn set_leaf(&self, session: &str, leaf: Option<&str>) -> Result<(), Defect> {
        self.store
            .write(|tx| {
                genie_store::sessions::ensure(tx, session)?;
                genie_store::sessions::set_active_leaf(tx, session, leaf)
            })
            .map_err(sd)
    }

    fn meta(&self, session: &str) -> Result<SessionMeta, Defect> {
        let v = self
            .store
            .read(|c| genie_store::kv::get(c, &format!("{META_PREFIX}{session}")))
            .map_err(sd)?;
        Ok(v.and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default())
    }

    fn set_meta(&self, session: &str, meta: &SessionMeta) -> Result<(), Defect> {
        let v = serde_json::to_value(meta).map_err(|e| Defect::new(e.to_string()))?;
        self.store
            .write(|tx| genie_store::kv::set(tx, &format!("{META_PREFIX}{session}"), &v))
            .map_err(sd)
    }

    fn wire_records(&self, session: &str) -> Vec<WireRecord> {
        self.wire
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(session)
            .cloned()
            .unwrap_or_default()
    }

    fn add_wire_records(&self, session: &str, records: Vec<WireRecord>) {
        let mut g = self.wire.lock().unwrap_or_else(|p| p.into_inner());
        let v = g.entry(session.to_string()).or_default();
        for r in records {
            v.retain(|w| !(w.message_id == r.message_id && w.route_key == r.route_key));
            v.push(r);
        }
    }

    fn record_usage(&self, session: &str, line: &UsageLine) -> Result<(), Defect> {
        let u = &line.usage;
        let tin = i64::try_from(u.input_total).unwrap_or(i64::MAX);
        let tout = i64::try_from(u.output_total).unwrap_or(i64::MAX);
        let day = crate::runtime::context::utc_day(crate::bus::now_ms());
        self.store
            .write(|tx| {
                genie_store::sessions::ensure(tx, session)?;
                genie_store::sessions::add_usage(tx, session, tin, tout, line.cost_usd)?;
                if let Some(id) = &line.message_id {
                    let v = serde_json::to_value(u)?;
                    genie_store::messages::set_usage(tx, id, &v, line.cost_usd)?;
                }
                genie_store::kv::add_usage(
                    tx,
                    &genie_store::kv::Usage {
                        day,
                        provider: line.provider.clone(),
                        model: line.model.clone(),
                        project_id: line.project_id.clone().unwrap_or_default(),
                        kind: line.kind.to_string(),
                        tokens_in: tin,
                        tokens_out: tout,
                        cache_read: i64::try_from(u.cache_read).unwrap_or(i64::MAX),
                        cache_write: i64::try_from(u.cache_write_5m + u.cache_write_1h)
                            .unwrap_or(i64::MAX),
                        calls: 1,
                        cost_usd: line.cost_usd,
                    },
                )
            })
            .map_err(sd)
    }
}
