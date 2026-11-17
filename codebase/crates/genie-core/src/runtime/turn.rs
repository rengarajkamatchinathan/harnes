//! The turn loop (06 §1).
//!
//! ```text
//! admit → (baseline delta) → loop step:
//!   assemble (§2) → compact if estimate ≥ threshold (§3)
//!   stream: deltas → ephemeral events; tool calls dispatch as their input ends (D20)
//!   join calls; write results (spill, §2.4); inject steer items (D20)
//!   decide: tool_calls → next | length → "continue smaller" (≤2) | overflow → compact
//!           with a shrinking tail and retry | steer injected → next | else settle
//!   step == maxSteps → final step with tool_choice none + "summarise and stop"
//! ```

// `ProviderError` is a flat, inspectable struct (03 §7), as in `genie-providers`.
#![allow(clippy::result_large_err)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use genie_protocol::common::{Cost, SessionKind};
use genie_protocol::event::{
    Activity, PartDelta, PartEnded, PartStarted, RetryScheduled, SessionCompacted, SessionEvent,
    SessionUpdated, StatusEvent, StepEnded, StepFinish, StepStarted, ToolEnded, ToolOutcome,
    UsageUpdated,
};
use genie_protocol::message::PartKind;
use genie_protocol::session::LiveStatus;
use genie_providers::error::{ErrorKind, ProviderError};
use genie_providers::event::StreamEvent;
use genie_providers::model::{
    Citation, CompactionPart, FinishReason, LlmRequest, Message, MessageId, Origin, Part,
    ReasoningPart, Role, TextPart, ToolCall, ToolChoice, ToolDef, ToolOutput, ToolResult, Usage,
    WireRecord,
};
use genie_tools::{Features, Services, ToolCtx, ToolEnv, ToolRegistry, apply_limits};
use serde_json::json;

use crate::error::{Defect, ToolError, TurnError};
use crate::ids::new_id;
use crate::runtime::AgentRuntime;
use crate::runtime::bridge::{CallGate, Started, TurnEvents};
use crate::runtime::context::{
    FrozenBaseline, compaction_threshold, effective_history, estimate_message, estimate_messages,
    estimate_text, lower_updates,
};
use crate::runtime::conversation::{MsgKind, SessionMeta, StoredMessage, UsageLine};
use crate::runtime::models::{cost, parse_effort};
use crate::runtime::ports::{ResolveError, ResolvedModel, SessionSettings};
use crate::runtime::prompts::{self, BasePromptInput, PromptFeatures, fragments};
use crate::runtime::wire::{self, part_id};
use crate::session::{InboxKind, TurnCtx};

fn resolve_err(e: ResolveError) -> TurnError {
    tracing::warn!(class = e.class, message = %e.message, "model not resolved");
    TurnError::Provider {
        class: e.class.to_string(),
        fix_link: e.fix_link,
    }
}

/// `rate_limited`, `context_overflow` … (03 §7).
pub(crate) fn error_class(k: &ErrorKind) -> String {
    serde_json::to_value(k)
        .ok()
        .and_then(|v| v.get("kind").and_then(|k| k.as_str()).map(String::from))
        .unwrap_or_else(|| "unknown".into())
}

fn provider_err(e: &ProviderError) -> TurnError {
    if e.kind == ErrorKind::Cancelled {
        return TurnError::Cancelled;
    }
    TurnError::Provider {
        class: error_class(&e.kind),
        fix_link: match e.kind {
            ErrorKind::Authentication | ErrorKind::QuotaExceeded => {
                Some("genie://settings/providers".into())
            }
            _ => None,
        },
    }
}

pub(crate) async fn run(rt: Arc<AgentRuntime>, turn: Arc<TurnCtx>) -> Result<(), TurnError> {
    let sid = turn.session_id.clone();
    let settings = rt
        .settings
        .settings(&sid)
        .ok_or_else(|| TurnError::Provider {
            class: "no_model_selected".into(),
            fix_link: None,
        })?;
    let model = rt.models.resolve(&settings.model).map_err(resolve_err)?;
    let meta = rt.conversation.meta(&sid)?;
    let path = rt.conversation.path(&sid)?;
    let first_reply = !path.iter().any(|m| m.message.role == Role::Assistant);
    let env = Arc::new(tool_env(&rt, &sid, &settings));
    // Dynamic tools (MCP) join the static registry at turn start (06 §7: 2 s grace for
    // servers still starting before the first model call).
    for p in &rt.tool_providers {
        p.ready(&settings).await;
    }
    let registry = rt.registry_for(&sid, &settings);
    let turns = rt.turns.clone();
    let _active = turns.enter(&turn);
    let mut r = Run {
        leaf: path.last().map(|m| m.id().to_string()),
        rt,
        turn,
        sid,
        settings,
        model,
        meta,
        env,
        registry,
        taint: Arc::new(Mutex::new(None)),
    };
    let res = r.go().await;
    if let Err(e) = r.save_meta() {
        tracing::error!(incident_id = %e.incident_id, "saving runtime meta failed");
    }
    res?;
    if first_reply && r.settings.title.is_none() && r.rt.cfg.titles {
        r.title().await;
    }
    Ok(())
}

pub(crate) fn tool_env(rt: &AgentRuntime, sid: &str, s: &SessionSettings) -> ToolEnv {
    ToolEnv {
        session_id: sid.to_string(),
        kind: s.kind,
        mode: s.mode,
        features: Features {
            memory: rt.cfg.memory && rt.memory.is_some() && !s.incognito,
            incognito: s.incognito,
            chat_search: false,
            artifacts: false,
            project_id: s.project_id.clone(),
        },
        output_limits: rt.cfg.output_limits,
        services: Services {
            spill: rt.spill.clone(),
            memory: rt.memory.clone(),
        },
    }
}

