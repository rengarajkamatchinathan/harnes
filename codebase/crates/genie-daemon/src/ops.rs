//! `doctor.run` and `diagnostics.collect` (02 §6.1, 01 §8, D34).
//!
//! **Doctor** checks (01 §8): daemon + versions, keychain backend, DB open, providers,
//! catalog age, Chrome/Edge, ripgrep, git, WebKitGTK + DMABUF (Linux), disk space, plus
//! managed policy, settings problems, audit export and telemetry state. Checks never send
//! anything over the network.
//!
//! **Diagnostics** is a plain-text support bundle: versions, OS, daemon state, providers
//! (status and endpoint host only), catalog, policy keys with their sources (no values),
//! settings problems, MCP server states, audit/telemetry state, the doctor checks and
//! optionally the log tail. No conversation content, credentials or settings values; the
//! whole text passes the on-device scrubber (home paths → `~`, secrets, e-mails).

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use genie_audit::Scrubber;
use genie_protocol::api::daemon::{
    CheckStatus, DiagnosticsParams, DiagnosticsResult, DoctorCheck, DoctorResult,
};
use genie_protocol::api::provider::{CatalogSource, ProviderStatus};
use genie_protocol::methods::{self as m, Method};
use genie_protocol::{Request, Response, RpcError};
use serde_json::Value;

use crate::server::{Conn, DaemonState};
use crate::services::{Handled, Services};

/// Free space below this warns.
const DISK_WARN_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Free space below this fails.
const DISK_FAIL_BYTES: u64 = 200 * 1024 * 1024;
/// Catalog older than this warns.
const CATALOG_STALE_DAYS: u64 = 14;
/// Log lines included with `include_logs`.
const LOG_TAIL_LINES: usize = 300;

pub(crate) fn dispatch(
    svc: &Arc<Services>,
    state: &Arc<DaemonState>,
    conn: &Arc<Conn>,
    r: &Request,
) -> Option<Handled> {
    let name = r.method.as_str();
    if name != m::DoctorRun::NAME && name != m::DiagnosticsCollect::NAME {
        return None;
    }
    let (svc, st, c, r) = (svc.clone(), state.clone(), conn.clone(), r.clone());
    conn.scope().child("ops").spawn_cancellable(async move {
        let res: Result<Value, RpcError> = if r.method == m::DoctorRun::NAME {
            let checks = run_checks(&svc, &st).await;
            if let Some(t) = &svc.telemetry {
                t.usage(genie_audit::UsageEvent::new(
                    genie_audit::UsageName::DoctorRun,
                ));
            }
            serde_json::to_value(DoctorResult { checks })
                .map_err(|e| RpcError::internal(e.to_string()))
        } else {
            let p: DiagnosticsParams = if r.params.is_null() {
                DiagnosticsParams {
                    include_logs: false,
                }
            } else {
                match serde_json::from_value(r.params.clone()) {
                    Ok(p) => p,
                    Err(e) => {
                        c.respond(Response::err(r.id, RpcError::invalid_params(e.to_string())));
                        return;
                    }
                }
            };
            let text = collect(&svc, &st, p.include_logs).await;
            serde_json::to_value(DiagnosticsResult { text })
                .map_err(|e| RpcError::internal(e.to_string()))
        };
        c.respond(match res {
            Ok(v) => Response::ok(r.id, v),
            Err(e) => Response::err(r.id, e),
        });
    });
    Some(Handled::Later)
}

fn check(id: &str, status: CheckStatus, detail: impl Into<String>) -> DoctorCheck {
    DoctorCheck {
        id: id.to_string(),
        status,
        detail: detail.into(),
        fix_link: None,
    }
}

fn fix(mut c: DoctorCheck, link: &str) -> DoctorCheck {
    if c.status != CheckStatus::Ok {
        c.fix_link = Some(link.to_string());
    }
    c
}

/// First line of `<tool> --version`, or `None` when it can't run.
async fn tool_version(tool: &str) -> Option<String> {
    let out = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new(tool)
            .arg("--version")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_string(),
    )
}

fn human_bytes(n: u64) -> String {
    const G: f64 = 1024.0 * 1024.0 * 1024.0;
    if n as f64 >= G {
        format!("{:.1} GiB", n as f64 / G)
    } else {
        format!("{} MiB", n / (1024 * 1024))
    }
}

