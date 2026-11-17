//! Opening the session a run works on: new, `--continue` or `--resume <id>` (07 §2.1).

use std::path::Path;

use anyhow::{Result, anyhow};
use genie_client::{ClientError, Connection};
use genie_protocol::ErrorKind;
use genie_protocol::api::session::{
    CreateParams, GetParams, ListParams, SessionIdParams, SessionSettingsOverlay, SubscribeParams,
    SubscribeResult, UpdateParams, WorktreeChoice,
};
use genie_protocol::common::{Mode, SessionKind};
use genie_protocol::message::SessionInfo;
use genie_protocol::methods as m;

use crate::errors;

/// Model id sent when neither `--model` nor `GENIE_MODEL` is set; the engine resolves it
/// to the configured default (D27).
pub const DEFAULT_MODEL: &str = "default";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    New,
    /// Most recent session of this kind.
    Continue,
    Resume(String),
}

#[derive(Debug, Clone)]
pub struct Spec {
    pub kind: SessionKind,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub agent: Option<String>,
    pub mode: Option<Mode>,
    /// `--worktree [name]` (`worktree_name` on the wire).
    pub worktree: Option<String>,
    /// Session-only settings: `--allow`/`--deny`, `--settings`, `--add-dir`.
    pub overlay: Option<SessionSettingsOverlay>,
}

impl Spec {
    pub fn code() -> Spec {
        Spec {
            kind: SessionKind::Code,
            model: None,
            effort: None,
            agent: None,
            mode: None,
            worktree: None,
            overlay: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Opened {
    pub session_id: String,
    pub info: SessionInfo,
    pub created: bool,
}

/// Create or find the session, then apply model/effort/mode overrides to an existing one.
pub async fn open(conn: &Connection, target: &Target, spec: &Spec, cwd: &Path) -> Result<Opened> {
    let existing = match target {
        Target::New => None,
        Target::Continue => conn
            .call::<m::SessionList>(ListParams {
                kind: Some(spec.kind),
                limit: Some(1),
                ..Default::default()
            })
            .await
            .map_err(|e| anyhow!("listing sessions: {}", errors::describe(&e)))?
            .items
            .into_iter()
            .next()
            .map(|s| s.session_id),
        Target::Resume(id) => Some(id.clone()),
    };
    let (session_id, created) = match existing {
        Some(id) => (id, false),
        None => {
            let res = conn
                .call::<m::SessionCreate>(CreateParams {
                    kind: spec.kind,
                    project_id: None,
                    workspace_path: Some(cwd.to_string_lossy().into_owned()),
                    model: spec
                        .model
                        .clone()
                        .unwrap_or_else(|| DEFAULT_MODEL.to_string()),
                    effort: spec.effort.clone(),
                    agent: spec.agent.clone(),
                    mode: spec.mode,
                    incognito: false,
                    worktree: spec.worktree.as_ref().map(|_| WorktreeChoice::New),
                    worktree_name: spec.worktree.clone().filter(|w| !w.is_empty()),
                    settings: spec.overlay.clone(),
                    ..CreateParams::default()
                })
                .await
                .map_err(|e| anyhow!("creating a session: {}", errors::describe(&e)))?;
            (res.session_id, true)
        }
    };
    let info = get(conn, &session_id).await?;
    if !created && (spec.model.is_some() || spec.effort.is_some() || spec.mode.is_some()) {
        let update = UpdateParams {
            session_id: session_id.clone(),
            model: spec.model.clone(),
            effort: spec.effort.clone(),
            mode: spec.mode,
            ..Default::default()
        };
        with_takeover(conn, &session_id, || {
            conn.call::<m::SessionUpdate>(update.clone())
        })
        .await
        .map_err(|e| anyhow!("updating session {session_id}: {}", errors::describe(&e)))?;
    }
    let info = if created {
        info
    } else {
        get(conn, &session_id).await?
    };
    Ok(Opened {
        session_id,
        info,
        created,
    })
}

/// Run a mutating call; when another client holds the lease (02 §5), take it over and
/// retry once. Used where the user explicitly chose this session (`--resume`,
/// `--continue`, `genie chat open`).
pub async fn with_takeover<T, F, Fut>(
    conn: &Connection,
    session_id: &str,
    f: F,
) -> Result<T, ClientError>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<T, ClientError>>,
{
    match f().await {
        Err(ClientError::Rpc(e)) if e.kind() == ErrorKind::NotDriver => {
            conn.call::<m::SessionTakeOver>(SessionIdParams {
                session_id: session_id.to_string(),
            })
            .await?;
            f().await
        }
        other => other,
    }
}

pub async fn get(conn: &Connection, session_id: &str) -> Result<SessionInfo> {
    Ok(conn
        .call::<m::SessionGet>(GetParams {
            session_id: session_id.to_string(),
            leaf_id: None,
        })
        .await
        .map_err(|e| anyhow!("session {session_id}: {}", errors::describe(&e)))?
        .session)
}

pub async fn subscribe(
    conn: &Connection,
    session_id: &str,
    after_seq: Option<u64>,
) -> Result<SubscribeResult> {
    conn.call::<m::SessionSubscribe>(SubscribeParams {
        session_id: session_id.to_string(),
        after_seq,
    })
    .await
    .map_err(|e| anyhow!("subscribing: {}", errors::describe(&e)))
}

/// A fresh idempotency key for `session.prompt`.
pub fn client_msg_id() -> String {
    format!("cm_{}", ulid::Ulid::new().to_string().to_lowercase())
}
