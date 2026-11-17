//! What the desktop's Settings and sidebar need beyond the first protocol cut:
//! `session.update.project_id` (+ `session.listed`), per-chat capability toggles,
//! `project.file.list`, `trust.list` + revoke, `permissions.rules.list/remove` and
//! `data.delete_all.begin` / `data.delete_all.confirm`. In-memory transport, real store. No network.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use futures::future::BoxFuture;
use genie_client::Endpoint;
use genie_client::connection::{ClientError, ClientInfo, Connection, Incoming};
use genie_core::config::ConfigHub;
use genie_core::runtime::artifacts::ArtifactHub;
use genie_core::runtime::memory_ctl::MemoryControl;
use genie_core::runtime::permissions::PermissionAuthorizer;
use genie_core::runtime::projects::{ProjectsConfig, ProjectsHub};
use genie_core::runtime::{NoModels, RuntimeBuilder};
use genie_core::{EngineBuilder, TaskScope, TurnCtx, TurnError, TurnRunner};
use genie_daemon::{DaemonConfig, DaemonState, Registry, Services, serve_stream};
use genie_protocol::api::config::{
    DeleteAllParams, PathParams, PermissionRuleClass, PermissionRuleRemoveParams,
    PermissionRuleScope, PermissionRulesListParams, TrustDecision, TrustSetParams, TrustState,
};
use genie_protocol::api::project::{
    BlobPutBeginParams, BlobPutChunkParams, ProjectCreateParams, ProjectFileAddParams,
    ProjectIdParams, UploadIdParams,
};
use genie_protocol::api::session::{
    CreateParams, GetParams, ListParams, SessionSettingsOverlay, UpdateParams,
};
use genie_protocol::common::{Empty, SessionKind};
use genie_protocol::handshake::{ClientCapabilities, ClientKind};
use genie_protocol::methods as m;
use genie_protocol::methods::Method;
use genie_protocol::workspace::{ListedChange, SessionListed, WorkspaceSnapshot};
use genie_protocol::{Channel, ErrorKind, Notification};
use serde_json::json;
use tokio::sync::mpsc::UnboundedReceiver;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const WAIT: Duration = Duration::from_secs(5);

struct Idle;

impl TurnRunner for Idle {
    fn run(&self, _ctx: TurnCtx) -> BoxFuture<'static, Result<(), TurnError>> {
        Box::pin(async { Ok(()) })
    }
}

struct Mem {
    state: Arc<DaemonState>,
    store: Arc<genie_store::Store>,
    registry: Arc<Registry>,
    dir: tempfile::TempDir,
}

fn daemon() -> Mem {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = DaemonConfig::new(Endpoint::in_dir(dir.path(), Channel::Dev).unwrap());
    cfg.engine_version = "1.0.0".into();
    cfg.write_logs = false;
    let store = Arc::new(
        genie_store::Store::open_with_keystore(
            &genie_store::StoreOptions::new(dir.path(), "genie.db"),
            &genie_store::MemoryKeyStore::new(),
        )
        .unwrap(),
    );
    let root = TaskScope::root();
    let registry = Arc::new(Registry::default());
    let hub = Arc::new(
        ConfigHub::new(
            dir.path().join("home/.genie"),
            dir.path().join("home/.genie/trust.json"),
            Some(dir.path().join("home").display().to_string()),
        )
        .unwrap(),
    );
    let artifacts = ArtifactHub::new(Some(store.clone()));
    let memory = MemoryControl::new(Some(store.clone()));
    let projects = ProjectsHub::new(
        store.clone(),
        Arc::new(NoModels),
        None,
        root.child("indexing"),
        ProjectsConfig::default(),
    );
    let authz = Arc::new(PermissionAuthorizer::new(hub.clone(), registry.clone()));
    let runtime = RuntimeBuilder::new(registry.clone(), Arc::new(NoModels))
        .store(store.clone())
        .artifacts(artifacts.clone())
        .projects(projects.clone())
        .memory_control(memory.clone())
        .authorizer(authz.clone())
        .config_hub(hub.clone())
        .build();
    let engine = EngineBuilder::new()
        .turn_runner(Arc::new(Idle))
        .build(&root);
    let services = Services {
        runtime: Some(runtime.runtime().clone()),
        store: Some(store.clone()),
        hub: Some(hub),
        artifacts: Some(artifacts),
        projects: Some(projects),
        memory_ctl: Some(memory),
        authz: Some(authz),
        ..Services::default()
    };
    let state = Arc::new(DaemonState::with_services(
        cfg,
        engine,
        TOKEN.into(),
        root,
        registry.clone(),
        Some(runtime.conversation()),
        Some(Arc::new(services)),
    ));
    Mem {
        state,
        store,
        registry,
        dir,
    }
}

