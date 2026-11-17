//! Chat tools (06 §4.1) that need no other subsystem than ports injected by core.
//!
//! Not here (owned by other specs, extension points only): `artifact_*` (D28),
//! `web_search` / `code_execution` (provider-hosted, declared via `ServerTools`),
//! `web_fetch` (§8), `mcp__*` and `tool_search` (§7), `chat_search` / `project_search`
//! (retrieval, M3), `skill` / `skill_read` (§9), `browser_*` (§10).

pub mod memory;
pub mod output_read;

use std::sync::Arc;

pub use memory::{MemoryDelete, MemorySave, MemorySearch, MemoryUpdate};
pub use output_read::ToolOutputRead;

use crate::registry::ToolRegistry;

/// The v1 chat registry.
pub fn registry() -> ToolRegistry {
    ToolRegistry::new()
        .with(Arc::new(ToolOutputRead::default()))
        .with(Arc::new(MemorySave::default()))
        .with(Arc::new(MemoryUpdate::default()))
        .with(Arc::new(MemoryDelete::default()))
        .with(Arc::new(MemorySearch::default()))
}
