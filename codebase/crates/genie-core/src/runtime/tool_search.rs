//! `tool_search` deferral (06 §7, pick-list #27): when the MCP tool definitions of a turn
//! would take more than ~10 % of the model's context window, they are **deferred**: the
//! request carries only their names (in the `tool_search` description) and the model
//! loads the ones it needs with `tool_search {query}`. Loaded tools join the request from
//! the next step on and stay loaded for the session (until the server drops them).
//!
//! Deferred tools stay in the turn's registry, so resolution and permissions are the same
//! as for any other tool; only the specs sent to the model are filtered ([`filter`]).
//!
//! [`filter`]: ToolSearch::filter

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use genie_tools::{Tool, ToolCtx, ToolEnv, ToolError, ToolKind, ToolOutput, ToolRegistry, ToolSpec};
use serde::Deserialize;
use serde_json::{Value, json};

pub const TOOL_SEARCH: &str = "tool_search";
/// Share of the context window MCP definitions may take before deferral (06 §7).
pub const DEFAULT_FRACTION: f64 = 0.10;
const DEFAULT_RESULTS: usize = 5;
const MAX_RESULTS: usize = 20;
/// Deferred names listed in the description (the rest are counted).
const LISTED_NAMES: usize = 200;

#[derive(Default)]
struct SessionState {
    /// The turn's deferred tools (all MCP tools when deferral is on).
    deferred: Vec<ToolSpec>,
    /// Names the model loaded with `tool_search`.
    loaded: HashSet<String>,
}

/// Deferral state per session, and the `tool_search` tool itself.
pub struct ToolSearch {
    fraction: f64,
    sessions: Mutex<HashMap<String, SessionState>>,
    schema: Value,
}

#[derive(Deserialize)]
struct Input {
    query: String,
    #[serde(default)]
    max_results: Option<usize>,
}

/// Rough token count of a definition as the model sees it (4 chars/token, 06 §2.3).
fn spec_tokens(s: &ToolSpec) -> u64 {
    ((s.name.len() + s.description.len() + s.input_schema.to_string().len()) / 4) as u64
}

fn is_deferrable(name: &str) -> bool {
    name.starts_with("mcp__")
}

impl ToolSearch {
    pub fn new(fraction: f64) -> Arc<ToolSearch> {
        Arc::new(ToolSearch {
            fraction,
            sessions: Mutex::new(HashMap::new()),
            schema: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description":
                        "Words to match against tool names and descriptions, or `select:<name>,<name>` to load tools by exact name."},
                    "max_results": {"type": "integer", "minimum": 1, "maximum": MAX_RESULTS,
                        "description": "How many tools to load (default 5)."}
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        })
    }

    fn sessions(&self) -> MutexGuard<'_, HashMap<String, SessionState>> {
        self.sessions.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// At turn start: decide whether the MCP tools are deferred for this turn and, if so,
    /// add `tool_search` to the registry.
    pub fn prepare(
        self: &Arc<Self>,
        session_id: &str,
        mut registry: ToolRegistry,
        env: &ToolEnv,
        context_window: u64,
    ) -> ToolRegistry {
        let mcp: Vec<ToolSpec> = registry
            .specs(env)
            .into_iter()
            .filter(|s| is_deferrable(&s.name))
            .collect();
        let tokens: u64 = mcp.iter().map(spec_tokens).sum();
        let mut map = self.sessions();
        #[allow(clippy::cast_precision_loss)]
        let over = !mcp.is_empty() && tokens as f64 > context_window as f64 * self.fraction;
        if !over {
            if let Some(st) = map.get_mut(session_id) {
                st.deferred.clear();
            }
            return registry;
        }
        let st = map.entry(session_id.to_string()).or_default();
        let names: HashSet<&str> = mcp.iter().map(|s| s.name.as_str()).collect();
        st.loaded.retain(|n| names.contains(n.as_str()));
        st.deferred = mcp;
        drop(map);
        registry.register(self.clone());
        registry
    }

    /// The specs sent to the model: deferred tools only once loaded.
    pub fn filter(&self, session_id: &str, specs: Vec<ToolSpec>) -> Vec<ToolSpec> {
        let map = self.sessions();
        let Some(st) = map.get(session_id).filter(|s| !s.deferred.is_empty()) else {
            return specs;
        };
        let deferred: HashSet<&str> = st.deferred.iter().map(|s| s.name.as_str()).collect();
        specs
            .into_iter()
            .filter(|s| !deferred.contains(s.name.as_str()) || st.loaded.contains(&s.name))
            .collect()
    }

    /// Whether the session's MCP tools are deferred this turn.
    pub fn is_deferring(&self, session_id: &str) -> bool {
        self.sessions()
            .get(session_id)
            .is_some_and(|s| !s.deferred.is_empty())
    }

    pub fn end_session(&self, session_id: &str) {
        self.sessions().remove(session_id);
    }

    fn search(&self, session_id: &str, query: &str, max: usize) -> (Vec<ToolSpec>, Vec<String>) {
        let mut map = self.sessions();
        let Some(st) = map.get_mut(session_id) else {
            return (vec![], vec![]);
        };
        let all: Vec<String> = st.deferred.iter().map(|s| s.name.clone()).collect();
        let picked: Vec<ToolSpec> = if let Some(list) = query.trim().strip_prefix("select:") {
            let want: Vec<&str> = list.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
            st.deferred
                .iter()
                .filter(|s| want.contains(&s.name.as_str()))
                .take(MAX_RESULTS)
                .cloned()
                .collect()
        } else {
            let words: Vec<String> = query
                .split(|c: char| !c.is_alphanumeric())
                .filter(|w| !w.is_empty())
                .map(str::to_lowercase)
                .collect();
            let mut scored: Vec<(u32, &ToolSpec)> = st
                .deferred
                .iter()
                .map(|s| {
                    let name = s.name.to_lowercase();
                    let desc = s.description.to_lowercase();
                    let score = words
                        .iter()
                        .map(|w| {
                            u32::from(name.contains(w.as_str())) * 3
                                + u32::from(desc.contains(w.as_str()))
                        })
                        .sum();
                    (score, s)
                })
                .filter(|(score, _)| *score > 0)
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
            scored.into_iter().take(max).map(|(_, s)| s.clone()).collect()
        };
        for s in &picked {
            st.loaded.insert(s.name.clone());
        }
        (picked, all)
    }
}

