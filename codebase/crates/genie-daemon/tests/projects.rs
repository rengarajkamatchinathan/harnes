//! Projects, artifacts, memory, blobs (02 §6.9) and `session.search` / `session.export`
//! over the in-memory transport with a real store and the runtime's hubs. No network.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use std::sync::Arc;

use base64::Engine as _;
use futures::future::BoxFuture;
use genie_client::Endpoint;
use genie_client::connection::{ClientError, ClientInfo, Connection};
use genie_core::runtime::artifacts::ArtifactHub;
use genie_core::runtime::memory_ctl::MemoryControl;
use genie_core::runtime::projects::{ProjectsConfig, ProjectsHub};
use genie_core::runtime::{NoModels, RuntimeBuilder};
use genie_core::{EngineBuilder, TaskScope, TurnCtx, TurnError, TurnRunner};
use genie_daemon::{DaemonConfig, DaemonState, Registry, Services, serve_stream};
use genie_protocol::api::project::{
    ArtifactGetParams, ArtifactListParams, BLOB_CHUNK_BYTES, BlobGetParams, BlobPutBeginParams,
    BlobPutChunkParams, BlobPutPathParams, IndexMode, MemoryIdParams, MemoryScopeParams,
    MemorySetStateParams, MemoryState, MemoryUpdateParams, ProjectCreateParams,
    ProjectFileAddParams, ProjectFileRemoveParams, ProjectIdParams, ProjectUpdateParams,
    UploadIdParams,
};
use genie_protocol::api::session::{ExportFormat, ExportParams, SearchParams, SearchScope};
use genie_protocol::common::Empty;
use genie_protocol::handshake::{ClientCapabilities, ClientKind};
use genie_protocol::methods as m;
use genie_protocol::{Channel, ErrorKind};
use serde_json::json;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

struct Idle;

impl TurnRunner for Idle {
    fn run(&self, _ctx: TurnCtx) -> BoxFuture<'static, Result<(), TurnError>> {
        Box::pin(async { Ok(()) })
    }
}

struct Mem {
    state: Arc<DaemonState>,
    store: Arc<genie_store::Store>,
    memory: Arc<MemoryControl>,
    _dir: tempfile::TempDir,
}

fn daemon() -> Mem {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = DaemonConfig::new(Endpoint::in_dir(dir.path(), Channel::Dev).unwrap());
    cfg.engine_version = "1.0.0".into();
    let store = Arc::new(
        genie_store::Store::open_with_keystore(
            &genie_store::StoreOptions::new(dir.path().join("data"), "genie.db"),
            &genie_store::MemoryKeyStore::new(),
        )
        .unwrap(),
    );
    let root = TaskScope::root();
    let registry = Arc::new(Registry::default());
    let artifacts = ArtifactHub::new(Some(store.clone()));
    let memory = MemoryControl::new(Some(store.clone()));
    let projects = ProjectsHub::new(
        store.clone(),
        Arc::new(NoModels),
        None,
        root.child("indexing"),
        ProjectsConfig::default(),
    );
    let runtime = RuntimeBuilder::new(registry.clone(), Arc::new(NoModels))
        .store(store.clone())
        .artifacts(artifacts.clone())
        .projects(projects.clone())
        .memory_control(memory.clone())
        .build();
    let engine = EngineBuilder::new()
        .turn_runner(Arc::new(Idle))
        .build(&root);
    let services = Services {
        runtime: Some(runtime.runtime().clone()),
        store: Some(store.clone()),
        artifacts: Some(artifacts),
        projects: Some(projects),
        memory_ctl: Some(memory.clone()),
        ..Services::default()
    };
    let state = Arc::new(DaemonState::with_services(
        cfg,
        engine,
        TOKEN.into(),
        root,
        registry,
        Some(runtime.conversation()),
        Some(Arc::new(services)),
    ));
    Mem {
        state,
        store,
        memory,
        _dir: dir,
    }
}

async fn connect(m: &Mem) -> Connection {
    let (client, server) = genie_testkit::memory_pair();
    let st = m.state.clone();
    tokio::spawn(async move { serve_stream(st, Box::new(server), TaskScope::root()).await });
    let info = ClientInfo {
        kind: ClientKind::Desktop,
        version: "1.0.0".into(),
        instance_id: "desk".into(),
        capabilities: ClientCapabilities::default(),
    };
    Connection::handshake(Box::new(client), &info, TOKEN.into())
        .await
        .unwrap()
}

