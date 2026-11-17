//! Data in and out (04 §9, 07 §5, 06 §11): `export.all` (SA-17 zip), `session.import`
//! (id remapping), `import.scan/preview/apply` (the shared importer in
//! `genie_config::import`, same code as `genie import`) and `agents.list`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use genie_config::import::{self, ApplyOptions, Filter, Level, Roots, SecretSink, Source};
use genie_core::runtime::agents::{AgentCatalog, AgentSource};
use genie_protocol::api::code::{ExportAllResult, ImportResult};
use genie_protocol::api::session::{
    AgentInfo, AgentListResult, AgentProblemInfo, AgentSourceKind, CreateParams,
    SessionImportResult, UpdateParams,
};
use genie_protocol::methods::{self as m, Method};
use genie_protocol::workspace::ListedChange;
use genie_protocol::{ErrorKind, Request, RpcError};
use genie_secrets::SecretBackend;
use serde::Serialize;
use serde_json::{Value, json};

use crate::server::DaemonState;
use crate::services::{Services, canonical, not_implemented};

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
        genie_store::StoreError::Format(_) | genie_store::StoreError::Json(_) => {
            RpcError::invalid_params(e.to_string())
        }
        genie_store::StoreError::NotFound(_) => RpcError::new(ErrorKind::NotFound, e.to_string()),
        _ => RpcError::internal(e.to_string()),
    }
}

/// Requests served here; `None` for anything else.
pub(crate) fn dispatch(
    svc: &Arc<Services>,
    state: &Arc<DaemonState>,
    r: &Request,
) -> Option<Result<Value, RpcError>> {
    let name = r.method.as_str();
    let res = match name {
        n if n == m::ExportAll::NAME => params::<m::ExportAll>(r).and_then(|p| export_all(svc, &p)),
        n if n == m::SessionImport::NAME => {
            params::<m::SessionImport>(r).and_then(|p| session_import(svc, state, &p))
        }
        n if n == m::ImportScan::NAME || n == m::ImportPreview::NAME || n == m::ImportApply::NAME => {
            params::<m::ImportScan>(r).and_then(|p| config_import(svc, name, &p.options))
        }
        n if n == m::AgentsList::NAME => params::<m::AgentsList>(r).and_then(|p| {
            let ws = p.cwd.filter(|c| !c.is_empty()).map(|c| PathBuf::from(canonical(&c)));
            agents_list(svc, ws.as_deref())
        }),
        _ => return None,
    };
    Some(res)
}

fn export_blocked(svc: &Services) -> Result<(), RpcError> {
    if let Some(h) = &svc.hub
        && h.policy().bool("export.allowed") == Some(false)
    {
        return Err(RpcError::new(
            ErrorKind::PolicyBlocked,
            "Export is turned off by your organization's policy",
        ));
    }
    Ok(())
}

/// `export.all`: a plaintext zip at `path` (the client confirmed with the user first).
/// Written to a temp file next to it and renamed, 0600 on Unix.
fn export_all(
    svc: &Services,
    p: &genie_protocol::api::code::ExportAllParams,
) -> Result<Value, RpcError> {
    export_blocked(svc)?;
    let store = svc.store.as_ref().ok_or_else(|| not_implemented(m::ExportAll::NAME))?;
    let dest = PathBuf::from(&p.path);
    if p.path.is_empty() || !dest.is_absolute() {
        return Err(RpcError::invalid_params("path must be an absolute file path"));
    }
    let dir = dest
        .parent()
        .filter(|d| d.is_dir())
        .ok_or_else(|| RpcError::invalid_params("the folder for path does not exist"))?;
    let tmp = tempfile::Builder::new()
        .prefix(".genie-export-")
        .tempfile_in(dir)
        .map_err(|e| RpcError::new(ErrorKind::InvalidState, e.to_string()))?;
    let file = tmp
        .reopen()
        .map_err(|e| RpcError::new(ErrorKind::InvalidState, e.to_string()))?;
    let counts = store.export_all(file, p.include_blobs).map_err(store_err)?;
    tmp.persist(&dest)
        .map_err(|e| RpcError::new(ErrorKind::InvalidState, e.to_string()))?;
    to_result(&ExportAllResult {
        path: dest.display().to_string(),
        sessions: counts.sessions,
        projects: counts.projects,
        memories: counts.memories,
        blobs: counts.blobs,
        artifacts: counts.artifacts,
    })
}