/// A streamed part that has started but not ended.
struct Open {
    ordinal: u32,
    kind: PartKind,
    text: String,
    citations: Vec<Citation>,
}

/// A dispatched or already-settled tool call.
struct Call {
    call: ToolCall,
    started: Option<Arc<Started>>,
    task: Option<tokio::task::JoinHandle<Result<genie_tools::ToolOutput, genie_tools::ToolError>>>,
    immediate: Option<ToolError>,
}

enum StepResult {
    Done {
        finish: FinishReason,
        client_calls: bool,
    },
    Overflow,
}

struct Run {
    rt: Arc<AgentRuntime>,
    turn: Arc<TurnCtx>,
    sid: String,
    settings: SessionSettings,
    model: ResolvedModel,
    meta: SessionMeta,
    env: Arc<ToolEnv>,
    registry: ToolRegistry,
    leaf: Option<String>,
    /// Untrusted content seen in this turn (SA-29): the source label.
    taint: Arc<Mutex<Option<String>>>,
}

impl Run {
    fn save_meta(&self) -> Result<(), Defect> {
        self.rt.conversation.set_meta(&self.sid, &self.meta)
    }

    fn publish(&self, e: SessionEvent) -> Result<(), TurnError> {
        self.turn.publish(e).map(|_| ())
    }

    fn append(&mut self, message: Message, kind: MsgKind) -> Result<(), TurnError> {
        let mut sm = StoredMessage::new(message, kind);
        sm.parent_id.clone_from(&self.leaf);
        sm.turn_id = Some(self.turn.turn_id.clone());
        if sm.message.role == Role::Assistant {
            sm.model = Some(self.settings.model.clone());
        }
        self.rt.conversation.append(&self.sid, &sm)?;
        self.leaf = Some(sm.id().to_string());
        Ok(())
    }

    fn append_synthetic(&mut self, text: &str) -> Result<(), TurnError> {
        self.append(
            Message::new(new_id("msg"), Role::User, vec![Part::text(text)]),
            MsgKind::Synthetic,
        )
    }

    async fn go(&mut self) -> Result<(), TurnError> {
        match self.turn.input.kind.clone() {
            InboxKind::Compact { focus } => {
                self.baseline_delta()?;
                return self.compact("manual", focus.as_deref(), 1.0).await;
            }
            InboxKind::Regenerate { message_id } => self.regenerate_from(&message_id)?,
            InboxKind::Resume { .. } => {
                self.baseline_delta()?;
                self.repair_unsettled()?;
                self.append_synthetic(fragments::INTERRUPTED_RESUME.trim_end())?;
            }
            InboxKind::User | InboxKind::Synthetic { .. } => {
                self.baseline_delta()?;
                let input = &self.turn.input;
                let msg = Message::new(
                    input.message_id.clone(),
                    Role::User,
                    wire::input_parts(&self.rt.resolve_input(&self.sid, &input.parts)),
                );
                let kind = if matches!(input.kind, InboxKind::Synthetic { .. }) {
                    MsgKind::Synthetic
                } else {
                    MsgKind::Normal
                };
                self.append(msg, kind)?;
            }
        }
        self.steps().await
    }

    /// Freeze the baseline at the first turn; later changes become a frozen
    /// `<system-update>` before the user message (06 §2.2).
    fn baseline_delta(&mut self) -> Result<(), TurnError> {
        let sources = self.rt.instructions.sources(&self.sid, &self.settings);
        match self.meta.baseline.as_mut() {
            None => self.meta.baseline = Some(FrozenBaseline::freeze(sources, 1)),
            Some(b) => {
                if let Some(text) = b.delta(&sources) {
                    self.append(
                        Message::new(new_id("sup"), Role::System, vec![Part::text(text)]),
                        MsgKind::SystemUpdate,
                    )?;
                }
            }
        }
        Ok(())
    }

    /// `session.regenerate`: branch from the user message above `message_id` (D24).
    fn regenerate_from(&mut self, message_id: &str) -> Result<(), TurnError> {
        let conv = &self.rt.conversation;
        let mut cur = conv.get(&self.sid, message_id)?.ok_or_else(|| {
            TurnError::Internal(Defect::new(format!("regenerate: no message {message_id}")))
        })?;
        loop {
            if cur.message.role == Role::User && cur.kind == MsgKind::Normal {
                break;
            }
            let Some(p) = cur.parent_id.clone() else {
                return Err(TurnError::Internal(Defect::new(
                    "regenerate: no user message above",
                )));
            };
            cur = conv.get(&self.sid, &p)?.ok_or_else(|| {
                TurnError::Internal(Defect::new(format!("regenerate: dangling {p}")))
            })?;
        }
        conv.set_leaf(&self.sid, Some(cur.id()))?;
        self.leaf = Some(cur.id().to_string());
        self.meta.measured = None;
        Ok(())
    }

    /// Resume (01 §5.4): tool calls without results are failed with `interrupted`.
    fn repair_unsettled(&mut self) -> Result<(), TurnError> {
        let path = self.rt.conversation.path(&self.sid)?;
        let Some(last) = path.last() else {
            return Ok(());
        };
        if last.message.role != Role::Assistant {
            return Ok(());
        }
        let results: Vec<Part> = last
            .message
            .parts
            .iter()
            .filter_map(|p| match p {
                Part::ToolCall(c) if !c.provider_executed => Some(Part::ToolResult(ToolResult {
                    call_id: c.id.clone(),
                    content: ToolOutput::Text(
                        "interrupted: Genie stopped before this call finished.".into(),
                    ),
                    is_error: true,
                    cache: None,
                    opaque: None,
                })),
                _ => None,
            })
            .collect();
        if !results.is_empty() {
            self.append(
                Message::new(new_id("msg"), Role::Tool, results),
                MsgKind::Normal,
            )?;
        }
        Ok(())
    }

