//! Settings and policy freshness (D23, D38; 08 §5.1) and the export loops (08 §6–7).
//!
//! - **Hot reload:** an OS file watcher ([`genie_config::PathWatcher`]) on every settings
//!   file plus the system policy file / macOS managed preferences; events are debounced
//!   ([`DEBOUNCE`], 100 ms) and then settings, local policy and telemetry consent are
//!   re-read (content-hash suppressed, so only real changes notify). A slow fallback
//!   ([`FALLBACK`]) re-reads anyway (registry policy, new project stores, network drives);
//!   without a watcher it falls back to the old 2 s poll.
//! - **Remote policy:** fetched at start and every [`REMOTE_INTERVAL`] (and when the
//!   `policyUrl` changes) through [`HttpPolicyFetcher`] — the `PolicyFetcher` port — off the
//!   async runtime; the last good copy is kept on failure.
//! - **OTLP audit** queue flushed every [`OTLP_INTERVAL`]; **telemetry** queue every
//!   [`TELEMETRY_INTERVAL`] when a DSN is configured.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use genie_config::policy::{SignedPolicy, system_policy_paths};
use genie_config::{PathWatcher, PolicyFetcher};
use genie_core::TaskScope;
use genie_core::config::ConfigHub;

use crate::services::Services;

pub const DEBOUNCE: Duration = Duration::from_millis(100);
pub const FALLBACK: Duration = Duration::from_secs(30);
const POLL_WITHOUT_WATCHER: Duration = Duration::from_secs(2);
pub const REMOTE_INTERVAL: Duration = Duration::from_secs(60 * 60);
pub const OTLP_INTERVAL: Duration = Duration::from_secs(5);
pub const TELEMETRY_INTERVAL: Duration = Duration::from_secs(60);
const SKILLS_INTERVAL: Duration = Duration::from_secs(10);
/// Largest remote policy document accepted.
const MAX_POLICY_BYTES: usize = 1024 * 1024;

/// `PolicyFetcher` over HTTPS. Call it off the async runtime (it blocks on `handle`).
pub struct HttpPolicyFetcher {
    pub client: reqwest::Client,
    pub handle: tokio::runtime::Handle,
}

/// `https`, or `http` to a loopback host (tests, local mirrors).
pub fn check_policy_url(url: &str) -> Result<reqwest::Url, String> {
    let u = reqwest::Url::parse(url).map_err(|e| format!("bad policyUrl: {e}"))?;
    let loopback = matches!(u.host_str(), Some("127.0.0.1" | "localhost" | "[::1]" | "::1"));
    match u.scheme() {
        "https" => Ok(u),
        "http" if loopback => Ok(u),
        s => Err(format!("policyUrl must be https (got {s})")),
    }
}

async fn get_text(client: &reqwest::Client, url: &str) -> Result<String, String> {
    let resp = client
        .get(url)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("{url} answered {}", resp.status()));
    }
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    if bytes.len() > MAX_POLICY_BYTES {
        return Err(format!("{url} is larger than {MAX_POLICY_BYTES} bytes"));
    }
    String::from_utf8(bytes.to_vec()).map_err(|_| format!("{url} is not UTF-8"))
}

/// The remote policy: either a `{body, signature}` JSON envelope, or the policy document
/// with a detached base64 signature at `<url>.sig` (like the catalog feed).
pub async fn fetch_signed(client: &reqwest::Client, url: &str) -> Result<SignedPolicy, String> {
    check_policy_url(url)?;
    let body = get_text(client, url).await?;
    if let Ok(sp) = serde_json::from_str::<SignedPolicy>(&body) {
        return Ok(sp);
    }
    let signature = get_text(client, &format!("{url}.sig")).await?;
    Ok(SignedPolicy {
        body,
        signature: signature.trim().to_string(),
    })
}

impl PolicyFetcher for HttpPolicyFetcher {
    fn fetch(&self, url: &str) -> Result<SignedPolicy, String> {
        self.handle.block_on(fetch_signed(&self.client, url))
    }
}

fn watch_list(hub: &ConfigHub) -> Vec<PathBuf> {
    let mut v = hub.watched_paths();
    v.extend(system_policy_paths());
    v
}

/// Re-read local policy; returns whether the remote URL changed.
fn reload_policy(svc: &Services, hub: &ConfigHub) -> bool {
    let Some(pm) = &svc.policy else {
        return false;
    };
    let (eff, url_before, url_after) = {
        let mut g = pm.lock().unwrap_or_else(|e| e.into_inner());
        let before = g.remote_url();
        g.refresh_local();
        (g.effective(), before, g.remote_url())
    };
    hub.set_policy(eff);
    url_before != url_after
}

fn apply_consent(svc: &Services, hub: &ConfigHub) {
    if let Some(t) = &svc.telemetry {
        let c = crate::observe::consent_from(hub);
        if c != t.consent() {
            t.set_consent(c);
        }
    }
}

