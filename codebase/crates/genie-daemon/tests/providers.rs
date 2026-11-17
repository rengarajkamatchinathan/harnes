//! Secrets and provider methods (02 §2 `locked`, §6.1, §6.5): credentials, status feed,
//! redaction, discovery + overrides, Entra device-code sign-in against a fake local
//! token endpoint, usage + budget, catalog refresh, the locked phase over the real
//! endpoint. Never the real OS keychain and never real Microsoft / Google endpoints.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use ed25519_dalek::Signer;
use genie_catalog::refresh::{FeedFetcher, FeedResponse};
use genie_client::connection::{ClientError, ClientInfo, Connection, Incoming};
use genie_client::{Endpoint, EnsureOptions, EnsureOutcome, ensure};
use genie_core::config::ConfigHub;
use genie_core::runtime::ModelResolver;
use genie_core::{EngineBuilder, TaskScope};
use genie_daemon::{
    DaemonConfig, DaemonState, Feed, ProviderHub, Secrets, SecretsChoice, Services, serve_stream,
};
use genie_protocol::api::daemon::UnlockParams;
use genie_protocol::api::provider::{
    BudgetSetParams, CatalogSource, CredentialAddParams, CredentialIdParams, CredentialKind,
    LoginMethod, LoginStartParams, LoginState, LoginStatusParams, ModelListParams, ProviderParams,
    ProviderStatus, UsageGroup, UsageQueryParams,
};
use genie_protocol::common::Empty;
use genie_protocol::handshake::{ClientCapabilities, ClientKind};
use genie_protocol::methods as m;
use genie_protocol::methods::Method;
use genie_protocol::{Channel, ErrorKind, Notification};
use genie_secrets::vault::KdfParams;
use genie_secrets::{MemoryBackend, SecretBackend};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc::UnboundedReceiver;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const WAIT: Duration = Duration::from_secs(10);
const OPENAI_KEY: &str = "sk-openai-test-key-1234";

// ---- a fake provider + Entra endpoint ------------------------------------------------

struct Fake {
    base: String,
}

async fn read_req(sock: &mut tokio::net::TcpStream) -> (String, HashMap<String, String>, String) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = sock.read(&mut chunk).await.unwrap_or(0);
        if n == 0 {
            return (String::new(), HashMap::new(), String::new());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..pos]).to_string();
            let mut lines = head.lines();
            let first = lines.next().unwrap_or("").to_string();
            let headers: HashMap<String, String> = lines
                .filter_map(|l| l.split_once(':'))
                .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
                .collect();
            let len: usize = headers
                .get("content-length")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            while buf.len() < pos + 4 + len {
                let n = sock.read(&mut chunk).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            let body = String::from_utf8_lossy(&buf[pos + 4..]).to_string();
            return (first, headers, body);
        }
    }
}

impl Fake {
    async fn start() -> Fake {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://127.0.0.1:{}", l.local_addr().unwrap().port());
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = l.accept().await.unwrap();
                tokio::spawn(async move {
                    let (first, h, _body) = read_req(&mut sock).await;
                    let path = first.split(' ').nth(1).unwrap_or("").to_string();
                    let auth = h.get("authorization").cloned().unwrap_or_default();
                    let (status, v) = match path.as_str() {
                        p if p.ends_with("/oauth2/v2.0/devicecode") => (
                            200,
                            json!({"device_code": "dev", "user_code": "WXYZ-1234",
                                "verification_uri": "https://microsoft.com/devicelogin",
                                "expires_in": 600, "interval": 1}),
                        ),
                        p if p.ends_with("/oauth2/v2.0/token") => (
                            200,
                            json!({"access_token": "at-entra", "refresh_token": "rt",
                                "token_type": "Bearer", "expires_in": 3600}),
                        ),
                        "/openai/v1/models" if auth == "Bearer at-entra" => {
                            (200, json!({"data": []}))
                        }
                        "/v1/models" if auth == format!("Bearer {OPENAI_KEY}") => {
                            (200, json!({"data": [{"id": "gpt-local-x"}]}))
                        }
                        _ => (401, json!({"error": "unauthorized"})),
                    };
                    let b = v.to_string();
                    let resp = format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}",
                        b.len()
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        Fake { base }
    }
}

// ---- in-memory daemon ------------------------------------------------------------------

struct Mem {
    state: Arc<DaemonState>,
    hub: Arc<ProviderHub>,
    store: Arc<genie_store::Store>,
    _dir: tempfile::TempDir,
}

struct StaticFeed(Vec<u8>, String);

#[async_trait]
impl FeedFetcher for StaticFeed {
    async fn fetch(&self, _url: &str, _etag: Option<&str>) -> Result<FeedResponse, String> {
        Ok(FeedResponse::Body {
            catalog: self.0.clone(),
            signature: self.1.clone(),
            etag: Some("\"v2\"".into()),
        })
    }
}

fn signed_feed() -> Feed {
    let key = ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]);
    let body = genie_catalog::BUNDLED
        .replacen("\"version\": 20261117", "\"version\": 20991231", 1)
        .into_bytes();
    let sig = base64::engine::general_purpose::STANDARD.encode(key.sign(&body).to_bytes());
    Feed {
        url: "https://feed.invalid/catalog.json".into(),
        fetcher: Arc::new(StaticFeed(body, sig)),
        keys: vec![key.verifying_key()],
    }
}

