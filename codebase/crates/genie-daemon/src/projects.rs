//! Projects, artifacts, memory, blobs (02 §6.9) and `session.search` / `session.export`
//! (02 §6.2), served from the store and the runtime's hubs. None of these need the
//! driver lease (read-only or not session-bound, 02 §5).

use std::sync::Arc;

use genie_core::TaskScope;
use genie_core::config::ConfigHub;
use genie_core::runtime::artifacts::ArtifactHub;
use genie_core::runtime::attachments::Attachments;
use genie_core::runtime::memory_ctl::MemoryControl;
use genie_core::runtime::projects::{ProjectError, ProjectsConfig, ProjectsHub, embedder_from_env};
use genie_core::runtime::{ModelResolver, RuntimeBuilder};
use genie_protocol::api::project::{
    ArtifactGetResult, ArtifactListResult, ArtifactSummary, FileIdResult, MemoryItem,
    MemoryListResult, MemoryState, ProjectIdResult, ProjectInfo, ProjectListResult,
};
use genie_protocol::api::session::{
    ExportFormat, ExportResult, SearchHit, SearchResult, SearchScope,
};
use genie_protocol::common::Empty;
use genie_protocol::event::MemoryChanged;
use genie_protocol::methods::{self as m, Method};
use genie_protocol::{ErrorKind, Request, RpcError};
use serde::Serialize;
use serde_json::{Value, json};

use crate::services::{Services, not_implemented};

/// Exports larger than this are returned as a blob.
pub const EXPORT_INLINE_MAX: usize = 1024 * 1024;

/// Wire artifacts, memory state, projects/retrieval, attachments and chat search into the
/// runtime and the method table. Policy: `artifacts.enabled`, `memory.enabled`,
/// `chatSearch.enabled` set to `false` turn the feature off (D28, D31, D38).
pub(crate) fn wire(
    mut rb: RuntimeBuilder,
    services: &mut Services,
    store: Option<&Arc<genie_store::Store>>,
    models: &Arc<dyn ModelResolver>,
    root: &TaskScope,
    hub: Option<&ConfigHub>,
) -> RuntimeBuilder {
    let off = |key: &str| hub.is_some_and(|h| h.policy().bool(key) == Some(false));
    let memory = MemoryControl::new(store.cloned());
    memory.set_allowed(!off("memory.enabled"));
    let artifacts = ArtifactHub::new(store.cloned());
    artifacts.set_enabled(!off("artifacts.enabled"));
    rb = rb
        .memory_control(memory.clone())
        .artifacts(artifacts.clone());
    if let Some(store) = store {
        let projects = ProjectsHub::new(
            store.clone(),
            models.clone(),
            embedder_from_env(),
            root.child("indexing"),
            ProjectsConfig::default(),
        );
        rb = rb
            .projects(projects.clone())
            .attachments(Attachments::new(store.clone()));
        if !off("chatSearch.enabled") {
            rb = rb.chat_search(store.clone());
        }
        services.projects = Some(projects);
    }
    services.artifacts = Some(artifacts);
    services.memory_ctl = Some(memory);
    rb
}

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

fn store_err(e: genie_store::StoreError) -> RpcError {
    match e {
        genie_store::StoreError::NotFound(what) => {
            RpcError::new(ErrorKind::NotFound, format!("{what} not found"))
        }
        genie_store::StoreError::Invalid(msg) => RpcError::invalid_params(msg),
        other => {
            tracing::error!(error = %other, "store error");
            RpcError::new(ErrorKind::InternalError, "storage error")
        }
    }
}

fn project_err(e: ProjectError) -> RpcError {
    match e {
        ProjectError::NotFound(w) => RpcError::new(ErrorKind::NotFound, format!("{w} not found")),
        ProjectError::Invalid(m) => RpcError::invalid_params(m),
        ProjectError::Capacity(m) => {
            RpcError::invalid_params(m).with_data(json!({ "reason": "capacity" }))
        }
        ProjectError::Store(m) => {
            tracing::error!(error = %m, "project store error");
            RpcError::new(ErrorKind::InternalError, "storage error")
        }
    }
}

fn not_found(what: &str) -> RpcError {
    RpcError::new(ErrorKind::NotFound, format!("{what} not found"))
}

fn info(p: genie_store::projects::Project) -> ProjectInfo {
    ProjectInfo {
        project_id: p.id,
        name: p.name,
        instructions: p.instructions.unwrap_or_default(),
        starred: p.starred,
    }
}