struct Client {
    conn: Connection,
    rx: UnboundedReceiver<Incoming>,
}

async fn connect(m: &Mem) -> Client {
    connect_as(m, "desk").await
}

async fn connect_as(m: &Mem, instance: &str) -> Client {
    let (client, server) = genie_testkit::memory_pair();
    let st = m.state.clone();
    tokio::spawn(async move { serve_stream(st, Box::new(server), TaskScope::root()).await });
    let info = ClientInfo {
        kind: ClientKind::Desktop,
        version: "1.0.0".into(),
        instance_id: instance.into(),
        capabilities: ClientCapabilities::default(),
    };
    let conn = Connection::handshake(Box::new(client), &info, TOKEN.into())
        .await
        .unwrap();
    let rx = conn.take_incoming().unwrap();
    Client { conn, rx }
}

impl Client {
    async fn note(&mut self, method: &str) -> Notification {
        loop {
            let i = tokio::time::timeout(WAIT, self.rx.recv())
                .await
                .expect("timed out")
                .expect("closed");
            if let Incoming::Notification(n) = i
                && n.method == method
            {
                return n;
            }
        }
    }

    async fn listed(&mut self) -> SessionListed {
        serde_json::from_value(self.note(m::SessionListed::NAME).await.params).unwrap()
    }
}

fn rpc_kind(e: ClientError) -> ErrorKind {
    match e {
        ClientError::Rpc(r) => r.kind(),
        other => panic!("not an rpc error: {other:?}"),
    }
}

async fn chat(c: &Connection, kind: SessionKind) -> String {
    c.call::<m::SessionCreate>(CreateParams {
        kind,
        model: "anthropic/claude-test".into(),
        ..CreateParams::default()
    })
    .await
    .unwrap()
    .session_id
}

async fn project(c: &Connection, name: &str) -> String {
    c.call::<m::ProjectCreate>(ProjectCreateParams {
        name: name.into(),
        instructions: String::new(),
    })
    .await
    .unwrap()
    .project_id
}

async fn upload(c: &Connection, data: &[u8], mime: &str) -> String {
    let up = c
        .call::<m::BlobPutBegin>(BlobPutBeginParams {
            size: data.len() as u64,
            mime: Some(mime.into()),
            name: None,
        })
        .await
        .unwrap();
    c.call::<m::BlobPutChunk>(BlobPutChunkParams {
        upload_id: up.upload_id.clone(),
        offset: 0,
        data_b64: base64::engine::general_purpose::STANDARD.encode(data),
    })
    .await
    .unwrap();
    c.call::<m::BlobPutEnd>(UploadIdParams {
        upload_id: up.upload_id,
    })
    .await
    .unwrap()
    .blob_id
}

fn store_project(store: &genie_store::Store, session: &str) -> Option<String> {
    store
        .read(|c| {
            c.query_row(
                "SELECT project_id FROM session WHERE id = ?1",
                [session],
                |r| r.get::<_, Option<String>>(0),
            )
            .map_err(Into::into)
        })
        .unwrap()
}

