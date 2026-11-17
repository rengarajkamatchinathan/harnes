//! `chat_search {query, scope}` (06 §4.1, §6, D31): past chats over `message_fts`.
//!
//! Scopes: the current project's chats, or chats outside any project (D31). The current
//! chat and incognito chats are never returned. Hits carry the chat and message ids so
//! the model can cite the chat it used. Always allowed; an admin can turn it off
//! ([`ChatSearch::set_enabled`]).

use std::borrow::Cow;
use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use genie_protocol::common::SessionKind;
use genie_store::search::Scope;
use genie_tools::{Availability, Tool, ToolCtx, ToolEnv, ToolError, ToolKind, ToolOutput};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::runtime::ports::SettingsSource;

pub const CHAT_SEARCH: &str = "chat_search";

#[derive(Deserialize)]
struct Input {
    query: String,
    #[serde(default)]
    scope: Option<String>,
}

pub struct ChatSearch {
    store: Arc<genie_store::Store>,
    settings: Arc<dyn SettingsSource>,
    schema: Value,
    enabled: AtomicBool,
}

impl ChatSearch {
    pub fn new(store: Arc<genie_store::Store>, settings: Arc<dyn SettingsSource>) -> Self {
        ChatSearch {
            store,
            settings,
            schema: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Words to find in past chats."},
                    "scope": {"type": "string", "enum": ["current_project", "non_project"],
                        "description": "current_project: chats in this chat's project (default inside a project); non_project: chats outside any project."}
                },
                "required": ["query"],
                "additionalProperties": false
            }),
            enabled: AtomicBool::new(true),
        }
    }

    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::SeqCst);
    }
}

#[async_trait]
impl Tool for ChatSearch {
    fn name(&self) -> &str {
        CHAT_SEARCH
    }
    fn description(&self, _env: &ToolEnv) -> Cow<'_, str> {
        Cow::Borrowed(
            "Search the user's past chats (not this one). Returns snippets with chat and \
             message ids; cite the chat you used.",
        )
    }
    fn input_schema(&self) -> &Value {
        &self.schema
    }
    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }
    fn availability(&self, env: &ToolEnv) -> Availability {
        if !self.enabled.load(Ordering::SeqCst) {
            Availability::Unavailable(Cow::Borrowed("chat search is off"))
        } else if env.features.incognito {
            Availability::Unavailable(Cow::Borrowed("incognito"))
        } else if env.kind != SessionKind::Chat {
            Availability::Unavailable(Cow::Borrowed("chat search is a Chat feature"))
        } else {
            Availability::Available
        }
    }
    async fn execute(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let i: Input = genie_tools::parse_input(input, &self.schema)?;
        let project = ctx.env.features.project_id.clone();
        let scope = match (i.scope.as_deref(), project) {
            (Some("non_project"), _) | (None, None) => Scope::NonProject,
            (_, Some(p)) => Scope::Project(p),
            (Some(_), None) => {
                return Err(ToolError::invalid_input(vec![
                    "this chat is not in a project; use scope `non_project`".into(),
                ]));
            }
        };
        let me = ctx.env.session_id.clone();
        let (hits, titles) = self
            .store
            .read(|c| {
                let hits = genie_store::search::chat_search(c, &i.query, &scope, 40)?;
                let mut titles = std::collections::HashMap::new();
                for h in &hits {
                    if !titles.contains_key(&h.session_id) {
                        let t = genie_store::sessions::get(c, &h.session_id)?
                            .and_then(|s| s.title)
                            .unwrap_or_else(|| "Untitled chat".into());
                        titles.insert(h.session_id.clone(), t);
                    }
                }
                Ok((hits, titles))
            })
            .map_err(|e| ToolError::failed(format!("chat search failed: {e}")))?;
        let mut out = String::new();
        let mut n = 0;
        for h in hits {
            if h.session_id == me
                || self
                    .settings
                    .settings(&h.session_id)
                    .is_some_and(|s| s.incognito)
            {
                continue;
            }
            n += 1;
            if n > 10 {
                break;
            }
            let title = titles.get(&h.session_id).map_or("", String::as_str);
            let _ = write!(
                out,
                "<chat_hit chat=\"{}\" title=\"{}\"",
                h.session_id,
                title.replace('"', "&quot;")
            );
            if let Some(m) = &h.message_id {
                let _ = write!(out, " message=\"{m}\"");
            }
            let _ = writeln!(out, ">{}</chat_hit>", h.snippet.trim());
        }
        if out.is_empty() {
            return Ok(ToolOutput::text(format!(
                "No past chats match \"{}\".",
                i.query
            )));
        }
        Ok(ToolOutput::Text(format!(
            "<chat_search_results>\n{out}</chat_search_results>"
        )))
    }
}
