//! Settings → Privacy / Permissions methods: `trust.list` (08 §4), saved permission
//! rules per scope (`permissions.rules.list/remove`, 06 §5.4) and "Delete all data"
//! (`data.delete_all.begin` + `data.delete_all`, 04 §8).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use genie_audit::{Attrs, AuditEvent};
use genie_config::LayerKind;
use genie_core::config::project_root;
use genie_protocol::api::config::{
    DeleteAllBeginResult, DeleteAllResult, PermissionRule, PermissionRuleClass,
    PermissionRuleScope, PermissionRulesListResult, TrustDecision, TrustListResult, TrustedFolder,
};
use genie_protocol::common::{ConfigScope, Empty, SessionKind};
use genie_protocol::event::MemoryChanged;
use genie_protocol::methods::{self as m, Method};
use genie_protocol::{ErrorKind, Request, RpcError};
use serde::Serialize;
use serde_json::{Value, json};

use crate::server::DaemonState;
use crate::services::{Services, canonical, config_err, not_implemented};

/// How long a `data.delete_all.begin` token stays valid.
pub const DELETE_TOKEN_TTL: Duration = Duration::from_secs(5 * 60);

fn to_result<T: Serialize>(v: &T) -> Result<Value, RpcError> {
    serde_json::to_value(v).map_err(|e| RpcError::internal(e.to_string()))
}

fn params<M: Method>(r: &Request) -> Result<M::Params, RpcError> {
    let v = if r.params.is_null() {
        json!({})
    } else {
        r.params.clone()
    };
    serde_json::from_value(v).map_err(|e| RpcError::invalid_params(e.to_string()))
}

pub(crate) fn dispatch(
    svc: &Arc<Services>,
    state: &Arc<DaemonState>,
    r: &Request,
) -> Option<Result<Value, RpcError>> {
    let name = r.method.as_str();
    let res = match name {
        n if n == m::TrustList::NAME => trust_list(svc),
        n if n == m::PermissionRulesList::NAME => {
            params::<m::PermissionRulesList>(r).and_then(|p| rules_list(svc, state, &p))
        }
        n if n == m::PermissionRulesRemove::NAME => {
            params::<m::PermissionRulesRemove>(r).and_then(|p| rules_remove(svc, state, &p))
        }
        n if n == m::DataDeleteAllBegin::NAME => delete_begin(svc, state),
        n if n == m::DataDeleteAll::NAME => {
            params::<m::DataDeleteAll>(r).and_then(|p| delete_all(svc, state, &p.token))
        }
        _ => return None,
    };
    Some(res)
}

// ---------------------------------------------------------------- trust

fn trust_list(svc: &Services) -> Result<Value, RpcError> {
    let hub = svc
        .hub
        .clone()
        .ok_or_else(|| not_implemented(m::TrustList::NAME))?;
    let items = hub
        .trust_list()
        .into_iter()
        .map(|e| TrustedFolder {
            path: e.canonical_path,
            decision: match e.decision {
                genie_config::trust::StoredDecision::Trust => TrustDecision::Trust,
                genie_config::trust::StoredDecision::Deny => TrustDecision::Deny,
            },
            identity: e.identity,
            decided_at: e.decided_at,
            approvals: u32::try_from(e.approvals.len()).unwrap_or(u32::MAX),
        })
        .collect();
    to_result(&TrustListResult { items })
}

// ---------------------------------------------------------------- permission rules

/// The project root whose layers apply: `cwd`, else the session's Code workspace.
fn rules_root(state: &DaemonState, cwd: Option<&str>, session: Option<&str>) -> Option<PathBuf> {
    if let Some(c) = cwd.filter(|c| !c.is_empty()) {
        return Some(project_root(Path::new(&canonical(c))));
    }
    let sid = session?;
    let info = state.registry().get(sid)?;
    if info.summary.kind != SessionKind::Code {
        return None;
    }
    state
        .registry()
        .extra(sid)
        .workspace
        .as_deref()
        .map(project_root)
}

