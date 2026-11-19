//! The engine's [`BrowserHost`] (06 §10, D21): sensitive-action confirmations as durable
//! `browser_confirm` asks, taint, image blobs, the user-typed-URL check, vision, and the
//! banner state as ephemeral `browser.control` events.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use genie_browser::host::{BrowserHost, ConfirmDecision, ConfirmRequest, Control};
use genie_protocol::ask::{Ask, AskAnswer, AskKind, Decision};
use genie_protocol::event::{BrowserControl, SessionEvent};
use genie_tools::web::TaintPort;

use crate::bus::CoreEvent;
use crate::engine::Engine;
use crate::runtime::active::ActiveTurns;
use crate::runtime::conversation::ConversationStore;
use crate::runtime::host::TurnTaints;
use crate::runtime::ports::{ModelResolver, SettingsSource};

/// Browser tools are hidden in headless sessions unless the session-only settings enable
/// them (`--enable-browser`, or an `--allow 'Browser(site:…)'` rule; SA-28c).
pub struct HeadlessGated {
    pub inner: Arc<dyn genie_tools::Tool>,
    pub settings: Arc<dyn SettingsSource>,
}

#[async_trait]
impl genie_tools::Tool for HeadlessGated {
    fn name(&self) -> &str {
        self.inner.name()
    }
    fn description(&self, env: &genie_tools::ToolEnv) -> std::borrow::Cow<'_, str> {
        self.inner.description(env)
    }
    fn input_schema(&self) -> &serde_json::Value {
        self.inner.input_schema()
    }
    fn kind(&self) -> genie_tools::ToolKind {
        self.inner.kind()
    }
    fn authorizes(&self) -> bool {
        self.inner.authorizes()
    }
    fn availability(&self, env: &genie_tools::ToolEnv) -> genie_tools::Availability {
        if let Some(s) = self.settings.settings(&env.session_id)
            && s.headless
        {
            let on = s.overlay.as_ref().is_some_and(|o| {
                o.enable_browser || o.allow.iter().any(|r| r.starts_with("Browser(site:"))
            });
            if !on {
                return genie_tools::Availability::Unavailable(std::borrow::Cow::Borrowed(
                    "browser tools are off in headless sessions (use --enable-browser)",
                ));
            }
        }
        self.inner.availability(env)
    }
    async fn execute(
        &self,
        input: serde_json::Value,
        ctx: genie_tools::ToolCtx,
    ) -> Result<genie_tools::ToolOutput, genie_tools::ToolError> {
        self.inner.execute(input, ctx).await
    }
}

/// Whether some client attached to the session can answer an ask of this kind.
pub type CanAnswer = Arc<dyn Fn(&str, AskKind) -> bool + Send + Sync>;

pub struct EngineBrowserHost {
    pub turns: Arc<ActiveTurns>,
    pub taints: Arc<TurnTaints>,
    pub settings: Arc<dyn SettingsSource>,
    pub models: Option<Arc<dyn ModelResolver>>,
    pub store: Option<Arc<genie_store::Store>>,
    pub conversation: Option<Arc<dyn ConversationStore>>,
    engine: OnceLock<Engine>,
    can_answer: OnceLock<CanAnswer>,
}

impl EngineBrowserHost {
    pub fn new(
        turns: Arc<ActiveTurns>,
        taints: Arc<TurnTaints>,
        settings: Arc<dyn SettingsSource>,
    ) -> EngineBrowserHost {
        EngineBrowserHost {
            turns,
            taints,
            settings,
            models: None,
            store: None,
            conversation: None,
            engine: OnceLock::new(),
            can_answer: OnceLock::new(),
        }
    }

    /// The engine, for banner events outside a turn (takeover from a client).
    pub fn bind(&self, engine: Engine) {
        let _ = self.engine.set(engine);
    }

    /// The daemon's view of attached clients (`Unavailable` when none can answer).
    pub fn set_can_answer(&self, f: CanAnswer) {
        let _ = self.can_answer.set(f);
    }

