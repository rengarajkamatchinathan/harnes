//! Artifacts, projects (full and retrieval mode), memory state, chat search and
//! attachments end to end: real engine, store, `Executor` and Anthropic adapter; only the
//! transport is a [`CassetteTransport`] and embeddings come from the offline
//! [`HashEmbedder`]. No network.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use std::sync::{Arc, Mutex};
use std::time::Duration;

use genie_core::bus::{SubItem, Subscription};
use genie_core::journal::StoreJournal;
use genie_core::runtime::artifacts::ArtifactHub;
use genie_core::runtime::attachments::Attachments;
use genie_core::runtime::memory_ctl::MemoryControl;
use genie_core::runtime::projects::{ProjectsConfig, ProjectsHub};
use genie_core::runtime::{
    ModelResolver, ResolveError, ResolvedModel, RuntimeBuilder, RuntimeConfig, SessionSettings,
    SettingsSource,
};
use genie_core::{Engine, EngineBuilder, SessionHandle, TaskScope};
use genie_protocol::api::project::IndexMode;
use genie_protocol::event::{SessionEvent, ToolOutcome, TurnStatus};
use genie_protocol::message::InputPart;
use genie_protocol::session::Delivery;
use genie_providers::auth::NoAuth;
use genie_providers::cassette::{CassetteTransport, RecordedResponse};
use genie_providers::compat::ModelSpec;
use genie_providers::embedding::HashEmbedder;
use genie_providers::model::{ProtocolKind, ProviderId};
use genie_providers::route::{Preset, PresetProvider, ProviderConfig, Route};
use genie_providers::{Executor, Provider, RetryPolicy};
use serde_json::{Value, json};

const WAIT: Duration = Duration::from_secs(10);

// ------------------------------------------------------------------ SSE builders

fn ev(name: &str, data: Value) -> String {
    format!("event: {name}\ndata: {data}\n\n")
}

fn msg_start() -> String {
    ev(
        "message_start",
        json!({"type":"message_start","message":{"id":"msg_x","type":"message","role":"assistant",
            "model":"claude-test","content":[],"stop_reason":null,
            "usage":{"input_tokens":100,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"output_tokens":1}}}),
    )
}

fn text_block(idx: u32, t: &str) -> String {
    ev(
        "content_block_start",
        json!({"type":"content_block_start","index":idx,"content_block":{"type":"text","text":""}}),
    ) + &ev(
        "content_block_delta",
        json!({"type":"content_block_delta","index":idx,"delta":{"type":"text_delta","text":t}}),
    ) + &ev(
        "content_block_stop",
        json!({"type":"content_block_stop","index":idx}),
    )
}

fn tool_block(idx: u32, id: &str, name: &str, input: &Value) -> String {
    ev(
        "content_block_start",
        json!({"type":"content_block_start","index":idx,"content_block":{"type":"tool_use","id":id,"name":name,"input":{}}}),
    ) + &ev(
        "content_block_delta",
        json!({"type":"content_block_delta","index":idx,"delta":{"type":"input_json_delta","partial_json":input.to_string()}}),
    ) + &ev(
        "content_block_stop",
        json!({"type":"content_block_stop","index":idx}),
    )
}

fn reply(blocks: &[String], stop: &str) -> RecordedResponse {
    let end = ev(
        "message_delta",
        json!({"type":"message_delta","delta":{"stop_reason":stop,"stop_sequence":null},"usage":{"output_tokens":20}}),
    ) + &ev("message_stop", json!({"type":"message_stop"}));
    RecordedResponse::sse(200, &(msg_start() + &blocks.concat() + &end), 64)
}

fn text(t: &str) -> RecordedResponse {
    reply(&[text_block(0, t)], "end_turn")
}

fn tool(id: &str, name: &str, input: Value) -> RecordedResponse {
    reply(&[tool_block(0, id, name, &input)], "tool_use")
}

// ------------------------------------------------------------------ harness

/// A resolver whose cassette can be replaced between turns (artifact ids are only known
/// after the turn that created them).
struct Resolver {
    route: Mutex<Route>,
    exec: Mutex<(Arc<CassetteTransport>, Arc<Executor>)>,
}

impl Resolver {
    fn new(window: u64, image: bool, pdf: bool) -> Arc<Self> {
        let r = Arc::new(Resolver {
            route: Mutex::new(Self::route("claude-test", window, image, pdf)),
            exec: Mutex::new(Self::exec(vec![])),
        });
        r.load(vec![]);
        r
    }