#[tokio::test]
async fn move_a_chat_into_and_out_of_a_project() {
    let mem = daemon();
    let mut c = connect(&mem).await;
    c.conn
        .call::<m::WorkspaceSubscribe>(Empty {})
        .await
        .unwrap();
    let p = project(&c.conn, "Trips").await;
    let s = chat(&c.conn, SessionKind::Chat).await;
    assert_eq!(c.listed().await.change, ListedChange::Created);

    let into = |pid: &str| UpdateParams {
        session_id: s.clone(),
        project_id: Some(pid.into()),
        ..UpdateParams::default()
    };
    c.conn.call::<m::SessionUpdate>(into(&p)).await.unwrap();
    let l = c.listed().await;
    assert_eq!(l.change, ListedChange::Updated);
    assert_eq!(l.session.project_id.as_deref(), Some(p.as_str()));
    let listed = c
        .conn
        .call::<m::SessionList>(ListParams {
            project_id: Some(p.clone()),
            ..ListParams::default()
        })
        .await
        .unwrap();
    assert_eq!(listed.items.len(), 1);
    assert_eq!(store_project(&mem.store, &s).as_deref(), Some(p.as_str()));

    // Out again with "".
    c.conn.call::<m::SessionUpdate>(into("")).await.unwrap();
    let l = c.listed().await;
    assert_eq!(l.session.project_id, None);
    let got = c
        .conn
        .call::<m::SessionGet>(GetParams {
            session_id: s.clone(),
            leaf_id: None,
        })
        .await
        .unwrap();
    assert_eq!(got.session.summary.project_id, None);
    assert_eq!(store_project(&mem.store, &s), None);

    // Unknown project → not found; Code sessions don't belong to projects.
    let e = c.conn.call::<m::SessionUpdate>(into("prj_nope")).await;
    assert_eq!(rpc_kind(e.unwrap_err()), ErrorKind::NotFound);
    let code = chat(&c.conn, SessionKind::Code).await;
    let e = c
        .conn
        .call::<m::SessionUpdate>(UpdateParams {
            session_id: code,
            project_id: Some(p.clone()),
            ..UpdateParams::default()
        })
        .await
        .unwrap_err();
    assert_eq!(rpc_kind(e), ErrorKind::InvalidParams);
}

#[tokio::test]
async fn per_chat_capability_toggles() {
    let mem = daemon();
    let c = connect(&mem).await;
    let s = chat(&c.conn, SessionKind::Chat).await;
    let get = || async {
        c.conn
            .call::<m::SessionGet>(GetParams {
                session_id: s.clone(),
                leaf_id: None,
            })
            .await
            .unwrap()
            .session
    };
    let info = get().await;
    assert!(info.web_search, "web search defaults on");
    assert!(!info.code_execution, "code execution defaults off");
    c.conn
        .call::<m::SessionUpdate>(UpdateParams {
            session_id: s.clone(),
            web_search: Some(false),
            code_execution: Some(true),
            ..UpdateParams::default()
        })
        .await
        .unwrap();
    let info = get().await;
    assert!(!info.web_search);
    assert!(info.code_execution);
    // The engine sees the toggles through the session settings.
    let settings = genie_core::runtime::SettingsSource::settings(&*mem.registry, &s).unwrap();
    assert!(settings.web_search_off && settings.code_execution);
    // Toggles change how the session runs: they take the driver lease like `model`.
    let other = connect_as(&mem, "phone").await;
    let e = other
        .conn
        .call::<m::SessionUpdate>(UpdateParams {
            session_id: s.clone(),
            web_search: Some(true),
            ..UpdateParams::default()
        })
        .await
        .unwrap_err();
    assert_eq!(rpc_kind(e), ErrorKind::NotDriver);
}