fn rpc_kind(e: ClientError) -> ErrorKind {
    match e {
        ClientError::Rpc(r) => r.kind(),
        other => panic!("not an rpc error: {other:?}"),
    }
}

fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(b)
}

/// Upload `data` in 1 MiB chunks; returns the blob id.
async fn upload(c: &Connection, data: &[u8], mime: &str) -> String {
    let up = c
        .call::<m::BlobPutBegin>(BlobPutBeginParams {
            size: data.len() as u64,
            mime: Some(mime.into()),
            name: None,
        })
        .await
        .unwrap();
    for (i, chunk) in data.chunks(BLOB_CHUNK_BYTES).enumerate() {
        c.call::<m::BlobPutChunk>(BlobPutChunkParams {
            upload_id: up.upload_id.clone(),
            offset: (i * BLOB_CHUNK_BYTES) as u64,
            data_b64: b64(chunk),
        })
        .await
        .unwrap();
    }
    c.call::<m::BlobPutEnd>(UploadIdParams {
        upload_id: up.upload_id,
    })
    .await
    .unwrap()
    .blob_id
}

#[tokio::test]
async fn chunked_blob_upload_round_trip() {
    let mem = daemon();
    let c = connect(&mem).await;
    let data: Vec<u8> = (0..(2 * BLOB_CHUNK_BYTES + 12_345))
        .map(|i| (i % 251) as u8)
        .collect();
    let up = c
        .call::<m::BlobPutBegin>(BlobPutBeginParams {
            size: data.len() as u64,
            mime: Some("application/octet-stream".into()),
            name: Some("data.bin".into()),
        })
        .await
        .unwrap();
    let chunks: Vec<&[u8]> = data.chunks(BLOB_CHUNK_BYTES).collect();
    let send = |i: usize| {
        c.call::<m::BlobPutChunk>(BlobPutChunkParams {
            upload_id: up.upload_id.clone(),
            offset: (i * BLOB_CHUNK_BYTES) as u64,
            data_b64: b64(chunks[i]),
        })
    };
    send(0).await.unwrap();
    // Out of order → conflict; a retried chunk is fine.
    assert_eq!(rpc_kind(send(2).await.unwrap_err()), ErrorKind::Conflict);
    send(0).await.unwrap();
    send(1).await.unwrap();
    // Ending early is refused.
    let e = c
        .call::<m::BlobPutEnd>(UploadIdParams {
            upload_id: up.upload_id.clone(),
        })
        .await
        .unwrap_err();
    assert_eq!(rpc_kind(e), ErrorKind::InvalidParams);
    send(2).await.unwrap();
    let put = c
        .call::<m::BlobPutEnd>(UploadIdParams {
            upload_id: up.upload_id.clone(),
        })
        .await
        .unwrap();
    assert_eq!(put.blob_id, put.sha256);
    assert_eq!(put.blob_id, genie_store::blobs::sha256_hex(&data));
    // Ranged reads across chunk boundaries.
    let mut back = vec![];
    let mut off = 0u64;
    loop {
        let r = c
            .call::<m::BlobGet>(BlobGetParams {
                blob_id: put.blob_id.clone(),
                offset: off,
                len: 700_000,
            })
            .await
            .unwrap();
        assert_eq!(r.size, data.len() as u64);
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(r.data_b64)
            .unwrap();
        if bytes.is_empty() {
            break;
        }
        off += bytes.len() as u64;
        back.extend(bytes);
    }
    assert_eq!(back, data);
    // The upload is gone after `end`.
    let e = c
        .call::<m::BlobPutEnd>(UploadIdParams {
            upload_id: up.upload_id,
        })
        .await
        .unwrap_err();
    assert_eq!(rpc_kind(e), ErrorKind::NotFound);
    // blob.put.path reads a local file.
    let p = mem._dir.path().join("note.md");
    std::fs::write(&p, "# Hello").unwrap();
    let r = c
        .call::<m::BlobPutPath>(BlobPutPathParams {
            path: p.display().to_string(),
        })
        .await
        .unwrap();
    assert_eq!(mem.store.read_blob(&r.blob_id).unwrap(), b"# Hello");
    assert_eq!(
        mem.store
            .blob_row(&r.blob_id)
            .unwrap()
            .unwrap()
            .mime
            .as_deref(),
        Some("text/markdown")
    );
    let e = c
        .call::<m::BlobGet>(BlobGetParams {
            blob_id: "0".repeat(64),
            offset: 0,
            len: 1,
        })
        .await
        .unwrap_err();
    assert_eq!(rpc_kind(e), ErrorKind::NotFound);
}

