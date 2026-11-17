//! Telemetry (08 §7, D39; SA-32): crash reports on by default (opt-out), usage analytics
//! opt-in, never content; admins cap both with policy `telemetry.level`.
//!
//! - **What is collected** is fixed by types: [`UsageEvent`] names come from an enum and
//!   their properties are numbers, booleans or `&'static str` enum values, so free text
//!   (prompts, paths, names) can't be recorded. A [`CrashReport`] carries versions, OS,
//!   the panic location as a module path, function names from the backtrace and the
//!   panic message after strict scrubbing ([`crate::scrub`]).
//! - Records go to a **local queue** (`<data dir>/telemetry/queue.jsonl`, 0600, bounded)
//!   and are sent by [`Telemetry::flush`] through a [`TelemetryExporter`] port. The daemon's
//!   exporter speaks the Sentry envelope protocol ([`sentry_envelope`]) to a self-hosted
//!   Sentry-compatible endpoint (SA-32); without an endpoint nothing leaves the device.
//! - The install id is random, kept in `<data dir>/telemetry/install_id`, separate from the
//!   audit install id, and rotatable ([`Telemetry::rotate_install_id`]).

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::audit::rfc3339_millis;
use crate::otlp::BoxFuture;
use crate::scrub::{Scrubber, module_path};

/// Records kept in the local queue (oldest dropped first).
pub const QUEUE_MAX: usize = 1000;
/// Records per flush.
pub const FLUSH_BATCH: usize = 50;
/// Frames kept per crash report.
const MAX_FRAMES: usize = 64;

/// Policy `telemetry.level` (08 §5.2): the most an admin allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TelemetryLevel {
    Off,
    CrashOnly,
    Usage,
}

impl TelemetryLevel {
    pub fn parse(s: &str) -> Option<TelemetryLevel> {
        match s {
            "off" => Some(TelemetryLevel::Off),
            "crash-only" => Some(TelemetryLevel::CrashOnly),
            "usage" => Some(TelemetryLevel::Usage),
            _ => None,
        }
    }
}

/// What may be collected, after user consent and the admin cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TelemetryConsent {
    pub crash_reports: bool,
    pub usage: bool,
}

impl TelemetryConsent {
    /// User settings `telemetry.crashReports` (default on) and `telemetry.usage` (default
    /// off), capped by policy `telemetry.level` (no policy = no cap; an unknown level
    /// counts as `off`).
    pub fn resolve(
        crash_setting: Option<bool>,
        usage_setting: Option<bool>,
        policy_level: Option<&str>,
    ) -> TelemetryConsent {
        let cap = match policy_level {
            None => TelemetryLevel::Usage,
            Some(s) => TelemetryLevel::parse(s).unwrap_or(TelemetryLevel::Off),
        };
        TelemetryConsent {
            crash_reports: crash_setting.unwrap_or(true) && cap >= TelemetryLevel::CrashOnly,
            usage: usage_setting.unwrap_or(false) && cap >= TelemetryLevel::Usage,
        }
    }
}

/// Usage analytics event names (feature counts and timings only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageName {
    SessionCreated,
    TurnCompleted,
    ArtifactCreated,
    McpServerAdded,
    SkillInstalled,
    CredentialAdded,
    BrowserUsed,
    DoctorRun,
}

impl UsageName {
    pub fn as_str(self) -> &'static str {
        match self {
            UsageName::SessionCreated => "session.created",
            UsageName::TurnCompleted => "turn.completed",
            UsageName::ArtifactCreated => "artifact.created",
            UsageName::McpServerAdded => "mcp.server_added",
            UsageName::SkillInstalled => "skill.installed",
            UsageName::CredentialAdded => "provider.credential_added",
            UsageName::BrowserUsed => "browser.used",
            UsageName::DoctorRun => "doctor.run",
        }
    }
}

/// A property value: never free text.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UsageProp {
    Count(u64),
    Flag(bool),
    /// One of a fixed set of values chosen in code (`"chat"`, `"code"`, `"stdio"`…).
    Enum(&'static str),
}

