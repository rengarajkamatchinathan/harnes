//! `todo_write {todos}` (06 §4.2): whole-list replace. Clients render the list from the
//! tool call's input; the engine keeps the latest list per session.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{CodeConfig, code_only};
use crate::registry::{parse_input, schema_of};
use crate::tool::{Availability, Tool, ToolCtx, ToolEnv, ToolError, ToolKind, ToolOutput};

pub const NAME: &str = "todo_write";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Todo {
    /// What to do, in the imperative ("Add tests for the parser").
    pub content: String,
    pub status: TodoStatus,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct Input {
    /// The complete list; it replaces the previous one.
    todos: Vec<Todo>,
}

/// The latest list per session.
#[derive(Default)]
pub struct TodoStore {
    map: Mutex<HashMap<String, Vec<Todo>>>,
}

impl TodoStore {
    pub fn get(&self, session: &str) -> Vec<Todo> {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session)
            .cloned()
            .unwrap_or_default()
    }

    pub fn set(&self, session: &str, todos: Vec<Todo>) {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session.to_string(), todos);
    }

    pub fn end_session(&self, session: &str) {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(session);
    }
}

pub struct TodoWriteTool {
    cfg: CodeConfig,
    schema: Value,
}

impl TodoWriteTool {
    pub fn new(cfg: CodeConfig) -> Self {
        TodoWriteTool {
            cfg,
            schema: schema_of::<Input>(),
        }
    }
}

#[async_trait]
impl Tool for TodoWriteTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self, _env: &ToolEnv) -> Cow<'_, str> {
        Cow::Borrowed(
            "Keep a short task list for multi-step work; the user sees it. Send the whole \
             list each time (it replaces the previous one). Keep exactly one item \
             `in_progress` while working, and mark items `completed` as soon as they are done.",
        )
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }

    fn availability(&self, env: &ToolEnv) -> Availability {
        code_only(env)
    }

    fn authorizes(&self) -> bool {
        false
    }

    async fn execute(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let input: Input = parse_input(input, &self.schema)?;
        let doing = input
            .todos
            .iter()
            .filter(|t| t.status == TodoStatus::InProgress)
            .count();
        let done = input
            .todos
            .iter()
            .filter(|t| t.status == TodoStatus::Completed)
            .count();
        let total = input.todos.len();
        self.cfg.todos.set(&ctx.env.session_id, input.todos);
        let mut out = format!("Todo list updated: {done}/{total} done.");
        if doing > 1 {
            out.push_str(" Note: more than one item is in progress; keep one at a time.");
        }
        Ok(ToolOutput::Text(out))
    }
}