/// Days since an RFC 3339 timestamp (`None` if unparsable).
fn age_days(ts: &str) -> Option<u64> {
    let nanos = genie_audit::otlp::rfc3339_nanos(ts);
    if nanos == 0 {
        return None;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some((now.saturating_sub(nanos) / 86_400_000_000_000) as u64)
}

pub(crate) fn disk_check(dir: &Path) -> DoctorCheck {
    match genie_audit::sys::disk_free(dir) {
        Ok(free) if free < DISK_FAIL_BYTES => check(
            "disk",
            CheckStatus::Fail,
            format!("only {} free for Genie data", human_bytes(free)),
        ),
        Ok(free) if free < DISK_WARN_BYTES => check(
            "disk",
            CheckStatus::Warn,
            format!("{} free for Genie data", human_bytes(free)),
        ),
        Ok(free) => check(
            "disk",
            CheckStatus::Ok,
            format!("{} free", human_bytes(free)),
        ),
        Err(e) => check(
            "disk",
            CheckStatus::Unknown,
            format!("can't read free space: {e}"),
        ),
    }
}

#[cfg(target_os = "linux")]
async fn webkit_checks(out: &mut Vec<DoctorCheck>) {
    // WebKitGTK is the desktop app's webview (spec 05); the CLI doesn't need it.
    let lib = tokio::process::Command::new("ldconfig")
        .arg("-p")
        .output()
        .await
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    let found = lib
        .lines()
        .find(|l| l.contains("libwebkit2gtk-4.1.so"))
        .map(|l| l.trim().to_string());
    out.push(match found {
        Some(_) => check("webkitgtk", CheckStatus::Ok, "libwebkit2gtk-4.1 found"),
        None => check(
            "webkitgtk",
            CheckStatus::Warn,
            "libwebkit2gtk-4.1 not found (needed by the desktop app only)",
        ),
    });
    let dmabuf = std::env::var("WEBKIT_DISABLE_DMABUF_RENDERER").ok();
    out.push(check(
        "webkit_dmabuf",
        CheckStatus::Ok,
        match dmabuf.as_deref() {
            Some("1") => "DMABUF renderer disabled (workaround active)".to_string(),
            _ => {
                "DMABUF renderer enabled; the desktop app disables it on affected GPUs".to_string()
            }
        },
    ));
}

/// The 01 §8 checks plus policy/config/audit/telemetry state.
pub(crate) async fn run_checks(svc: &Services, st: &DaemonState) -> Vec<DoctorCheck> {
    let mut out = Vec::new();
    let status = st.status();
    let cfg = st.config();
    out.push(check(
        "daemon",
        CheckStatus::Ok,
        format!(
            "engine {} ({}, protocol {}) pid {}, up {} s, {} client(s)",
            status.version,
            cfg.build,
            genie_protocol::version::PROTOCOL_VERSION,
            status.pid,
            status.uptime_s,
            status.clients.len()
        ),
    ));
    let mut skew: Vec<String> = status
        .clients
        .iter()
        .filter(|c| c.degraded || c.client_version != status.version)
        .map(|c| format!("{:?} {}", c.client_kind, c.client_version))
        .collect();
    skew.sort();
    skew.dedup();
    out.push(if skew.is_empty() {
        check(
            "versions",
            CheckStatus::Ok,
            "clients match the engine version",
        )
    } else {
        check(
            "versions",
            CheckStatus::Warn,
            format!("clients on other versions: {}", skew.join(", ")),
        )
    });

    // Keychain / secrets backend.
    out.push(fix(
        match &svc.secrets {
            Some(s) => {
                let kind = format!("{:?}", s.kind()).to_lowercase();
                if s.is_locked() {
                    check(
                        "keychain",
                        CheckStatus::Warn,
                        format!("{kind} backend is locked (passphrase required)"),
                    )
                } else if kind == "memory" {
                    check(
                        "keychain",
                        CheckStatus::Warn,
                        "in-memory secrets (not persisted)",
                    )
                } else {
                    check("keychain", CheckStatus::Ok, format!("{kind} backend"))
                }
            }
            None => check("keychain", CheckStatus::Unknown, "secrets not initialised"),
        },
        "genie://settings/privacy",
    ));

    // Database.
    let locked = svc.secrets.as_ref().is_some_and(|s| s.is_locked());
    out.push(if svc.store.is_some() {
        check("database", CheckStatus::Ok, "encrypted store open")
    } else if locked {
        check(
            "database",
            CheckStatus::Warn,
            "waiting for the secrets passphrase",
        )
    } else {
        check(
            "database",
            CheckStatus::Fail,
            "store unavailable; sessions are kept in memory only",
        )
    });

    // Providers and catalog.
    if let Some(p) = svc.providers.as_ref().filter(|_| !locked) {
        let list = p.list().providers;
        let ready: Vec<&str> = list
            .iter()
            .filter(|i| i.status == ProviderStatus::Ready)
            .map(|i| i.id.as_str())
            .collect();
        let errors: Vec<&str> = list
            .iter()
            .filter(|i| i.status == ProviderStatus::Error)
            .map(|i| i.id.as_str())
            .collect();
        out.push(fix(
            if !errors.is_empty() {
                check(
                    "providers",
                    CheckStatus::Warn,
                    format!("errors: {}; ready: {}", errors.join(", "), ready.join(", ")),
                )
            } else if ready.is_empty() {
                check("providers", CheckStatus::Warn, "no provider is signed in")
            } else {
                check(
                    "providers",
                    CheckStatus::Ok,
                    format!("ready: {}", ready.join(", ")),
                )
            },
            "genie://settings/providers",
        ));
        let cs = p.catalog_status();
        let age = age_days(&cs.fetched_at);
        out.push(match (cs.source, age) {
            (CatalogSource::Bundled, _) => check(
                "catalog",
                CheckStatus::Warn,
                "using the bundled model catalog (never refreshed)",
            ),
            (_, Some(d)) if d > CATALOG_STALE_DAYS => check(
                "catalog",
                CheckStatus::Warn,
                format!("catalog is {d} days old"),
            ),
            (src, d) => check(
                "catalog",
                CheckStatus::Ok,
                format!(
                    "{} catalog, {} day(s) old",
                    format!("{src:?}").to_lowercase(),
                    d.unwrap_or(0)
                ),
            ),
        });
    }

    // Chrome / Edge for the agent browser.
    let browsers = genie_browser::discover::find_installed().await;
    let usable: Vec<String> = browsers
        .iter()
        .filter(|b| genie_browser::discover::supported(&b.version))
        .map(|b| format!("{:?} {}", b.kind, b.version))
        .collect();
    out.push(if !usable.is_empty() {
        check("browser", CheckStatus::Ok, usable.join(", "))
    } else if !browsers.is_empty() {
        check(
            "browser",
            CheckStatus::Warn,
            format!(
                "Chrome/Edge too old (need {}+)",
                genie_browser::discover::MIN_MAJOR
            ),
        )
    } else {
        check(
            "browser",
            CheckStatus::Warn,
            "Chrome or Edge not found (needed for the agent browser)",
        )
    });

    // ripgrep and git.
    for (id, tool, why) in [
        ("ripgrep", "rg", "needed for fast search in Code"),
        ("git", "git", "needed for Code checkpoints and worktrees"),
    ] {
        out.push(match tool_version(tool).await {
            Some(v) => check(id, CheckStatus::Ok, v),
            None => check(
                id,
                CheckStatus::Warn,
                format!("`{tool}` not found on PATH ({why})"),
            ),
        });
    }

    #[cfg(target_os = "linux")]
    webkit_checks(&mut out).await;

    out.push(disk_check(&cfg.endpoint.data_dir));

    // Managed policy.
    if let Some(hub) = &svc.hub {
        let problems = svc.policy_problems();
        let eff = hub.policy();
        out.push(if !problems.is_empty() {
            check(
                "policy",
                CheckStatus::Warn,
                problems
                    .iter()
                    .map(|p| p.message.clone())
                    .collect::<Vec<_>>()
                    .join("; "),
            )
        } else if eff.is_empty() {
            check("policy", CheckStatus::Ok, "no managed policy")
        } else {
            check(
                "policy",
                CheckStatus::Ok,
                format!(
                    "managed by {} ({} keys)",
                    eff.org_name().unwrap_or("your organization"),
                    eff.to_result().values.len()
                ),
            )
        });
        let diags = hub.diagnostics(None, Vec::new()).problems;
        let errors = diags
            .iter()
            .filter(|d| d.severity == genie_protocol::common::Severity::Error)
            .count();
        out.push(fix(
            if diags.is_empty() {
                check("settings", CheckStatus::Ok, "no problems")
            } else {
                check(
                    "settings",
                    if errors > 0 {
                        CheckStatus::Warn
                    } else {
                        CheckStatus::Ok
                    },
                    format!(
                        "{} problem(s), {errors} error(s); see `genie config doctor`",
                        diags.len()
                    ),
                )
            },
            "genie://settings/developer",
        ));
    }

    // Audit and telemetry.
    let audit = match (&svc.audit, &svc.otlp) {
        (None, _) => check("audit", CheckStatus::Ok, "audit log off"),
        (Some(_), Some((sink, _))) if sink.dropped() > 0 => check(
            "audit",
            CheckStatus::Warn,
            format!(
                "OTLP export behind: {} queued, {} dropped",
                sink.len(),
                sink.dropped()
            ),
        ),
        (Some(_), Some((sink, _))) => check(
            "audit",
            CheckStatus::Ok,
            format!("local JSONL + OTLP ({} queued)", sink.len()),
        ),
        (Some(_), None) => check("audit", CheckStatus::Ok, "local JSONL"),
    };
    out.push(audit);
    if let Some(t) = &svc.telemetry {
        let c = t.consent();
        let dsn = crate::observe::telemetry_dsn(svc.hub.as_deref()).is_some();
        out.push(check(
            "telemetry",
            CheckStatus::Ok,
            format!(
                "crash reports {}, usage analytics {}, {} queued{}",
                if c.crash_reports { "on" } else { "off" },
                if c.usage { "on" } else { "off" },
                t.queued(),
                if dsn {
                    ""
                } else {
                    " (no endpoint configured; kept on device)"
                }
            ),
        ));
    }
    out
}

fn tail(path: &Path, n: usize) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let lines: Vec<&str> = text.lines().collect();
    Some(lines[lines.len().saturating_sub(n)..].join("\n"))
}