fn daemon(fake: &Fake, secrets: SecretsChoice, policy: Option<Value>) -> Mem {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home/.genie");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join("settings.json"),
        json!({
            "providers": {
                "openai": {"base_url": fake.base},
                "azure-foundry": {"base_url": fake.base, "client_id": "genie-test",
                                  "authority": fake.base}
            },
            "models": {"anthropic/claude-opus-5-5": {"context_window": 123456}}
        })
        .to_string(),
    )
    .unwrap();
    let mut cfg = DaemonConfig::new(Endpoint::in_dir(dir.path(), Channel::Dev).unwrap());
    cfg.engine_version = "1.0.0".into();
    cfg.write_logs = false;
    let hub = Arc::new(
        ConfigHub::new(
            home.clone(),
            home.join("trust.json"),
            Some(dir.path().join("home").display().to_string()),
        )
        .unwrap(),
    );
    if let Some(p) = policy {
        let vals: genie_config::policy::PolicyValues = serde_json::from_value(p).unwrap();
        hub.set_policy(genie_config::policy::merge(&[(
            genie_config::policy::PolicySourceKind::File,
            &vals,
        )]));
    }
    let store = Arc::new(
        genie_store::Store::open_with_keystore(
            &genie_store::StoreOptions::new(dir.path().join("data"), "genie-dev.db"),
            &genie_store::MemoryKeyStore::new(),
        )
        .unwrap(),
    );
    let secrets = Secrets::open(&secrets, &cfg.endpoint);
    let providers = Arc::new(
        ProviderHub::new(secrets, dir.path().join("data"))
            .with_config(Some(hub.clone()))
            .with_store(Some(store.clone()))
            .with_env(Arc::new(|k| {
                (k == "ANTHROPIC_API_KEY").then(|| "sk-ant-env-key-9999".to_string())
            }))
            .with_feed(signed_feed()),
    );
    let root = TaskScope::root();
    let services = Services {
        hub: Some(hub),
        providers: Some(providers.clone()),
        ..Services::default()
    };
    let state = Arc::new(DaemonState::with_services(
        cfg,
        EngineBuilder::new().build(&root),
        TOKEN.into(),
        root,
        Arc::new(genie_daemon::Registry::default()),
        None,
        Some(Arc::new(services)),
    ));
    state.start_background();
    Mem {
        state,
        hub: providers,
        store,
        _dir: dir,
    }
}

struct Client {
    conn: Connection,
    rx: UnboundedReceiver<Incoming>,
}

fn info() -> ClientInfo {
    ClientInfo {
        kind: ClientKind::Desktop,
        version: "1.0.0".into(),
        instance_id: "desk".into(),
        capabilities: ClientCapabilities {
            notifications: true,
            ..Default::default()
        },
    }
}