fn memory_item(m: genie_store::memory::Memory) -> MemoryItem {
    MemoryItem {
        memory_id: m.id,
        scope: m.scope,
        text: m.text,
        updated_at: genie_protocol::rfc3339_ms(u64::try_from(m.updated).unwrap_or(0)),
    }
}

/// Handle the method if it is one of ours.
/// The scope of the memory a `memory.update/delete` names, read before the change.
pub(crate) fn memory_scope(svc: &Services, r: &Request) -> Option<String> {
    let name = r.method.as_str();
    if name != m::MemoryUpdate::NAME && name != m::MemoryDelete::NAME {
        return None;
    }
    let id = r.params.get("memory_id")?.as_str()?;
    let store = svc.store.as_ref()?;
    store
        .read(|c| genie_store::memory::get(c, id))
        .ok()
        .flatten()
        .map(|m| m.scope)
}

/// `memory.changed` on the workspace feed after a successful `memory.*` change (D31).
/// `memory.reset` reports `memory_id: "*"` with op `delete`.
pub(crate) fn memory_changed(
    state: &crate::server::DaemonState,
    r: &Request,
    scope_before: Option<String>,
) {
    let name = r.method.as_str();
    let id = || {
        r.params
            .get("memory_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let note = if name == m::MemoryUpdate::NAME {
        MemoryChanged {
            memory_id: id(),
            op: "update".into(),
            scope: scope_before.unwrap_or_default(),
        }
    } else if name == m::MemoryDelete::NAME {
        MemoryChanged {
            memory_id: id(),
            op: "delete".into(),
            scope: scope_before.unwrap_or_default(),
        }
    } else if name == m::MemoryReset::NAME {
        MemoryChanged {
            memory_id: "*".into(),
            op: "delete".into(),
            scope: r
                .params
                .get("scope")
                .and_then(Value::as_str)
                .unwrap_or("all")
                .to_string(),
        }
    } else {
        return;
    };
    state.workspace_send(m::MemoryChangedNote::NAME, &note);
}

pub(crate) fn dispatch(svc: &Services, r: &Request) -> Option<Result<Value, RpcError>> {
    let name = r.method.as_str();
    let known = [
        m::ProjectList::NAME,
        m::ProjectCreate::NAME,
        m::ProjectUpdate::NAME,
        m::ProjectDelete::NAME,
        m::ProjectFileAdd::NAME,
        m::ProjectFileRemove::NAME,
        m::ProjectIndexStatus::NAME,
        m::ArtifactList::NAME,
        m::ArtifactGet::NAME,
        m::MemoryList::NAME,
        m::MemoryUpdate::NAME,
        m::MemoryDelete::NAME,
        m::MemorySetState::NAME,
        m::MemoryReset::NAME,
        m::BlobPutBegin::NAME,
        m::BlobPutChunk::NAME,
        m::BlobPutEnd::NAME,
        m::BlobPutPath::NAME,
        m::BlobGet::NAME,
        m::SessionSearch::NAME,
        m::SessionExport::NAME,
    ];
    if !known.contains(&name) {
        return None;
    }
    let Some(store) = svc.store.clone() else {
        return Some(Err(not_implemented(name)));
    };
    let projects = || svc.projects.clone().ok_or_else(|| not_implemented(name));
    let res = match name {
        n if n == m::ProjectList::NAME => store
            .read(genie_store::projects::list)
            .map_err(store_err)
            .and_then(|v| {
                to_result(&ProjectListResult {
                    projects: v.into_iter().map(info).collect(),
                })
            }),
        n if n == m::ProjectCreate::NAME => params::<m::ProjectCreate>(r).and_then(|p| {
            if p.name.trim().is_empty() {
                return Err(RpcError::invalid_params("name is empty"));
            }
            let instr = Some(p.instructions.as_str()).filter(|s| !s.trim().is_empty());
            let pr = store
                .write(|tx| genie_store::projects::create(tx, p.name.trim(), instr))
                .map_err(store_err)?;
            to_result(&ProjectIdResult { project_id: pr.id })
        }),
        n if n == m::ProjectUpdate::NAME => params::<m::ProjectUpdate>(r).and_then(|p| {
            store
                .write(|tx| {
                    let cur = genie_store::projects::get(tx, &p.project_id)?
                        .ok_or_else(|| genie_store::StoreError::NotFound("project".into()))?;
                    let name = p.name.clone().unwrap_or(cur.name);
                    let instructions = match &p.instructions {
                        Some(s) if s.trim().is_empty() => None,
                        Some(s) => Some(s.clone()),
                        None => cur.instructions,
                    };
                    genie_store::projects::update(
                        tx,
                        &p.project_id,
                        &name,
                        instructions.as_deref(),
                        cur.settings.as_ref(),
                        p.starred.unwrap_or(cur.starred),
                    )
                })
                .map_err(store_err)?;
            to_result(&Empty {})
        }),
        n if n == m::ProjectDelete::NAME => params::<m::ProjectDelete>(r).and_then(|p| {
            if !store
                .write(|tx| genie_store::projects::delete(tx, &p.project_id))
                .map_err(store_err)?
            {
                return Err(not_found("project"));
            }
            to_result(&Empty {})
        }),
        n if n == m::ProjectFileAdd::NAME => params::<m::ProjectFileAdd>(r).and_then(|p| {
            let f = projects()?
                .add_file(&p.project_id, &p.blob_id, &p.name)
                .map_err(project_err)?;
            to_result(&FileIdResult { file_id: f.id })
        }),
        n if n == m::ProjectFileRemove::NAME => params::<m::ProjectFileRemove>(r).and_then(|p| {
            projects()?
                .remove_file(&p.project_id, &p.file_id)
                .map_err(project_err)?;
            to_result(&Empty {})
        }),
        n if n == m::ProjectIndexStatus::NAME => params::<m::ProjectIndexStatus>(r)
            .and_then(|p| to_result(&projects()?.status(&p.project_id).map_err(project_err)?)),

        n if n == m::ArtifactList::NAME => params::<m::ArtifactList>(r).and_then(|p| {
            let hub = svc.artifacts.clone().ok_or_else(|| not_implemented(name))?;
            let items = hub
                .list(&p.session_id, p.leaf_id.as_deref())
                .map_err(|e| e.to_rpc())?;
            to_result(&ArtifactListResult {
                artifacts: items
                    .into_iter()
                    .map(|(a, ns)| ArtifactSummary {
                        artifact_id: a.id,
                        artifact_type: a.ty,
                        title: a.title,
                        versions: ns,
                    })
                    .collect(),
            })
        }),
        n if n == m::ArtifactGet::NAME => params::<m::ArtifactGet>(r).and_then(|p| {
            let hub = svc.artifacts.clone().ok_or_else(|| not_implemented(name))?;
            let (a, v) = hub
                .get(&p.artifact_id, p.n)
                .map_err(|e| e.to_rpc())?
                .ok_or_else(|| not_found("artifact version"))?;
            to_result(&ArtifactGetResult {
                artifact_type: a.ty,
                title: a.title,
                content: v.content,
                blob_id: v.blob_id,
            })
        }),

        n if n == m::MemoryList::NAME => params::<m::MemoryList>(r).and_then(|p| {
            let v = match p.scope.as_deref() {
                Some(s) => store.read(|c| genie_store::memory::list(c, s)),
                None => store.read(genie_store::memory::all),
            }
            .map_err(store_err)?;
            to_result(&MemoryListResult {
                items: v.into_iter().map(memory_item).collect(),
            })
        }),
        n if n == m::MemoryUpdate::NAME => params::<m::MemoryUpdate>(r).and_then(|p| {
            if p.text.trim().is_empty() {
                return Err(RpcError::invalid_params("text is empty"));
            }
            store
                .write(|tx| genie_store::memory::update_text(tx, &p.memory_id, p.text.trim()))
                .map_err(store_err)?;
            to_result(&Empty {})
        }),
        n if n == m::MemoryDelete::NAME => params::<m::MemoryDelete>(r).and_then(|p| {
            if !store
                .write(|tx| genie_store::memory::delete(tx, &p.memory_id))
                .map_err(store_err)?
            {
                return Err(not_found("memory"));
            }
            to_result(&Empty {})
        }),
        n if n == m::MemorySetState::NAME => params::<m::MemorySetState>(r).and_then(|p| {
            let ctl = svc
                .memory_ctl
                .clone()
                .ok_or_else(|| not_implemented(name))?;
            let on = match p.state {
                MemoryState::On => true,
                MemoryState::Paused => false,
                MemoryState::Unknown => {
                    return Err(RpcError::invalid_params("state must be on or paused"));
                }
            };
            ctl.set_on(on)
                .map_err(|e| RpcError::new(ErrorKind::InternalError, e))?;
            to_result(&Empty {})
        }),
        n if n == m::MemoryReset::NAME => params::<m::MemoryReset>(r).and_then(|p| {
            store
                .write(|tx| genie_store::memory::reset(tx, p.scope.as_deref()))
                .map_err(store_err)?;
            to_result(&Empty {})
        }),

        n if n == m::BlobPutBegin::NAME => {
            params::<m::BlobPutBegin>(r).and_then(|p| to_result(&svc.uploads.begin(&p)?))
        }
        n if n == m::BlobPutChunk::NAME => params::<m::BlobPutChunk>(r).and_then(|p| {
            svc.uploads.chunk(&p)?;
            to_result(&Empty {})
        }),
        n if n == m::BlobPutEnd::NAME => params::<m::BlobPutEnd>(r)
            .and_then(|p| to_result(&svc.uploads.end(&store, &p.upload_id)?)),
        n if n == m::BlobPutPath::NAME => params::<m::BlobPutPath>(r)
            .and_then(|p| to_result(&crate::blobs::put_path(&store, &p)?)),
        n if n == m::BlobGet::NAME => {
            params::<m::BlobGet>(r).and_then(|p| to_result(&crate::blobs::get(&store, &p)?))
        }

        n if n == m::SessionSearch::NAME => {
            params::<m::SessionSearch>(r).and_then(|p| session_search(svc, &store, p))
        }
        n if n == m::SessionExport::NAME => {
            params::<m::SessionExport>(r).and_then(|p| session_export(svc, &store, &p))
        }
        _ => return None,
    };
    Some(res)
}

fn incognito(svc: &Services, session_id: &str) -> bool {
    svc.runtime
        .as_ref()
        .and_then(|rt| rt.settings.settings(session_id))
        .is_some_and(|s| s.incognito)
}

/// `session.search` (02 §6.2): sidebar search over `message_fts` (titles and text).
fn session_search(
    svc: &Services,
    store: &genie_store::Store,
    p: genie_protocol::api::session::SearchParams,
) -> Result<Value, RpcError> {
    use genie_store::search::Scope;
    let scope = match (p.scope, p.project_id) {
        (Some(SearchScope::Project), Some(pid)) => Scope::Project(pid),
        (Some(SearchScope::Project), None) => {
            return Err(RpcError::invalid_params("scope `project` needs project_id"));
        }
        (Some(SearchScope::Unknown), _) => {
            return Err(RpcError::invalid_params("scope must be all or project"));
        }
        (_, Some(pid)) => Scope::Project(pid),
        _ => Scope::All,
    };
    let hits = store
        .read(|c| genie_store::search::chat_search(c, &p.query, &scope, 50))
        .map_err(store_err)?;
    to_result(&SearchResult {
        hits: hits
            .into_iter()
            .filter(|h| !incognito(svc, &h.session_id))
            .map(|h| SearchHit {
                session_id: h.session_id,
                // Title matches carry no message.
                message_id: h.message_id.unwrap_or_default(),
                snippet: h.snippet,
            })
            .collect(),
    })
}

/// `session.export` (04 §9): JSON `{info, messages, parts, artifacts}` or Markdown;
/// inline text up to 1 MiB, a blob above. Policy `export.allowed: false` refuses.
fn session_export(
    svc: &Services,
    store: &genie_store::Store,
    p: &genie_protocol::api::session::ExportParams,
) -> Result<Value, RpcError> {
    if let Some(h) = &svc.hub
        && h.policy().bool("export.allowed") == Some(false)
    {
        return Err(RpcError::new(
            ErrorKind::PolicyBlocked,
            "Export is turned off by your organization's policy",
        ));
    }
    if incognito(svc, &p.session_id) {
        return Err(not_found("session"));
    }
    let text = match p.format {
        ExportFormat::Json => {
            let v = store
                .read(|c| genie_store::export::session_json(c, &p.session_id, p.sanitize))
                .map_err(store_err)?;
            serde_json::to_string_pretty(&v).map_err(|e| RpcError::internal(e.to_string()))?
        }
        ExportFormat::Markdown => store
            .read(|c| genie_store::export::session_markdown(c, &p.session_id))
            .map_err(store_err)?,
        ExportFormat::Unknown => {
            return Err(RpcError::invalid_params("format must be json or markdown"));
        }
    };
    if text.len() > EXPORT_INLINE_MAX {
        let mime = match p.format {
            ExportFormat::Json => "application/json",
            _ => "text/markdown",
        };
        let id = store
            .put_blob(text.as_bytes(), Some(mime))
            .map_err(store_err)?;
        return to_result(&ExportResult {
            blob_id: Some(id),
            text: None,
        });
    }
    to_result(&ExportResult {
        blob_id: None,
        text: Some(text),
    })
}