    fn route(wire: &str, window: u64, image: bool, pdf: bool) -> Route {
        let mut spec = ModelSpec::new("anthropic", wire, ProtocolKind::AnthropicMessages);
        spec.context_window = Some(window);
        spec.max_output = Some(1_024);
        spec.input.image = image;
        spec.input.pdf = pdf;
        PresetProvider(ProviderConfig {
            id: ProviderId::new("anthropic"),
            preset: Preset::Anthropic,
            base_url: None,
            project: None,
            location: None,
            auth: Arc::new(NoAuth),
            account: None,
            compat_overlay: None,
            headers: vec![],
        })
        .route(&spec)
        .unwrap()
    }

    fn exec(responses: Vec<RecordedResponse>) -> (Arc<CassetteTransport>, Arc<Executor>) {
        let t = Arc::new(CassetteTransport::new(responses));
        let e = Arc::new(Executor::new(
            t.clone(),
            RetryPolicy {
                base: Duration::from_millis(5),
                max_gap: Duration::from_millis(20),
                jitter: false,
                ..RetryPolicy::default()
            },
        ));
        (t, e)
    }

    fn load(&self, responses: Vec<RecordedResponse>) {
        *self.exec.lock().unwrap() = Self::exec(responses);
    }

    fn bodies(&self) -> Vec<Value> {
        self.exec
            .lock()
            .unwrap()
            .0
            .recorded()
            .iter()
            .map(|r| serde_json::from_str(&r.body).unwrap())
            .collect()
    }
}

impl ModelResolver for Resolver {
    fn resolve(&self, _model: &str) -> Result<ResolvedModel, ResolveError> {
        Ok(ResolvedModel {
            route: self.route.lock().unwrap().clone(),
            client: self.exec.lock().unwrap().1.clone(),
            display_name: "Claude Test".into(),
            provider_display_name: "Anthropic".into(),
            pricing: None,
            native_compaction: false,
        })
    }
}

#[derive(Default)]
struct DynSettings(Mutex<SessionSettings>);

impl SettingsSource for DynSettings {
    fn settings(&self, _id: &str) -> Option<SessionSettings> {
        Some(self.0.lock().unwrap().clone())
    }
}

struct H {
    _dir: tempfile::TempDir,
    _root: TaskScope,
    store: Arc<genie_store::Store>,
    models: Arc<Resolver>,
    settings: Arc<DynSettings>,
    engine: Engine,
    artifacts: Arc<ArtifactHub>,
    projects: Arc<ProjectsHub>,
    memory: Arc<MemoryControl>,
    s: SessionHandle,
    sub: Subscription,
}

fn harness(window: u64, image: bool, pdf: bool) -> H {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(
        genie_store::Store::open_with_keystore(
            &genie_store::StoreOptions::new(dir.path(), "genie.db"),
            &genie_store::MemoryKeyStore::new(),
        )
        .unwrap(),
    );
    let models = Resolver::new(window, image, pdf);
    let settings = Arc::new(DynSettings(Mutex::new(SessionSettings {
        model: "anthropic/claude-test".into(),
        ..SessionSettings::default()
    })));
    let root = TaskScope::root();
    let artifacts = ArtifactHub::new(Some(store.clone()));
    let projects = ProjectsHub::new(
        store.clone(),
        models.clone(),
        Some(Arc::new(HashEmbedder::default())),
        root.child("indexing"),
        ProjectsConfig::default(),
    );
    let memory = MemoryControl::new(Some(store.clone()));
    let runner = RuntimeBuilder::new(settings.clone(), models.clone())
        .config(RuntimeConfig {
            titles: false,
            ..RuntimeConfig::default()
        })
        .store(store.clone())
        .artifacts(artifacts.clone())
        .projects(projects.clone())
        .memory_control(memory.clone())
        .attachments(Attachments::new(store.clone()))
        .chat_search(store.clone())
        .build();
    let engine = EngineBuilder::new()
        .journal(Arc::new(StoreJournal::new(store.clone())))
        .turn_runner(Arc::new(runner))
        .build(&root);
    let s = engine.create_session();
    let sub = engine.bus().subscribe(s.id(), 0).unwrap();
    H {
        _dir: dir,
        _root: root,
        store,
        models,
        settings,
        engine,
        artifacts,
        projects,
        memory,
        s,
        sub,
    }
}