fn host_of(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| "(custom)".into())
}

/// The sanitised support text.
pub(crate) async fn collect(svc: &Services, st: &DaemonState, include_logs: bool) -> String {
    let mut s = String::new();
    let mut line = |l: String| {
        s.push_str(&l);
        s.push('\n');
    };
    let status = st.status();
    let cfg = st.config();
    line(
        "Genie diagnostics (sanitised: no conversation content, credentials or settings values)"
            .into(),
    );
    line(format!(
        "generated: {}",
        genie_audit::audit::rfc3339_millis(std::time::SystemTime::now())
    ));
    line(format!(
        "engine: {} build {} channel {} protocol {}",
        status.version,
        cfg.build,
        status.channel.as_str(),
        genie_protocol::version::PROTOCOL_VERSION
    ));
    line(format!(
        "os: {} {} ; pid {} ; uptime {} s ; sessions running {}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        status.pid,
        status.uptime_s,
        status.sessions_running
    ));
    line(format!("endpoint: {}", status.socket));
    line(format!("data dir: {}", cfg.endpoint.data_dir.display()));
    for c in &status.clients {
        line(format!(
            "client: {:?} {}{}",
            c.client_kind,
            c.client_version,
            if c.degraded { " (degraded)" } else { "" }
        ));
    }
    if let Some(p) = &svc.providers
        && !svc.secrets.as_ref().is_some_and(|s| s.is_locked())
    {
        line("providers:".into());
        for i in p.list().providers {
            line(format!(
                "  {} ({}): {:?}, {} credential(s){}",
                i.id,
                i.kind,
                i.status,
                i.credentials.len(),
                i.endpoint
                    .as_deref()
                    .map(|e| format!(", endpoint host {}", host_of(e)))
                    .unwrap_or_default()
            ));
        }
        let cs = p.catalog_status();
        line(format!(
            "catalog: {:?} fetched {}",
            cs.source,
            if cs.fetched_at.is_empty() {
                "never"
            } else {
                cs.fetched_at.as_str()
            }
        ));
    }
    if let Some(hub) = &svc.hub {
        let eff = hub.policy().to_result();
        line(format!("policy: {} key(s)", eff.values.len()));
        for (k, src) in &eff.source_per_key {
            line(format!("  {k} ← {src}"));
        }
        for p in svc.policy_problems() {
            line(format!("  problem: {}", p.message));
        }
        let diags = hub.diagnostics(None, Vec::new()).problems;
        line(format!("settings problems: {}", diags.len()));
        for d in diags.iter().take(50) {
            line(format!(
                "  {:?} {}: {}",
                d.severity,
                d.key.as_deref().unwrap_or("-"),
                d.message
            ));
        }
    }
    if let Some(mcp) = &svc.mcp {
        let servers = mcp.list(None);
        line(format!("mcp servers: {}", servers.len()));
        for sv in servers {
            line(format!(
                "  {}: {:?} ({} tools){}",
                sv.name,
                sv.status,
                sv.tools,
                sv.error.map(|e| format!(" error: {e}")).unwrap_or_default()
            ));
        }
    }
    line("doctor:".into());
    for c in run_checks(svc, st).await {
        line(format!("  {:<14} {:?}: {}", c.id, c.status, c.detail));
    }
    if include_logs {
        let path = cfg.endpoint.logs_dir().join("daemon.log");
        line(format!("--- last {LOG_TAIL_LINES} lines of daemon.log ---"));
        line(tail(&path, LOG_TAIL_LINES).unwrap_or_else(|| "(no log file)".into()));
    }
    Scrubber::from_env().scrub(&s)
}
