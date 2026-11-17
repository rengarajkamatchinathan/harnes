//! Memory state and memory in the prompt (06 §6, D31, ticket 10).
//!
//! - **State:** on by default; `memory.set_state paused` turns the memory tools off and
//!   keeps memory out of the prompt until it is switched back on. Persisted in `kv`
//!   (`memory.state`), so it survives restarts. An admin switch (`set_allowed(false)`)
//!   overrides it.
//! - **Prompt:** global memory and the session's project memory are baseline sources
//!   (`<memory scope="global|project">`, ranks 5 and 8), most recently used first and
//!   capped at ~8 KiB each; the model reaches the rest with `memory_search`. Changes reach
//!   a running session as `<system-update>` deltas (06 §2.2).
//! - Incognito sessions never read or write memory.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use genie_protocol::common::SessionKind;
use serde_json::json;

use crate::runtime::context::InstructionSource;
use crate::runtime::ports::SessionSettings;

/// `kv` key of the on/paused switch.
pub const STATE_KEY: &str = "memory.state";
/// Memory included in the prompt per scope (06 §6: "up to ~8 KiB").
pub const PROMPT_BYTES: usize = 8 * 1024;

pub struct MemoryControl {
    store: Option<Arc<genie_store::Store>>,
    on: AtomicBool,
    allowed: AtomicBool,
}

impl MemoryControl {
    pub fn new(store: Option<Arc<genie_store::Store>>) -> Arc<Self> {
        let on = store
            .as_ref()
            .and_then(|s| {
                s.read(|c| genie_store::kv::get(c, STATE_KEY))
                    .ok()
                    .flatten()
            })
            .and_then(|v| v.as_str().map(|s| s != "paused"))
            .unwrap_or(true);
        Arc::new(MemoryControl {
            store,
            on: AtomicBool::new(on),
            allowed: AtomicBool::new(true),
        })
    }

    /// On and allowed by policy.
    pub fn is_on(&self) -> bool {
        self.on.load(Ordering::SeqCst) && self.allowed.load(Ordering::SeqCst)
    }

    /// The user's switch (`memory.set_state`).
    pub fn user_state_on(&self) -> bool {
        self.on.load(Ordering::SeqCst)
    }

    pub fn set_on(&self, on: bool) -> Result<(), String> {
        if let Some(s) = &self.store {
            s.write(|tx| {
                genie_store::kv::set(tx, STATE_KEY, &json!(if on { "on" } else { "paused" }))
            })
            .map_err(|e| e.to_string())?;
        }
        self.on.store(on, Ordering::SeqCst);
        Ok(())
    }

    /// Admin policy (memory disabled org-wide).
    pub fn set_allowed(&self, allowed: bool) {
        self.allowed.store(allowed, Ordering::SeqCst);
    }

    /// The memory scope of a session's project: the project id in Chat. Code memory is
    /// global until workspaces get stored ids (README gap).
    pub fn project_scope(settings: &SessionSettings) -> Option<&str> {
        match settings.kind {
            SessionKind::Code => None,
            _ => settings.project_id.as_deref(),
        }
    }

    /// Baseline sources for a session.
    pub fn sources(&self, settings: &SessionSettings) -> Vec<InstructionSource> {
        if settings.incognito || !self.is_on() {
            return vec![];
        }
        let Some(store) = &self.store else {
            return vec![];
        };
        let mut out = vec![];
        let mut scopes = vec![(genie_store::memory::GLOBAL, "global")];
        if let Some(p) = Self::project_scope(settings) {
            scopes.push((p, "project"));
        }
        for (scope, label) in scopes {
            let entries = match store.read(|c| genie_store::memory::list(c, scope)) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(error = %e, "memory not readable");
                    continue;
                }
            };
            let mut lines = vec![];
            let mut used = 0usize;
            for m in entries {
                let line = format!("{} (id: {})", m.text.trim(), m.id);
                if used + line.len() > PROMPT_BYTES {
                    lines.push(
                        "(more memories are stored; use `memory_search` to find them)".into(),
                    );
                    break;
                }
                used += line.len();
                lines.push(line);
            }
            if !lines.is_empty() {
                out.push(InstructionSource::memory(label, &lines));
            }
        }
        out
    }
}