    fn max_steps(&self) -> u32 {
        match self.settings.kind {
            SessionKind::Code => self.rt.cfg.max_steps_code,
            _ => self.rt.cfg.max_steps_chat,
        }
        .max(1)
    }

    async fn steps(&mut self) -> Result<(), TurnError> {
        let max = self.max_steps();
        let mut step = 0u32;
        let mut continues = 0u32;
        let mut overflow = 0usize;
        let mut compacted_for: Option<u32> = None;
        loop {
            if self.turn.token().is_cancelled() {
                return Err(TurnError::Cancelled);
            }
            step += 1;
            let final_step = step >= max;
            if final_step {
                self.append_synthetic(fragments::MAX_STEPS.trim_end())?;
            }
            let (req, estimate) = self.assemble(final_step)?;
            let threshold =
                compaction_threshold(self.model.context_window(), self.rt.cfg.compaction_buffer);
            if estimate >= threshold && compacted_for != Some(step) {
                compacted_for = Some(step);
                self.compact("auto", None, 1.0).await?;
                step -= 1;
                if final_step {
                    // The max-steps note is re-added on the retried step.
                    self.drop_last_synthetic()?;
                }
                continue;
            }
            match self.step(step, final_step, req).await? {
                StepResult::Overflow => {
                    let Some(scale) = self.rt.cfg.overflow_tail.get(overflow).copied() else {
                        return Err(TurnError::Provider {
                            class: "context_overflow".into(),
                            fix_link: None,
                        });
                    };
                    overflow += 1;
                    self.compact("overflow", None, scale).await?;
                    step -= 1;
                    if final_step {
                        self.drop_last_synthetic()?;
                    }
                    continue;
                }
                StepResult::Done {
                    finish,
                    client_calls,
                } => {
                    overflow = 0;
                    let steered = self.inject_steer()?;
                    if final_step {
                        return Ok(());
                    }
                    if client_calls {
                        continues = 0;
                        continue;
                    }
                    if finish == FinishReason::Length && continues < self.rt.cfg.max_continue {
                        continues += 1;
                        self.append_synthetic(fragments::CONTINUE_SMALLER.trim_end())?;
                        continue;
                    }
                    continues = 0;
                    if steered || finish == FinishReason::Paused {
                        continue;
                    }
                    // Extension points: `finish = stop` with subagents running (D47) waits
                    // for their reports; a `turn.end` hook may ask to continue (D48).
                    return Ok(());
                }
            }
        }
    }

    /// Undo the max-steps note when the step is retried after compaction.
    fn drop_last_synthetic(&mut self) -> Result<(), TurnError> {
        // The note stays in the tree; moving the leaf back hides it from this branch.
        let path = self.rt.conversation.path(&self.sid)?;
        if let Some(last) = path.last()
            && last.kind == MsgKind::Synthetic
        {
            self.rt
                .conversation
                .set_leaf(&self.sid, last.parent_id.as_deref())?;
            self.leaf.clone_from(&last.parent_id);
        }
        Ok(())
    }

    /// Steer items become user messages at the step boundary (01 §5.2, D20).
    fn inject_steer(&mut self) -> Result<bool, TurnError> {
        let items = self.turn.step_boundary();
        let any = !items.is_empty();
        for i in items {
            let msg = Message::new(
                i.message_id.clone(),
                Role::User,
                wire::input_parts(&self.rt.resolve_input(&self.sid, &i.parts)),
            );
            self.append(msg, MsgKind::Normal)?;
        }
        Ok(any)
    }

    fn base_prompt(&self, tool_names: Vec<String>) -> String {
        let spec = &self.model.route.model;
        let memory_on = tool_names.iter().any(|t| t == "memory_save");
        let skills_on = tool_names.iter().any(|t| t == "skill");
        let mcp_on = tool_names.iter().any(|t| t.starts_with("mcp__"));
        prompts::base_prompt(&BasePromptInput {
            kind: self.settings.kind,
            model_display_name: self.model.display_name.clone(),
            provider_display_name: self.model.provider_display_name.clone(),
            workspace_display: self
                .settings
                .workspace
                .as_ref()
                .map(|w| w.display().to_string())
                .unwrap_or_else(|| "the workspace".into()),
            mode: self.settings.mode,
            tools: tool_names,
            shell: None,
            features: PromptFeatures {
                memory: memory_on,
                skills: skills_on,
                mcp: mcp_on,
                ..PromptFeatures::default()
            },
            family: spec.family.clone(),
            provider: spec.provider.0.clone(),
            agent_body: None,
        })
    }