impl From<UsageProp> for Value {
    fn from(p: UsageProp) -> Value {
        match p {
            UsageProp::Count(n) => json!(n),
            UsageProp::Flag(b) => json!(b),
            UsageProp::Enum(s) => json!(s),
        }
    }
}

/// One usage event as produced by the engine.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageEvent {
    pub name: UsageName,
    pub props: Vec<(&'static str, UsageProp)>,
}

impl UsageEvent {
    pub fn new(name: UsageName) -> UsageEvent {
        UsageEvent {
            name,
            props: Vec::new(),
        }
    }

    pub fn with(mut self, key: &'static str, value: UsageProp) -> UsageEvent {
        self.props.push((key, value));
        self
    }
}

/// Where the report comes from (set once per process).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TelemetryContext {
    pub version: String,
    pub channel: String,
    /// `"genie-daemon"`, `"genie-cli"`…
    pub component: String,
}

/// One frame of a crash backtrace: function name and anonymised module path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrashFrame {
    pub function: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrashReport {
    /// Scrubbed panic message (strict: no home paths, secrets, e-mails).
    pub message: String,
    /// `genie-core/src/session.rs:120`.
    pub location: Option<String>,
    pub thread: Option<String>,
    pub frames: Vec<CrashFrame>,
}

/// A queued record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TelemetryRecord {
    pub id: String,
    pub ts: String,
    pub install_id: String,
    pub version: String,
    pub channel: String,
    pub component: String,
    pub os: String,
    pub arch: String,
    #[serde(flatten)]
    pub body: RecordBody,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecordBody {
    Crash(CrashReport),
    Usage {
        name: String,
        props: Map<String, Value>,
    },
}

/// Sends a batch of records.
pub trait TelemetryExporter: Send + Sync {
    fn export<'a>(&'a self, batch: &'a [TelemetryRecord]) -> BoxFuture<'a, Result<(), String>>;
}