impl H {
    async fn turn(
        &mut self,
        responses: Vec<RecordedResponse>,
        parts: Vec<InputPart>,
    ) -> Vec<SessionEvent> {
        self.models.load(responses);
        self.s.prompt_parts(parts, Delivery::Steer).unwrap();
        let mut out = vec![];
        loop {
            let item = tokio::time::timeout(WAIT, self.sub.next())
                .await
                .expect("turn did not settle")
                .expect("subscription ended");
            if let SubItem::Event(e) = item {
                let w = e.to_wire().event;
                let done = matches!(w, SessionEvent::TurnSettled(_));
                out.push(w);
                if done {
                    return out;
                }
            }
        }
    }

    async fn say(&mut self, responses: Vec<RecordedResponse>, t: &str) -> Vec<SessionEvent> {
        self.turn(responses, vec![InputPart::text(t)]).await
    }

    fn project(&self, name: &str, instructions: &str) -> String {
        let p = self
            .store
            .write(|tx| genie_store::projects::create(tx, name, Some(instructions)))
            .unwrap();
        self.settings.0.lock().unwrap().project_id = Some(p.id.clone());
        p.id
    }

    fn file(&self, project: &str, name: &str, body: &str) -> String {
        let blob = self
            .store
            .put_blob(body.as_bytes(), Some("text/markdown"))
            .unwrap();
        self.projects.add_file(project, &blob, name).unwrap().id
    }
}

fn settled_ok(ev: &[SessionEvent]) {
    match ev.last() {
        Some(SessionEvent::TurnSettled(t)) => {
            assert_eq!(t.status, TurnStatus::Completed, "{t:?}");
        }
        other => panic!("not settled: {other:?}"),
    }
}

fn versioned(ev: &[SessionEvent]) -> Vec<genie_protocol::event::ArtifactVersioned> {
    ev.iter()
        .filter_map(|e| match e {
            SessionEvent::ArtifactVersioned(a) => Some(a.clone()),
            _ => None,
        })
        .collect()
}

fn tool_names(body: &Value) -> Vec<String> {
    body["tools"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|t| t["name"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn last_tool_result(body: &Value) -> String {
    let msgs = body["messages"].as_array().unwrap();
    for m in msgs.iter().rev() {
        if let Some(parts) = m["content"].as_array() {
            for p in parts {
                if p["type"] == "tool_result" {
                    return p["content"].to_string();
                }
            }
        }
    }
    String::new()
}

// ------------------------------------------------------------------ artifacts

#[tokio::test]
async fn artifact_create_update_versions_follow_the_branch() {
    let mut h = harness(200_000, false, false);
    let ev = h
        .say(
            vec![
                tool(
                    "toolu_a",
                    "artifact_create",
                    json!({"type":"document","title":"Trip plan","content":"# Trip\n\nDay one: Rome"}),
                ),
                text("Here is the plan."),
            ],
            "plan my trip",
        )
        .await;
    settled_ok(&ev);
    let v = versioned(&ev);
    assert_eq!(v.len(), 1, "{ev:?}");
    assert_eq!(
        (v[0].n, v[0].artifact_type.as_str(), v[0].title.as_str()),
        (1, "document", "Trip plan")
    );
    let id = v[0].artifact_id.clone();
    assert!(id.starts_with("art_"));
    // No permission ask for artifacts; tool ended ok; the model saw the id.
    assert!(
        ev.iter()
            .all(|e| !matches!(e, SessionEvent::PermissionAsked(_)))
    );
    assert!(last_tool_result(&h.models.bodies()[1]).contains(&id));
    // The base prompt carries the artifacts guidance with the library list.
    let sys = h.models.bodies()[0]["system"].to_string();
    assert!(sys.contains("Recharts"), "{sys}");
    let first_leaf = v[0].message_id.clone();

    let ev = h
        .say(
            vec![
                tool(
                    "toolu_b",
                    "artifact_update",
                    json!({"id": id, "old_str": "Day one: Rome", "new_str": "Day one: Florence"}),
                ),
                text("Updated."),
            ],
            "Florence instead",
        )
        .await;
    settled_ok(&ev);
    let v2 = versioned(&ev);
    assert_eq!(v2.len(), 1);
    assert_eq!(v2[0].n, 2);
    let (_, ver) = h.artifacts.get(&id, 2).unwrap().unwrap();
    assert_eq!(ver.content.as_deref(), Some("# Trip\n\nDay one: Florence"));
    let list = h.artifacts.list(h.s.id(), None).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].1, vec![1, 2]);
    // The branch before the update only sees v1 (D28).
    let list = h.artifacts.list(h.s.id(), Some(&first_leaf)).unwrap();
    assert_eq!(list[0].1, vec![1]);

    // A failing update (text not found) is a model-readable error, no new version.
    let ev = h
        .say(
            vec![
                tool(
                    "toolu_c",
                    "artifact_update",
                    json!({"id": id, "old_str": "Venice", "new_str": "x"}),
                ),
                text("Sorry."),
            ],
            "edit",
        )
        .await;
    settled_ok(&ev);
    assert!(versioned(&ev).is_empty());
    let ended: Vec<ToolOutcome> = ev
        .iter()
        .filter_map(|e| match e {
            SessionEvent::ToolEnded(t) => Some(t.outcome),
            _ => None,
        })
        .collect();
    assert_eq!(ended, vec![ToolOutcome::Error]);
    // Rows survive in the store with the version tied to its message.
    let rows = h
        .store
        .read(|c| genie_store::artifacts::versions(c, &id))
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].message_id, first_leaf);
}