fn rules_list(
    svc: &Services,
    state: &DaemonState,
    p: &genie_protocol::api::config::PermissionRulesListParams,
) -> Result<Value, RpcError> {
    let hub = svc
        .hub
        .clone()
        .ok_or_else(|| not_implemented(m::PermissionRulesList::NAME))?;
    let root = rules_root(state, p.cwd.as_deref(), p.session_id.as_deref());
    let mut rules = Vec::new();
    let mut push = |rule: &str, class, scope, source: Option<String>, locked| {
        rules.push(PermissionRule {
            rule: rule.to_string(),
            class,
            scope,
            source,
            locked,
        });
    };
    for (layer, path, perms) in hub.permission_rule_files(root.as_deref()) {
        let (scope, locked) = match layer {
            LayerKind::User => (PermissionRuleScope::User, false),
            LayerKind::Project => (PermissionRuleScope::Project, false),
            LayerKind::Local => (PermissionRuleScope::Local, false),
            LayerKind::Cli => (PermissionRuleScope::Cli, true),
            LayerKind::Policy => (PermissionRuleScope::Managed, true),
            LayerKind::Default => continue,
        };
        let source = path.map(|p| p.display().to_string());
        for (class, list) in [
            (PermissionRuleClass::Allow, &perms.allow),
            (PermissionRuleClass::Ask, &perms.ask),
            (PermissionRuleClass::Deny, &perms.deny),
        ] {
            for r in list.iter().flatten() {
                push(r, class, scope, source.clone(), locked);
            }
        }
    }
    let policy = hub.policy();
    for (class, key) in [
        (PermissionRuleClass::Deny, "permissions.deny"),
        (PermissionRuleClass::Ask, "permissions.ask"),
    ] {
        for r in policy.strings(key) {
            push(
                &r,
                class,
                PermissionRuleScope::Managed,
                Some("policy".into()),
                true,
            );
        }
    }
    if let Some(sid) = p.session_id.as_deref() {
        if let Some(a) = &svc.authz {
            for r in a.session_rules(sid) {
                push(
                    &r,
                    PermissionRuleClass::Allow,
                    PermissionRuleScope::Session,
                    Some("session".into()),
                    false,
                );
            }
        }
        if let Some(o) = state.registry().extra(sid).overlay {
            for (class, list) in [
                (PermissionRuleClass::Allow, &o.allow),
                (PermissionRuleClass::Ask, &o.ask),
                (PermissionRuleClass::Deny, &o.deny),
            ] {
                for r in list {
                    push(r, class, PermissionRuleScope::Cli, Some("cli".into()), true);
                }
            }
        }
    }
    if let Some(s) = p.scope {
        rules.retain(|r| r.scope == s);
    }
    to_result(&PermissionRulesListResult { rules })
}

fn rules_remove(
    svc: &Services,
    state: &DaemonState,
    p: &genie_protocol::api::config::PermissionRuleRemoveParams,
) -> Result<Value, RpcError> {
    if p.class == PermissionRuleClass::Unknown {
        return Err(RpcError::invalid_params("class must be allow, ask or deny"));
    }
    let not_there = || RpcError::new(ErrorKind::NotFound, "no such rule in that scope");
    let scope = match p.scope {
        PermissionRuleScope::Session => {
            let sid = p
                .session_id
                .as_deref()
                .ok_or_else(|| RpcError::invalid_params("session_id is required"))?;
            let authz = svc
                .authz
                .clone()
                .ok_or_else(|| not_implemented(m::PermissionRulesRemove::NAME))?;
            if p.class != PermissionRuleClass::Allow || !authz.forget_rule(Some(sid), &p.rule, true)
            {
                return Err(not_there());
            }
            return to_result(&Empty {});
        }
        PermissionRuleScope::User => ConfigScope::User,
        PermissionRuleScope::Project => ConfigScope::Project,
        PermissionRuleScope::Local => ConfigScope::Local,
        PermissionRuleScope::Managed | PermissionRuleScope::Cli => {
            return Err(RpcError::new(
                ErrorKind::Locked,
                "this rule is managed outside Genie's settings and cannot be removed here",
            ));
        }
        PermissionRuleScope::Unknown => {
            return Err(RpcError::invalid_params("unknown scope"));
        }
    };
    let hub = svc
        .hub
        .clone()
        .ok_or_else(|| not_implemented(m::PermissionRulesRemove::NAME))?;
    let root = rules_root(state, p.cwd.as_deref(), p.session_id.as_deref());
    let removed = hub
        .remove_permission_rule(root.as_deref(), scope, p.class.as_str(), &p.rule)
        .map_err(config_err)?;
    if !removed {
        return Err(not_there());
    }
    // An "always" saved earlier is also held in memory by the sessions that saved it.
    if p.class == PermissionRuleClass::Allow
        && let Some(a) = &svc.authz
    {
        a.forget_rule(None, &p.rule, false);
    }
    if let Some(a) = &svc.audit {
        a.record(
            None,
            AuditEvent::SettingsChanged,
            Attrs::new()
                .with("key", format!("permissions.{}", p.class.as_str()))
                .with("scope", json!(scope))
                .content("removed_rule", p.rule.clone()),
        );
    }
    to_result(&Empty {})
}

