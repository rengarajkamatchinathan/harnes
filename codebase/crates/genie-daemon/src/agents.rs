//! Daemon glue for agents, subagents and hooks (06 §11–12): child sessions in the
//! session registry, `session.interrupt {stop_subagents}`, `session.end` hooks.

use std::sync::Arc;

use genie_core::TaskScope;
use genie_core::runtime::subagents::ChildRegistrar;
use genie_core::runtime::{AgentRuntime, SessionSettings, SettingsSource};
use genie_protocol::api::session::CreateParams;

use crate::registry::{Extra, Registry};

/// Subagent sessions are listed like any session (with `parent_session_id`), and
/// permissions, the shell and MCP see their settings through the registry.
impl ChildRegistrar for Registry {
    fn register(&self, child_id: &str, parent_id: &str, s: &SessionSettings) {
        let p = CreateParams {
            kind: s.kind,
            project_id: s.project_id.clone(),
            model: s.model.clone(),
            effort: s.effort.clone(),
            agent: s.agent.clone(),
            mode: Some(s.mode),
            incognito: s.incognito,
            ..Default::default()
        };
        let extra = Extra {
            workspace: s.workspace.clone(),
            overlay: s.overlay.clone(),
            worktree_name: None,
            headless: s.headless,
        };
        self.create_with(child_id, &p, Some(parent_id.to_string()), extra);
        if let Some(t) = &s.title {
            self.set_title(child_id, t);
        }
    }

    fn unregister(&self, child_id: &str) {
        self.remove(child_id);
    }
}

/// `session.interrupt {stop_subagents: true}`: stop the session's running children too.
pub fn stop_subagents(rt: Option<&Arc<AgentRuntime>>, session_id: &str) -> usize {
    // Stop also ends the session's background shell jobs (06 §13.1).
    if let Some(sh) = rt.and_then(|r| r.shell.as_ref()) {
        let n = sh.kill_session_jobs(session_id);
        if n > 0 {
            tracing::info!(session_id, jobs = n, "background jobs killed on stop");
        }
    }
    rt.and_then(|r| r.subagents.as_ref())
        .map_or(0, |h| h.stop_children(session_id))
}

/// `session.delete`: run `session.end` hooks (read-only; recorded in the audit log) with
/// the settings captured before the session was removed.
pub fn session_end(
    rt: Option<&Arc<AgentRuntime>>,
    session_id: &str,
    settings: Option<SessionSettings>,
    scope: &TaskScope,
) {
    let (Some(rt), Some(s)) = (rt, settings) else {
        return;
    };
    if let Some(h) = &rt.hooks {
        h.session_end(session_id, &s, scope);
    }
}
