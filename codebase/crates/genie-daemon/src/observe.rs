//! Audit completeness, OTLP export and telemetry wiring (08 §6–7, D38, D39).
//!
//! - [`EventTap`] wraps the engine journal: every durable session event passes through it,
//!   so tool calls (`tool.called`: name, resource, outcome, duration, error code — no
//!   arguments), per-request usage (`usage.request`: model, route, tokens, cost) and agent
//!   browser actions (`browser.action`) are audited without touching the turn loop; the
//!   same tap feeds usage analytics (session created, turn completed, artifact created).
//! - [`HttpOtlp`] sends the audit OTLP queue to `<audit.otlp.endpoint>/v1/logs` with
//!   headers from the keychain account [`OTLP_HEADERS_ACCOUNT`] (a JSON object).
//! - [`SentryExporter`] sends telemetry envelopes to the DSN from policy
//!   `telemetry.endpoint` or `GENIE_TELEMETRY_DSN`; without one nothing is sent.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use genie_audit::otlp::BoxFuture;
use genie_audit::telemetry::{Dsn, TelemetryContext, TelemetryExporter, TelemetryRecord};
use genie_audit::{
    Attrs, AuditEvent, Auditor, OtlpTransport, Telemetry, TelemetryConsent, UsageEvent, UsageName,
    UsageProp,
};
use genie_core::bus::Envelope;
use genie_core::config::ConfigHub;
use genie_core::error::Defect;
use genie_core::ids::{SessionId, TurnId};
use genie_core::journal::{Journal, TurnRecord};
use genie_protocol::common::SessionKind;
use genie_protocol::event::{SessionEvent, ToolOutcome, TurnStatus};
use genie_protocol::session::TurnFinish;
use serde_json::Value;

/// Keychain account holding the OTLP export headers (`{"Authorization": "…"}`).
pub const OTLP_HEADERS_ACCOUNT: &str = "audit.otlp.headers";

struct Started {
    name: String,
    resource: Option<String>,
    at: Instant,
}

/// A journal decorator that derives audit records and usage analytics from durable events.
pub struct EventTap {
    inner: Arc<dyn Journal>,
    audit: Option<Arc<Auditor>>,
    telemetry: Option<Arc<Telemetry>>,
    tools: Mutex<HashMap<(String, String), Started>>,
    steps: Mutex<HashMap<String, (String, String)>>,
}

impl EventTap {
    pub fn wrap(
        inner: Arc<dyn Journal>,
        audit: Option<Arc<Auditor>>,
        telemetry: Option<Arc<Telemetry>>,
    ) -> Arc<dyn Journal> {
        if audit.is_none() && telemetry.is_none() {
            return inner;
        }
        Arc::new(EventTap {
            inner,
            audit,
            telemetry,
            tools: Mutex::new(HashMap::new()),
            steps: Mutex::new(HashMap::new()),
        })
    }

    fn record(&self, session: &str, ev: AuditEvent, attrs: Attrs) {
        if let Some(a) = &self.audit {
            a.record(Some(session), ev, attrs);
        }
    }

    fn usage(&self, ev: UsageEvent) {
        if let Some(t) = &self.telemetry {
            t.usage(ev);
        }
    }

