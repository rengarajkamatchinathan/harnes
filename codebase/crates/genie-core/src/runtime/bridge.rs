//! Core implementations of the `genie-tools` ports (01 §2.1 rule 3): the permission
//! gate over [`ToolAuthorizer`] + [`TurnCtx::ask_with`], the event sink, blob-backed
//! spill and store-backed memory.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use genie_protocol::ask::{AskAnswer, AutoResolver, Decision, ResolvedBy};
use genie_protocol::common::Mode;
use genie_protocol::event::{ErrorInfo, SessionEvent, ToolProgress, ToolStarted};
use genie_tools::{
    BlockSource, EventSink, MemoryEntry, MemoryPort, PermissionGate, PermissionRequest, SpillStore,
    ToolError as TError, ToolKind,
};

use crate::error::{BlockSource as CoreBlock, ToolError};
use crate::ids::new_id;
use crate::runtime::ports::{AuthzDecision, AuthzRequest, SettingsSource, ToolAuthorizer};
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

/// Per-call state shared by the gate and the turn loop: publishes `tool.started` once,
/// after authorisation (02 §4.1), and remembers why a call was blocked.
pub(crate) struct Started {
    flag: AtomicBool,
    turn: Arc<TurnCtx>,
    call_id: String,
    pub(crate) name: String,
    pub(crate) kind: ToolKind,
    blocked: Mutex<Option<ErrorInfo>>,
    /// The user declined without feedback: like Claude's "No", the turn stops after this
    /// step and waits for the user instead of letting the model try something else.
    stop: AtomicBool,
}

impl Started {
    pub(crate) fn new(
        turn: Arc<TurnCtx>,
        call_id: String,
        name: String,
        kind: ToolKind,
    ) -> Arc<Self> {
        Arc::new(Started {
            flag: AtomicBool::new(false),
            turn,
            call_id,
            name,
            kind,
            blocked: Mutex::new(None),
            stop: AtomicBool::new(false),
        })
    }

    pub(crate) fn stop_requested(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    pub(crate) fn set_blocked(&self, info: ErrorInfo) {
        *self.blocked.lock().unwrap_or_else(|e| e.into_inner()) = Some(info);
    }

    /// `tool.ended.error` for a blocked call (`policy_blocked` + the 08 §5.3 text).
    pub(crate) fn blocked(&self) -> Option<ErrorInfo> {
        self.blocked
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
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
    /// The mode at turn start; the live session mode wins when it can be read, so a
    /// mode switch (e.g. to accept-edits) applies to the calls still to come in this turn.
    pub mode: Mode,
    pub settings: Arc<dyn SettingsSource>,
    /// The agent's `permissionMode`, which only narrows the session mode (D46).
    pub agent_mode: Option<Mode>,
    pub started: Arc<Started>,
    /// The turn's taint (SA-29), set by earlier tool results.
    pub taint: Arc<Mutex<Option<String>>>,
    /// `tool.before` verdict and `permission.request` hooks of this call (06 §12).
    pub hooks: Option<Arc<crate::runtime::hooks::CallHooks>>,
}

#[async_trait]
impl PermissionGate for CallGate {
    async fn assert(&self, req: PermissionRequest) -> Result<(), TError> {
        let mode = self
            .settings
            .settings(&self.turn.session_id)
            .map_or(self.mode, |s| {
                crate::runtime::agents::narrow_mode(s.mode, self.agent_mode)
            });
        let areq = AuthzRequest {
            session_id: self.turn.session_id.clone(),
            turn_id: self.turn.turn_id.clone(),
            tool: self.tool.clone(),
            kind: self.kind,
            resource: req.resource.clone(),
            preview: req.preview,
            save_suggestions: req.save_suggestions,
            mode,
            taint: self.taint.lock().unwrap_or_else(|e| e.into_inner()).clone(),
        };
        let mut decision = self.authorizer.authorize(&areq);
        if let Some(h) = &self.hooks {
            // Deny rules and policy stand; a hook only moves between allow and ask.
            decision = h.adjust(decision, &areq);
        }
        match decision {
            AuthzDecision::Allow => {}
            AuthzDecision::Deny {
                rule,
                source,
                message,
            } => {
                let code = match source {
                    CoreBlock::Policy => "policy_blocked",
                    CoreBlock::Rule => "GENIE-TOOL-BLOCKED",
                };
                self.started.set_blocked(ErrorInfo {
                    code: code.into(),
                    message,
                    link: None,
                });
                return Err(TError::Blocked {
                    rule,
                    origin: match source {
                        CoreBlock::Policy => BlockSource::Policy,
                        CoreBlock::Rule => BlockSource::Rule,
                    },
                });
            }
            AuthzDecision::Ask(mut ask) => {
                if let Some(h) = &self.hooks
                    && let Some(r) = h.before_ask(&areq).await
                {
                    r?;
                    self.started.mark(req.resource);
                    return Ok(());
                }
                if ask.id.is_empty() {
                    ask.id = new_id("ask");
                }
                let ask_id = ask.id.clone();
                self.authorizer.asked(&areq, &ask_id, &ask);
                let ans = match self.turn.ask_with(*ask).await {
                    Ok(v) => serde_json::from_value::<AskAnswer>(v)
                        .unwrap_or_else(|_| AskAnswer::decision(Decision::Cancel)),
                    Err(_) => AskAnswer::decision(Decision::Cancel),
                };
                for other in self.authorizer.answered(&areq, &ask_id, &ans) {
                    // Released by the new rule (02 §7 step 6); already answered is fine.
                    let _ = self.turn.resolve_ask(
                        &other,
                        AskAnswer::decision(Decision::Once),
                        ResolvedBy::Auto(AutoResolver::Rule),
                    );
                }
                match ans.decision {
                    Decision::Once | Decision::Always | Decision::Submit => {}
                    Decision::Reject => {
                        if ans.message.as_deref().is_none_or(|m| m.trim().is_empty()) {
                            self.started.stop.store(true, Ordering::SeqCst);
                            return Err(TError::Declined { message: None });
                        }
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