    /// The request for the next call, and its estimated context size (§2.3).
    fn assemble(&self, final_step: bool) -> Result<(LlmRequest, u64), TurnError> {
        let path = self.rt.conversation.path(&self.sid)?;
        let history = effective_history(&path, &self.meta);
        let compat = &self.model.route.compat;
        let messages = lower_updates(history, self.model.route.protocol.kind(), compat);
        let specs = self.registry.specs(&self.env);
        let names = specs.iter().map(|s| s.name.clone()).collect();
        let baseline = self.meta.baseline.clone().unwrap_or_default();
        let mut req = LlmRequest::new(self.model.route.model.clone(), messages);
        req.system = vec![
            TextPart {
                text: self.base_prompt(names),
                ..TextPart::default()
            },
            TextPart {
                text: baseline.render(),
                ..TextPart::default()
            },
        ];
        req.tools = specs
            .into_iter()
            .map(|s| ToolDef {
                name: s.name,
                description: s.description,
                input_schema: s.input_schema,
                cache: None,
            })
            .collect();
        req.tool_choice = if final_step {
            ToolChoice::None
        } else {
            ToolChoice::Auto
        };
        req.effort = self.settings.effort.as_deref().and_then(parse_effort);
        req.baseline_long_ttl = baseline.has_project_knowledge();
        req.prompt_cache_key = Some(self.sid.clone());
        req.wire_records = self.rt.conversation.wire_records(&self.sid);

        let fixed: u64 = req
            .system
            .iter()
            .map(|s| estimate_text(&s.text))
            .sum::<u64>()
            + req
                .tools
                .iter()
                .map(|t| estimate_text(&t.description) + estimate_text(&t.input_schema.to_string()))
                .sum::<u64>();
        // Measured usage from the last same-model step + estimate for what followed (t11).
        let estimate = match &self.meta.measured {
            Some((key, tokens, n)) if *key == self.model.key() && *n <= path.len() => {
                tokens
                    + path[*n..]
                        .iter()
                        .map(|m| estimate_message(&m.message))
                        .sum::<u64>()
            }
            _ => fixed + estimate_messages(&req.messages),
        };
        // `max_tokens` fitted to the remaining room.
        let cap = u64::from(self.model.route.model.max_output.unwrap_or(8_192));
        let room = self.model.context_window().saturating_sub(estimate);
        if room < cap {
            req.max_output_tokens = Some(u32::try_from(room.max(1_024)).unwrap_or(u32::MAX));
        }
        Ok((req, estimate))
    }

    fn origin(&self) -> Origin {
        let r = &self.model.route;
        Origin {
            provider: r.provider.clone(),
            model: r.model.wire_id.clone(),
            protocol: r.protocol.kind(),
            account: r.account,
        }
    }

    fn dispatch(&self, call: &ToolCall, final_step: bool) -> Call {
        let immediate = |e: ToolError| Call {
            call: call.clone(),
            started: None,
            task: None,
            immediate: Some(e),
        };
        if final_step {
            return immediate(ToolError::Failed {
                message: "tools are disabled for this step".into(),
                may_have_run: false,
            });
        }
        let Some(tool) = self.registry.resolve(&call.name) else {
            let names: Vec<String> = self
                .registry
                .specs(&self.env)
                .into_iter()
                .map(|s| s.name)
                .collect();
            return immediate(ToolError::invalid_input(vec![format!(
                "unknown tool `{}`; available: {}",
                call.name,
                names.join(", ")
            )]));
        };
        if !tool.availability(&self.env).is_available() {
            return immediate(ToolError::Failed {
                message: format!("`{}` is not available in this session", tool.name()),
                may_have_run: false,
            });
        }
        let name = tool.name().to_string();
        let kind = tool.kind();
        let started = Started::new(self.turn.clone(), call.id.0.clone(), name.clone(), kind);
        if !tool.authorizes() {
            started.mark(None);
        }
        let scope = self.turn.scope().child("tool");
        let ctx = ToolCtx {
            env: self.env.clone(),
            turn_id: self.turn.turn_id.clone(),
            call_id: call.id.0.clone(),
            tool_name: name.clone(),
            cancel: scope.token().clone(),
            permissions: Arc::new(CallGate {
                turn: self.turn.clone(),
                authorizer: self.rt.authorizer.clone(),
                tool: name,
                kind,
                mode: self.settings.mode,
                started: started.clone(),
                taint: self.taint.clone(),
            }),
            events: Arc::new(TurnEvents(self.turn.clone())),
        };
        let input = call.input.clone();
        let token = scope.token().clone();
        let task = scope.spawn(async move {
            tokio::select! {
                r = tool.execute(input, ctx) => r,
                () = token.cancelled() => Err(genie_tools::ToolError::Cancelled),
            }
        });
        Call {
            call: call.clone(),
            started: Some(started),
            task: Some(task),
            immediate: None,
        }
    }

    fn end_part(&self, msg_id: &str, ordinal: u32, part: &Part) -> Result<(), TurnError> {
        self.publish(SessionEvent::PartEnded(PartEnded {
            part_id: part_id(msg_id, ordinal as usize),
            part: wire::wire_part(part_id(msg_id, ordinal as usize), part, None),
        }))
    }

    fn start_part(&self, msg_id: &str, ordinal: u32, kind: PartKind) -> Result<(), TurnError> {
        self.publish(SessionEvent::PartStarted(PartStarted {
            message_id: msg_id.to_string(),
            part_id: part_id(msg_id, ordinal as usize),
            kind,
        }))
    }