#[tokio::test]
async fn project_files_are_listed() {
    let mem = daemon();
    let c = connect(&mem).await;
    let p = project(&c.conn, "Docs").await;
    let empty = c
        .conn
        .call::<m::ProjectFileList>(ProjectIdParams {
            project_id: p.clone(),
        })
        .await
        .unwrap();
    assert!(empty.files.is_empty());
    let text = b"# Notes\nThe launch is on Monday.\n";
    let blob = upload(&c.conn, text, "text/markdown").await;
    let f = c
        .conn
        .call::<m::ProjectFileAdd>(ProjectFileAddParams {
            project_id: p.clone(),
            blob_id: blob.clone(),
            name: "notes.md".into(),
        })
        .await
        .unwrap()
        .file_id;
    let list = c
        .conn
        .call::<m::ProjectFileList>(ProjectIdParams {
            project_id: p.clone(),
        })
        .await
        .unwrap();
    assert_eq!(list.files.len(), 1);
    let file = &list.files[0];
    assert_eq!(file.file_id, f);
    assert_eq!(file.name, "notes.md");
    assert_eq!(file.blob_id.as_deref(), Some(blob.as_str()));
    assert_eq!(file.size, Some(text.len() as u64));
    assert!(file.tokens.is_some_and(|t| t > 0));
    assert!(file.added_at.ends_with('Z'));
    let e = c
        .conn
        .call::<m::ProjectFileList>(ProjectIdParams {
            project_id: "prj_nope".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(rpc_kind(e), ErrorKind::NotFound);
}

#[tokio::test]
async fn trusted_folders_are_listed_and_revoked_by_path() {
    let mem = daemon();
    let c = connect(&mem).await;
    let a = mem.dir.path().join("work/a");
    let b = mem.dir.path().join("work/b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    for (p, d) in [(&a, TrustDecision::Trust), (&b, TrustDecision::Deny)] {
        c.conn
            .call::<m::TrustSet>(TrustSetParams {
                path: p.display().to_string(),
                decision: d,
            })
            .await
            .unwrap();
    }
    let list = c.conn.call::<m::TrustList>(Empty {}).await.unwrap();
    assert_eq!(list.items.len(), 2);
    let canon_a = std::fs::canonicalize(&a).unwrap().display().to_string();
    let ia = list.items.iter().find(|i| i.path == canon_a).unwrap();
    assert_eq!(ia.decision, TrustDecision::Trust);
    assert!(!ia.decided_at.is_empty());
    assert_eq!(ia.approvals, 0);
    assert!(list.items.iter().any(|i| i.decision == TrustDecision::Deny));

    // Revoke by the listed path, even after the folder is gone.
    std::fs::remove_dir_all(&a).unwrap();
    c.conn
        .call::<m::TrustSet>(TrustSetParams {
            path: canon_a.clone(),
            decision: TrustDecision::Revoke,
        })
        .await
        .unwrap();
    let list = c.conn.call::<m::TrustList>(Empty {}).await.unwrap();
    assert_eq!(list.items.len(), 1);
    let st = c
        .conn
        .call::<m::TrustGet>(PathParams { path: canon_a })
        .await
        .unwrap();
    assert_eq!(st.state, TrustState::Unknown);
}

#[tokio::test]
async fn saved_permission_rules_per_scope() {
    let mem = daemon();
    let c = connect(&mem).await;
    let home = mem.dir.path().join("home/.genie");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join("settings.json"),
        json!({"permissions": {"allow": ["Bash(npm test:*)"], "deny": ["WebFetch(domain:evil.test)"]}})
            .to_string(),
    )
    .unwrap();
    // A trusted Code workspace with a local rule.
    let ws = mem.dir.path().join("work/app");
    std::fs::create_dir_all(ws.join(".genie")).unwrap();
    std::fs::create_dir_all(ws.join(".git")).unwrap();
    std::fs::write(
        ws.join(".genie/settings.local.json"),
        json!({"permissions": {"allow": ["Read(./src/**)"]}}).to_string(),
    )
    .unwrap();
    c.conn
        .call::<m::TrustSet>(TrustSetParams {
            path: ws.display().to_string(),
            decision: TrustDecision::Trust,
        })
        .await
        .unwrap();
    let cwd = Some(ws.display().to_string());

    let all = c
        .conn
        .call::<m::PermissionRulesList>(PermissionRulesListParams {
            cwd: cwd.clone(),
            ..Default::default()
        })
        .await
        .unwrap()
        .rules;
    let find = |r: &str| all.iter().find(|x| x.rule == r).cloned().unwrap();
    let npm = find("Bash(npm test:*)");
    assert_eq!(
        (npm.scope, npm.class, npm.locked),
        (PermissionRuleScope::User, PermissionRuleClass::Allow, false)
    );
    assert!(npm.source.unwrap().ends_with("settings.json"));
    assert_eq!(
        find("WebFetch(domain:evil.test)").class,
        PermissionRuleClass::Deny
    );
    let local = find("Read(./src/**)");
    assert_eq!(local.scope, PermissionRuleScope::Local);
    assert!(local.source.unwrap().ends_with("settings.local.json"));

    // Filter by scope.
    let only_local = c
        .conn
        .call::<m::PermissionRulesList>(PermissionRulesListParams {
            scope: Some(PermissionRuleScope::Local),
            cwd: cwd.clone(),
            ..Default::default()
        })
        .await
        .unwrap()
        .rules;
    assert_eq!(only_local.len(), 1);

    // Session-only CLI rules are listed and locked.
    let s = c
        .conn
        .call::<m::SessionCreate>(CreateParams {
            kind: SessionKind::Code,
            model: "anthropic/claude-test".into(),
            workspace_path: cwd.clone(),
            settings: Some(SessionSettingsOverlay {
                deny: vec!["Bash(rm:*)".into()],
                ..Default::default()
            }),
            ..CreateParams::default()
        })
        .await
        .unwrap()
        .session_id;
    let with_session = c
        .conn
        .call::<m::PermissionRulesList>(PermissionRulesListParams {
            session_id: Some(s.clone()),
            ..Default::default()
        })
        .await
        .unwrap()
        .rules;
    let cli = with_session
        .iter()
        .find(|r| r.rule == "Bash(rm:*)")
        .unwrap();
    assert_eq!((cli.scope, cli.locked), (PermissionRuleScope::Cli, true));
    assert!(
        with_session.iter().any(|r| r.rule == "Read(./src/**)"),
        "the session's workspace layers apply without cwd"
    );
    let e = c
        .conn
        .call::<m::PermissionRulesRemove>(PermissionRuleRemoveParams {
            rule: "Bash(rm:*)".into(),
            class: PermissionRuleClass::Deny,
            scope: PermissionRuleScope::Cli,
            cwd: None,
            session_id: Some(s.clone()),
        })
        .await
        .unwrap_err();
    assert_eq!(rpc_kind(e), ErrorKind::Locked);

    // Remove a user rule and a local rule; the files change.
    c.conn
        .call::<m::PermissionRulesRemove>(PermissionRuleRemoveParams {
            rule: "Bash(npm test:*)".into(),
            class: PermissionRuleClass::Allow,
            scope: PermissionRuleScope::User,
            cwd: None,
            session_id: None,
        })
        .await
        .unwrap();
    let user: serde_json::Value =
        serde_json::from_slice(&std::fs::read(home.join("settings.json")).unwrap()).unwrap();
    assert_eq!(user["permissions"]["allow"], json!([]));
    assert_eq!(
        user["permissions"]["deny"],
        json!(["WebFetch(domain:evil.test)"])
    );
    c.conn
        .call::<m::PermissionRulesRemove>(PermissionRuleRemoveParams {
            rule: "Read(./src/**)".into(),
            class: PermissionRuleClass::Allow,
            scope: PermissionRuleScope::Local,
            cwd: cwd.clone(),
            session_id: None,
        })
        .await
        .unwrap();
    let left = c
        .conn
        .call::<m::PermissionRulesList>(PermissionRulesListParams {
            cwd: cwd.clone(),
            ..Default::default()
        })
        .await
        .unwrap()
        .rules;
    assert_eq!(left.len(), 1);
    // Removing again → not found; a session rule that doesn't exist → not found.
    let again = c
        .conn
        .call::<m::PermissionRulesRemove>(PermissionRuleRemoveParams {
            rule: "Bash(npm test:*)".into(),
            class: PermissionRuleClass::Allow,
            scope: PermissionRuleScope::User,
            cwd: None,
            session_id: None,
        })
        .await
        .unwrap_err();
    assert_eq!(rpc_kind(again), ErrorKind::NotFound);
    let e = c
        .conn
        .call::<m::PermissionRulesRemove>(PermissionRuleRemoveParams {
            rule: "Bash(ls)".into(),
            class: PermissionRuleClass::Allow,
            scope: PermissionRuleScope::Session,
            cwd: None,
            session_id: Some(s),
        })
        .await
        .unwrap_err();
    assert_eq!(rpc_kind(e), ErrorKind::NotFound);
}

#[tokio::test]
async fn delete_all_data_needs_a_token_and_leaves_an_empty_store() {
    let mem = daemon();
    let mut c = connect(&mem).await;
    c.conn
        .call::<m::WorkspaceSubscribe>(Empty {})
        .await
        .unwrap();
    c.note(m::WorkspaceSnapshot::NAME).await;
    let p = project(&c.conn, "Work").await;
    let blob = upload(&c.conn, b"Release notes for Monday.\n", "text/plain").await;
    c.conn
        .call::<m::ProjectFileAdd>(ProjectFileAddParams {
            project_id: p.clone(),
            blob_id: blob.clone(),
            name: "notes.txt".into(),
        })
        .await
        .unwrap();
    let s = chat(&c.conn, SessionKind::Chat).await;
    mem.store
        .write(|tx| {
            genie_store::memory::add(tx, "global", "Likes tea", None, None)?;
            genie_store::sessions::ensure(tx, &s)
        })
        .unwrap();
    let audit = mem.dir.path().join("audit");
    std::fs::create_dir_all(&audit).unwrap();
    std::fs::write(audit.join("audit.jsonl"), "{\"event\":\"x\"}\n").unwrap();
    std::fs::write(audit.join("audit.jsonl.1"), "{\"event\":\"y\"}\n").unwrap();
    assert!(mem.store.blob_files().exists(&blob));

    // Without the token from `begin` nothing happens.
    let e = c
        .conn
        .call::<m::DataDeleteAllConfirm>(DeleteAllParams {
            token: "guess".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(rpc_kind(e), ErrorKind::InvalidParams);
    assert_eq!(mem.store.data_counts().unwrap().projects, 1);

    let begin = c
        .conn
        .call::<m::DataDeleteAllBegin>(Empty {})
        .await
        .unwrap();
    assert_eq!(begin.sessions, 1);
    assert_eq!(begin.projects, 1);
    assert_eq!(begin.memories, 1);
    assert!(begin.blob_bytes > 0);
    assert!(begin.clears_audit);
    assert!(begin.expires_in_s > 0);
    let done = c
        .conn
        .call::<m::DataDeleteAllConfirm>(DeleteAllParams {
            token: begin.token.clone(),
        })
        .await
        .unwrap();
    assert_eq!(done.sessions_deleted, 1);
    assert!(done.audit_cleared);

    // The feed: the chat is gone, then a fresh (empty) snapshot.
    let l = loop {
        let l = c.listed().await;
        if l.change == ListedChange::Deleted {
            break l;
        }
    };
    assert_eq!(l.session.session_id, s);
    let snap: WorkspaceSnapshot =
        serde_json::from_value(c.note(m::WorkspaceSnapshot::NAME).await.params).unwrap();
    assert!(snap.sessions.is_empty());

    // Every table is empty except the migration journal and the blob key.
    let counts = mem
        .store
        .read(|cn| {
            let mut st = cn.prepare(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
                   AND sql NOT LIKE 'CREATE VIRTUAL%' AND name NOT LIKE '%_fts_%'",
            )?;
            let names: Vec<String> = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
            let mut out = vec![];
            for n in names {
                let k: i64 =
                    cn.query_row(&format!("SELECT COUNT(*) FROM \"{n}\""), [], |r| r.get(0))?;
                out.push((n, k));
            }
            Ok(out)
        })
        .unwrap();
    for (name, n) in counts {
        match name.as_str() {
            "migration" => assert!(n > 0),
            "kv" => {
                let keys: Vec<String> = mem
                    .store
                    .read(|cn| {
                        let mut st = cn.prepare("SELECT key FROM kv ORDER BY key")?;
                        let v = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
                        Ok(v)
                    })
                    .unwrap();
                assert_eq!(
                    keys,
                    ["blob_key", "maintenance.last_vacuum", "memory.state"],
                    "{n}"
                );
            }
            _ => assert_eq!(n, 0, "{name} not emptied"),
        }
    }
    let hits = mem
        .store
        .read(|cn| {
            genie_store::search::chat_search(cn, "Monday", &genie_store::search::Scope::All, 5)
        })
        .unwrap();
    assert!(hits.is_empty());
    assert!(!mem.store.blob_files().exists(&blob));
    assert!(!audit.join("audit.jsonl.1").exists());
    assert_eq!(std::fs::read(audit.join("audit.jsonl")).unwrap().len(), 0);

    // The daemon keeps serving on the empty store; the token was single use.
    let list = c
        .conn
        .call::<m::SessionList>(ListParams::default())
        .await
        .unwrap();
    assert!(list.items.is_empty());
    let projects = c.conn.call::<m::ProjectList>(Empty {}).await.unwrap();
    assert!(projects.projects.is_empty());
    let p2 = project(&c.conn, "Again").await;
    let b2 = upload(&c.conn, b"fresh", "text/plain").await;
    c.conn
        .call::<m::ProjectFileAdd>(ProjectFileAddParams {
            project_id: p2,
            blob_id: b2,
            name: "a.txt".into(),
        })
        .await
        .unwrap();
    let e = c
        .conn
        .call::<m::DataDeleteAllConfirm>(DeleteAllParams { token: begin.token })
        .await
        .unwrap_err();
    assert_eq!(rpc_kind(e), ErrorKind::InvalidParams);
    let _ = c
        .conn
        .call::<m::SessionGet>(GetParams {
            session_id: s.clone(),
            leaf_id: None,
        })
        .await
        .unwrap_err();
}
