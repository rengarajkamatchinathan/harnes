//! `export.all`, `session.import`, `import.scan/preview/apply`, `agents.list` over the
//! in-memory transport with a real store.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use std::sync::Arc;

use futures::future::BoxFuture;
use genie_client::Endpoint;
use genie_client::connection::{ClientError, ClientInfo, Connection};
use genie_core::{EngineBuilder, TaskScope, TurnCtx, TurnError, TurnRunner};
use genie_daemon::{DaemonConfig, DaemonState, Registry, Services, serve_stream};
use genie_protocol::api::code::{ExportAllParams, ImportParams};
use genie_protocol::api::config::CwdParams;
use genie_protocol::api::session::{AgentSourceKind, ListParams, SessionImportParams};
use genie_protocol::handshake::{ClientCapabilities, ClientKind};
use genie_protocol::methods as m;
use genie_protocol::{Channel, ErrorKind};
use genie_store::messages::{self, Message, Part};
use genie_store::sessions::{self, NewSession};
use serde_json::{Value, json};

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

struct Idle;

impl TurnRunner for Idle {
    fn run(&self, _ctx: TurnCtx) -> BoxFuture<'static, Result<(), TurnError>> {
        Box::pin(async { Ok(()) })
    }
}

async fn daemon(dir: &std::path::Path) -> (Arc<genie_store::Store>, Connection) {
    let mut cfg = DaemonConfig::new(Endpoint::in_dir(dir, Channel::Dev).unwrap());
    cfg.engine_version = "1.0.0".into();
    cfg.write_logs = false;
    let store = Arc::new(
        genie_store::Store::open_with_keystore(
            &genie_store::StoreOptions::new(dir.join("data"), "genie.db"),
            &genie_store::MemoryKeyStore::new(),
        )
        .unwrap(),
    );
    let root = TaskScope::root();
    let engine = EngineBuilder::new()
        .turn_runner(Arc::new(Idle))
        .build(&root);
    let services = Services {
        store: Some(store.clone()),
        ..Services::default()
    };
    let state = Arc::new(DaemonState::with_services(
        cfg,
        engine,
        TOKEN.into(),
        root,
        Arc::new(Registry::default()),
        None,
        Some(Arc::new(services)),
    ));
    let (client, server) = genie_testkit::memory_pair();
    tokio::spawn(async move { serve_stream(state, Box::new(server), TaskScope::root()).await });
    let info = ClientInfo {
        kind: ClientKind::Desktop,
        version: "1.0.0".into(),
        instance_id: "desk".into(),
        capabilities: ClientCapabilities::default(),
    };
    let c = Connection::handshake(Box::new(client), &info, TOKEN.into())
        .await
        .unwrap();
    (store, c)
}

fn rpc_kind(e: ClientError) -> ErrorKind {
    match e {
        ClientError::Rpc(r) => r.kind(),
        other => panic!("not an rpc error: {other:?}"),
    }
}

fn seed(store: &genie_store::Store) -> String {
    store
        .write(|tx| {
            let s = sessions::create(
                tx,
                &NewSession {
                    title: Some("Exported".into()),
                    model: Some("anthropic/claude-test".into()),
                    ..Default::default()
                },
            )?;
            let u = Message::new(&s.id, None, "user");
            messages::insert(tx, &u)?;
            messages::insert_part(tx, &Part::text(&u.id, 0, "hi"))?;
            let a = Message::new(&s.id, Some(&u.id), "assistant");
            messages::insert(tx, &a)?;
            messages::insert_part(tx, &Part::text(&a.id, 0, "hello"))?;
            sessions::set_active_leaf(tx, &s.id, Some(&a.id))?;
            Ok(s.id)
        })
        .unwrap()
}