    /// One provider call with its tool calls (06 §1).
    async fn step(
        &mut self,
        n: u32,
        final_step: bool,
        req: LlmRequest,
    ) -> Result<StepResult, TurnError> {
        let msg_id = new_id("msg");
        let route = &self.model.route;
        self.publish(SessionEvent::StepStarted(StepStarted {
            step: n,
            model: self.settings.model.clone(),
            route: format!("{}/{}", route.provider, route.protocol.kind().as_str()),
        }))?;
        let step_scope = self.turn.scope().child("step");
        let mut stream = self
            .model
            .client
            .stream(route, req, step_scope.token().clone());

        let mut open: BTreeMap<u32, Open> = BTreeMap::new();
        let mut done: BTreeMap<u32, Part> = BTreeMap::new();
        let mut calls: Vec<Call> = vec![];
        let mut usage = Usage::default();
        let mut finish = FinishReason::Unknown;
        let mut wire_records: Vec<WireRecord> = vec![];
        let mut failure: Option<ProviderError> = None;

        loop {
            let item = tokio::select! {
                i = stream.next() => i,
                () = self.turn.token().cancelled() => {
                    failure = Some(ProviderError::cancelled());
                    break;
                }
            };
            let Some(item) = item else { break };
            let ev = match item {
                Ok(ev) => ev,
                Err(e) => {
                    failure = Some(e);
                    break;
                }
            };
            match ev {
                StreamEvent::TextStart { part } | StreamEvent::ReasoningStart { part } => {
                    let kind = if matches!(ev, StreamEvent::TextStart { .. }) {
                        PartKind::Text
                    } else {
                        PartKind::Reasoning
                    };
                    self.start_part(&msg_id, part, kind)?;
                    open.insert(
                        part,
                        Open {
                            ordinal: part,
                            kind,
                            text: String::new(),
                            citations: vec![],
                        },
                    );
                }
                StreamEvent::TextDelta { part, delta }
                | StreamEvent::ReasoningDelta { part, delta } => {
                    if let Some(o) = open.get_mut(&part) {
                        o.text.push_str(&delta);
                        self.turn
                            .emit_part_delta(part_id(&msg_id, o.ordinal as usize), delta);
                    }
                }
                StreamEvent::Citation { part, citation } => {
                    if let Some(o) = open.get_mut(&part) {
                        o.citations.push(citation);
                    }
                }
                StreamEvent::TextEnd {
                    part,
                    citations,
                    opaque,
                } => {
                    let o = open.remove(&part);
                    let mut cites = o.as_ref().map(|o| o.citations.clone()).unwrap_or_default();
                    cites.extend(citations);
                    let p = Part::Text(TextPart {
                        text: o.map(|o| o.text).unwrap_or_default(),
                        citations: cites,
                        cache: None,
                        opaque,
                    });
                    self.end_part(&msg_id, part, &p)?;
                    done.insert(part, p);
                }
                StreamEvent::ReasoningEnd {
                    part,
                    final_text,
                    opaque,
                } => {
                    let o = open.remove(&part);
                    let text = final_text.unwrap_or_else(|| o.map(|o| o.text).unwrap_or_default());
                    let p = Part::Reasoning(ReasoningPart {
                        redacted: text.is_empty() && opaque.is_some(),
                        text,
                        opaque,
                    });
                    self.end_part(&msg_id, part, &p)?;
                    done.insert(part, p);
                }
                StreamEvent::ToolInputStart { part, .. } => {
                    self.start_part(&msg_id, part, PartKind::ToolCall)?;
                    open.insert(
                        part,
                        Open {
                            ordinal: part,
                            kind: PartKind::ToolCall,
                            text: String::new(),
                            citations: vec![],
                        },
                    );
                }
                StreamEvent::ToolInputDelta {
                    part,
                    json_fragment,
                } => {
                    if let Some(o) = open.get_mut(&part) {
                        o.text.push_str(&json_fragment);
                        self.turn
                            .publish_ephemeral(SessionEvent::PartDelta(PartDelta {
                                part_id: part_id(&msg_id, o.ordinal as usize),
                                text: None,
                                json: Some(json_fragment),
                            }));
                    }
                }
                StreamEvent::ToolInputEnd { part, call } => {
                    open.remove(&part);
                    let p = Part::ToolCall(call.clone());
                    self.end_part(&msg_id, part, &p)?;
                    done.insert(part, p);
                    if !call.provider_executed {
                        // D20: dispatch as soon as the input is complete.
                        calls.push(self.dispatch(&call, final_step));
                    }
                }
                StreamEvent::ToolInputError {
                    part,
                    call_id,
                    raw,
                    error,
                } => {
                    let name = open
                        .remove(&part)
                        .map(|_| String::new())
                        .unwrap_or_default();
                    let call = ToolCall {
                        id: call_id,
                        name,
                        input: json!({}),
                        provider_executed: false,
                        opaque: None,
                    };
                    let p = Part::ToolCall(call.clone());
                    self.end_part(&msg_id, part, &p)?;
                    done.insert(part, p);
                    tracing::debug!(%raw, "unparseable tool input");
                    calls.push(Call {
                        call,
                        started: None,
                        task: None,
                        immediate: Some(ToolError::invalid_input(vec![format!(
                            "the tool input was not valid JSON: {error}"
                        )])),
                    });
                }
                StreamEvent::ProviderToolResult { part, result } => {
                    let p = Part::ToolResult(result);
                    self.start_part(&msg_id, part, PartKind::ToolResult)?;
                    self.end_part(&msg_id, part, &p)?;
                    done.insert(part, p);
                }
                StreamEvent::OpaqueBlock { part, value } => {
                    done.insert(part, value);
                }
                StreamEvent::StepFinish {
                    reason, usage: u, ..
                } => {
                    finish = reason;
                    usage = u;
                }
                StreamEvent::Finish { reason, usage: u } => {
                    if finish == FinishReason::Unknown {
                        finish = reason;
                    }
                    if usage == Usage::default() {
                        usage = u;
                    }
                }
                StreamEvent::RetryScheduled {
                    attempt,
                    delay_ms,
                    kind,
                    ..
                } => {
                    self.publish(SessionEvent::RetryScheduled(RetryScheduled {
                        attempt,
                        after_ms: delay_ms,
                        error_class: error_class(&kind),
                    }))?;
                    self.turn
                        .publish_ephemeral(SessionEvent::Status(StatusEvent {
                            state: LiveStatus::Running,
                            activity: Some(Activity::Retrying),
                        }));
                }
                StreamEvent::WireRecorded { records } => wire_records.extend(records),
                StreamEvent::ModelSwitched { from, to } => {
                    tracing::info!(%from, %to, "provider switched model");
                }
                StreamEvent::Warning { message } => tracing::debug!(%message, "provider warning"),
                StreamEvent::StepStart { .. } | StreamEvent::Raw { .. } => {}
            }
        }
        drop(stream);
        if !wire_records.is_empty() {
            self.rt
                .conversation
                .add_wire_records(&self.sid, wire_records);
        }

        let overflow = failure
            .as_ref()
            .is_some_and(|e| e.kind == ErrorKind::ContextOverflow);
        let cancelled = self.turn.token().is_cancelled()
            || failure
                .as_ref()
                .is_some_and(|e| e.kind == ErrorKind::Cancelled);
        if failure.is_some() {
            // Settle partial text: it stays visible and in history (01 §5.3).
            for (idx, o) in std::mem::take(&mut open) {
                let p = match o.kind {
                    PartKind::Text if !o.text.is_empty() => Part::text(o.text),
                    PartKind::Reasoning if !o.text.is_empty() => Part::Reasoning(ReasoningPart {
                        text: o.text,
                        redacted: false,
                        opaque: None,
                    }),
                    _ => continue,
                };
                self.end_part(&msg_id, idx, &p)?;
                done.insert(idx, p);
            }
            if !cancelled {
                // Don't leave tools running for a failed step.
                for c in &calls {
                    if let Some(t) = &c.task {
                        t.abort();
                    }
                }
            }
        }

        // Join every call; results in emitted order.
        let tool_msg = new_id("msg");
        let mut results: Vec<Part> = vec![];
        let mut ended: Vec<ToolEnded> = vec![];
        for (i, c) in calls.into_iter().enumerate() {
            let mut was_cancelled = cancelled;
            let outcome = match (c.immediate, c.task) {
                (Some(e), _) => Err(e),
                (None, Some(t)) => match t.await {
                    Ok(Ok(o)) => Ok(o),
                    Ok(Err(genie_tools::ToolError::Cancelled)) | Err(_) => {
                        was_cancelled = true;
                        Err(ToolError::Failed {
                            message: "the call was cancelled".into(),
                            may_have_run: true,
                        })
                    }
                    Ok(Err(e)) => Err(ToolError::from(e)),
                },
                (None, None) => Err(ToolError::Failed {
                    message: "not run".into(),
                    may_have_run: false,
                }),
            };
            if let Some(s) = &c.started
                && !s.is_set()
                && outcome.is_ok()
            {
                s.mark(None);
            }
            let mut error_info = None;
            let (content, is_error, oc, spill_ref) = match outcome {
                Ok(o) => {
                    if let Some(s) = &c.started
                        && let Some(src) = self.rt.taint_source(&s.name, s.kind)
                    {
                        let mut t = self.taint.lock().unwrap_or_else(|e| e.into_inner());
                        if t.is_none() {
                            *t = Some(src);
                        }
                    }
                    let lim = apply_limits(
                        o.to_text(),
                        self.env.output_limits,
                        self.env.services.spill.as_deref(),
                        &self.sid,
                    );
                    (lim.text, false, ToolOutcome::Ok, lim.spill_ref)
                }
                Err(e) => {
                    let oc = match &e {
                        ToolError::Declined { .. } => ToolOutcome::Declined,
                        ToolError::Blocked { .. } => ToolOutcome::Blocked,
                        _ if was_cancelled => ToolOutcome::Cancelled,
                        _ => ToolOutcome::Error,
                    };
                    let mut text = e.model_text();
                    if let ToolError::Blocked { origin, .. } = &e {
                        let info =
                            c.started
                                .as_ref()
                                .and_then(|s| s.blocked())
                                .unwrap_or_else(|| genie_protocol::event::ErrorInfo {
                                    code: match origin {
                                        crate::error::BlockSource::Policy => {
                                            "policy_blocked".into()
                                        }
                                        crate::error::BlockSource::Rule => e.code().into(),
                                    },
                                    message: text.clone(),
                                    link: None,
                                });
                        text = format!("{}.", info.message.trim_end_matches('.'));
                        error_info = Some(info);
                    }
                    (text, true, oc, None)
                }
            };
            let pid = part_id(&tool_msg, i);
            let p = Part::ToolResult(ToolResult {
                call_id: c.call.id.clone(),
                content: ToolOutput::Text(content),
                is_error,
                cache: None,
                opaque: None,
            });
            self.publish(SessionEvent::PartStarted(PartStarted {
                message_id: tool_msg.clone(),
                part_id: pid.clone(),
                kind: PartKind::ToolResult,
            }))?;
            self.publish(SessionEvent::PartEnded(PartEnded {
                part_id: pid.clone(),
                part: wire::wire_part(pid.clone(), &p, spill_ref.clone()),
            }))?;
            ended.push(ToolEnded {
                call_id: c.call.id.0.clone(),
                outcome: oc,
                result_part_id: pid,
                spill_ref,
                error: error_info,
            });
            results.push(p);
        }
        for e in ended {
            self.publish(SessionEvent::ToolEnded(e))?;
        }

        let client_calls = !results.is_empty();
        if !done.is_empty() {
            let mut m = Message::new(
                msg_id.clone(),
                Role::Assistant,
                done.into_values().collect(),
            );
            m.origin = Some(self.origin());
            self.append(m, MsgKind::Normal)?;
        }
        if client_calls {
            self.append(
                Message {
                    id: MessageId::new(tool_msg),
                    role: Role::Tool,
                    parts: results,
                    origin: None,
                },
                MsgKind::Normal,
            )?;
        }

        let step_finish = if cancelled {
            StepFinish::Cancelled
        } else if failure.is_some() {
            StepFinish::Error
        } else {
            wire::step_finish(finish)
        };
        self.account(&usage, "chat", step_finish)?;
        if failure.is_none() {
            let path_len = self.rt.conversation.path(&self.sid)?.len();
            let measured = usage
                .context_tokens
                .unwrap_or(usage.input_total + usage.output_total);
            if measured > 0 {
                self.meta.measured = Some((self.model.key(), measured, path_len));
            }
        }
        match failure {
            None => Ok(StepResult::Done {
                finish,
                client_calls,
            }),
            Some(_) if cancelled => Err(TurnError::Cancelled),
            Some(_) if overflow => Ok(StepResult::Overflow),
            Some(e) => Err(provider_err(&e)),
        }
    }