// ---------------------------------------------------------------- delete all data

/// A managed audit stream (08 §6) is the organisation's record: kept by "Delete all data".
fn audit_managed(svc: &Services) -> bool {
    svc.hub.as_ref().is_some_and(|h| {
        let p = h.policy();
        p.org_name().is_some()
            || p.include_content()
            || p.get("audit.jsonl.dir").is_some()
            || p.get("audit.otlp.endpoint").is_some()
    })
}

fn delete_begin(svc: &Services, state: &DaemonState) -> Result<Value, RpcError> {
    let store = svc
        .store
        .clone()
        .ok_or_else(|| not_implemented(m::DataDeleteAllBegin::NAME))?;
    let counts = store
        .data_counts()
        .map_err(|e| RpcError::internal(e.to_string()))?;
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).map_err(|e| RpcError::internal(e.to_string()))?;
    let token = hex::encode(b);
    *svc.delete_token.lock().unwrap_or_else(|e| e.into_inner()) =
        Some((token.clone(), Instant::now() + DELETE_TOKEN_TTL));
    let sessions = counts
        .sessions
        .max(u64::try_from(state.registry().count()).unwrap_or(u64::MAX));
    to_result(&DeleteAllBeginResult {
        token,
        expires_in_s: u32::try_from(DELETE_TOKEN_TTL.as_secs()).unwrap_or(u32::MAX),
        sessions,
        projects: counts.projects,
        memories: counts.memories,
        artifacts: counts.artifacts,
        blob_bytes: counts.blob_bytes,
        clears_audit: !audit_managed(svc),
    })
}

/// Truncate the current local audit file and remove the rotated ones.
fn clear_audit(dir: &Path) -> std::io::Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for i in 1..=genie_audit::audit::AUDIT_FILES {
        match std::fs::remove_file(genie_audit::JsonlSink::path(dir, i)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    let current = genie_audit::JsonlSink::path(dir, 0);
    if current.exists() {
        // The sink appends (O_APPEND), so it keeps writing at the new end.
        std::fs::OpenOptions::new()
            .write(true)
            .open(&current)?
            .set_len(0)?;
    }
    Ok(())
}

fn delete_all(
    svc: &Arc<Services>,
    state: &Arc<DaemonState>,
    token: &str,
) -> Result<Value, RpcError> {
    let store = svc
        .store
        .clone()
        .ok_or_else(|| not_implemented(m::DataDeleteAll::NAME))?;
    {
        let mut g = svc.delete_token.lock().unwrap_or_else(|e| e.into_inner());
        let ok = matches!(&*g, Some((t, exp)) if t == token && !token.is_empty() && Instant::now() < *exp);
        if !ok {
            return Err(RpcError::invalid_params(
                "confirmation token is invalid or expired; call data.delete_all.begin again",
            ));
        }
        *g = None;
    }
    let sessions_deleted = state.wipe_sessions();
    store.wipe_all_data().map_err(|e| {
        tracing::error!(error = %e, "delete all data failed");
        RpcError::internal("deleting the data failed; see the daemon log")
    })?;
    if let Some(m) = &svc.memory_ctl
        && let Err(e) = m.set_on(m.user_state_on())
    {
        tracing::warn!(error = %e, "memory state not re-saved");
    }
    let audit_cleared = !audit_managed(svc)
        && match clear_audit(&state.config().endpoint.data_dir.join("audit")) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(error = %e, "local audit log not cleared");
                false
            }
        };
    if let Some(a) = &svc.audit {
        a.record(
            None,
            AuditEvent::DataDeleted,
            Attrs::new()
                .with("sessions", sessions_deleted)
                .with("audit_cleared", audit_cleared),
        );
    }
    state.workspace_send(
        m::MemoryChangedNote::NAME,
        &MemoryChanged {
            memory_id: "*".into(),
            op: "delete".into(),
            scope: "all".into(),
        },
    );
    state.broadcast_snapshot();
    to_result(&DeleteAllResult {
        sessions_deleted,
        audit_cleared,
    })
}