    fn banner(&self, session_id: &str, control: &str, reason: Option<String>) {
        self.publish(
            session_id,
            SessionEvent::BrowserControl(BrowserControl {
                control: control.into(),
                reason,
            }),
        );
    }

    /// An ephemeral event to the session's clients, from inside or outside a turn.
    fn publish(&self, session_id: &str, ev: SessionEvent) {
        if let Some(t) = self.turns.get(session_id) {
            t.publish_ephemeral(ev);
        } else if let Some(e) = self.engine.get() {
            e.bus().publish_ephemeral(
                session_id,
                CoreEvent::Wire {
                    event: Box::new(ev),
                },
            );
        }
    }
}

/// Approve / Take over / Deny from an [`AskAnswer`]: `take_over` comes as
/// `data: {"action": "take_over"}` (or a `submit` with that action).
pub fn confirm_decision(a: &AskAnswer) -> ConfirmDecision {
    let action = a
        .data
        .as_ref()
        .and_then(|d| d.get("action"))
        .and_then(|v| v.as_str());
    match (a.decision, action) {
        (_, Some("take_over")) => ConfirmDecision::TakeOver,
        (Decision::Once | Decision::Always | Decision::Submit, _) => ConfirmDecision::Approve,
        _ => ConfirmDecision::Deny,
    }
}

#[async_trait]
impl BrowserHost for EngineBrowserHost {
    async fn confirm(&self, req: ConfirmRequest) -> ConfirmDecision {
        if self
            .can_answer
            .get()
            .is_some_and(|f| !f(&req.session_id, AskKind::BrowserConfirm))
        {
            return ConfirmDecision::Unavailable;
        }
        let Some(turn) = self.turns.get(&req.session_id) else {
            return ConfirmDecision::Unavailable;
        };
        let mut ask = Ask::new("", "", AskKind::BrowserConfirm, "");
        ask.tool = Some("browser".into());
        ask.resource = Some(req.confirm.origin.clone());
        ask.browser = Some(req.confirm);
        match turn.ask_with(ask).await {
            Ok(v) => serde_json::from_value::<AskAnswer>(v)
                .map(|a| confirm_decision(&a))
                .unwrap_or(ConfirmDecision::Deny),
            Err(_) => ConfirmDecision::Deny,
        }
    }

    fn taint(&self, session_id: &str, turn_id: &str, origin: &str) {
        self.taints
            .tainted(session_id, turn_id, &format!("the web page {origin}"));
    }

    fn vision(&self, session_id: &str) -> bool {
        let (Some(m), Some(s)) = (&self.models, self.settings.settings(session_id)) else {
            return false;
        };
        m.resolve(&s.model).is_ok_and(|r| r.route.model.input.image)
    }

    fn store_image(&self, _session_id: &str, bytes: &[u8], mime: &str) -> Option<String> {
        self.store.as_ref()?.put_blob(bytes, Some(mime)).ok()
    }

    fn user_supplied_url(&self, session_id: &str, url: &str) -> bool {
        let Some(c) = &self.conversation else {
            return false;
        };
        c.path(session_id).is_ok_and(|p| {
            p.iter()
                .filter(|m| m.message.role == genie_providers::model::Role::User)
                .flat_map(|m| m.message.parts.iter())
                .any(|p| matches!(p, genie_providers::model::Part::Text(t) if t.text.contains(url)))
        })
    }

    fn control_changed(&self, session_id: &str, control: Control) {
        let c = match control {
            Control::Agent => "agent",
            Control::User => "user",
            Control::WaitingForApproval => "waiting_for_approval",
        };
        self.banner(session_id, c, None);
    }

    fn handoff(&self, session_id: &str, reason: &str) {
        self.banner(session_id, "handoff", Some(reason.to_string()));
    }

    fn file_chooser(&self, session_id: &str, chooser: genie_protocol::event::BrowserFileChooser) {
        self.publish(session_id, SessionEvent::BrowserFileChooser(chooser));
    }
}