async fn connect(m: &Mem) -> Client {
    let (client, server) = genie_testkit::memory_pair();
    let st = m.state.clone();
    tokio::spawn(async move { serve_stream(st, Box::new(server), TaskScope::root()).await });
    let conn = Connection::handshake(Box::new(client), &info(), TOKEN.into())
        .await
        .unwrap();
    let rx = conn.take_incoming().unwrap();
    Client { conn, rx }
}

impl Client {
    async fn note(&mut self, method: &str, pred: impl Fn(&Value) -> bool) -> Notification {
        loop {
            let i = tokio::time::timeout(WAIT, self.rx.recv())
                .await
                .expect("timed out")
                .expect("closed");
            if let Incoming::Notification(n) = i
                && n.method == method
                && pred(&n.params)
            {
                return n;
            }
        }
    }
}

fn kind(e: ClientError) -> ErrorKind {
    match e {
        ClientError::Rpc(r) => r.kind(),
        other => panic!("expected rpc error, got {other:?}"),
    }
}

fn provider<'a>(
    l: &'a genie_protocol::api::provider::ProviderListResult,
    id: &str,
) -> &'a genie_protocol::api::provider::ProviderInfo {
    l.providers.iter().find(|p| p.id == id).unwrap()
}

#[tokio::test]
async fn credentials_status_feed_and_redaction() {
    let fake = Fake::start().await;
    let mem_backend = Arc::new(MemoryBackend::new());
    let mem = daemon(&fake, SecretsChoice::Backend(mem_backend.clone()), None);
    let mut c = connect(&mem).await;
    assert!(c.conn.init.locked.is_none());
    c.conn
        .call::<m::WorkspaceSubscribe>(Empty {})
        .await
        .unwrap();

    let l = c.conn.call::<m::ProviderList>(Empty {}).await.unwrap();
    assert_eq!(provider(&l, "openai").status, ProviderStatus::NeedsAuth);
    // Env key: listed with its source, active, hint only.
    let anth = provider(&l, "anthropic");
    assert_eq!(anth.status, ProviderStatus::Ready);
    assert_eq!(anth.credentials[0].credential_id, "env:ANTHROPIC_API_KEY");
    assert!(anth.credentials[0].active);
    assert_eq!(anth.credentials[0].hint.as_deref(), Some("9999"));
    assert_eq!(provider(&l, "ollama").status, ProviderStatus::Ready);
    let e = c
        .conn
        .call::<m::ProviderCredentialRemove>(CredentialIdParams {
            credential_id: "env:ANTHROPIC_API_KEY".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(kind(e), ErrorKind::InvalidParams);

    let id = c
        .conn
        .call::<m::ProviderCredentialAdd>(CredentialAddParams {
            provider: "openai".into(),
            label: "Work".into(),
            kind: CredentialKind::Key,
            secret: Some(OPENAI_KEY.into()),
        })
        .await
        .unwrap()
        .credential_id;
    let n = c
        .note(m::ProviderStatusNote::NAME, |p| p["provider"] == "openai")
        .await;
    assert_eq!(n.params["status"], "ready");
    let l = c.conn.call::<m::ProviderList>(Empty {}).await.unwrap();
    let oa = provider(&l, "openai");
    assert_eq!(oa.credentials[0].credential_id, id);
    assert!(oa.credentials[0].active);
    assert_eq!(oa.credentials[0].hint.as_deref(), Some("1234"));
    assert!(oa.credentials[0].label.contains("Session"));
    // The secret never comes back on the wire, and lives only in the backend.
    let wire = serde_json::to_string(&l).unwrap();
    assert!(!wire.contains(OPENAI_KEY));
    assert!(mem_backend.accounts().contains(&format!("credential:{id}")));

    // provider.test against the fake.
    let t = c
        .conn
        .call::<m::ProviderTest>(ProviderParams {
            provider: "openai".into(),
        })
        .await
        .unwrap();
    assert!(t.ok, "{:?}", t.error);

    // A second key, activate it, its test fails (401) → status error with a message.
    let id2 = c
        .conn
        .call::<m::ProviderCredentialAdd>(CredentialAddParams {
            provider: "openai".into(),
            label: "Bad".into(),
            kind: CredentialKind::Key,
            secret: Some("sk-wrong-key-000000".into()),
        })
        .await
        .unwrap()
        .credential_id;
    c.conn
        .call::<m::ProviderCredentialActivate>(CredentialIdParams {
            credential_id: id2.clone(),
        })
        .await
        .unwrap();
    let t = c
        .conn
        .call::<m::ProviderTest>(ProviderParams {
            provider: "openai".into(),
        })
        .await
        .unwrap();
    assert!(!t.ok);
    assert!(t.error.unwrap().contains("rejected"));
    c.note(m::ProviderStatusNote::NAME, |p| {
        p["provider"] == "openai" && p["status"] == "error"
    })
    .await;
    // Removing the active one promotes the remaining key.
    c.conn
        .call::<m::ProviderCredentialRemove>(CredentialIdParams { credential_id: id2 })
        .await
        .unwrap();
    let l = c.conn.call::<m::ProviderList>(Empty {}).await.unwrap();
    let oa = provider(&l, "openai");
    assert_eq!(oa.credentials.len(), 1);
    assert!(oa.credentials[0].active);
    assert_eq!(oa.status, ProviderStatus::Ready);

    // OAuth credentials only come from a sign-in.
    let e = c
        .conn
        .call::<m::ProviderCredentialAdd>(CredentialAddParams {
            provider: "azure-foundry".into(),
            label: "x".into(),
            kind: CredentialKind::Oauth,
            secret: Some("x".into()),
        })
        .await
        .unwrap_err();
    assert_eq!(kind(e), ErrorKind::InvalidParams);
}

#[tokio::test]
async fn models_from_catalog_discovery_overrides_and_the_resolver() {
    let fake = Fake::start().await;
    let mem = daemon(
        &fake,
        SecretsChoice::Backend(Arc::new(MemoryBackend::new())),
        None,
    );
    let c = connect(&mem).await;
    c.conn
        .call::<m::ProviderCredentialAdd>(CredentialAddParams {
            provider: "openai".into(),
            label: "k".into(),
            kind: CredentialKind::Key,
            secret: Some(OPENAI_KEY.into()),
        })
        .await
        .unwrap();
    let ms = c
        .conn
        .call::<m::ModelList>(ModelListParams {
            provider: Some("openai".into()),
        })
        .await
        .unwrap()
        .models;
    let found = ms.iter().find(|m| m.id == "openai/gpt-local-x").unwrap();
    assert_eq!(found.extra["limited_info"], true);
    assert!(ms.iter().all(|m| m.provider == "openai"));
    let all = c
        .conn
        .call::<m::ModelList>(ModelListParams::default())
        .await
        .unwrap()
        .models;
    let opus = all
        .iter()
        .find(|m| m.id == "anthropic/claude-opus-5-5")
        .unwrap();
    assert_eq!(opus.context_window, Some(123456));
    assert!(opus.effort_levels.contains(&"high".to_string()));

    // The engine's resolver sees the discovered model, the Settings endpoint and the key.
    let r = mem.hub.resolver().unwrap();
    let m = r.resolve("openai/gpt-local-x").unwrap();
    assert_eq!(m.route.endpoint.base, fake.base);
    let mut parts = http::Request::builder()
        .uri("https://x/")
        .body(())
        .unwrap()
        .into_parts()
        .0;
    m.route.auth.apply(&mut parts, b"").await.unwrap();
    assert_eq!(
        parts.headers["authorization"],
        format!("Bearer {OPENAI_KEY}").as_str()
    );
    // Env-only provider still resolves; one without a credential reports the class.
    assert!(r.resolve("anthropic/claude-opus-5-5").is_ok());
    let e = r.resolve("google/gemini-3-pro").err();
    if let Some(e) = e {
        assert!(matches!(
            e.class,
            "no_provider_configured" | "model_not_found"
        ));
    }
}

#[tokio::test]
async fn policy_hides_providers_and_blocks_methods() {
    let fake = Fake::start().await;
    let mem = daemon(
        &fake,
        SecretsChoice::Backend(Arc::new(MemoryBackend::new())),
        Some(json!({"providers.allowed": ["anthropic", "azure-foundry"],
                    "auth.methods": ["entra"]})),
    );
    let c = connect(&mem).await;
    let l = c.conn.call::<m::ProviderList>(Empty {}).await.unwrap();
    assert_eq!(provider(&l, "openai").status, ProviderStatus::Disabled);
    let e = c
        .conn
        .call::<m::ProviderCredentialAdd>(CredentialAddParams {
            provider: "openai".into(),
            label: "k".into(),
            kind: CredentialKind::Key,
            secret: Some(OPENAI_KEY.into()),
        })
        .await
        .unwrap_err();
    assert_eq!(kind(e), ErrorKind::PolicyBlocked);
    let e = c
        .conn
        .call::<m::ProviderCredentialAdd>(CredentialAddParams {
            provider: "azure-foundry".into(),
            label: "k".into(),
            kind: CredentialKind::Key,
            secret: Some("azure-key-123456789".into()),
        })
        .await
        .unwrap_err();
    assert_eq!(kind(e), ErrorKind::PolicyBlocked);
    let ms = c
        .conn
        .call::<m::ModelList>(ModelListParams::default())
        .await
        .unwrap()
        .models;
    assert!(
        ms.iter()
            .all(|m| m.provider == "anthropic" || m.provider == "azure-foundry")
    );
}

#[tokio::test]
async fn entra_device_code_sign_in_then_test() {
    let fake = Fake::start().await;
    let mem = daemon(
        &fake,
        SecretsChoice::Backend(Arc::new(MemoryBackend::new())),
        None,
    );
    let mut c = connect(&mem).await;
    c.conn
        .call::<m::WorkspaceSubscribe>(Empty {})
        .await
        .unwrap();
    let s = c
        .conn
        .call::<m::ProviderLoginStart>(LoginStartParams {
            provider: "azure-foundry".into(),
            method: LoginMethod::DeviceCode,
        })
        .await
        .unwrap();
    assert_eq!(s.user_code.as_deref(), Some("WXYZ-1234"));
    assert_eq!(
        s.verification_uri.as_deref(),
        Some("https://microsoft.com/devicelogin")
    );
    c.note(m::ProviderStatusNote::NAME, |p| {
        p["provider"] == "azure-foundry" && p["status"] == "ready"
    })
    .await;
    let st = c
        .conn
        .call::<m::ProviderLoginStatus>(LoginStatusParams {
            login_id: s.login_id,
        })
        .await
        .unwrap();
    assert_eq!(st.state, LoginState::Done);
    let l = c.conn.call::<m::ProviderList>(Empty {}).await.unwrap();
    let az = provider(&l, "azure-foundry");
    assert_eq!(az.credentials[0].kind, CredentialKind::Oauth);
    assert!(!serde_json::to_string(&l).unwrap().contains("at-entra"));
    // The stored token reaches the endpoint as a bearer token.
    let t = c
        .conn
        .call::<m::ProviderTest>(ProviderParams {
            provider: "azure-foundry".into(),
        })
        .await
        .unwrap();
    assert!(t.ok, "{:?}", t.error);
    // PKCE start returns a browser URL with our loopback redirect.
    let p = c
        .conn
        .call::<m::ProviderLoginStart>(LoginStartParams {
            provider: "azure-foundry".into(),
            method: LoginMethod::Pkce,
        })
        .await
        .unwrap();
    let url = p.auth_url.unwrap();
    assert!(url.contains("code_challenge_method=S256"));
    assert!(url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A"));
    // No sign-in for key-only providers.
    let e = c
        .conn
        .call::<m::ProviderLoginStart>(LoginStartParams {
            provider: "openai".into(),
            method: LoginMethod::Pkce,
        })
        .await
        .unwrap_err();
    assert_eq!(kind(e), ErrorKind::InvalidParams);
}

#[tokio::test]
async fn usage_query_and_budget_warnings() {
    let fake = Fake::start().await;
    let mem = daemon(
        &fake,
        SecretsChoice::Backend(Arc::new(MemoryBackend::new())),
        None,
    );
    let mut c = connect(&mem).await;
    c.conn
        .call::<m::WorkspaceSubscribe>(Empty {})
        .await
        .unwrap();
    let today = genie_core::runtime::context::utc_day(genie_core::bus::now_ms());
    let add = |provider: &str, kind: &str, cost: f64| genie_store::kv::Usage {
        day: today.clone(),
        provider: provider.into(),
        model: format!("{provider}/m"),
        project_id: String::new(),
        kind: kind.into(),
        tokens_in: 100,
        tokens_out: 10,
        cache_read: 50,
        cache_write: 5,
        calls: 1,
        cost_usd: cost,
    };
    mem.store
        .write(|tx| {
            genie_store::kv::add_usage(tx, &add("openai", "chat", 0.5))?;
            genie_store::kv::add_usage(tx, &add("openai", "compaction", 0.2))?;
            genie_store::kv::add_usage(tx, &add("anthropic", "chat", 0.2))
        })
        .unwrap();
    let r = c
        .conn
        .call::<m::UsageQuery>(UsageQueryParams {
            from: format!("{today}T00:00:00Z"),
            to: today.clone(),
            group_by: vec![UsageGroup::Provider],
        })
        .await
        .unwrap();
    let oa = r.rows.iter().find(|r| r.keys == ["openai"]).unwrap();
    assert_eq!(oa.usage.input_tokens, 200);
    assert_eq!(oa.usage.cache_read_tokens, 100);
    assert_eq!(oa.compaction_tokens, 110);
    assert!((oa.cost.usd - 0.7).abs() < 1e-9);
    let e = c
        .conn
        .call::<m::UsageQuery>(UsageQueryParams {
            from: "yesterday".into(),
            to: today.clone(),
            group_by: vec![],
        })
        .await
        .unwrap_err();
    assert_eq!(kind(e), ErrorKind::InvalidParams);

    // 0.9 of 1.0 → 80 % warning; lowering to 0.5 → 100 %; no repeat for the same level.
    c.conn
        .call::<m::UsageBudgetSet>(BudgetSetParams {
            monthly_usd: Some(1.0),
        })
        .await
        .unwrap();
    let n = c.note(m::UserNotification::NAME, |_| true).await;
    assert!(n.params["title"].as_str().unwrap().contains("80%"));
    assert_eq!(mem.hub.check_budget(), None);
    c.conn
        .call::<m::UsageBudgetSet>(BudgetSetParams {
            monthly_usd: Some(0.5),
        })
        .await
        .unwrap();
    let n = c.note(m::UserNotification::NAME, |_| true).await;
    assert!(n.params["title"].as_str().unwrap().contains("reached"));
}

#[tokio::test]
async fn catalog_status_and_signed_refresh() {
    let fake = Fake::start().await;
    let mem = daemon(
        &fake,
        SecretsChoice::Backend(Arc::new(MemoryBackend::new())),
        None,
    );
    let c = connect(&mem).await;
    c.conn
        .call::<m::WorkspaceSubscribe>(Empty {})
        .await
        .unwrap();
    let s = c
        .conn
        .call::<m::CatalogStatusMethod>(Empty {})
        .await
        .unwrap();
    // The background refresh may already have run; either way it ends "fresh".
    if s.source == CatalogSource::Bundled {
        let r = c.conn.call::<m::CatalogRefresh>(Empty {}).await.unwrap();
        assert_eq!(r.source, CatalogSource::Fresh);
    }
    let r = c
        .conn
        .call::<m::CatalogStatusMethod>(Empty {})
        .await
        .unwrap();
    assert_eq!(r.source, CatalogSource::Fresh);
    assert_eq!(r.etag.as_deref(), Some("\"v2\""));
    assert!(!r.fetched_at.is_empty());
}

#[tokio::test]
async fn locked_file_gates_methods_until_unlocked() {
    let fake = Fake::start().await;
    let mem = daemon(
        &fake,
        SecretsChoice::File(KdfParams::insecure_for_tests()),
        None,
    );
    let c = connect(&mem).await;
    assert_eq!(
        c.conn.init.locked.as_ref().unwrap().reason,
        "passphrase_required"
    );
    let e = c.conn.call::<m::ProviderList>(Empty {}).await.unwrap_err();
    assert_eq!(kind(e), ErrorKind::Locked);
    // Status still works while locked.
    c.conn
        .call::<m::DaemonStatusMethod>(Empty {})
        .await
        .unwrap();
    // First unlock creates the file with this passphrase.
    let ok = c
        .conn
        .call::<m::SecretsUnlock>(UnlockParams {
            passphrase: "correct horse".into(),
        })
        .await
        .unwrap();
    assert!(ok.ok);
    c.conn.call::<m::ProviderList>(Empty {}).await.unwrap();
    let e = c
        .conn
        .call::<m::SecretsUnlock>(UnlockParams {
            passphrase: "wrong".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(kind(e), ErrorKind::InvalidParams);
    let c2 = connect(&mem).await;
    assert!(c2.conn.init.locked.is_none());
}

// ---- the locked phase over the real endpoint -------------------------------------------

async fn ensure_conn(ep: &Endpoint) -> Connection {
    let opts = EnsureOptions::new(
        ep.clone(),
        ClientInfo {
            kind: ClientKind::Cli,
            version: env!("CARGO_PKG_VERSION").into(),
            instance_id: "cli".into(),
            capabilities: ClientCapabilities::default(),
        },
        None,
        vec![],
    );
    match ensure(&opts, |_| {}).await.unwrap() {
        EnsureOutcome::Ready(c) => c,
        _ => panic!("expected Ready"),
    }
}

#[tokio::test]
async fn real_daemon_locked_phase_then_engine_with_store_key_in_secrets_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = DaemonConfig::new(Endpoint::in_dir(dir.path(), Channel::Dev).unwrap());
    cfg.idle_timeout = Duration::from_secs(60);
    cfg.write_logs = false;
    cfg.secrets = SecretsChoice::File(KdfParams::insecure_for_tests());
    let ep = cfg.endpoint.clone();
    // A key from the old placeholder keystore must be migrated, then the file deleted.
    std::fs::create_dir_all(&ep.data_dir).unwrap();
    genie_store::KeyStore::set(
        &genie_daemon::PlaceholderKeyStore::new(&ep.data_dir),
        "data-key",
        &[5u8; 32],
    )
    .unwrap();
    let daemon = tokio::spawn(genie_daemon::run(cfg));

    let conn = ensure_conn(&ep).await;
    assert!(conn.init.locked.is_some());
    let mut rx = conn.take_incoming().unwrap();
    let e = conn.call::<m::ProviderList>(Empty {}).await.unwrap_err();
    assert_eq!(kind(e), ErrorKind::Locked);
    conn.call::<m::SecretsUnlock>(UnlockParams {
        passphrase: "pw".into(),
    })
    .await
    .unwrap();
    // daemon.restarting, then the full engine comes up on the same endpoint.
    loop {
        match tokio::time::timeout(WAIT, rx.recv()).await.unwrap() {
            Some(Incoming::Notification(n)) if n.method == m::DaemonRestarting::NAME => break,
            Some(_) => {}
            None => break,
        }
    }
    let conn = loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let c = ensure_conn(&ep).await;
        if c.init.locked.is_none() {
            break c;
        }
    };
    let l = conn.call::<m::ProviderList>(Empty {}).await.unwrap();
    assert!(!l.providers.is_empty());
    assert!(ep.data_dir.join("genie-dev.db").exists());
    assert!(ep.data_dir.join("secrets.enc").exists());
    assert!(!ep.data_dir.join("keystore.placeholder.json").exists());
    // The data key now lives (encrypted) in secrets.enc.
    let raw = std::fs::read_to_string(ep.data_dir.join("secrets.enc")).unwrap();
    assert!(raw.contains("argon2id"));
    let v = genie_secrets::vault::FileVault::at(
        ep.data_dir.join("secrets.enc"),
        KdfParams::insecure_for_tests(),
    );
    v.unlock(&genie_secrets::SecretString::from("pw")).unwrap();
    assert_eq!(v.get("data-key").unwrap().unwrap().as_slice(), [5u8; 32]);

    let _: Value = conn
        .request(m::DAEMON_RESTART, json!({"stop_running": true}))
        .await
        .unwrap();
    let _ = tokio::time::timeout(WAIT, daemon).await;
}