fn random_hex(n: usize) -> String {
    let mut b = vec![0u8; n];
    if getrandom::fill(&mut b).is_err() {
        // Fall back to time-derived bytes; ids are not secrets.
        let t = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        for (i, x) in b.iter_mut().enumerate() {
            *x = (t >> ((i % 16) * 8)) as u8 ^ i as u8;
        }
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn open_private(path: &Path, append: bool) -> io::Result<fs::File> {
    let mut o = OpenOptions::new();
    o.create(true);
    if append {
        o.append(true);
    } else {
        o.write(true).truncate(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)
}

struct State {
    consent: TelemetryConsent,
    install_id: String,
}

/// The telemetry client of one process.
pub struct Telemetry {
    dir: PathBuf,
    ctx: TelemetryContext,
    state: Mutex<State>,
    /// Serialises queue file access; caches the record count.
    queue: Mutex<Option<usize>>,
    scrubber: Scrubber,
}

impl std::fmt::Debug for Telemetry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Telemetry")
            .field("dir", &self.dir)
            .field("consent", &self.consent())
            .finish_non_exhaustive()
    }
}

impl Telemetry {
    /// `dir` is `<data dir>/telemetry`. Starts with nothing allowed until
    /// [`Self::set_consent`].
    pub fn open(dir: &Path, ctx: TelemetryContext) -> io::Result<Telemetry> {
        fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        }
        let id_path = dir.join("install_id");
        let install_id = match fs::read_to_string(&id_path) {
            Ok(s) if s.trim().len() >= 16 => s.trim().to_string(),
            _ => {
                let id = random_hex(16);
                open_private(&id_path, false)?.write_all(id.as_bytes())?;
                id
            }
        };
        Ok(Telemetry {
            dir: dir.to_path_buf(),
            ctx,
            state: Mutex::new(State {
                consent: TelemetryConsent::default(),
                install_id,
            }),
            queue: Mutex::new(None),
            scrubber: Scrubber::from_env().strict(),
        })
    }

    /// Replace the scrubber (tests: a fixed home directory).
    pub fn with_scrubber(mut self, s: Scrubber) -> Telemetry {
        self.scrubber = s.strict();
        self
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Apply settings + policy. Turning a kind off also drops its queued records.
    pub fn set_consent(&self, consent: TelemetryConsent) {
        self.lock().consent = consent;
        let mut g = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        *g = None;
        if let Ok(mut q) = self.read_queue() {
            let before = q.len();
            q.retain(|r| match r.body {
                RecordBody::Crash(_) => consent.crash_reports,
                RecordBody::Usage { .. } => consent.usage,
            });
            if q.len() != before {
                let _ = self.write_queue(&q);
            }
        }
    }

    pub fn consent(&self) -> TelemetryConsent {
        self.lock().consent
    }

    pub fn install_id(&self) -> String {
        self.lock().install_id.clone()
    }

    /// A fresh random install id (Settings → Privacy & data).
    pub fn rotate_install_id(&self) -> io::Result<String> {
        let id = random_hex(16);
        open_private(&self.dir.join("install_id"), false)?.write_all(id.as_bytes())?;
        self.lock().install_id = id.clone();
        Ok(id)
    }

    fn queue_path(&self) -> PathBuf {
        self.dir.join("queue.jsonl")
    }

    fn read_queue(&self) -> io::Result<Vec<TelemetryRecord>> {
        match fs::read_to_string(self.queue_path()) {
            Ok(s) => Ok(s
                .lines()
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e),
        }
    }

    fn write_queue(&self, q: &[TelemetryRecord]) -> io::Result<()> {
        let tmp = self.dir.join("queue.jsonl.tmp");
        let mut f = open_private(&tmp, false)?;
        for r in q {
            let mut line = serde_json::to_vec(r).map_err(io::Error::other)?;
            line.push(b'\n');
            f.write_all(&line)?;
        }
        f.sync_all()?;
        fs::rename(tmp, self.queue_path())
    }

    fn record(&self, body: RecordBody) -> TelemetryRecord {
        TelemetryRecord {
            id: random_hex(16),
            ts: rfc3339_millis(SystemTime::now()),
            install_id: self.install_id(),
            version: self.ctx.version.clone(),
            channel: self.ctx.channel.clone(),
            component: self.ctx.component.clone(),
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            body,
        }
    }

    fn enqueue(&self, rec: TelemetryRecord) {
        let mut count = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        let res = (|| -> io::Result<()> {
            let n = match *count {
                Some(n) => n,
                None => self.read_queue()?.len(),
            };
            let mut line = serde_json::to_vec(&rec).map_err(io::Error::other)?;
            line.push(b'\n');
            open_private(&self.queue_path(), true)?.write_all(&line)?;
            *count = Some(n + 1);
            if n + 1 > QUEUE_MAX {
                let q = self.read_queue()?;
                self.write_queue(&q[q.len().saturating_sub(QUEUE_MAX)..])?;
                *count = Some(q.len().min(QUEUE_MAX));
            }
            Ok(())
        })();
        if res.is_err() {
            *count = None;
        }
        if let Err(e) = res {
            tracing::debug!(error = %e, "telemetry queue write failed");
        }
    }

    /// Record a usage event (only with usage consent).
    pub fn usage(&self, ev: UsageEvent) {
        if !self.consent().usage {
            return;
        }
        let props = ev
            .props
            .into_iter()
            .map(|(k, v)| (k.to_string(), Value::from(v)))
            .collect();
        let rec = self.record(RecordBody::Usage {
            name: ev.name.as_str().to_string(),
            props,
        });
        self.enqueue(rec);
    }

    /// Build a crash report (scrubbed) from a panic.
    pub fn crash_report(
        &self,
        message: &str,
        location: Option<(&str, u32)>,
        thread: Option<&str>,
        backtrace: &str,
    ) -> CrashReport {
        let mut msg = self.scrubber.scrub(message);
        if msg.chars().count() > 300 {
            msg = msg.chars().take(300).collect::<String>() + "…";
        }
        CrashReport {
            message: msg,
            location: location.map(|(f, l)| format!("{}:{l}", module_path(f))),
            thread: thread.map(|t| self.scrubber.scrub(t)),
            frames: parse_backtrace(backtrace),
        }
    }

    /// Queue a crash report (only with crash consent).
    pub fn crash(&self, report: CrashReport) {
        if !self.consent().crash_reports {
            return;
        }
        let rec = self.record(RecordBody::Crash(report));
        self.enqueue(rec);
    }

    /// Chain a panic hook that queues a crash report before the previous hook runs.
    pub fn install_panic_hook(self: &Arc<Self>) {
        let me = Arc::downgrade(self);
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if let Some(t) = me.upgrade()
                && t.consent().crash_reports
            {
                let msg = info
                    .payload()
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| info.payload().downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "panic".into());
                let bt = std::backtrace::Backtrace::force_capture().to_string();
                let cur = std::thread::current();
                let report = t.crash_report(
                    &msg,
                    info.location().map(|l| (l.file(), l.line())),
                    cur.name(),
                    &bt,
                );
                t.crash(report);
            }
            prev(info);
        }));
    }

    /// Records waiting to be sent.
    pub fn queued(&self) -> usize {
        let _g = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        self.read_queue().map(|q| q.len()).unwrap_or(0)
    }

    /// Send queued records batch by batch; a failed batch stays queued. Returns how many
    /// were sent.
    pub async fn flush(&self, exporter: &dyn TelemetryExporter) -> Result<usize, String> {
        let mut sent = 0;
        loop {
            let batch: Vec<TelemetryRecord> = {
                let _g = self.queue.lock().unwrap_or_else(|e| e.into_inner());
                let q = self.read_queue().map_err(|e| e.to_string())?;
                q.into_iter().take(FLUSH_BATCH).collect()
            };
            if batch.is_empty() {
                return Ok(sent);
            }
            exporter.export(&batch).await?;
            sent += batch.len();
            let mut g = self.queue.lock().unwrap_or_else(|e| e.into_inner());
            *g = None;
            let mut q = self.read_queue().map_err(|e| e.to_string())?;
            q.retain(|r| !batch.iter().any(|b| b.id == r.id));
            self.write_queue(&q).map_err(|e| e.to_string())?;
        }
    }
}

