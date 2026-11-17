//! The org audit stream (08 §6, D38): versioned records to pluggable sinks. This module
//! ships the local rotating JSONL sink; the OTLP exporter implements [`AuditSink`] later.
//!
//! Record: `{audit_schema: 1, ts, install_id, user, org, session_id?, event, attrs}`.
//! Content (prompts, arguments, outputs) is dropped unless policy sets
//! `audit.includeContent` (the UI then shows a notice). Secret-looking attribute names are
//! always dropped.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const AUDIT_SCHEMA: u32 = 1;
/// Default size cap per file and number of files kept.
pub const AUDIT_FILE_BYTES: u64 = 10 * 1024 * 1024;
pub const AUDIT_FILES: usize = 10;

/// Audited event kinds (08 §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditEvent {
    PermissionAsked,
    PermissionDecided,
    ToolCalled,
    PolicyBlocked,
    SignedIn,
    CredentialChanged,
    McpConnected,
    McpAuth,
    SkillInstalled,
    BrowserSiteApproved,
    BrowserAction,
    SettingsChanged,
    Usage,
    HookRan,
    TrustChanged,
    /// Settings → Privacy → "Delete all data" (04 §8).
    DataDeleted,
}

impl AuditEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            AuditEvent::PermissionAsked => "permission.asked",
            AuditEvent::PermissionDecided => "permission.decided",
            AuditEvent::ToolCalled => "tool.called",
            AuditEvent::PolicyBlocked => "policy.blocked",
            AuditEvent::SignedIn => "auth.signed_in",
            AuditEvent::CredentialChanged => "credential.changed",
            AuditEvent::McpConnected => "mcp.connected",
            AuditEvent::McpAuth => "mcp.auth",
            AuditEvent::SkillInstalled => "skill.installed",
            AuditEvent::BrowserSiteApproved => "browser.site_approved",
            AuditEvent::BrowserAction => "browser.action",
            AuditEvent::SettingsChanged => "settings.changed",
            AuditEvent::Usage => "usage.request",
            AuditEvent::HookRan => "hook.ran",
            AuditEvent::TrustChanged => "trust.changed",
            AuditEvent::DataDeleted => "data.deleted",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditRecord {
    pub audit_schema: u32,
    pub ts: String,
    pub install_id: String,
    /// OS account name.
    pub user: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub event: String,
    pub attrs: Map<String, Value>,
}

/// Attributes of one record. `content` attributes are kept only with
/// `audit.includeContent`.
#[derive(Debug, Clone, Default)]
pub struct Attrs {
    plain: Map<String, Value>,
    content: Map<String, Value>,
}

impl Attrs {
    pub fn new() -> Attrs {
        Attrs::default()
    }

    /// Metadata: tool name, resource, outcome, duration, rule, hash…
    pub fn with(mut self, key: &str, value: impl Into<Value>) -> Attrs {
        self.plain.insert(key.to_string(), value.into());
        self
    }

    /// Prompt/response text, tool arguments or output.
    pub fn content(mut self, key: &str, value: impl Into<Value>) -> Attrs {
        self.content.insert(key.to_string(), value.into());
        self
    }
}

const SECRET_WORDS: &[&str] = &[
    "secret",
    "token",
    "password",
    "passphrase",
    "api_key",
    "apikey",
    "authorization",
    "cookie",
];

fn secret_key(k: &str) -> bool {
    let k = k.to_ascii_lowercase();
    // Token *counts* (`input_tokens`, `cache_read_tokens`) are usage, not credentials.
    if k.ends_with("_tokens") {
        return false;
    }
    SECRET_WORDS.iter().any(|w| k.contains(w))
}

/// Who/where, set once per daemon (08 §6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditContext {
    pub install_id: String,
    pub user: String,
    pub org: Option<String>,
    /// Policy `audit.includeContent`.
    pub include_content: bool,
}

impl AuditContext {
    /// The OS account from the environment (`USER` / `USERNAME`).
    pub fn os_user() -> String {
        std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "unknown".into())
    }
}

pub trait AuditSink: Send + Sync {
    fn write(&self, record: &AuditRecord) -> io::Result<()>;
}