    /// `step.ended` + usage line + `usage.updated` (06 §1, D34).
    fn account(
        &mut self,
        u: &Usage,
        kind: &'static str,
        finish: StepFinish,
    ) -> Result<(), TurnError> {
        let usd = cost(self.model.pricing.as_ref(), u);
        if kind == "chat" {
            self.publish(SessionEvent::StepEnded(StepEnded {
                finish,
                usage: wire::usage(u),
                cost: Cost { usd },
            }))?;
        }
        if *u != Usage::default() {
            self.rt.conversation.record_usage(
                &self.sid,
                &UsageLine {
                    kind,
                    provider: self.model.route.provider.0.clone(),
                    model: self.settings.model.clone(),
                    project_id: self.settings.project_id.clone(),
                    usage: *u,
                    cost_usd: usd,
                },
            )?;
        }
        let t = &mut self.meta.totals;
        t.input += u.input_total;
        t.output += u.output_total;
        t.cache_read += u.cache_read;
        t.cache_write += u.cache_write_5m + u.cache_write_1h;
        t.reasoning += u.reasoning.unwrap_or(0);
        t.cost_usd += usd;
        let totals = genie_protocol::common::Usage {
            input_tokens: t.input,
            output_tokens: t.output,
            cache_read_tokens: t.cache_read,
            cache_write_tokens: t.cache_write,
            reasoning_tokens: t.reasoning,
        };
        let context = u
            .context_tokens
            .or_else(|| self.meta.measured.as_ref().map(|m| m.1))
            .unwrap_or(0);
        self.turn
            .publish_ephemeral(SessionEvent::UsageUpdated(UsageUpdated {
                session: totals,
                cost: Cost { usd: t.cost_usd },
                context_tokens: context,
                context_window: self.model.context_window(),
            }));
        Ok(())
    }