/// Function names and module paths from `std::backtrace::Backtrace`'s text form.
pub fn parse_backtrace(bt: &str) -> Vec<CrashFrame> {
    let mut frames: Vec<CrashFrame> = Vec::new();
    for line in bt.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("at ") {
            if let Some(f) = frames.last_mut()
                && f.module.is_none()
            {
                // `path:line:col`
                let mut parts = rest.rsplitn(3, ':');
                let _col = parts.next();
                let line = parts.next().and_then(|l| l.parse().ok());
                let file = parts.next().unwrap_or(rest);
                f.module = Some(module_path(file));
                f.line = line;
            }
        } else if let Some((idx, func)) = t.split_once(": ")
            && idx.chars().all(|c| c.is_ascii_digit())
            && !idx.is_empty()
        {
            if frames.len() >= MAX_FRAMES {
                break;
            }
            // Strip the symbol hash suffix (`::h0123abcd`).
            let func = match func.rsplit_once("::h") {
                Some((f, h)) if h.len() == 16 && h.chars().all(|c| c.is_ascii_hexdigit()) => f,
                _ => func,
            };
            frames.push(CrashFrame {
                function: func.to_string(),
                module: None,
                line: None,
            });
        }
    }
    frames
}

/// A Sentry DSN (`https://<public key>@<host>[/<path>]/<project id>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dsn {
    pub scheme: String,
    pub public_key: String,
    pub host: String,
    pub path: String,
    pub project_id: String,
}

