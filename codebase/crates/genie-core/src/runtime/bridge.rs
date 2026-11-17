//! Core implementations of the `genie-tools` ports (01 §2.1 rule 3): the permission
//! gate over [`ToolAuthorizer`] + [`TurnCtx::ask_with`], the event sink, blob-backed
//! spill and store-backed memory.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use genie_protocol::ask::{AskAnswer, Decision};
use genie_protocol::common::Mode;
use genie_protocol::event::{SessionEvent, ToolProgress, ToolStarted};
use genie_tools::{
    BlockSource, EventSink, MemoryEntry, MemoryPort, PermissionGate, PermissionRequest, SpillStore,
    ToolError as TError, ToolKind,
};

use crate::error::{BlockSource as CoreBlock, ToolError};
use crate::runtime::ports::{AuthzDecision, AuthzRequest, ToolAuthorizer};
use crate::session::TurnCtx;

impl From<TError> for ToolError {
    fn from(e: TError) -> Self {
        match e {
            TError::InvalidInput { issues } => ToolError::invalid_input(issues),
            TError::Failed {
                message,
                may_have_run,
            } => ToolError::Failed {
                message,
                may_have_run,
            },
            TError::Declined { message } => ToolError::Declined { message },
            TError::Blocked { rule, origin } => ToolError::Blocked {
                rule,
                origin: match origin {
                    BlockSource::Policy => CoreBlock::Policy,
                    BlockSource::Rule => CoreBlock::Rule,
                },
            },
            TError::Timeout { after_s } => ToolError::Timeout { after_s },
            TError::Cancelled => ToolError::Failed {
                message: "the call was cancelled".into(),
                may_have_run: true,
            },
        }
    }
}

/// Publishes `tool.started` once per call: after authorisation (02 §4.1).
pub(crate) struct Started {
    flag: AtomicBool,
    turn: Arc<TurnCtx>,
    call_id: String,
    name: String,
}

impl Started {
    pub(crate) fn new(turn: Arc<TurnCtx>, call_id: String, name: String) -> Arc<Self> {
        Arc::new(Started {
            flag: AtomicBool::new(false),
            turn,
            call_id,
            name,
        })
    }

    pub(crate) fn mark(&self, resource: Option<String>) {
        if !self.flag.swap(true, Ordering::SeqCst)
            && let Err(e) = self.turn.publish(SessionEvent::ToolStarted(ToolStarted {
                call_id: self.call_id.clone(),
                name: self.name.clone(),
                resource,
            }))
        {
            tracing::error!(error = %e, "publish tool.started failed");
        }
    }

    pub(crate) fn is_set(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

/// `ctx.permissions` for one call.
pub(crate) struct CallGate {
    pub turn: Arc<TurnCtx>,
    pub authorizer: Arc<dyn ToolAuthorizer>,
    pub tool: String,
    pub kind: ToolKind,
    pub mode: Mode,
    pub started: Arc<Started>,
}

#[async_trait]
impl PermissionGate for CallGate {
    async fn assert(&self, req: PermissionRequest) -> Result<(), TError> {
        let areq = AuthzRequest {
            session_id: self.turn.session_id.clone(),
            tool: self.tool.clone(),
            kind: self.kind,
            resource: req.resource.clone(),
            preview: req.preview,
            save_suggestions: req.save_suggestions,
            mode: self.mode,
        };
        match self.authorizer.authorize(&areq) {
            AuthzDecision::Allow => {}
            AuthzDecision::Deny { reason } => {
                return Err(TError::Blocked {
                    rule: reason,
                    origin: BlockSource::Rule,
                });
            }
            AuthzDecision::Ask(ask) => {
                let v = self
                    .turn
                    .ask_with(*ask)
                    .await
                    .map_err(|_| TError::Cancelled)?;
                let ans: AskAnswer = serde_json::from_value(v)
                    .unwrap_or_else(|_| AskAnswer::decision(Decision::Cancel));
                match ans.decision {
                    Decision::Once | Decision::Always | Decision::Submit => {}
                    Decision::Reject => {
                        return Err(TError::Declined {
                            message: ans.message,
                        });
                    }
                    Decision::Cancel | Decision::Unknown => return Err(TError::Cancelled),
                }
            }
        }
        self.started.mark(req.resource);
        Ok(())
    }
}

/// Tool events go through the turn's bus.
pub(crate) struct TurnEvents(pub Arc<TurnCtx>);

impl EventSink for TurnEvents {
    fn publish(&self, event: SessionEvent) {
        if let Err(e) = self.0.publish(event) {
            tracing::error!(error = %e, "tool event publish failed");
        }
    }

    fn progress(&self, progress: ToolProgress) {
        self.0
            .publish_ephemeral(SessionEvent::ToolProgress(progress));
    }
}

/// Spilled output as store blobs (06 §2.4); the reference is the blob id. Blobs are
/// content-addressed, so a reference can't be guessed without the content.
pub struct BlobSpill {
    store: Arc<genie_store::Store>,
}

impl BlobSpill {
    pub fn new(store: Arc<genie_store::Store>) -> Self {
        BlobSpill { store }
    }
}

impl SpillStore for BlobSpill {
    fn put(&self, _session_id: &str, text: &str) -> Result<String, String> {
        self.store
            .put_blob(text.as_bytes(), Some("text/plain; charset=utf-8"))
            .map_err(|e| e.to_string())
    }

    fn read(&self, _session_id: &str, reference: &str) -> Result<String, String> {
        if reference.len() != 64 || !reference.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("{reference} is not a spilled-output reference"));
        }
        let bytes = self
            .store
            .read_blob(reference)
            .map_err(|_| format!("no spilled output {reference}"))?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
}

/// Memory on `genie-store` (04 §3.4, D31).
pub struct StoreMemory {
    store: Arc<genie_store::Store>,
}

impl StoreMemory {
    pub fn new(store: Arc<genie_store::Store>) -> Self {
        StoreMemory { store }
    }
}

fn entry(m: genie_store::memory::Memory) -> MemoryEntry {
    MemoryEntry {
        id: m.id,
        scope: m.scope,
        text: m.text,
    }
}

impl MemoryPort for StoreMemory {
    fn save(&self, scope: &str, text: &str, src: Option<&str>) -> Result<MemoryEntry, String> {
        self.store
            .write(|tx| genie_store::memory::add(tx, scope, text, src, None))
            .map(entry)
            .map_err(|e| e.to_string())
    }

    fn update(&self, id: &str, text: &str) -> Result<MemoryEntry, String> {
        self.store
            .write(|tx| {
                genie_store::memory::update_text(tx, id, text)?;
                genie_store::memory::get(tx, id)?
                    .ok_or_else(|| genie_store::StoreError::NotFound(id.to_string()))
            })
            .map(entry)
            .map_err(|e| e.to_string())
    }

    fn delete(&self, id: &str) -> Result<Option<MemoryEntry>, String> {
        self.store
            .write(|tx| {
                let m = genie_store::memory::get(tx, id)?;
                if m.is_some() {
                    genie_store::memory::delete(tx, id)?;
                }
                Ok(m.map(entry))
            })
            .map_err(|e| e.to_string())
    }

    fn search(&self, scopes: &[&str], query: &str, limit: u32) -> Result<Vec<MemoryEntry>, String> {
        self.store
            .read(|c| genie_store::memory::search(c, scopes, query, limit))
            .map(|v| v.into_iter().map(entry).collect())
            .map_err(|e| e.to_string())
    }
}
