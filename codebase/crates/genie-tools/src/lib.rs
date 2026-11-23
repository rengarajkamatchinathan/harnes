//! `genie-tools`: the `Tool` trait, registry, input repair/validation, output limits and
//! spill, and the v1 chat tools (spec/06-agent-runtime.md §2.4, §4, §4.1).
//!
//! Dependency rule (01 §2.1 rule 3): this crate depends on `genie-protocol` only (plus the
//! leaf exceptions in `xtask/src/deps.rs`). What a tool needs from another subsystem is a
//! port defined in [`tool`] ([`PermissionGate`], [`SpillStore`], [`MemoryPort`],
//! [`EventSink`]) and implemented by `genie-core`, which runs the calls.
//!
//! [`image`] (`image_generate`) brings [`image::ImageBackend`] and [`image::ImageStore`].
//!
//! [`shell`] (06 §13: the `shell` tool, shell selection, the command scanner) and [`web`]
//! (06 §8: `web_fetch` with the SSRF guard, web search) bring their own ports
//! ([`shell::SessionDirs`], [`shell::BackgroundJobs`], [`web::TaintPort`],
//! [`web::Resolver`], [`web::SearchBackend`]) in their configs.

pub mod chat;
pub mod code;
pub mod image;
pub mod output;
pub mod registry;
pub mod shell;
pub mod tool;
pub mod web;

pub use output::{Limited, MemorySpill, OutputLimits, apply_limits, strip_ansi};
pub use registry::{ToolRegistry, ToolSpec, parse_input, schema_of};
pub use tool::{
    AllowAll, Availability, BlockSource, EventSink, Features, MemoryEntry, MemoryPort, NullSink,
    PermissionGate, PermissionRequest, Services, SpillStore, Tool, ToolCtx, ToolEnv, ToolError,
    ToolKind, ToolOutput,
};

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use genie_protocol::common::SessionKind;
    use genie_protocol::event::{SessionEvent, ToolProgress};
    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    #[derive(Default)]
    struct Events(Mutex<Vec<SessionEvent>>);
    impl EventSink for Events {
        fn publish(&self, e: SessionEvent) {
            self.0.lock().unwrap().push(e);
        }
        fn progress(&self, _p: ToolProgress) {}
    }

    #[derive(Default)]
    struct Mem(Mutex<Vec<MemoryEntry>>);
    impl MemoryPort for Mem {
        fn save(&self, scope: &str, text: &str, _s: Option<&str>) -> Result<MemoryEntry, String> {
            let mut v = self.0.lock().unwrap();
            let m = MemoryEntry {
                id: format!("mem_{}", v.len() + 1),
                scope: scope.into(),
                text: text.into(),
            };
            v.push(m.clone());
            Ok(m)
        }
        fn update(&self, id: &str, text: &str) -> Result<MemoryEntry, String> {
            let mut v = self.0.lock().unwrap();
            let m = v.iter_mut().find(|m| m.id == id).ok_or("nope")?;
            m.text = text.into();
            Ok(m.clone())
        }
        fn delete(&self, id: &str) -> Result<Option<MemoryEntry>, String> {
            let mut v = self.0.lock().unwrap();
            let i = v.iter().position(|m| m.id == id);
            Ok(i.map(|i| v.remove(i)))
        }
        fn search(&self, s: &[&str], q: &str, _l: u32) -> Result<Vec<MemoryEntry>, String> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .iter()
                .filter(|m| s.contains(&m.scope.as_str()) && m.text.contains(q))
                .cloned()
                .collect())
        }
    }

    fn ctx(env: ToolEnv, name: &str, events: Arc<Events>) -> ToolCtx {
        ToolCtx {
            env: Arc::new(env),
            turn_id: "trn".into(),
            call_id: "call".into(),
            tool_name: name.into(),
            cancel: CancellationToken::new(),
            detach: CancellationToken::new(),
            permissions: Arc::new(AllowAll),
            events,
        }
    }

    #[test]
    fn registry_hides_unavailable_tools_and_repairs_names() {
        let reg = chat::registry();
        let env = ToolEnv::new("s", SessionKind::Chat);
        assert!(reg.specs(&env).is_empty(), "no spill, memory off");
        let mut env = ToolEnv::new("s", SessionKind::Chat);
        env.services.spill = Some(Arc::new(MemorySpill::new()));
        env.services.memory = Some(Arc::new(Mem::default()));
        env.features.memory = true;
        let names: Vec<String> = reg.specs(&env).into_iter().map(|s| s.name).collect();
        assert_eq!(
            names,
            [
                "memory_delete",
                "memory_save",
                "memory_search",
                "memory_update",
                "tool_output_read"
            ]
        );
        env.features.incognito = true;
        assert_eq!(reg.specs(&env).len(), 1);
        assert!(reg.resolve("Tool_Output_Read").is_some());
    }

    #[tokio::test]
    async fn tool_output_read_pages_spilled_output() {
        let spill = Arc::new(MemorySpill::new());
        let text: String = (1..=30).map(|i| format!("l{i}\n")).collect();
        let r = spill.put("s", &text).unwrap();
        let mut env = ToolEnv::new("s", SessionKind::Chat);
        env.services.spill = Some(spill);
        env.output_limits.max_lines = 10;
        let t = chat::ToolOutputRead::default();
        let out = t
            .execute(
                json!({"ref": r, "offset": 11}),
                ctx(env, "tool_output_read", Arc::default()),
            )
            .await
            .unwrap();
        let ToolOutput::Text(s) = out else { panic!() };
        assert!(s.starts_with("l11\nl12"), "{s}");
        assert!(s.ends_with(&format!("[showing 11–20 of 30; full output: {r}]")));
    }

    #[tokio::test]
    async fn memory_tools_round_trip_and_publish_changes() {
        let mem = Arc::new(Mem::default());
        let events = Arc::new(Events::default());
        let mk = || {
            let mut env = ToolEnv::new("s", SessionKind::Chat);
            env.features.memory = true;
            env.services.memory = Some(mem.clone());
            env
        };
        let out = chat::MemorySave::default()
            .execute(
                json!({"text": "Prefers metric units"}),
                ctx(mk(), "memory_save", events.clone()),
            )
            .await
            .unwrap();
        assert_eq!(
            out,
            ToolOutput::Json(json!({"id": "mem_1", "scope": "global"}))
        );
        let out = chat::MemorySearch::default()
            .execute(
                json!({"query": "metric"}),
                ctx(mk(), "memory_search", events.clone()),
            )
            .await
            .unwrap();
        assert!(out.to_text().contains("Prefers metric units"));
        let err = chat::MemorySave::default()
            .execute(
                json!({"text": "x", "scope": "project"}),
                ctx(mk(), "memory_save", events.clone()),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidInput { .. }));
        chat::MemoryDelete::default()
            .execute(
                json!({"id": "mem_1"}),
                ctx(mk(), "memory_delete", events.clone()),
            )
            .await
            .unwrap();
        let ops: Vec<String> = events
            .0
            .lock()
            .unwrap()
            .iter()
            .map(|e| match e {
                SessionEvent::MemoryChanged(m) => m.op.clone(),
                _ => "?".into(),
            })
            .collect();
        assert_eq!(ops, ["add", "delete"]);
    }
}