/// Fan-out to the configured sinks. Audit failures never fail the action; they are logged.
pub struct Auditor {
    ctx: AuditContext,
    sinks: Vec<Arc<dyn AuditSink>>,
    clock: fn() -> SystemTime,
}

impl std::fmt::Debug for Auditor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Auditor")
            .field("ctx", &self.ctx)
            .field("sinks", &self.sinks.len())
            .finish()
    }
}

impl Auditor {
    pub fn new(ctx: AuditContext) -> Auditor {
        Auditor {
            ctx,
            sinks: Vec::new(),
            clock: SystemTime::now,
        }
    }

    pub fn with_sink(mut self, sink: Arc<dyn AuditSink>) -> Auditor {
        self.sinks.push(sink);
        self
    }

    pub fn with_clock(mut self, clock: fn() -> SystemTime) -> Auditor {
        self.clock = clock;
        self
    }

    /// Policy changed (org name, `audit.includeContent`).
    pub fn set_context(&mut self, ctx: AuditContext) {
        self.ctx = ctx;
    }

    pub fn is_enabled(&self) -> bool {
        !self.sinks.is_empty()
    }

    pub fn build(&self, session_id: Option<&str>, event: AuditEvent, attrs: Attrs) -> AuditRecord {
        let mut map = attrs.plain;
        if self.ctx.include_content {
            map.extend(attrs.content);
        }
        map.retain(|k, _| !secret_key(k));
        AuditRecord {
            audit_schema: AUDIT_SCHEMA,
            ts: rfc3339_millis((self.clock)()),
            install_id: self.ctx.install_id.clone(),
            user: self.ctx.user.clone(),
            org: self.ctx.org.clone(),
            session_id: session_id.map(str::to_string),
            event: event.as_str().to_string(),
            attrs: map,
        }
    }

    pub fn record(&self, session_id: Option<&str>, event: AuditEvent, attrs: Attrs) {
        if self.sinks.is_empty() {
            return;
        }
        let rec = self.build(session_id, event, attrs);
        for s in &self.sinks {
            if let Err(e) = s.write(&rec) {
                tracing::warn!(error = %e, event = rec.event, "audit sink write failed");
            }
        }
    }
}

/// Local rotating JSONL: `<dir>/audit.jsonl`, `audit.jsonl.1` …; files 0600, dir 0700.
pub struct JsonlSink {
    inner: Mutex<JsonlInner>,
}

struct JsonlInner {
    dir: PathBuf,
    keep: usize,
    max_bytes: u64,
    file: Option<File>,
    written: u64,
}

impl JsonlSink {
    /// `dir` is `<data dir>/audit/` or the admin folder from `audit.jsonl.dir`.
    pub fn open(dir: &Path, keep: usize, max_bytes: u64) -> io::Result<JsonlSink> {
        fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        }
        let mut inner = JsonlInner {
            dir: dir.to_path_buf(),
            keep: keep.max(1),
            max_bytes,
            file: None,
            written: 0,
        };
        let f = inner.open_current(false)?;
        inner.written = f.metadata()?.len();
        inner.file = Some(f);
        Ok(JsonlSink {
            inner: Mutex::new(inner),
        })
    }

    pub fn path(dir: &Path, idx: usize) -> PathBuf {
        if idx == 0 {
            dir.join("audit.jsonl")
        } else {
            dir.join(format!("audit.jsonl.{idx}"))
        }
    }
}