    fn observe(&self, env: &Envelope) {
        let sid = env.session_id.as_str();
        let (event, _) = env.event.to_wire();
        match event {
            SessionEvent::ToolStarted(t) => {
                lock(&self.tools).insert(
                    (sid.to_string(), t.call_id),
                    Started {
                        name: t.name,
                        resource: t.resource,
                        at: Instant::now(),
                    },
                );
            }
            SessionEvent::ToolEnded(t) => {
                let started = lock(&self.tools).remove(&(sid.to_string(), t.call_id.clone()));
                let mut attrs = Attrs::new()
                    .with("call_id", t.call_id)
                    .with("outcome", outcome_str(t.outcome));
                if let Some(s) = started {
                    attrs = attrs
                        .with("tool", s.name)
                        .with("duration_ms", s.at.elapsed().as_millis() as u64);
                    if let Some(r) = s.resource {
                        attrs = attrs.with("resource", r);
                    }
                }
                if let Some(e) = t.error {
                    attrs = attrs.with("error_code", e.code);
                }
                self.record(sid, AuditEvent::ToolCalled, attrs);
            }
            SessionEvent::StepStarted(s) => {
                lock(&self.steps).insert(sid.to_string(), (s.model, s.route));
            }
            SessionEvent::StepEnded(s) => {
                let (model, route) = lock(&self.steps).remove(sid).unwrap_or_default();
                let u = s.usage;
                self.record(
                    sid,
                    AuditEvent::Usage,
                    Attrs::new()
                        .with("model", model)
                        .with("route", route)
                        .with("input_tokens", u.input_tokens)
                        .with("output_tokens", u.output_tokens)
                        .with("cache_read_tokens", u.cache_read_tokens)
                        .with("cache_write_tokens", u.cache_write_tokens)
                        .with("reasoning_tokens", u.reasoning_tokens)
                        .with("cost_usd", s.cost.usd)
                        .with(
                            "finish",
                            serde_json::to_value(s.finish).unwrap_or(Value::Null),
                        ),
                );
            }
            SessionEvent::BrowserAction(b) => {
                self.record(
                    sid,
                    AuditEvent::BrowserAction,
                    Attrs::new()
                        .with("tool", b.tool)
                        .with("origin", b.origin)
                        .with("decision", b.decision)
                        .with("status", b.status),
                );
                self.usage(UsageEvent::new(UsageName::BrowserUsed));
            }
            SessionEvent::SessionCreated(c) => {
                self.usage(UsageEvent::new(UsageName::SessionCreated).with(
                    "kind",
                    UsageProp::Enum(match c.kind {
                        SessionKind::Chat => "chat",
                        SessionKind::Code => "code",
                        SessionKind::Unknown => "other",
                    }),
                ))
            }
            SessionEvent::TurnSettled(t) => {
                self.usage(UsageEvent::new(UsageName::TurnCompleted).with(
                    "status",
                    UsageProp::Enum(match t.status {
                        TurnStatus::Completed => "completed",
                        TurnStatus::Failed => "failed",
                        TurnStatus::Cancelled => "cancelled",
                        TurnStatus::Unknown => "unknown",
                    }),
                ))
            }
            SessionEvent::ArtifactVersioned(a) if a.n <= 1 => {
                self.usage(UsageEvent::new(UsageName::ArtifactCreated));
            }
            _ => {}
        }
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn outcome_str(o: ToolOutcome) -> &'static str {
    match o {
        ToolOutcome::Ok => "ok",
        ToolOutcome::Error => "error",
        ToolOutcome::Declined => "declined",
        ToolOutcome::Blocked => "blocked",
        ToolOutcome::Cancelled => "cancelled",
        ToolOutcome::Unknown => "unknown",
    }
}

impl Journal for EventTap {
    fn append(&self, env: &Envelope) -> Result<(), Defect> {
        let r = self.inner.append(env);
        if r.is_ok() {
            self.observe(env);
        }
        r
    }
    fn events_after(&self, session: &str, after_seq: u64) -> Result<Vec<Envelope>, Defect> {
        self.inner.events_after(session, after_seq)
    }
    fn last_seq(&self, session: &str) -> Result<u64, Defect> {
        self.inner.last_seq(session)
    }
    fn claim_turn(&self, session: &str, turn: &str) -> Result<(), Defect> {
        self.inner.claim_turn(session, turn)
    }
    fn settle_turn(&self, session: &str, turn: &str, finish: TurnFinish) -> Result<(), Defect> {
        self.inner.settle_turn(session, turn, finish)
    }
    fn last_turn(&self, session: &str) -> Result<Option<TurnRecord>, Defect> {
        self.inner.last_turn(session)
    }
    fn unsettled_claims(&self) -> Result<Vec<(SessionId, TurnId)>, Defect> {
        self.inner.unsettled_claims()
    }
    fn log_floor(&self, session: &str) -> Result<u64, Defect> {
        self.inner.log_floor(session)
    }
}

// ---- OTLP ----------------------------------------------------------------------------

/// OTLP/HTTP JSON transport for the audit queue.
pub struct HttpOtlp {
    pub client: reqwest::Client,
    pub url: String,
    pub headers: Vec<(String, String)>,
}

impl HttpOtlp {
    /// `endpoint` is the collector base URL (`…/v1/logs` is appended unless present).
    pub fn new(client: reqwest::Client, endpoint: &str, headers: Vec<(String, String)>) -> Self {
        let e = endpoint.trim_end_matches('/');
        let url = if e.ends_with("/v1/logs") {
            e.to_string()
        } else {
            format!("{e}/v1/logs")
        };
        HttpOtlp {
            client,
            url,
            headers,
        }
    }
}

impl OtlpTransport for HttpOtlp {
    fn send(&self, body: Value) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async move {
            let mut req = self
                .client
                .post(&self.url)
                .timeout(Duration::from_secs(30))
                .header("Content-Type", "application/json")
                .body(serde_json::to_vec(&body).map_err(|e| e.to_string())?);
            for (k, v) in &self.headers {
                req = req.header(k, v);
            }
            let resp = req.send().await.map_err(|e| e.to_string())?;
            if resp.status().is_success() {
                Ok(())
            } else {
                Err(format!("collector answered {}", resp.status()))
            }
        })
    }
}