#[tokio::test]
async fn export_all_then_import_a_session_from_it() {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (store, c) = daemon(&base).await;
    let sid = seed(&store);

    let zip_path = base.join("export.zip");
    let res = c
        .call::<m::ExportAll>(ExportAllParams {
            path: zip_path.display().to_string(),
            include_blobs: true,
        })
        .await
        .unwrap();
    assert_eq!(res.sessions, 1);
    assert_eq!(res.path, zip_path.display().to_string());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&zip_path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "plaintext export is private");
    }
    // Relative paths are refused.
    let e = c
        .call::<m::ExportAll>(ExportAllParams {
            path: "rel.zip".into(),
            include_blobs: false,
        })
        .await
        .unwrap_err();
    assert_eq!(rpc_kind(e), ErrorKind::InvalidParams);

    // Pull the session JSON out of the zip and import it back: ids collide → remapped.
    let mut z = zip::ZipArchive::new(std::fs::File::open(&zip_path).unwrap()).unwrap();
    let json_path = base.join("session.json");
    {
        let mut f = z.by_name(&format!("sessions/{sid}.json")).unwrap();
        let mut out = std::fs::File::create(&json_path).unwrap();
        std::io::copy(&mut f, &mut out).unwrap();
    }
    let imp = c
        .call::<m::SessionImport>(SessionImportParams {
            path: Some(json_path.display().to_string()),
            blob_id: None,
        })
        .await
        .unwrap();
    assert!(imp.remapped);
    assert_ne!(imp.session_id, sid);
    assert_eq!(imp.messages, 2);
    let list = c
        .call::<m::SessionList>(ListParams::default())
        .await
        .unwrap();
    let item = list
        .items
        .iter()
        .find(|s| s.session_id == imp.session_id)
        .expect("imported session is listed");
    assert_eq!(item.title.as_deref(), Some("Exported"));
    let path = store
        .read(|cn| messages::active_path(cn, &imp.session_id))
        .unwrap();
    assert_eq!(path.len(), 2);

    // Not a session export.
    std::fs::write(base.join("bad.json"), b"[1,2]").unwrap();
    let e = c
        .call::<m::SessionImport>(SessionImportParams {
            path: Some(base.join("bad.json").display().to_string()),
            blob_id: None,
        })
        .await
        .unwrap_err();
    assert_eq!(rpc_kind(e), ErrorKind::InvalidParams);
}

#[tokio::test]
async fn config_import_scan_preview_apply_and_agents() {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (_store, c) = daemon(&base.join("daemon")).await;
    let home = base.join("home");
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::write(
        home.join(".claude/settings.json"),
        r#"{"permissions": {"allow": ["Bash(ls:*)"], "deny": ["Read(./.env)"]}}"#,
    )
    .unwrap();
    let proj = base.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    let opts = json!({"home": home, "cwd": proj, "source": "claude-code"});

    let scan = c
        .call::<m::ImportScan>(ImportParams {
            options: opts.clone(),
        })
        .await
        .unwrap();
    let items = scan.report["items"].as_array().unwrap();
    assert!(!items.is_empty());
    assert!(items.iter().all(|i| i["source"] == "claude-code"));

    let preview = c
        .call::<m::ImportPreview>(ImportParams {
            options: opts.clone(),
        })
        .await
        .unwrap();
    let settings = preview.report["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == "settings" && i["level"] == "user")
        .cloned()
        .expect("user settings item");
    assert_eq!(settings["status"], "import");
    let id = settings["id"].as_str().unwrap().to_string();
    // Nothing written by a preview.
    assert!(!home.join(".genie/settings.json").exists());

    let mut apply_opts = opts.clone();
    apply_opts["selected"] = json!([id]);
    let applied = c
        .call::<m::ImportApply>(ImportParams {
            options: apply_opts,
        })
        .await
        .unwrap();
    let report = &applied.report;
    assert!(report["path"].as_str().unwrap().contains("imports"));
    let it = report["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == id.as_str())
        .unwrap();
    assert_eq!(it["applied"], true);
    let written: Value =
        serde_json::from_str(&std::fs::read_to_string(home.join(".genie/settings.json")).unwrap())
            .unwrap();
    assert!(written["permissions"]["allow"]
        .as_array()
        .unwrap()
        .contains(&json!("Bash(ls:*)")));

    // Idempotent: the same item is now skipped.
    let again = c
        .call::<m::ImportPreview>(ImportParams { options: opts })
        .await
        .unwrap();
    let again_item = again.report["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == id.as_str())
        .cloned()
        .unwrap();
    assert_eq!(again_item["status"], "skip");

    // Bad options are refused.
    let e = c
        .call::<m::ImportScan>(ImportParams {
            options: json!({"source": "cursor"}),
        })
        .await
        .unwrap_err();
    assert_eq!(rpc_kind(e), ErrorKind::InvalidParams);

    // agents.list: built-ins (hidden ones left out).
    let agents = c
        .call::<m::AgentsList>(CwdParams { cwd: None })
        .await
        .unwrap();
    let names: Vec<&str> = agents.agents.iter().map(|a| a.name.as_str()).collect();
    assert!(names.contains(&"general") && names.contains(&"explore") && names.contains(&"plan"));
    assert!(!names.contains(&"title") && !names.contains(&"compaction"));
    assert!(agents.agents.iter().all(|a| a.source == AgentSourceKind::Builtin));
}