// ------------------------------------------------------------------ projects

#[tokio::test]
async fn project_knowledge_full_mode_is_in_the_baseline() {
    let mut h = harness(200_000, false, false);
    let p = h.project("Atlas", "Answer in British English.");
    h.file(
        &p,
        "glossary.md",
        "# Glossary\n\nFlux capacitor: the thing that makes time travel possible.",
    );
    let st = h.projects.status(&p).unwrap();
    assert_eq!(st.mode, IndexMode::Full);
    assert_eq!(st.files, 1);
    let ev = h
        .say(vec![text("Noted.")], "what is a flux capacitor?")
        .await;
    settled_ok(&ev);
    let body = &h.models.bodies()[0];
    let sys = body["system"].to_string();
    assert!(
        sys.contains("<project_instructions project=\\\"Atlas\\\">"),
        "{sys}"
    );
    assert!(sys.contains("<project_knowledge>"));
    assert!(sys.contains("Flux capacitor: the thing"));
    // Full mode: no excerpts on the message, no project_search tool.
    assert!(!body["messages"].to_string().contains("<project_excerpts>"));
    assert!(!tool_names(body).contains(&"project_search".to_string()));
}

#[tokio::test]
async fn project_knowledge_retrieval_mode_attaches_excerpts_and_offers_search() {
    // A 40k window: a ~25k-token project is over 50 % → retrieval.
    let mut h = harness(40_000, false, false);
    let p = h.project("Handbook", "");
    let mut doc = String::from("# Handbook\n\n");
    for i in 0..45 {
        doc.push_str(&format!(
            "## Topic {i}\n\nRoutine notes about office plants and weekly chores number {i}.\n\n"
        ));
        doc.push_str(&"Filler sentence about nothing in particular. ".repeat(40));
        doc.push_str("\n\n");
    }
    doc.push_str("## Holidays\n\nThe zeppelin parking policy allows two zeppelins per team.\n");
    h.file(&p, "handbook.md", &doc);
    h.projects.wait_indexed(&p).await;
    let (chunks, _) = h
        .store
        .read(|c| genie_store::retrieval::project_stats(c, &p))
        .unwrap();
    assert!(chunks > 3, "chunked: {chunks}");
    let missing = h
        .store
        .read(|c| genie_store::retrieval::missing_embeddings(c, &p, "genie/hash-256"))
        .unwrap();
    assert!(missing.is_empty(), "every chunk embedded");
    assert_eq!(
        h.projects.status(&p).unwrap().mode,
        IndexMode::Full,
        "status uses 200k"
    );

    let ev = h
        .say(
            vec![
                tool(
                    "toolu_s",
                    "project_search",
                    json!({"query": "zeppelin parking"}),
                ),
                text("Two per team."),
            ],
            "what is the zeppelin parking policy?",
        )
        .await;
    settled_ok(&ev);
    let bodies = h.models.bodies();
    let sys = bodies[0]["system"].to_string();
    assert!(
        !sys.contains("<project_knowledge>"),
        "retrieval mode keeps knowledge out of the baseline"
    );
    assert!(
        sys.contains("project_search"),
        "feature.project_search guidance"
    );
    let msgs = bodies[0]["messages"].to_string();
    assert!(msgs.contains("<project_excerpts>"), "{msgs}");
    assert!(msgs.contains("two zeppelins per team"));
    assert!(msgs.contains("file=\\\"handbook.md\\\""));
    assert!(tool_names(&bodies[0]).contains(&"project_search".to_string()));
    let result = last_tool_result(&bodies[1]);
    assert!(result.contains("zeppelin"), "{result}");
    assert!(result.contains("lines="));
}