#[tokio::test]
async fn project_methods_and_files() {
    let mem = daemon();
    let c = connect(&mem).await;
    let p = c
        .call::<m::ProjectCreate>(ProjectCreateParams {
            name: "Atlas".into(),
            instructions: "Be brief.".into(),
        })
        .await
        .unwrap()
        .project_id;
    c.call::<m::ProjectUpdate>(ProjectUpdateParams {
        project_id: p.clone(),
        name: None,
        instructions: Some("Be very brief.".into()),
        starred: Some(true),
    })
    .await
    .unwrap();
    let list = c.call::<m::ProjectList>(Empty {}).await.unwrap();
    assert_eq!(list.projects.len(), 1);
    assert_eq!(list.projects[0].instructions, "Be very brief.");
    assert!(list.projects[0].starred);

    let blob = upload(
        &c,
        b"# Notes\n\nThe launch is on Friday.\n",
        "text/markdown",
    )
    .await;
    let f = c
        .call::<m::ProjectFileAdd>(ProjectFileAddParams {
            project_id: p.clone(),
            blob_id: blob.clone(),
            name: "notes.md".into(),
        })
        .await
        .unwrap()
        .file_id;
    let st = c
        .call::<m::ProjectIndexStatus>(ProjectIdParams {
            project_id: p.clone(),
        })
        .await
        .unwrap();
    assert_eq!(st.mode, IndexMode::Full);
    assert_eq!(st.files, 1);
    assert!(st.tokens > 0 && st.capacity_pct >= 0.0 && !st.indexing);
    let (chunks, _) = mem
        .store
        .read(|c| genie_store::retrieval::project_stats(c, &p))
        .unwrap();
    assert_eq!(chunks, 1);

    // Same name again: identical content is a no-op, changed content re-indexes in place.
    let same = c
        .call::<m::ProjectFileAdd>(ProjectFileAddParams {
            project_id: p.clone(),
            blob_id: blob.clone(),
            name: "notes.md".into(),
        })
        .await
        .unwrap()
        .file_id;
    assert_eq!(same, f);
    let changed = upload(
        &c,
        b"# Notes\n\nThe launch moved to Monday.\n\n# Risks\n\nWeather.\n",
        "text/markdown",
    )
    .await;
    let again = c
        .call::<m::ProjectFileAdd>(ProjectFileAddParams {
            project_id: p.clone(),
            blob_id: changed.clone(),
            name: "notes.md".into(),
        })
        .await
        .unwrap()
        .file_id;
    assert_eq!(again, f, "same file, updated");
    let files = mem
        .store
        .read(|cn| genie_store::projects::files(cn, &p))
        .unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].blob_id.as_deref(), Some(changed.as_str()));
    let text: Vec<String> = mem
        .store
        .read(|cn| {
            let mut st = cn.prepare("SELECT text FROM kchunk WHERE file_id = ?1")?;
            let rows = st.query_map([&f], |r| r.get::<_, String>(0))?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
        .unwrap();
    assert!(text.iter().any(|t| t.contains("Monday")), "{text:?}");
    assert!(!text.iter().any(|t| t.contains("Friday")), "{text:?}");
    // The old blob is no longer referenced.
    assert_eq!(mem.store.blob_row(&blob).unwrap().unwrap().refcount, 0);

    // A binary file is refused with a reason; unknown ids are not found.
    let bin = upload(&c, &[0, 1, 2, 3, 0, 9], "application/zip").await;
    let e = c
        .call::<m::ProjectFileAdd>(ProjectFileAddParams {
            project_id: p.clone(),
            blob_id: bin,
            name: "a.zip".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(rpc_kind(e), ErrorKind::InvalidParams);
    let e = c
        .call::<m::ProjectFileAdd>(ProjectFileAddParams {
            project_id: "prj_nope".into(),
            blob_id: blob,
            name: "x.md".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(rpc_kind(e), ErrorKind::NotFound);

    c.call::<m::ProjectFileRemove>(ProjectFileRemoveParams {
        project_id: p.clone(),
        file_id: f,
    })
    .await
    .unwrap();
    let (chunks, _) = mem
        .store
        .read(|c| genie_store::retrieval::project_stats(c, &p))
        .unwrap();
    assert_eq!(chunks, 0, "chunks cascade");
    c.call::<m::ProjectDelete>(ProjectIdParams {
        project_id: p.clone(),
    })
    .await
    .unwrap();
    let e = c
        .call::<m::ProjectIndexStatus>(ProjectIdParams { project_id: p })
        .await
        .unwrap_err();
    assert_eq!(rpc_kind(e), ErrorKind::NotFound);
}

#[tokio::test]
async fn memory_methods_and_state() {
    let mem = daemon();
    let c = connect(&mem).await;
    let mut rx = c.take_incoming().unwrap();
    c.call::<m::WorkspaceSubscribe>(Default::default())
        .await
        .unwrap();
    // `memory.changed` on the workspace feed (D31) after each change.
    async fn changed(
        rx: &mut tokio::sync::mpsc::UnboundedReceiver<genie_client::connection::Incoming>,
    ) -> genie_protocol::event::MemoryChanged {
        loop {
            let i = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
                .await
                .expect("memory.changed in time")
                .expect("open");
            if let genie_client::connection::Incoming::Notification(n) = i
                && n.method == "memory.changed"
            {
                return serde_json::from_value(n.params).unwrap();
            }
        }
    }
    let (a, _b) = mem
        .store
        .write(|tx| {
            let a = genie_store::memory::add(tx, "global", "Likes tea", None, None)?;
            let b = genie_store::memory::add(tx, "prj_x", "Uses Rust", None, None)?;
            Ok((a, b))
        })
        .unwrap();
    let all = c
        .call::<m::MemoryList>(MemoryScopeParams::default())
        .await
        .unwrap();
    assert_eq!(all.items.len(), 2);
    let global = c
        .call::<m::MemoryList>(MemoryScopeParams {
            scope: Some("global".into()),
        })
        .await
        .unwrap();
    assert_eq!(global.items.len(), 1);
    assert!(global.items[0].updated_at.ends_with('Z'));
    c.call::<m::MemoryUpdate>(MemoryUpdateParams {
        memory_id: a.id.clone(),
        text: "Likes green tea".into(),
    })
    .await
    .unwrap();
    assert_eq!(
        mem.store
            .read(|cn| genie_store::memory::get(cn, &a.id))
            .unwrap()
            .unwrap()
            .text,
        "Likes green tea"
    );
    let note = changed(&mut rx).await;
    assert_eq!(
        (
            note.memory_id.as_str(),
            note.op.as_str(),
            note.scope.as_str()
        ),
        (a.id.as_str(), "update", "global")
    );

    c.call::<m::MemorySetState>(MemorySetStateParams {
        state: MemoryState::Paused,
    })
    .await
    .unwrap();
    assert!(!mem.memory.is_on());
    c.call::<m::MemorySetState>(MemorySetStateParams {
        state: MemoryState::On,
    })
    .await
    .unwrap();
    assert!(mem.memory.is_on());

    c.call::<m::MemoryDelete>(MemoryIdParams {
        memory_id: a.id.clone(),
    })
    .await
    .unwrap();
    let note = changed(&mut rx).await;
    assert_eq!(
        (note.op.as_str(), note.scope.as_str()),
        ("delete", "global")
    );
    let e = c
        .call::<m::MemoryDelete>(MemoryIdParams { memory_id: a.id })
        .await
        .unwrap_err();
    assert_eq!(rpc_kind(e), ErrorKind::NotFound);
    c.call::<m::MemoryReset>(MemoryScopeParams::default())
        .await
        .unwrap();
    let note = changed(&mut rx).await;
    assert_eq!((note.memory_id.as_str(), note.scope.as_str()), ("*", "all"));
    let all = c
        .call::<m::MemoryList>(MemoryScopeParams::default())
        .await
        .unwrap();
    assert!(all.items.is_empty());
}

/// A session with two text messages and an artifact with a version on each.
fn seed_session(store: &genie_store::Store) -> (String, String, String, String) {
    use genie_store::messages as sm;
    store
        .write(|tx| {
            let sid = "ses_seed".to_string();
            genie_store::sessions::ensure(tx, &sid)?;
            genie_store::sessions::set_title(tx, &sid, Some("Trip planning"))?;
            let u = sm::Message::new(&sid, None, "user");
            sm::insert(tx, &u)?;
            sm::insert_part(tx, &sm::Part::text(&u.id, 0, "plan a trip to Lisbon"))?;
            let a1 = sm::Message::new(&sid, Some(&u.id), "assistant");
            sm::insert(tx, &a1)?;
            sm::insert_part(tx, &sm::Part::text(&a1.id, 0, "Here is a Lisbon plan"))?;
            let a2 = sm::Message::new(&sid, Some(&a1.id), "assistant");
            sm::insert(tx, &a2)?;
            genie_store::sessions::set_active_leaf(tx, &sid, Some(&a2.id))?;
            let art = genie_store::artifacts::Artifact {
                id: "art_seed".into(),
                session_id: sid.clone(),
                ty: "document".into(),
                title: Some("Lisbon".into()),
                created: 1,
            };
            genie_store::artifacts::insert(tx, &art)?;
            genie_store::artifacts::add_version(tx, &art.id, &a1.id, Some("v1"), None)?;
            genie_store::artifacts::add_version(tx, &art.id, &a2.id, Some("v2"), None)?;
            Ok((sid, u.id, a1.id, a2.id))
        })
        .unwrap()
}

#[tokio::test]
async fn artifact_list_get_search_and_export() {
    let mem = daemon();
    let c = connect(&mem).await;
    let (sid, _u, a1, _a2) = seed_session(&mem.store);

    let list = c
        .call::<m::ArtifactList>(ArtifactListParams {
            session_id: sid.clone(),
            leaf_id: None,
        })
        .await
        .unwrap();
    assert_eq!(list.artifacts.len(), 1);
    assert_eq!(list.artifacts[0].versions, vec![1, 2]);
    assert_eq!(list.artifacts[0].artifact_type, "document");
    let list = c
        .call::<m::ArtifactList>(ArtifactListParams {
            session_id: sid.clone(),
            leaf_id: Some(a1),
        })
        .await
        .unwrap();
    assert_eq!(list.artifacts[0].versions, vec![1]);
    let got = c
        .call::<m::ArtifactGet>(ArtifactGetParams {
            artifact_id: "art_seed".into(),
            n: 2,
        })
        .await
        .unwrap();
    assert_eq!(
        (got.title.as_str(), got.content.as_deref()),
        ("Lisbon", Some("v2"))
    );
    let e = c
        .call::<m::ArtifactGet>(ArtifactGetParams {
            artifact_id: "art_seed".into(),
            n: 9,
        })
        .await
        .unwrap_err();
    assert_eq!(rpc_kind(e), ErrorKind::NotFound);

    let hits = c
        .call::<m::SessionSearch>(SearchParams {
            query: "lisbon".into(),
            scope: Some(SearchScope::All),
            project_id: None,
        })
        .await
        .unwrap()
        .hits;
    assert!(hits.len() >= 2, "{hits:?}");
    assert!(hits.iter().all(|h| h.session_id == sid));
    assert!(hits.iter().any(|h| h.snippet.contains("[Lisbon]")));
    let e = c
        .call::<m::SessionSearch>(SearchParams {
            query: "lisbon".into(),
            scope: Some(SearchScope::Project),
            project_id: None,
        })
        .await
        .unwrap_err();
    assert_eq!(rpc_kind(e), ErrorKind::InvalidParams);

    let md = c
        .call::<m::SessionExport>(ExportParams {
            session_id: sid.clone(),
            format: ExportFormat::Markdown,
            sanitize: false,
        })
        .await
        .unwrap();
    let text = md.text.unwrap();
    assert!(text.starts_with("# Trip planning"));
    assert!(text.contains("plan a trip to Lisbon"));
    let js = c
        .call::<m::SessionExport>(ExportParams {
            session_id: sid.clone(),
            format: ExportFormat::Json,
            sanitize: true,
        })
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(&js.text.unwrap()).unwrap();
    assert_eq!(
        v["artifacts"][0]["versions"][0]["content"],
        json!("[redacted]")
    );
    assert_eq!(v["messages"].as_array().unwrap().len(), 3);
    let e = c
        .call::<m::SessionExport>(ExportParams {
            session_id: "ses_missing".into(),
            format: ExportFormat::Json,
            sanitize: false,
        })
        .await
        .unwrap_err();
    assert_eq!(rpc_kind(e), ErrorKind::NotFound);
}