    /// A one-shot call without tools; returns the text and usage.
    async fn one_shot(
        &self,
        model: &ResolvedModel,
        req: LlmRequest,
    ) -> Result<(String, Usage), ProviderError> {
        let scope = self.turn.scope().child("step");
        let mut s = model
            .client
            .stream(&model.route, req, scope.token().clone());
        let mut text = String::new();
        let mut usage = Usage::default();
        loop {
            let next = tokio::select! {
                n = s.next() => n,
                () = self.turn.token().cancelled() => return Err(ProviderError::cancelled()),
            };
            match next {
                None => break,
                Some(Err(e)) => return Err(e),
                Some(Ok(StreamEvent::TextDelta { delta, .. })) => text.push_str(&delta),
                Some(Ok(StreamEvent::StepFinish { usage: u, .. })) => usage = u,
                Some(Ok(StreamEvent::Finish { usage: u, .. })) if usage == Usage::default() => {
                    usage = u;
                }
                Some(Ok(_)) => {}
            }
        }
        Ok((text, usage))
    }

    /// Compaction (06 §3): our summary with the same model and cached prefix.
    async fn compact(
        &mut self,
        trigger: &str,
        focus: Option<&str>,
        tail_scale: f64,
    ) -> Result<(), TurnError> {
        self.turn.set_compacting(true);
        let r = self.compact_inner(trigger, focus, tail_scale).await;
        self.turn.set_compacting(false);
        r
    }