#[tokio::test]
async fn capacity_cap_and_unknown_files_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(
        genie_store::Store::open_with_keystore(
            &genie_store::StoreOptions::new(dir.path(), "genie.db"),
            &genie_store::MemoryKeyStore::new(),
        )
        .unwrap(),
    );
    let hub = ProjectsHub::new(
        store.clone(),
        Resolver::new(1_000, false, false),
        None,
        TaskScope::root(),
        ProjectsConfig {
            capacity_window: 100,
            max_files: 2,
            ..ProjectsConfig::default()
        },
    );
    let p = store
        .write(|tx| genie_store::projects::create(tx, "P", None))
        .unwrap();
    assert_eq!(hub.cap_tokens(), 1_000);
    let small = store.put_blob(b"hello world", Some("text/plain")).unwrap();
    hub.add_file(&p.id, &small, "a.txt").unwrap();
    let big = store
        .put_blob("x ".repeat(4_000).as_bytes(), Some("text/plain"))
        .unwrap();
    let e = hub.add_file(&p.id, &big, "big.txt").unwrap_err();
    assert!(
        matches!(e, genie_core::runtime::projects::ProjectError::Capacity(_)),
        "{e:?}"
    );
    let bin = store
        .put_blob(&[0, 1, 2, 3], Some("application/zip"))
        .unwrap();
    assert!(matches!(
        hub.add_file(&p.id, &bin, "a.zip").unwrap_err(),
        genie_core::runtime::projects::ProjectError::Invalid(_)
    ));
    hub.add_file(&p.id, &small, "b.txt").unwrap();
    assert!(matches!(
        hub.add_file(&p.id, &small, "c.txt").unwrap_err(),
        genie_core::runtime::projects::ProjectError::Capacity(_)
    ));
    let st = hub.status(&p.id).unwrap();
    assert_eq!(st.files, 2);
    assert!(st.capacity_pct > 0.0 && !st.indexing);
}

// ------------------------------------------------------------------ memory

#[tokio::test]
async fn memory_paused_hides_tools_and_prompt_memory() {
    let mut h = harness(200_000, false, false);
    h.store
        .write(|tx| genie_store::memory::add(tx, "global", "Prefers metric units", None, None))
        .unwrap();
    let ev = h.say(vec![text("ok")], "hi").await;
    settled_ok(&ev);
    let body = &h.models.bodies()[0];
    assert!(
        body["system"]
            .to_string()
            .contains("<memory scope=\\\"global\\\">")
    );
    assert!(body["system"].to_string().contains("Prefers metric units"));
    assert!(tool_names(body).contains(&"memory_save".to_string()));

    h.memory.set_on(false).unwrap();
    let ev = h
        .say(
            vec![
                tool("toolu_m", "memory_save", json!({"text": "Likes jazz"})),
                text("ok"),
            ],
            "remember jazz",
        )
        .await;
    settled_ok(&ev);
    let bodies = h.models.bodies();
    assert!(!tool_names(&bodies[0]).contains(&"memory_save".to_string()));
    // The baseline is frozen; pausing removes memory through a <system-update>.
    assert!(
        bodies[0]["messages"]
            .to_string()
            .contains("Removed memory:global")
    );
    assert!(
        !ev.iter()
            .any(|e| matches!(e, SessionEvent::MemoryChanged(_)))
    );
    let all = h.store.read(genie_store::memory::all).unwrap();
    assert_eq!(all.len(), 1, "nothing saved while paused");
    // The state survives a restart.
    assert!(!MemoryControl::new(Some(h.store.clone())).is_on());
    h.memory.set_on(true).unwrap();
    assert!(MemoryControl::new(Some(h.store.clone())).is_on());
}