/// Start the watcher, the remote policy loop and the export loops.
pub(crate) fn start(svc: &Arc<Services>, scope: &TaskScope, hub: Arc<ConfigHub>) {
    apply_consent(svc, &hub);
    let kick = Arc::new(tokio::sync::Notify::new());

    // Settings + local policy: watcher with debounce, plus a fallback re-read.
    {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let watcher = match PathWatcher::new(move || {
            let _ = tx.send(());
        }) {
            Ok(w) => {
                for (dir, e) in w.set_paths(&watch_list(&hub)) {
                    tracing::warn!(dir = %dir.display(), error = %e, "cannot watch settings dir");
                }
                Some(w)
            }
            Err(e) => {
                tracing::warn!(error = %e, "file watcher unavailable; polling settings");
                None
            }
        };
        let fallback = if watcher.is_some() {
            FALLBACK
        } else {
            POLL_WITHOUT_WATCHER
        };
        let me = svc.clone();
        let h = hub.clone();
        let k = kick.clone();
        scope.spawn_cancellable(async move {
            let mut fall = tokio::time::interval(fallback);
            let mut skills = tokio::time::interval(SKILLS_INTERVAL);
            fall.tick().await;
            skills.tick().await;
            loop {
                tokio::select! {
                    Some(()) = rx.recv() => {
                        // Debounce: wait until no event arrived for DEBOUNCE.
                        while let Ok(Some(())) = tokio::time::timeout(DEBOUNCE, rx.recv()).await {}
                    }
                    _ = fall.tick() => {}
                    _ = skills.tick() => {
                        if let Some(sk) = &me.skills {
                            sk.refresh();
                        }
                        continue;
                    }
                }
                h.refresh();
                if reload_policy(&me, &h) {
                    k.notify_one();
                }
                apply_consent(&me, &h);
                if let Some(w) = &watcher {
                    w.set_paths(&watch_list(&h));
                }
            }
        });
    }

    // Signed remote policy: at start, hourly, and when the URL changes.
    if let Some(pm) = &svc.policy {
        let fetcher = pm.lock().unwrap_or_else(|e| e.into_inner()).fetcher();
        if fetcher.is_some() {
            let me = svc.clone();
            let h = hub.clone();
            scope.spawn_cancellable(async move {
                loop {
                    let (url, fetcher) = match &me.policy {
                        Some(pm) => {
                            let g = pm.lock().unwrap_or_else(|e| e.into_inner());
                            (g.remote_url(), g.fetcher())
                        }
                        None => (None, None),
                    };
                    if let (Some(url), Some(f), Some(pm)) = (url, fetcher, &me.policy) {
                        let res = tokio::task::spawn_blocking(move || f.fetch(&url))
                            .await
                            .unwrap_or_else(|e| Err(e.to_string()));
                        if let Err(e) = &res {
                            tracing::warn!(error = %e, "remote policy fetch failed");
                        }
                        let eff = {
                            let mut g = pm.lock().unwrap_or_else(|e| e.into_inner());
                            g.apply_remote(res);
                            g.effective()
                        };
                        h.set_policy(eff);
                        apply_consent(&me, &h);
                    }
                    tokio::select! {
                        _ = tokio::time::sleep(REMOTE_INTERVAL) => {}
                        _ = kick.notified() => {}
                    }
                }
            });
        }
    }

    // OTLP audit export.
    if let Some((sink, transport)) = svc.otlp.clone() {
        scope.spawn_cancellable(async move {
            let mut last_err: Option<String> = None;
            loop {
                tokio::time::sleep(OTLP_INTERVAL).await;
                if sink.is_empty() {
                    continue;
                }
                match genie_audit::otlp::flush(&sink, transport.as_ref(), env!("CARGO_PKG_VERSION"))
                    .await
                {
                    Ok(_) => last_err = None,
                    Err(e) => {
                        if last_err.as_deref() != Some(e.as_str()) {
                            tracing::warn!(error = %e, queued = sink.len(), "audit OTLP export failed");
                        }
                        last_err = Some(e);
                    }
                }
            }
        });
    }

    // Telemetry export (crash reports from earlier runs go out first).
    if let Some(t) = svc.telemetry.clone() {
        let client = genie_providers::oauth::http_client().unwrap_or_default();
        let h = hub.clone();
        scope.spawn_cancellable(async move {
            loop {
                if let Some(dsn) = crate::observe::telemetry_dsn(Some(&h))
                    && t.queued() > 0
                {
                    let exp = crate::observe::SentryExporter {
                        client: client.clone(),
                        dsn,
                    };
                    if let Err(e) = t.flush(&exp).await {
                        tracing::debug!(error = %e, "telemetry export failed; will retry");
                    }
                }
                tokio::time::sleep(TELEMETRY_INTERVAL).await;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_url_rules() {
        assert!(check_policy_url("https://p.example/policy.json").is_ok());
        assert!(check_policy_url("http://127.0.0.1:8080/p").is_ok());
        assert!(check_policy_url("http://p.example/p").is_err());
        assert!(check_policy_url("file:///etc/passwd").is_err());
        assert!(check_policy_url("nonsense").is_err());
    }
}