    async fn compact_inner(
        &mut self,
        trigger: &str,
        focus: Option<&str>,
        tail_scale: f64,
    ) -> Result<(), TurnError> {
        let path = self.rt.conversation.path(&self.sid)?;
        let history = effective_history(&path, &self.meta);
        if history.len() < 2 {
            return Ok(());
        }
        let tokens_before = history
            .iter()
            .map(|m| estimate_message(&m.message))
            .sum::<u64>();
        // Kept tail: whole messages, never starting at a tool result (06 §3).
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let budget = (self.rt.cfg.compaction_tail_tokens as f64 * tail_scale) as u64;
        let mut cut = history.len();
        let mut tail = 0u64;
        let mut fallback = None;
        for k in (1..history.len()).rev() {
            tail += estimate_message(&history[k].message);
            let valid = history[k].message.role != Role::Tool;
            if valid && fallback.is_none() {
                fallback = Some(k);
            }
            if tail > budget {
                break;
            }
            if valid {
                cut = k;
            }
        }
        if cut == history.len()
            && let Some(f) = fallback
            && trigger != "manual"
        {
            // Even the newest message is bigger than the budget: keep it anyway so the
            // turn can continue (the summary covers the rest).
            cut = f;
        }
        let kept_from = history.get(cut).map(|m| m.id().to_string());
        let to_summarise: Vec<StoredMessage> = history[..cut].to_vec();
        let protocol = self.model.route.protocol.kind();
        let mut messages = lower_updates(to_summarise, protocol, &self.model.route.compat);
        messages.push(Message::new(
            new_id("msg"),
            Role::User,
            vec![Part::text(prompts::compaction_prompt(focus))],
        ));
        let (mut req, _) = self.assemble(false)?;
        req.messages = messages;
        req.tool_choice = ToolChoice::None;
        req.wire_records = self.rt.conversation.wire_records(&self.sid);
        req.max_output_tokens = None;

        let mut summary = None;
        let mut used = Usage::default();
        for _attempt in 0..2 {
            match self.one_shot(&self.model, req.clone()).await {
                Ok((text, u)) => {
                    used.non_cached_input += u.non_cached_input;
                    used.cache_read += u.cache_read;
                    used.cache_write_5m += u.cache_write_5m;
                    used.cache_write_1h += u.cache_write_1h;
                    used.output_total += u.output_total;
                    let ok = prompts::summary_follows_template(&text);
                    if !text.trim().is_empty() {
                        summary = Some(text);
                    }
                    if ok {
                        break;
                    }
                }
                Err(e) if e.kind == ErrorKind::Cancelled => return Err(TurnError::Cancelled),
                Err(e) => {
                    tracing::warn!(error = %e, "compaction call failed");
                    return Err(TurnError::CompactionFailed);
                }
            }
        }
        let used = used.finalize();
        let Some(summary) = summary else {
            return Err(TurnError::CompactionFailed);
        };
        let comp_id = new_id("msg");
        let part = Part::Compaction(CompactionPart {
            summary: Some(summary.trim().to_string()),
            opaque: None,
        });
        self.start_part(&comp_id, 0, PartKind::Compaction)?;
        self.end_part(&comp_id, 0, &part)?;
        self.append(
            Message::new(comp_id.clone(), Role::User, vec![part]),
            MsgKind::Compaction,
        )?;
        self.meta.compactions.insert(comp_id.clone(), kept_from);
        // Rebuilt baseline from current sources (§2.2).
        let version = self.meta.baseline.as_ref().map_or(1, |b| b.version + 1);
        let mut sources = self.rt.instructions.sources(&self.sid, &self.settings);
        sources.extend(
            self.rt
                .instructions
                .after_compaction(&self.sid, &self.settings),
        );
        self.meta.baseline = Some(FrozenBaseline::freeze(sources, version));
        self.meta.measured = None;
        let path = self.rt.conversation.path(&self.sid)?;
        let tokens_after = effective_history(&path, &self.meta)
            .iter()
            .map(|m| estimate_message(&m.message))
            .sum::<u64>();
        self.publish(SessionEvent::SessionCompacted(SessionCompacted {
            compaction_message_id: comp_id,
            trigger: trigger.to_string(),
            tokens_before,
            tokens_after,
            native: false,
        }))?;
        self.account(&used, "compaction", StepFinish::Stop)?;
        self.save_meta()?;
        Ok(())
    }

    /// A 3–6 word title from the small model after the first reply (D27).
    async fn title(&mut self) {
        let Ok(model) = self.rt.models.small_model(&self.settings.model) else {
            return;
        };
        let Ok(path) = self.rt.conversation.path(&self.sid) else {
            return;
        };
        let text_of = |role: Role| {
            path.iter()
                .find(|m| m.message.role == role && m.kind == MsgKind::Normal)
                .map(|m| {
                    m.message
                        .parts
                        .iter()
                        .filter_map(|p| match p {
                            Part::Text(t) => Some(t.text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default()
        };
        let clip = |s: String| s.chars().take(2_000).collect::<String>();
        let prompt = format!(
            "{}\n\n<conversation>\nUser: {}\nAssistant: {}\n</conversation>",
            fragments::TITLE.trim_end(),
            clip(text_of(Role::User)),
            clip(text_of(Role::Assistant))
        );
        let mut req = LlmRequest::new(
            model.route.model.clone(),
            vec![Message::user_text(new_id("msg"), prompt)],
        );
        req.max_output_tokens = Some(64);
        req.tool_choice = ToolChoice::None;
        let (text, usage) = match self.one_shot(&model, req).await {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!(error = %e, "title generation failed");
                return;
            }
        };
        let title: String = text
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim()
            .trim_matches(|c| c == '"' || c == '\'' || c == '`')
            .trim_end_matches(['.', '!', '?', ':', ';'])
            .chars()
            .take(80)
            .collect();
        if title.is_empty() {
            return;
        }
        let usd = cost(model.pricing.as_ref(), &usage);
        if let Err(e) = self.rt.conversation.record_usage(
            &self.sid,
            &UsageLine {
                kind: "title",
                provider: model.route.provider.0.clone(),
                model: model.route.model.catalog_id.clone(),
                project_id: self.settings.project_id.clone(),
                usage,
                cost_usd: usd,
            },
        ) {
            tracing::error!(incident_id = %e.incident_id, "title usage not recorded");
        }
        self.rt.settings.set_title(&self.sid, &title);
        if let Err(e) = self.publish(SessionEvent::SessionUpdated(SessionUpdated {
            title: Some(title),
            ..SessionUpdated::default()
        })) {
            tracing::error!(error = %e, "publish title failed");
        }
    }
}