impl Dsn {
    pub fn parse(s: &str) -> Option<Dsn> {
        let (scheme, rest) = s.trim().split_once("://")?;
        if scheme != "https" && scheme != "http" {
            return None;
        }
        let (key, rest) = rest.split_once('@')?;
        let public_key = key.split(':').next()?.to_string();
        let (host, path) = rest.split_once('/')?;
        let path = path.trim_end_matches('/');
        let (prefix, project_id) = match path.rsplit_once('/') {
            Some((p, id)) => (format!("/{p}"), id.to_string()),
            None => (String::new(), path.to_string()),
        };
        if public_key.is_empty() || host.is_empty() || project_id.is_empty() {
            return None;
        }
        Some(Dsn {
            scheme: scheme.to_string(),
            public_key,
            host: host.to_string(),
            path: prefix,
            project_id,
        })
    }

    pub fn envelope_url(&self) -> String {
        format!(
            "{}://{}{}/api/{}/envelope/",
            self.scheme, self.host, self.path, self.project_id
        )
    }

    pub fn auth_header(&self, client: &str) -> String {
        format!(
            "Sentry sentry_version=7, sentry_key={}, sentry_client={client}",
            self.public_key
        )
    }
}

/// One record as a Sentry envelope body (one event item per envelope).
pub fn sentry_envelope(record: &TelemetryRecord) -> String {
    let event_id = record.id.clone();
    let tags = json!({
        "install_id": record.install_id,
        "channel": record.channel,
        "component": record.component,
    });
    let contexts = json!({ "os": { "name": record.os }, "device": { "arch": record.arch } });
    let event = match &record.body {
        RecordBody::Crash(c) => json!({
            "event_id": event_id,
            "timestamp": record.ts,
            "platform": "native",
            "level": "fatal",
            "release": format!("genie@{}", record.version),
            "environment": record.channel,
            "tags": tags,
            "contexts": contexts,
            "exception": { "values": [{
                "type": "panic",
                "value": c.message,
                "mechanism": { "type": "panic", "handled": false },
                "stacktrace": { "frames": c.frames.iter().rev().map(|f| json!({
                    "function": f.function,
                    "module": f.module,
                    "lineno": f.line,
                })).collect::<Vec<_>>() },
            }]},
            "extra": { "location": c.location, "thread": c.thread },
        }),
        RecordBody::Usage { name, props } => json!({
            "event_id": event_id,
            "timestamp": record.ts,
            "platform": "other",
            "level": "info",
            "logger": "genie.usage",
            "release": format!("genie@{}", record.version),
            "environment": record.channel,
            "message": { "formatted": name },
            "tags": tags,
            "contexts": contexts,
            "extra": props,
        }),
    };
    let header = json!({ "event_id": event_id, "sent_at": record.ts });
    let item = json!({ "type": "event", "content_type": "application/json" });
    format!("{header}\n{item}\n{event}\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> TelemetryContext {
        TelemetryContext {
            version: "0.1.0".into(),
            channel: "stable".into(),
            component: "genie-daemon".into(),
        }
    }

    #[test]
    fn consent_matrix() {
        let r = TelemetryConsent::resolve;
        assert_eq!(
            r(None, None, None),
            TelemetryConsent {
                crash_reports: true,
                usage: false
            },
            "crash on by default, usage opt-in"
        );
        assert!(r(None, Some(true), None).usage);
        assert!(!r(Some(false), None, None).crash_reports);
        assert!(!r(None, Some(true), Some("crash-only")).usage);
        assert!(r(None, None, Some("crash-only")).crash_reports);
        assert_eq!(r(None, Some(true), Some("off")), TelemetryConsent::default());
        assert_eq!(r(None, Some(true), Some("bogus")), TelemetryConsent::default());
    }

    #[test]
    fn queue_respects_consent_bound_and_flushes() {
        let dir = tempfile::tempdir().unwrap();
        let t = Telemetry::open(dir.path(), ctx())
            .unwrap()
            .with_scrubber(Scrubber::new(Some("/home/bob"), None));
        // Nothing before consent.
        t.usage(UsageEvent::new(UsageName::SessionCreated));
        assert_eq!(t.queued(), 0);
        t.set_consent(TelemetryConsent {
            crash_reports: true,
            usage: true,
        });
        t.usage(
            UsageEvent::new(UsageName::McpServerAdded)
                .with("transport", UsageProp::Enum("stdio"))
                .with("count", UsageProp::Count(2)),
        );
        let report = t.crash_report(
            "failed to open /home/bob/clients/acme/notes.md with token=abc123",
            Some(("/home/bob/genie/crates/genie-core/src/session.rs", 120)),
            Some("tokio-runtime-worker"),
            "   0: std::backtrace::Backtrace::create::h0123456789abcdef\n             at /rustc/abc/library/std/src/backtrace.rs:331:13\n   1: genie_core::session::run\n             at /home/bob/genie/crates/genie-core/src/session.rs:120:9\n",
        );
        assert!(!report.message.contains("acme") && !report.message.contains("abc123"));
        assert_eq!(report.location.as_deref(), Some("genie-core/src/session.rs:120"));
        assert_eq!(report.frames[0].function, "std::backtrace::Backtrace::create");
        assert_eq!(report.frames[1].module.as_deref(), Some("genie-core/src/session.rs"));
        assert_eq!(report.frames[1].line, Some(120));
        t.crash(report);
        assert_eq!(t.queued(), 2);
        let raw = fs::read_to_string(dir.path().join("queue.jsonl")).unwrap();
        assert!(!raw.contains("/home/bob"), "{raw}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let m = fs::metadata(dir.path().join("queue.jsonl")).unwrap().permissions().mode();
            assert_eq!(m & 0o777, 0o600);
        }

        // Turning usage off drops queued usage records.
        t.set_consent(TelemetryConsent {
            crash_reports: true,
            usage: false,
        });
        assert_eq!(t.queued(), 1);

        struct Exp(Mutex<Vec<String>>, bool);
        impl TelemetryExporter for Exp {
            fn export<'a>(
                &'a self,
                batch: &'a [TelemetryRecord],
            ) -> BoxFuture<'a, Result<(), String>> {
                Box::pin(async move {
                    if self.1 {
                        return Err("offline".into());
                    }
                    for r in batch {
                        self.0.lock().unwrap().push(sentry_envelope(r));
                    }
                    Ok(())
                })
            }
        }
        let run = |e: &Exp| {
            let waker = std::task::Waker::noop();
            let mut cx = std::task::Context::from_waker(waker);
            let mut f = std::pin::pin!(t.flush(e));
            loop {
                if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
                    return v;
                }
            }
        };
        assert!(run(&Exp(Mutex::new(vec![]), true)).is_err());
        assert_eq!(t.queued(), 1, "kept for the next try");
        let ok = Exp(Mutex::new(vec![]), false);
        assert_eq!(run(&ok).unwrap(), 1);
        assert_eq!(t.queued(), 0);
        let env = ok.0.lock().unwrap()[0].clone();
        let lines: Vec<Value> = env.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines[1]["type"], "event");
        assert_eq!(lines[2]["level"], "fatal");
        assert_eq!(lines[2]["release"], "genie@0.1.0");

        // Bounded queue.
        t.set_consent(TelemetryConsent {
            crash_reports: false,
            usage: true,
        });
        for _ in 0..(QUEUE_MAX + 5) {
            t.usage(UsageEvent::new(UsageName::TurnCompleted));
        }
        assert_eq!(t.queued(), QUEUE_MAX);

        let old = t.install_id();
        let new = t.rotate_install_id().unwrap();
        assert_ne!(old, new);
        assert_eq!(
            fs::read_to_string(dir.path().join("install_id")).unwrap(),
            new
        );
    }

    #[test]
    fn dsn() {
        let d = Dsn::parse("https://abc123@sentry.synergech.example/genie/42").unwrap();
        assert_eq!(
            d.envelope_url(),
            "https://sentry.synergech.example/genie/api/42/envelope/"
        );
        assert!(d.auth_header("genie/0.1").contains("sentry_key=abc123"));
        assert_eq!(
            Dsn::parse("https://k@h/7").unwrap().envelope_url(),
            "https://h/api/7/envelope/"
        );
        assert!(Dsn::parse("off").is_none());
        assert!(Dsn::parse("ftp://k@h/1").is_none());
    }
}