impl JsonlInner {
    fn open_current(&self, truncate: bool) -> io::Result<File> {
        let mut o = OpenOptions::new();
        o.create(true);
        if truncate {
            o.write(true).truncate(true);
        } else {
            o.append(true);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        o.open(JsonlSink::path(&self.dir, 0))
    }

    fn rotate(&mut self) -> io::Result<()> {
        if let Some(mut f) = self.file.take() {
            f.flush()?;
        }
        let last = self.keep - 1;
        if last > 0 {
            let _ = fs::remove_file(JsonlSink::path(&self.dir, last));
            for i in (0..last).rev() {
                let from = JsonlSink::path(&self.dir, i);
                if from.exists() {
                    fs::rename(&from, JsonlSink::path(&self.dir, i + 1))?;
                }
            }
        }
        self.file = Some(self.open_current(true)?);
        self.written = 0;
        Ok(())
    }
}

impl AuditSink for JsonlSink {
    fn write(&self, record: &AuditRecord) -> io::Result<()> {
        let mut line = serde_json::to_vec(record).map_err(io::Error::other)?;
        line.push(b'\n');
        let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if g.written > 0 && g.written + line.len() as u64 > g.max_bytes {
            g.rotate()?;
        }
        if g.file.is_none() {
            let f = g.open_current(false)?;
            g.file = Some(f);
        }
        let f = g
            .file
            .as_mut()
            .ok_or_else(|| io::Error::other("audit file unavailable"))?;
        f.write_all(&line)?;
        f.flush()?;
        g.written += line.len() as u64;
        Ok(())
    }
}

/// `2026-11-17T09:30:00.123Z`.
pub fn rfc3339_millis(t: SystemTime) -> String {
    let d = t.duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs() as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        d.subsec_millis()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    fn clock() -> SystemTime {
        UNIX_EPOCH + Duration::from_millis(1_795_000_000_250)
    }

    fn ctx(include_content: bool) -> AuditContext {
        AuditContext {
            install_id: "inst-1".into(),
            user: "alice".into(),
            org: Some("Synergech".into()),
            include_content,
        }
    }

    #[test]
    fn content_and_secrets_are_excluded_by_default() {
        let attrs = || {
            Attrs::new()
                .with("tool", "shell")
                .with("outcome", "ok")
                .with("duration_ms", 12)
                .with("api_key", "sk-123")
                .content("arguments", "rm -rf /")
        };
        let a = Auditor::new(ctx(false)).with_clock(clock);
        let r = a.build(Some("s1"), AuditEvent::ToolCalled, attrs());
        assert_eq!(r.ts, "2026-11-18T11:06:40.250Z");
        assert_eq!(r.event, "tool.called");
        assert_eq!(r.attrs.len(), 3);
        assert!(!r.attrs.contains_key("arguments") && !r.attrs.contains_key("api_key"));

        let a = Auditor::new(ctx(true)).with_clock(clock);
        let r = a.build(None, AuditEvent::ToolCalled, attrs());
        assert_eq!(r.attrs["arguments"], "rm -rf /");
        assert!(
            !r.attrs.contains_key("api_key"),
            "secrets never, even with content"
        );
        assert!(secret_key("refresh_token") && secret_key("tokens") && !secret_key("input_tokens"));
    }

    #[test]
    fn jsonl_sink_writes_rotates_and_is_private() {
        let dir = tempfile::tempdir().unwrap();
        let audit_dir = dir.path().join("audit");
        let sink = Arc::new(JsonlSink::open(&audit_dir, 3, 400).unwrap());
        let a = Auditor::new(ctx(false)).with_clock(clock).with_sink(sink);
        for i in 0..10 {
            a.record(
                Some("s1"),
                AuditEvent::PermissionDecided,
                Attrs::new().with("decision", "once").with("n", i),
            );
        }
        let cur = fs::read_to_string(JsonlSink::path(&audit_dir, 0)).unwrap();
        let first: AuditRecord = serde_json::from_str(cur.lines().next().unwrap()).unwrap();
        assert_eq!(first.audit_schema, 1);
        assert_eq!(first.user, "alice");
        assert_eq!(first.org.as_deref(), Some("Synergech"));
        assert!(JsonlSink::path(&audit_dir, 1).exists());
        assert!(JsonlSink::path(&audit_dir, 2).exists());
        assert!(
            !JsonlSink::path(&audit_dir, 3).exists(),
            "size-capped to 3 files"
        );
        for i in 0..3 {
            assert!(fs::metadata(JsonlSink::path(&audit_dir, i)).unwrap().len() <= 400);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let m = fs::metadata(JsonlSink::path(&audit_dir, 0))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(m & 0o777, 0o600);
            let d = fs::metadata(&audit_dir).unwrap().permissions().mode();
            assert_eq!(d & 0o777, 0o700);
        }
    }
}