/// Headers for the OTLP exporter from the secrets backend (JSON object of strings).
pub fn otlp_headers(backend: &dyn genie_secrets::SecretBackend) -> Vec<(String, String)> {
    match backend.get(OTLP_HEADERS_ACCOUNT) {
        Ok(Some(bytes)) => serde_json::from_slice::<serde_json::Map<String, Value>>(&bytes)
            .map(|m| {
                m.into_iter()
                    .filter_map(|(k, v)| v.as_str().map(|v| (k, v.to_string())))
                    .collect()
            })
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "audit OTLP headers in the keychain are not a JSON object");
                Vec::new()
            }),
        _ => Vec::new(),
    }
}

// ---- Telemetry -----------------------------------------------------------------------

/// Sentry envelope exporter (SA-32).
pub struct SentryExporter {
    pub client: reqwest::Client,
    pub dsn: Dsn,
}

impl TelemetryExporter for SentryExporter {
    fn export<'a>(&'a self, batch: &'a [TelemetryRecord]) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let url = self.dsn.envelope_url();
            let auth = self
                .dsn
                .auth_header(&format!("genie/{}", env!("CARGO_PKG_VERSION")));
            for r in batch {
                let resp = self
                    .client
                    .post(&url)
                    .timeout(Duration::from_secs(30))
                    .header("Content-Type", "application/x-sentry-envelope")
                    .header("X-Sentry-Auth", &auth)
                    .body(genie_audit::telemetry::sentry_envelope(r))
                    .send()
                    .await
                    .map_err(|e| e.to_string())?;
                // 429 / 5xx: retry later; 4xx other than 429: the record is dropped by
                // treating it as sent (it would never be accepted).
                let st = resp.status();
                if st.as_u16() == 429 || st.is_server_error() {
                    return Err(format!("telemetry endpoint answered {st}"));
                }
            }
            Ok(())
        })
    }
}

/// Open the telemetry client in `<data dir>/telemetry`.
pub fn open_telemetry(data_dir: &std::path::Path, channel: &str) -> Option<Arc<Telemetry>> {
    match Telemetry::open(
        &data_dir.join("telemetry"),
        TelemetryContext {
            version: env!("CARGO_PKG_VERSION").to_string(),
            channel: channel.to_string(),
            component: "genie-daemon".into(),
        },
    ) {
        Ok(t) => Some(Arc::new(t)),
        Err(e) => {
            tracing::warn!(error = %e, "telemetry unavailable");
            None
        }
    }
}

/// Consent from user settings (`telemetry.crashReports`, `telemetry.usage`) capped by
/// policy `telemetry.level`.
pub fn consent_from(hub: &ConfigHub) -> TelemetryConsent {
    let policy = hub.policy();
    TelemetryConsent::resolve(
        hub.value(None, "telemetry.crashReports")
            .and_then(|v| v.as_bool()),
        hub.value(None, "telemetry.usage").and_then(|v| v.as_bool()),
        policy.get("telemetry.level").and_then(Value::as_str),
    )
}

