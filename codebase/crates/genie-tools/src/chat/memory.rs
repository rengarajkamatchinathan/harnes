//! `memory_save / memory_update / memory_delete / memory_search` (06 §4.1, D31).
//!
//! Always allowed; available only when memory is on and the session is not incognito.
//! Each change publishes `memory.changed` (the "Memory updated" chip).

use std::borrow::Cow;
use std::sync::Arc;

use async_trait::async_trait;
use genie_protocol::event::{MemoryChanged, SessionEvent};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::registry::{parse_input, schema_of};
use crate::tool::{
    Availability, MemoryPort, Tool, ToolCtx, ToolEnv, ToolError, ToolKind, ToolOutput,
};

pub const GLOBAL: &str = "global";

fn available(env: &ToolEnv) -> Availability {
    if env.features.incognito {
        Availability::Unavailable(Cow::Borrowed("incognito"))
    } else if !env.features.memory {
        Availability::Unavailable(Cow::Borrowed("memory is off"))
    } else if env.services.memory.is_none() {
        Availability::Unavailable(Cow::Borrowed("no memory store"))
    } else {
        Availability::Available
    }
}

fn port(ctx: &ToolCtx) -> Result<Arc<dyn MemoryPort>, ToolError> {
    ctx.env
        .services
        .memory
        .clone()
        .ok_or_else(|| ToolError::failed("memory is not available"))
}

/// Scopes a session reads: global, plus its project's.
fn scopes(env: &ToolEnv) -> Vec<String> {
    let mut v = vec![GLOBAL.to_string()];
    if let Some(p) = &env.features.project_id {
        v.push(p.clone());
    }
    v
}

fn changed(ctx: &ToolCtx, id: &str, op: &str, scope: &str) {
    ctx.events
        .publish(SessionEvent::MemoryChanged(MemoryChanged {
            memory_id: id.to_string(),
            op: op.to_string(),
            scope: scope.to_string(),
        }));
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Scope {
    Global,
    Project,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SaveInput {
    /// One durable fact or preference, written as a short sentence.
    text: String,
    /// `project` saves to the current project's memory (only inside a project).
    #[serde(default)]
    scope: Option<Scope>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct UpdateInput {
    id: String,
    text: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct DeleteInput {
    id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SearchInput {
    query: String,
    /// Default 10, max 50.
    #[serde(default)]
    limit: Option<u32>,
}

macro_rules! memory_tool {
    ($ty:ident, $name:literal, $input:ty) => {
        pub struct $ty {
            schema: Value,
        }
        impl Default for $ty {
            fn default() -> Self {
                $ty {
                    schema: schema_of::<$input>(),
                }
            }
        }
        impl $ty {
            pub const NAME: &'static str = $name;
        }
    };
}

memory_tool!(MemorySave, "memory_save", SaveInput);
memory_tool!(MemoryUpdate, "memory_update", UpdateInput);
memory_tool!(MemoryDelete, "memory_delete", DeleteInput);
memory_tool!(MemorySearch, "memory_search", SearchInput);

#[async_trait]
impl Tool for MemorySave {
    fn name(&self) -> &str {
        Self::NAME
    }
    fn description(&self, _env: &ToolEnv) -> Cow<'_, str> {
        Cow::Borrowed(
            "Save a durable fact or preference about the user to memory. Use only for things \
             the user would expect you to remember in later chats.",
        )
    }
    fn input_schema(&self) -> &Value {
        &self.schema
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Mutating
    }
    /// Memory tools are always allowed (06 §4.1); the policy switch hides them instead.
    fn authorizes(&self) -> bool {
        false
    }
    fn availability(&self, env: &ToolEnv) -> Availability {
        available(env)
    }
    async fn execute(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let i: SaveInput = parse_input(input, &self.schema)?;
        let text = i.text.trim();
        if text.is_empty() {
            return Err(ToolError::invalid_input(vec!["`text` is empty".into()]));
        }
        let scope = match (i.scope, &ctx.env.features.project_id) {
            (Some(Scope::Project), Some(p)) => p.clone(),
            (Some(Scope::Project), None) => {
                return Err(ToolError::invalid_input(vec![
                    "this chat is not in a project; use scope `global`".into(),
                ]));
            }
            _ => GLOBAL.to_string(),
        };
        let m = port(&ctx)?
            .save(&scope, text, Some(&ctx.env.session_id))
            .map_err(ToolError::failed)?;
        changed(&ctx, &m.id, "add", &m.scope);
        Ok(ToolOutput::Json(json!({"id": m.id, "scope": m.scope})))
    }
}

#[async_trait]
impl Tool for MemoryUpdate {
    fn name(&self) -> &str {
        Self::NAME
    }
    fn description(&self, _env: &ToolEnv) -> Cow<'_, str> {
        Cow::Borrowed("Replace the text of a memory entry by id.")
    }
    fn input_schema(&self) -> &Value {
        &self.schema
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Mutating
    }
    /// Memory tools are always allowed (06 §4.1); the policy switch hides them instead.
    fn authorizes(&self) -> bool {
        false
    }
    fn availability(&self, env: &ToolEnv) -> Availability {
        available(env)
    }
    async fn execute(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let i: UpdateInput = parse_input(input, &self.schema)?;
        let m = port(&ctx)?
            .update(&i.id, i.text.trim())
            .map_err(ToolError::failed)?;
        changed(&ctx, &m.id, "update", &m.scope);
        Ok(ToolOutput::Json(json!({"id": m.id, "updated": true})))
    }
}

#[async_trait]
impl Tool for MemoryDelete {
    fn name(&self) -> &str {
        Self::NAME
    }
    fn description(&self, _env: &ToolEnv) -> Cow<'_, str> {
        Cow::Borrowed("Delete a memory entry by id.")
    }
    fn input_schema(&self) -> &Value {
        &self.schema
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Mutating
    }
    /// Memory tools are always allowed (06 §4.1); the policy switch hides them instead.
    fn authorizes(&self) -> bool {
        false
    }
    fn availability(&self, env: &ToolEnv) -> Availability {
        available(env)
    }
    async fn execute(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let i: DeleteInput = parse_input(input, &self.schema)?;
        match port(&ctx)?.delete(&i.id).map_err(ToolError::failed)? {
            Some(m) => {
                changed(&ctx, &m.id, "delete", &m.scope);
                Ok(ToolOutput::Json(json!({"id": m.id, "deleted": true})))
            }
            None => Err(ToolError::failed(format!("no memory with id {}", i.id))),
        }
    }
}

#[async_trait]
impl Tool for MemorySearch {
    fn name(&self) -> &str {
        Self::NAME
    }
    fn description(&self, _env: &ToolEnv) -> Cow<'_, str> {
        Cow::Borrowed("Search saved memory entries (global and this project's).")
    }
    fn input_schema(&self) -> &Value {
        &self.schema
    }
    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }
    fn availability(&self, env: &ToolEnv) -> Availability {
        available(env)
    }
    async fn execute(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let i: SearchInput = parse_input(input, &self.schema)?;
        let scopes = scopes(&ctx.env);
        let refs: Vec<&str> = scopes.iter().map(String::as_str).collect();
        let hits = port(&ctx)?
            .search(&refs, &i.query, i.limit.unwrap_or(10).clamp(1, 50))
            .map_err(ToolError::failed)?;
        Ok(ToolOutput::Json(json!({
            "results": hits.iter().map(|m| json!({"id": m.id, "scope": m.scope, "text": m.text})).collect::<Vec<_>>()
        })))
    }
}