/// `session.import`: one session JSON; the new session joins the session list.
fn session_import(
    svc: &Services,
    state: &Arc<DaemonState>,
    p: &genie_protocol::api::session::SessionImportParams,
) -> Result<Value, RpcError> {
    let store = svc
        .store
        .as_ref()
        .ok_or_else(|| not_implemented(m::SessionImport::NAME))?;
    let bytes = match (&p.path, &p.blob_id) {
        (Some(path), None) => std::fs::read(path)
            .map_err(|e| RpcError::invalid_params(format!("{path}: {e}")))?,
        (None, Some(b)) => store.read_blob(b).map_err(store_err)?,
        _ => return Err(RpcError::invalid_params("give exactly one of path or blob_id")),
    };
    let json: Value = serde_json::from_slice(&bytes)
        .map_err(|e| RpcError::invalid_params(format!("not a session export: {e}")))?;
    let done = store.import_session(&json).map_err(store_err)?;
    // Session metadata is served from the daemon registry (README gaps): register it.
    let row = store
        .read(|c| genie_store::sessions::get(c, &done.session_id))
        .map_err(store_err)?
        .ok_or_else(|| RpcError::internal("imported session vanished"))?;
    let reg = state.registry();
    reg.create(
        &done.session_id,
        &CreateParams {
            kind: match row.kind {
                genie_store::sessions::SessionKind::Chat => genie_protocol::common::SessionKind::Chat,
                genie_store::sessions::SessionKind::Code => genie_protocol::common::SessionKind::Code,
            },
            project_id: row.project_id.clone(),
            model: row.model.clone().unwrap_or_default(),
            effort: row.effort.clone(),
            agent: row.agent.clone(),
            ..CreateParams::default()
        },
        None,
    );
    if let Some(info) = reg.update(&UpdateParams {
        session_id: done.session_id.clone(),
        title: row.title.clone(),
        pinned: Some(row.pinned),
        archived: Some(row.archived),
        model: None,
        effort: None,
        mode: None,
        active_leaf_id: row.active_leaf_id.clone(),
    }) {
        state.listed(ListedChange::Created, info.summary);
    }
    to_result(&SessionImportResult {
        session_id: done.session_id,
        remapped: done.remapped,
        messages: done.messages,
    })
}

/// Secrets from imported configs go to the D26 backend (`import:<name>` accounts),
/// never into settings; settings keep a `keychain:<name>` reference.
struct BackendSink(Arc<dyn SecretBackend>);

impl SecretSink for BackendSink {
    fn store(&mut self, name: &str, value: &str) -> Result<(), String> {
        self.0
            .set(&format!("import:{name}"), value.as_bytes())
            .map_err(|e| e.to_string())
    }
}

#[derive(serde::Deserialize, Default)]
struct ImportOptions {
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    source: Option<Source>,
    #[serde(default)]
    level: Option<Level>,
    #[serde(default)]
    selected: Option<Vec<String>>,
    #[serde(default)]
    confirm_append: bool,
    /// Tests: a fixed home folder.
    #[serde(default)]
    home: Option<String>,
}

fn config_import(svc: &Services, method: &str, options: &Value) -> Result<Value, RpcError> {
    let o: ImportOptions = if options.is_null() {
        ImportOptions::default()
    } else {
        serde_json::from_value(options.clone())
            .map_err(|e| RpcError::invalid_params(format!("options: {e}")))?
    };
    let home = match &o.home {
        Some(h) => PathBuf::from(h),
        None => std::env::home_dir()
            .ok_or_else(|| RpcError::new(ErrorKind::InvalidState, "no home folder"))?,
    };
    // Without a folder only user-level items make sense.
    let (project, level) = match o.cwd.as_deref().filter(|c| !c.is_empty()) {
        Some(c) => (PathBuf::from(canonical(c)), o.level),
        None => (home.clone(), Some(Level::User)),
    };
    let roots = Roots::detect(home, project);
    let filter = Filter {
        source: o.source,
        level,
    };
    if method == m::ImportScan::NAME {
        let items: Vec<_> = import::scan(&roots)
            .into_iter()
            .filter(|i| filter.source.is_none_or(|s| s == i.source))
            .filter(|i| filter.level.is_none_or(|l| l == i.level))
            .collect();
        return to_result(&ImportResult {
            report: json!({ "items": items }),
        });
    }
    let mut items = import::preview(&roots, filter);
    if method == m::ImportPreview::NAME {
        return to_result(&ImportResult {
            report: json!({ "items": items }),
        });
    }
    let mut sink: Box<dyn SecretSink> = match &svc.providers {
        Some(p) => Box::new(BackendSink(p.secrets().backend())),
        None => Box::new(import::NoKeychain),
    };
    let report = import::apply(
        &roots,
        &mut items,
        &ApplyOptions {
            selected: o.selected,
            confirm_append: o.confirm_append,
            date: import::today(),
        },
        sink.as_mut(),
    )
    .map_err(|e| RpcError::new(ErrorKind::InvalidState, format!("{e:#}")))?;
    let mut v = serde_json::to_value(&report).map_err(|e| RpcError::internal(e.to_string()))?;
    v["path"] = json!(report.path.as_ref().map(|p| p.display().to_string()));
    to_result(&ImportResult { report: v })
}

fn agents_list(svc: &Services, ws: Option<&Path>) -> Result<Value, RpcError> {
    let builtin;
    let cat: &AgentCatalog = match svc.runtime.as_ref().and_then(|rt| rt.agents.as_ref()) {
        Some(c) => c,
        None => {
            builtin = AgentCatalog::builtin_only();
            &builtin
        }
    };
    let (defs, problems) = cat.scan(ws);
    let mut agents: Vec<AgentInfo> = defs
        .into_iter()
        .filter(|d| !d.hidden)
        .map(|d| AgentInfo {
            name: d.name,
            description: d.description,
            source: match d.source {
                AgentSource::Builtin => AgentSourceKind::Builtin,
                AgentSource::User => AgentSourceKind::User,
                AgentSource::Project => AgentSourceKind::Project,
            },
            model: d.model,
            path: d.path.map(|p| p.display().to_string()),
        })
        .collect();
    agents.sort_by(|a, b| a.name.cmp(&b.name));
    to_result(&AgentListResult {
        agents,
        problems: problems
            .into_iter()
            .map(|p| AgentProblemInfo {
                path: p.path.display().to_string(),
                message: p.message,
            })
            .collect(),
    })
}