/// The telemetry DSN: policy `telemetry.endpoint`, else `GENIE_TELEMETRY_DSN` (run time,
/// then build time). `off` or nothing = no export.
pub fn telemetry_dsn(hub: Option<&ConfigHub>) -> Option<Dsn> {
    let from_policy = hub.and_then(|h| {
        h.policy()
            .get("telemetry.endpoint")
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    let s = from_policy
        .or_else(|| std::env::var("GENIE_TELEMETRY_DSN").ok())
        .or_else(|| option_env!("GENIE_TELEMETRY_DSN").map(str::to_string))?;
    if s.trim().eq_ignore_ascii_case("off") {
        return None;
    }
    Dsn::parse(&s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use genie_core::bus::CoreEvent;
    use genie_core::journal::MemoryJournal;
    use std::sync::Arc;

    #[derive(Default)]
    struct Capture(Mutex<Vec<genie_audit::AuditRecord>>);
    impl genie_audit::AuditSink for Capture {
        fn write(&self, r: &genie_audit::AuditRecord) -> std::io::Result<()> {
            self.0.lock().unwrap().push(r.clone());
            Ok(())
        }
    }

    #[test]
    fn otlp_url() {
        let c = reqwest::Client::new();
        assert_eq!(
            HttpOtlp::new(c.clone(), "https://o/", vec![]).url,
            "https://o/v1/logs"
        );
        assert_eq!(
            HttpOtlp::new(c, "https://o/v1/logs", vec![]).url,
            "https://o/v1/logs"
        );
    }

    #[test]
    fn non_wire_events_pass_through() {
        let cap = Arc::new(Capture::default());
        let audit = Arc::new(
            Auditor::new(genie_audit::AuditContext {
                install_id: "i".into(),
                user: "u".into(),
                org: None,
                include_content: false,
            })
            .with_sink(cap.clone()),
        );
        let j = EventTap::wrap(Arc::new(MemoryJournal::new()), Some(audit), None);
        let env = Envelope {
            session_id: "s1".into(),
            seq: Some(1),
            ref_seq: None,
            ts_ms: 0,
            event: CoreEvent::InboxRemoved {
                item_id: "x".into(),
                items: vec![],
            },
        };
        j.append(&env).unwrap();
        assert_eq!(j.last_seq("s1").unwrap(), 1);
        assert!(cap.0.lock().unwrap().is_empty());
    }

    #[test]
    fn tool_calls_usage_and_browser_actions_are_audited() {
        use genie_protocol::common::{Cost, Usage};
        use genie_protocol::event as ev;
        let cap = Arc::new(Capture::default());
        let audit = Arc::new(
            Auditor::new(genie_audit::AuditContext {
                install_id: "i".into(),
                user: "u".into(),
                org: Some("Org".into()),
                include_content: false,
            })
            .with_sink(cap.clone()),
        );
        let dir = tempfile::tempdir().unwrap();
        let tel = open_telemetry(dir.path(), "dev").unwrap();
        tel.set_consent(TelemetryConsent {
            crash_reports: false,
            usage: true,
        });
        let j = EventTap::wrap(
            Arc::new(MemoryJournal::new()),
            Some(audit),
            Some(tel.clone()),
        );
        let mut seq = 0;
        let mut send = |e: SessionEvent| {
            seq += 1;
            j.append(&Envelope {
                session_id: "s1".into(),
                seq: Some(seq),
                ref_seq: None,
                ts_ms: 0,
                event: CoreEvent::Wire { event: Box::new(e) },
            })
            .unwrap();
        };
        send(SessionEvent::StepStarted(ev::StepStarted {
            step: 1,
            model: "claude-x".into(),
            route: "anthropic/default".into(),
        }));
        send(SessionEvent::ToolStarted(ev::ToolStarted {
            call_id: "c1".into(),
            name: "shell".into(),
            resource: Some("git status".into()),
        }));
        send(SessionEvent::ToolEnded(ev::ToolEnded {
            call_id: "c1".into(),
            outcome: ToolOutcome::Blocked,
            result_part_id: "p".into(),
            spill_ref: None,
            error: Some(ev::ErrorInfo {
                code: "policy_blocked".into(),
                message: "Blocked by Org policy (Bash)".into(),
                link: None,
            }),
        }));
        send(SessionEvent::StepEnded(ev::StepEnded {
            finish: ev::StepFinish::ToolUse,
            usage: Usage {
                input_tokens: 100,
                output_tokens: 20,
                ..Default::default()
            },
            cost: Cost { usd: 0.01 },
        }));
        send(SessionEvent::BrowserAction(ev::BrowserAction {
            tab_id: "t".into(),
            tool: "browser_click".into(),
            origin: "https://example.com".into(),
            decision: "allowed".into(),
            status: "ok".into(),
            thumbnail_ref: None,
        }));
        let recs = cap.0.lock().unwrap();
        let names: Vec<&str> = recs.iter().map(|r| r.event.as_str()).collect();
        assert_eq!(names, ["tool.called", "usage.request", "browser.action"]);
        let tool = &recs[0].attrs;
        assert_eq!(tool["tool"], "shell");
        assert_eq!(tool["resource"], "git status");
        assert_eq!(tool["outcome"], "blocked");
        assert_eq!(tool["error_code"], "policy_blocked");
        assert!(tool.contains_key("duration_ms"));
        assert_eq!(recs[0].session_id.as_deref(), Some("s1"));
        let usage = &recs[1].attrs;
        assert_eq!(usage["model"], "claude-x");
        assert_eq!(usage["input_tokens"], 100);
        assert_eq!(usage["cost_usd"], 0.01);
        assert_eq!(recs[2].attrs["origin"], "https://example.com");
        assert_eq!(tel.queued(), 1, "browser use counted for analytics");
    }
}