// ------------------------------------------------------------------ chat search

#[tokio::test]
async fn chat_search_finds_other_chats_in_scope() {
    let mut h = harness(200_000, false, false);
    let ev = h
        .say(
            vec![text("Noted the quokka.")],
            "my favourite animal is the quokka",
        )
        .await;
    settled_ok(&ev);
    // A second chat searches the first one.
    let s2 = h.engine.create_session();
    let mut sub2 = h.engine.bus().subscribe(s2.id(), 0).unwrap();
    h.models.load(vec![
        tool("toolu_q", "chat_search", json!({"query": "quokka"})),
        text("You like quokkas."),
    ]);
    s2.prompt("what animal do I like?", Delivery::Steer)
        .unwrap();
    loop {
        let Some(SubItem::Event(e)) = tokio::time::timeout(WAIT, sub2.next()).await.unwrap() else {
            continue;
        };
        if matches!(e.to_wire().event, SessionEvent::TurnSettled(_)) {
            break;
        }
    }
    let bodies = h.models.bodies();
    assert!(tool_names(&bodies[0]).contains(&"chat_search".to_string()));
    assert!(
        bodies[0]["system"]
            .to_string()
            .contains("Search past chats")
    );
    let result = last_tool_result(&bodies[1]);
    assert!(result.contains(h.s.id()), "{result}");
    assert!(result.contains("quokka"));
    assert!(!result.contains(s2.id()), "the current chat is excluded");
}

// ------------------------------------------------------------------ attachments

#[tokio::test]
async fn attachments_text_image_and_pdf_reach_the_model() {
    let mut h = harness(200_000, true, false);
    let txt = h
        .store
        .put_blob(b"col_a,col_b\n1,2\n", Some("text/csv"))
        .unwrap();
    let png = h
        .store
        .put_blob(b"\x89PNG\r\n\x1a\nfakeimage", Some("image/png"))
        .unwrap();
    let pdf = h
        .store
        .put_blob(&genie_core_pdf(), Some("application/pdf"))
        .unwrap();
    let ev = h
        .turn(
            vec![text("Got them.")],
            vec![
                InputPart::text("see attached"),
                InputPart::Attachment {
                    blob_id: txt,
                    name: "data.csv".into(),
                    mime: "text/csv".into(),
                },
                InputPart::Attachment {
                    blob_id: png.clone(),
                    name: "pic.png".into(),
                    mime: "image/png".into(),
                },
                InputPart::Attachment {
                    blob_id: pdf,
                    name: "report.pdf".into(),
                    mime: "application/pdf".into(),
                },
            ],
        )
        .await;
    settled_ok(&ev);
    let msgs = h.models.bodies()[0]["messages"].to_string();
    assert!(msgs.contains("<attachment name=\\\"data.csv\\\""), "{msgs}");
    assert!(msgs.contains("col_a,col_b"));
    assert!(msgs.contains("\"type\":\"image\""), "{msgs}");
    assert!(msgs.contains("\"base64\""));
    assert!(
        msgs.contains("Quarterly revenue grew"),
        "pdf text fallback: {msgs}"
    );
    // The image blob is referenced by the stored part (kept alive by its refcount).
    assert_eq!(h.store.blob_row(&png).unwrap().unwrap().refcount, 1);

    // A model without vision gets a note instead of the image.
    // (Another model: same-route history would be replayed byte-exact, 03 §4.)
    *h.models.route.lock().unwrap() = Resolver::route("claude-text-only", 200_000, false, false);
    let ev = h.say(vec![text("ok")], "and now?").await;
    settled_ok(&ev);
    let msgs = h.models.bodies()[0]["messages"].to_string();
    assert!(msgs.contains("can't read images"), "{msgs}");
    assert!(!msgs.contains("\"type\":\"image\""));
}

/// A one-page PDF with an uncompressed content stream.
fn genie_core_pdf() -> Vec<u8> {
    let content = "BT /F1 12 Tf 72 712 Td (Quarterly revenue grew 12%.) Tj ET";
    format!(
        "%PDF-1.4\n1 0 obj << /Type /Catalog >> endobj\n4 0 obj << /Length {} >>\nstream\n{content}\nendstream endobj\n%%EOF\n",
        content.len()
    )
    .into_bytes()
}