#[async_trait]
impl Tool for ToolSearch {
    fn name(&self) -> &str {
        TOOL_SEARCH
    }

    fn description(&self, env: &ToolEnv) -> Cow<'_, str> {
        let map = self.sessions();
        let (names, loaded): (Vec<&str>, usize) = match map.get(&env.session_id) {
            Some(st) => (
                st.deferred
                    .iter()
                    .filter(|s| !st.loaded.contains(&s.name))
                    .map(|s| s.name.as_str())
                    .collect(),
                st.loaded.len(),
            ),
            None => (vec![], 0),
        };
        let mut out = String::from(
            "Load MCP tools before calling them. These tools are connected but their \
             definitions were left out to save context; search by keywords (or \
             `select:<name>,<name>`) and the matching tools become callable from your next \
             step. Load only what the task needs.",
        );
        if loaded > 0 {
            out.push_str(&format!(" {loaded} tool(s) are already loaded."));
        }
        if !names.is_empty() {
            out.push_str("\n\nDeferred tools: ");
            out.push_str(&names[..names.len().min(LISTED_NAMES)].join(", "));
            if names.len() > LISTED_NAMES {
                out.push_str(&format!(" … and {} more", names.len() - LISTED_NAMES));
            }
        }
        Cow::Owned(out)
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }

    fn authorizes(&self) -> bool {
        false
    }

    async fn execute(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let input: Input = serde_json::from_value(input)
            .map_err(|e| ToolError::invalid_input(vec![e.to_string()]))?;
        let max = input
            .max_results
            .unwrap_or(DEFAULT_RESULTS)
            .clamp(1, MAX_RESULTS);
        let (picked, all) = self.search(&ctx.env.session_id, &input.query, max);
        if picked.is_empty() {
            return Ok(ToolOutput::text(format!(
                "No deferred tool matches `{}`. Deferred tools: {}",
                input.query,
                all.join(", ")
            )));
        }
        let tools: Vec<Value> = picked
            .iter()
            .map(|s| {
                json!({
                    "name": s.name,
                    "description": s.description,
                    "input_schema": s.input_schema,
                })
            })
            .collect();
        Ok(ToolOutput::Json(json!({
            "loaded": picked.iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
            "tools": tools,
            "note": "These tools are now available; call them directly.",
        })))
    }
}
