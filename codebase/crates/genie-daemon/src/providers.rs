//! Providers, credentials, models, catalog and usage in the daemon (02 §6.1, §6.5;
//! 03 §8, §10; D26, D27, D34).
//!
//! [`ProviderHub`] owns:
//! * the credential store ([`BackendStore`] on the keychain / `secrets.enc`), the
//!   `secrets.unlock` method and the locked-state gate;
//! * per-provider auth: API keys (stored beat env, 03 §10.1), Entra OAuth (PKCE with a
//!   loopback redirect, device code; refresh under a cross-process lock), Google ADC,
//!   Ollama / custom endpoints without auth; it is the engine's
//!   [`ProviderBinder`], so turns use exactly what Settings shows;
//! * provider status + `provider.status` feed events;
//! * the model catalog: bundled → verified cache → refresh (6 h, ETag, Ed25519) → live
//!   discovery → user overrides `models.<id>` → policy; shared with the resolver;
//! * usage queries over `usage_daily` and the monthly budget (warnings at 80 / 100 %,
//!   `notification` on the workspace feed, no hard stop).
//!
//! Provider settings live under `providers.<id>` in the settings files (no secrets):
//! `{base_url, project, location, kind, client_id, tenant, authority, deployments}`.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use genie_audit::{Attrs, AuditEvent, Auditor};
use genie_catalog::discovery::{self, Discovered};
use genie_catalog::refresh::{CatalogCache, FeedFetcher, FeedResponse, RefreshOutcome};
use genie_catalog::{Catalog, Entry, Protocol, Source};
use genie_core::TaskScope;
use genie_core::config::ConfigHub;
use genie_core::runtime::models::{CatalogResolver, ProviderBinder};
use genie_core::runtime::ports::ResolveError;
use genie_protocol::api::provider::{
    CatalogSource, CatalogStatus, CredentialAddResult, CredentialInfo, CredentialKind as WireKind,
    LoginMethod, LoginStartResult, LoginState, LoginStatusResult, ModelInfo, ModelListResult,
    ProviderInfo, ProviderListResult, ProviderStatus, ProviderTestResult, UsageGroup,
    UsageQueryResult, UsageRow,
};
use genie_protocol::common::{Cost, Empty, OkResult, Usage};
use genie_protocol::methods::{self as m, Method};
use genie_protocol::workspace::{CatalogUpdated, ProviderStatusChanged, UserNotification};
use genie_protocol::{ErrorKind, Request, Response, RpcError};
use genie_providers::adc::{AdcTokenSource, adc_path};
use genie_providers::auth::{ApiKeyHeader, Auth, BearerKey, NoAuth, TokenAuth};
use genie_providers::model::{AccountTag, ProviderId};
use genie_providers::oauth::{self, EntraConfig, GrantStore, OAuthGrant, OAuthTokenSource};
use genie_providers::route::{Preset, ProviderConfig};
use genie_secrets::credentials::{self, CredentialKind, CredentialMeta, key_hint};
use genie_secrets::store::BackendStore;
use genie_secrets::{ExposeSecret, SecretString, SecretsError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::secrets::{LOCKED_ALLOWED, Secrets};
use crate::server::{Conn, DaemonState};
use crate::services::Handled;

type EnvFn = dyn Fn(&str) -> Option<String> + Send + Sync;

/// Built-in providers: (id, kind).
const BUILTIN: &[(&str, &str)] = &[
    ("anthropic", "anthropic"),
    ("openai", "openai"),
    ("google", "google"),
    ("azure-foundry", "azure-foundry"),
    ("openrouter", "openrouter"),
    ("ollama", "ollama"),
];

/// The Genie multi-tenant Entra app (03 §10.2). Set at build time once it is registered
/// in the Synergech tenant; Settings `providers.<id>.client_id` overrides it.
const ENTRA_CLIENT_ID: Option<&str> = option_env!("GENIE_ENTRA_CLIENT_ID");
/// Discovery results are reused this long.
const DISCOVERY_TTL: Duration = Duration::from_secs(10 * 60);
const BUDGET_KEY: &str = "usage.budget";

/// `providers.<id>` in settings.
#[derive(Clone, Debug, Default, Deserialize)]
struct ProviderSettings {
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    project: Option<String>,
    #[serde(default)]
    location: Option<String>,
    #[serde(default)]
    client_id: Option<String>,
    #[serde(default)]
    tenant: Option<String>,
    #[serde(default)]
    authority: Option<String>,
    /// Azure Foundry deployments entered by hand: `[{name, model}]`.
    #[serde(default)]
    deployments: Vec<Deployment>,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct Deployment {
    name: String,
    model: String,
}

/// The secret of an `external` Google credential (no secret inside).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct AdcRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    adc_path: Option<String>,
}

struct CatState {
    base: Catalog,
    fetched_at: String,
    discovered: HashMap<String, (Instant, Vec<Discovered>)>,
}

struct Login {
    provider: String,
    state: LoginState,
    error: Option<String>,
    finished: Option<Instant>,
}

/// The catalog feed (03 §8.1 step 3).
pub struct Feed {
    pub url: String,
    pub fetcher: Arc<dyn FeedFetcher>,
    pub keys: Vec<ed25519_dalek::VerifyingKey>,
}

pub struct ProviderHub {
    secrets: Arc<Secrets>,
    creds: Arc<BackendStore>,
    hub: Option<Arc<ConfigHub>>,
    store: Option<Arc<genie_store::Store>>,
    audit: Option<Arc<Auditor>>,
    data_dir: PathBuf,
    catalog: Arc<RwLock<Catalog>>,
    cat: Mutex<CatState>,
    cache: CatalogCache,
    feed: Option<Feed>,
    /// Token endpoints: rustls, no redirects.
    http: reqwest::Client,
    env: Arc<EnvFn>,
    logins: Mutex<HashMap<String, Login>>,
    statuses: Mutex<HashMap<String, ProviderStatus>>,
    errors: Mutex<HashMap<String, String>>,
    tokens: Mutex<HashMap<String, Arc<TokenAuth>>>,
    state: Mutex<Weak<DaemonState>>,
    scope: Mutex<Option<TaskScope>>,
    entra_client_id: Option<String>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn to_result<T: Serialize>(v: &T) -> Result<Value, RpcError> {
    serde_json::to_value(v).map_err(|e| RpcError::new(ErrorKind::InternalError, e.to_string()))
}

fn params<M: Method>(r: &Request) -> Result<M::Params, RpcError> {
    let v = if r.params.is_null() {
        json!({})
    } else {
        r.params.clone()
    };
    serde_json::from_value(v).map_err(|e| RpcError::invalid_params(e.to_string()))
}

fn secrets_err(e: SecretsError) -> RpcError {
    match e {
        SecretsError::Locked => RpcError::new(ErrorKind::Locked, e.to_string())
            .with_data(json!({"reason": "passphrase_required"})),
        SecretsError::BadPassphrase => {
            RpcError::invalid_params(e.to_string()).with_data(json!({"reason": "bad_passphrase"}))
        }
        other => RpcError::new(ErrorKind::InternalError, other.to_string()),
    }
}

fn not_found(what: &str) -> RpcError {
    RpcError::new(ErrorKind::NotFound, format!("{what} not found"))
}

fn policy_blocked(rule: &str, what: String) -> RpcError {
    RpcError::new(ErrorKind::PolicyBlocked, what)
        .with_data(json!({"rule": rule, "source": "policy"}))
}

fn wire_kind(k: CredentialKind) -> WireKind {
    match k {
        CredentialKind::Key => WireKind::Key,
        CredentialKind::OAuth => WireKind::Oauth,
        CredentialKind::External => WireKind::External,
    }
}

/// Auth method names for policy `auth.methods`.
fn method_name(k: CredentialKind) -> &'static str {
    match k {
        CredentialKind::Key => "api_key",
        CredentialKind::OAuth => "entra",
        CredentialKind::External => "adc",
    }
}

fn random_id(prefix: &str) -> String {
    let mut b = [0u8; 10];
    let _ = getrandom::fill(&mut b);
    format!("{prefix}_{}", hex::encode(b))
}

fn rfc3339_now() -> String {
    genie_audit::audit::rfc3339_millis(std::time::SystemTime::now())
}

/// `x*` prefix globs and exact ids.
fn matches_any(id: &str, pats: &[String]) -> bool {
    pats.iter().any(|p| match p.strip_suffix('*') {
        Some(pre) => id.starts_with(pre),
        None => p == id,
    })
}

/// A grant persisted as the secret of one OAuth credential.
struct CredGrant {
    creds: Arc<BackendStore>,
    id: String,
}

impl GrantStore for CredGrant {
    fn load(&self) -> Result<Option<OAuthGrant>, String> {
        match self.creds.try_get(&self.id).map_err(|e| e.to_string())? {
            Some(c) => OAuthGrant::from_secret(&c.secret).map(Some),
            None => Ok(None),
        }
    }
    fn save(&self, g: &OAuthGrant) -> Result<(), String> {
        let s = g.to_secret()?;
        self.creds
            .update_secret(&self.id, &s)
            .map_err(|e| e.to_string())
    }
}

/// HTTP feed fetcher for the signed catalog (`<url>` + `<url>.sig`).
pub struct HttpFeed(pub reqwest::Client);

#[async_trait]
impl FeedFetcher for HttpFeed {
    async fn fetch(&self, url: &str, etag: Option<&str>) -> Result<FeedResponse, String> {
        let mut req = self.0.get(url);
        if let Some(e) = etag {
            req = req.header("if-none-match", e);
        }
        let resp = req.send().await.map_err(|e| e.to_string())?;
        if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
            return Ok(FeedResponse::NotModified);
        }
        if !resp.status().is_success() {
            return Err(format!("catalog feed returned {}", resp.status()));
        }
        let etag = resp
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        let catalog = resp.bytes().await.map_err(|e| e.to_string())?.to_vec();
        let sig = self
            .0
            .get(format!("{url}.sig"))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !sig.status().is_success() {
            return Err(format!("catalog signature returned {}", sig.status()));
        }
        let signature = sig.text().await.map_err(|e| e.to_string())?;
        Ok(FeedResponse::Body {
            catalog,
            signature: signature.trim().to_string(),
            etag,
        })
    }
}

impl ProviderHub {
    pub fn new(secrets: Arc<Secrets>, data_dir: PathBuf) -> Self {
        let creds = Arc::new(BackendStore::new(secrets.backend()));
        let cache = CatalogCache::new(data_dir.join("cache"));
        let base =
            genie_catalog::refresh::load_best(&cache, &genie_catalog::signature::pinned_keys());
        let fetched_at = if base.source == Source::Bundled {
            String::new()
        } else {
            rfc3339_now()
        };
        let http = oauth::http_client().unwrap_or_default();
        ProviderHub {
            secrets,
            creds,
            hub: None,
            store: None,
            audit: None,
            data_dir,
            catalog: Arc::new(RwLock::new(base.clone())),
            cat: Mutex::new(CatState {
                base,
                fetched_at,
                discovered: HashMap::new(),
            }),
            cache,
            feed: None,
            http,
            env: Arc::new(|k| std::env::var(k).ok()),
            logins: Mutex::new(HashMap::new()),
            statuses: Mutex::new(HashMap::new()),
            errors: Mutex::new(HashMap::new()),
            tokens: Mutex::new(HashMap::new()),
            state: Mutex::new(Weak::new()),
            scope: Mutex::new(None),
            entra_client_id: ENTRA_CLIENT_ID.map(String::from),
        }
    }

    /// The hub of the locked phase: only `secrets.unlock` and the gate.
    pub fn locked(secrets: Arc<Secrets>) -> Arc<Self> {
        Arc::new(Self::new(secrets, std::env::temp_dir()))
    }

    pub fn with_config(mut self, hub: Option<Arc<ConfigHub>>) -> Self {
        self.hub = hub;
        self.rebuild_catalog();
        self
    }
    pub fn with_store(mut self, store: Option<Arc<genie_store::Store>>) -> Self {
        self.store = store;
        self
    }
    pub fn with_audit(mut self, audit: Option<Arc<Auditor>>) -> Self {
        self.audit = audit;
        self
    }
    /// Environment lookup (tests).
    pub fn with_env(mut self, env: Arc<EnvFn>) -> Self {
        self.env = env;
        self
    }
    pub fn with_feed(mut self, feed: Feed) -> Self {
        self.feed = Some(feed);
        self
    }
    pub fn with_entra_client_id(mut self, id: Option<String>) -> Self {
        self.entra_client_id = id;
        self
    }

    /// The feed from policy `catalog.url` (D38) or `GENIE_CATALOG_URL`, pinned keys.
    pub fn default_feed(&self) -> Option<Feed> {
        let url = self
            .hub
            .as_ref()
            .and_then(|h| {
                h.policy()
                    .get("catalog.url")
                    .and_then(Value::as_str)
                    .map(String::from)
            })
            .or_else(|| (self.env)("GENIE_CATALOG_URL"))?;
        Some(Feed {
            url,
            fetcher: Arc::new(HttpFeed(self.http.clone())),
            keys: genie_catalog::signature::pinned_keys(),
        })
    }

    pub fn credentials(&self) -> Arc<BackendStore> {
        self.creds.clone()
    }

    pub fn shared_catalog(&self) -> Arc<RwLock<Catalog>> {
        self.catalog.clone()
    }

    pub fn locked_info(&self) -> Option<genie_protocol::handshake::LockedInfo> {
        self.secrets.locked_info()
    }

    /// The engine's model resolver: shared catalog + this hub as binder.
    #[allow(clippy::result_large_err)] // ProviderError is a flat struct (03 §7)
    pub fn resolver(self: &Arc<Self>) -> Result<CatalogResolver, genie_providers::ProviderError> {
        let exec = genie_providers::Executor::new(
            Arc::new(genie_providers::executor::ReqwestTransport::new()?),
            genie_providers::RetryPolicy::default(),
        );
        let env = self.env.clone();
        Ok(
            CatalogResolver::new(Catalog::bundled(), self.creds.clone(), Arc::new(exec))
                .with_shared_catalog(self.catalog.clone())
                .with_binder(self.clone())
                .with_env(Arc::new(move |k| env(k))),
        )
    }

    fn audit(&self, ev: AuditEvent, attrs: Attrs) {
        if let Some(a) = &self.audit {
            a.record(None, ev, attrs);
        }
    }

    fn emit<P: Serialize>(&self, method: &str, p: &P) {
        if let Some(st) = lock(&self.state).upgrade() {
            st.workspace_send(method, p);
        }
    }

    fn spawn(&self, fut: impl std::future::Future<Output = ()> + Send + 'static) {
        if let Some(s) = lock(&self.scope).as_ref() {
            s.spawn_cancellable(fut);
        }
    }

    // ---- settings and policy -------------------------------------------------------

    fn setting(&self, key: &str) -> Option<Value> {
        self.hub.as_ref().and_then(|h| h.value(None, key))
    }

    fn settings(&self, id: &str) -> ProviderSettings {
        self.setting(&format!("providers.{id}"))
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default()
    }

    fn policy_list(&self, key: &str) -> Option<Vec<String>> {
        let p = self.hub.as_ref()?.policy();
        p.get(key)?;
        Some(p.strings(key))
    }

    fn allowed(&self, id: &str) -> bool {
        self.policy_list("providers.allowed")
            .is_none_or(|l| l.iter().any(|x| x == id))
    }

    /// Where requests to provider `id` go: the configured base URL or the preset's default
    /// (`None` = not configured yet; Azure / OpenAI-compatible need a base URL).
    fn endpoint_of(&self, id: &str) -> Option<String> {
        let s = self.settings(id);
        if let Some(b) = s.base_url.clone() {
            return Some(b);
        }
        let url = match self.kind_of(id)?.as_str() {
            "anthropic" => "https://api.anthropic.com".to_string(),
            "openai" => "https://api.openai.com".to_string(),
            "google" if s.project.is_some() => match s.location.as_deref() {
                None | Some("global") => "https://aiplatform.googleapis.com".to_string(),
                Some(loc) => format!("https://{loc}-aiplatform.googleapis.com"),
            },
            "google" => "https://generativelanguage.googleapis.com".to_string(),
            "openrouter" => "https://openrouter.ai/api".to_string(),
            "ollama" => genie_providers::route::OLLAMA_DEFAULT_BASE.to_string(),
            _ => return None,
        };
        Some(url)
    }

    /// Policy `endpoints.allowed` (08 §5.2): `Err(message)` when the provider's endpoint is
    /// not on the list.
    fn endpoint_check(&self, id: &str) -> Result<(), String> {
        let Some(list) = self.policy_list("endpoints.allowed") else {
            return Ok(());
        };
        match self.endpoint_of(id) {
            Some(url) if !genie_config::endpoint_allowed(&url, &list) => Err(format!(
                "the {id} endpoint {url} is not allowed by your organization's policy"
            )),
            _ => Ok(()),
        }
    }

    fn method_allowed(&self, k: CredentialKind) -> bool {
        self.policy_list("auth.methods")
            .is_none_or(|l| l.iter().any(|x| x == method_name(k)))
    }

    /// Built-in providers plus custom ones from `providers.*` settings.
    fn provider_ids(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = BUILTIN
            .iter()
            .map(|(i, k)| (i.to_string(), k.to_string()))
            .collect();
        if let Some(Value::Object(m)) = self.setting("providers") {
            for (id, v) in m {
                if BUILTIN.iter().any(|(b, _)| *b == id) {
                    continue;
                }
                let kind = v
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("openai-compatible")
                    .to_string();
                out.push((id, kind));
            }
        }
        out
    }

    fn kind_of(&self, id: &str) -> Option<String> {
        self.provider_ids()
            .into_iter()
            .find(|(i, _)| i == id)
            .map(|(_, k)| k)
    }

    // ---- auth ----------------------------------------------------------------------

    fn token_auth(&self, cred: &str, make: impl FnOnce() -> Arc<TokenAuth>) -> Arc<TokenAuth> {
        lock(&self.tokens)
            .entry(cred.to_string())
            .or_insert_with(make)
            .clone()
    }

    fn forget_tokens(&self, cred: &str) {
        lock(&self.tokens).remove(cred);
    }

    /// Preset, auth and settings for a provider. `Ok(None)` = needs a credential.
    #[allow(clippy::type_complexity)]
    fn auth_for(
        &self,
        id: &str,
    ) -> Result<Option<(Preset, Arc<dyn Auth>, ProviderSettings, Option<AccountTag>)>, String> {
        let kind = self
            .kind_of(id)
            .ok_or_else(|| format!("unknown provider {id}"))?;
        if !self.allowed(id) {
            return Err(format!("{id} is not allowed by your organization's policy"));
        }
        self.endpoint_check(id)?;
        let s = self.settings(id);
        let key_preset = match kind.as_str() {
            "anthropic" => Preset::Anthropic,
            "openai" => Preset::OpenAi,
            "google" => Preset::GeminiApi,
            "azure-foundry" => Preset::AzureFoundry,
            "openrouter" => Preset::OpenRouter,
            "ollama" => Preset::Ollama,
            _ => Preset::OpenAiCompatible,
        };
        let key_auth = |preset: Preset, key: SecretString| -> Arc<dyn Auth> {
            let header = |h: &'static str| -> Arc<dyn Auth> {
                Arc::new(ApiKeyHeader {
                    header: http::HeaderName::from_static(h),
                    key: key.clone(),
                })
            };
            match preset {
                Preset::Anthropic => header("x-api-key"),
                Preset::GeminiApi => header("x-goog-api-key"),
                Preset::AzureFoundry => header("api-key"),
                _ => Arc::new(BearerKey(key.clone())),
            }
        };
        let host = s.base_url.clone().unwrap_or_default();
        let tag = |cred: &str, tenant: &str| {
            Some(AccountTag::derive(
                &ProviderId::new(id),
                cred,
                tenant,
                &host,
            ))
        };
        let stored = match self.creds.try_active(id) {
            Ok(Some(cid)) => self.creds.try_get(&cid).map_err(|e| e.to_string())?,
            Ok(None) => None,
            Err(SecretsError::Locked) => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        if let Some(c) = stored {
            if !self.method_allowed(c.meta.kind) {
                return Err(format!(
                    "{} sign-in is not allowed by your organization's policy",
                    method_name(c.meta.kind)
                ));
            }
            let cid = c.meta.id.clone();
            return Ok(Some(match c.meta.kind {
                CredentialKind::Key => {
                    (key_preset, key_auth(key_preset, c.secret), s, tag(&cid, ""))
                }
                CredentialKind::OAuth => {
                    let grant = OAuthGrant::from_secret(&c.secret)?;
                    let tenant = grant.config.tenant.clone();
                    let auth = self.token_auth(&cid, || {
                        Arc::new(TokenAuth::new(Arc::new(OAuthTokenSource::new(
                            Arc::new(CredGrant {
                                creds: self.creds.clone(),
                                id: cid.clone(),
                            }),
                            self.http.clone(),
                            self.data_dir.join("run").join(format!("oauth-{cid}.lock")),
                        ))))
                    });
                    (
                        Preset::AzureFoundry,
                        auth as Arc<dyn Auth>,
                        s,
                        tag(&cid, &tenant),
                    )
                }
                CredentialKind::External => {
                    let r: AdcRef =
                        serde_json::from_str(c.secret.expose_secret()).unwrap_or_default();
                    let env = self.env.clone();
                    let path = adc_path(
                        r.adc_path.as_deref().map(std::path::Path::new),
                        &move |k| env(k),
                    )
                    .ok_or(
                        "no Google credentials found; run `gcloud auth application-default login`",
                    )?;
                    let auth = self.token_auth(&cid, || {
                        Arc::new(TokenAuth::new(Arc::new(AdcTokenSource::new(
                            path,
                            self.http.clone(),
                        ))))
                    });
                    let project = s.project.clone().unwrap_or_default();
                    (
                        Preset::Vertex,
                        auth as Arc<dyn Auth>,
                        s,
                        tag(&cid, &project),
                    )
                }
            }));
        }
        let env = self.env.clone();
        if let Some(var) = credentials::env_vars(id)
            .iter()
            .find(|v| env(v).is_some_and(|x| !x.trim().is_empty()))
        {
            if !self.method_allowed(CredentialKind::Key) {
                return Err("API keys are not allowed by your organization's policy".into());
            }
            let key = SecretString::from(env(var).unwrap_or_default());
            return Ok(Some((key_preset, key_auth(key_preset, key), s, None)));
        }
        if matches!(key_preset, Preset::Ollama | Preset::OpenAiCompatible) {
            return Ok(Some((key_preset, Arc::new(NoAuth), s, None)));
        }
        Ok(None)
    }

    fn status_of(&self, id: &str) -> ProviderStatus {
        if !self.allowed(id) || self.endpoint_check(id).is_err() {
            return ProviderStatus::Disabled;
        }
        if self.secrets.is_locked() {
            return ProviderStatus::NeedsAuth;
        }
        if lock(&self.errors).contains_key(id) {
            return ProviderStatus::Error;
        }
        match self.auth_for(id) {
            Ok(Some((preset, _, s, _))) => {
                let missing = match preset {
                    Preset::AzureFoundry | Preset::OpenAiCompatible => s.base_url.is_none(),
                    Preset::Vertex => s.project.is_none(),
                    _ => false,
                };
                if missing {
                    ProviderStatus::Error
                } else {
                    ProviderStatus::Ready
                }
            }
            Ok(None) => ProviderStatus::NeedsAuth,
            Err(_) => ProviderStatus::Error,
        }
    }

    /// Recompute statuses and send `provider.status` for every change.
    pub fn refresh_statuses(&self) {
        let mut changes = vec![];
        {
            let ids = self.provider_ids();
            let mut cur = lock(&self.statuses);
            for (id, _) in ids {
                let st = self.status_of(&id);
                if cur.get(&id) != Some(&st) {
                    cur.insert(id.clone(), st);
                    let error = lock(&self.errors).get(&id).cloned();
                    changes.push(ProviderStatusChanged {
                        provider: id,
                        status: st,
                        error,
                    });
                }
            }
        }
        for c in changes {
            self.emit(m::ProviderStatusNote::NAME, &c);
        }
    }

    fn credential_changed(&self, provider: &str, action: &str, kind: Option<CredentialKind>) {
        lock(&self.errors).remove(provider);
        lock(&self.cat).discovered.remove(provider);
        self.rebuild_catalog();
        self.audit(
            AuditEvent::CredentialChanged,
            Attrs::new()
                .with("provider", provider)
                .with("action", action)
                .with("method", kind.map(method_name).unwrap_or("")),
        );
        self.refresh_statuses();
    }

    // ---- methods -------------------------------------------------------------------

    /// Methods served here; `None` lets the other services / the server answer.
    pub(crate) fn dispatch(
        self: &Arc<Self>,
        state: &Arc<DaemonState>,
        conn: &Arc<Conn>,
        r: &Request,
    ) -> Option<Handled> {
        let name = r.method.as_str();
        if self.secrets.is_locked() && !LOCKED_ALLOWED.contains(&name) {
            return Some(Handled::Now(Err(secrets_err(SecretsError::Locked))));
        }
        let now = |res: Result<Value, RpcError>| Some(Handled::Now(res));
        match name {
            n if n == m::SecretsUnlock::NAME => now(self.unlock(r)),
            n if n == m::ProviderList::NAME => now(to_result(&self.list())),
            n if n == m::ProviderCredentialAdd::NAME => now(self.credential_add(r)),
            n if n == m::ProviderCredentialRemove::NAME => now(self.credential_remove(r)),
            n if n == m::ProviderCredentialActivate::NAME => now(self.credential_activate(r)),
            n if n == m::ProviderLoginStatus::NAME => now(self.login_status(r)),
            n if n == m::CatalogStatusMethod::NAME => now(to_result(&self.catalog_status())),
            n if n == m::UsageQuery::NAME => now(self.usage_query(r)),
            n if n == m::UsageBudgetSet::NAME => now(self.budget_set(r)),
            n if n == m::ProviderLoginStart::NAME
                || n == m::ProviderTest::NAME
                || n == m::ModelList::NAME
                || n == m::CatalogRefresh::NAME =>
            {
                let _ = state;
                let (me, c, r) = (self.clone(), conn.clone(), r.clone());
                conn.scope()
                    .child("provider-call")
                    .spawn_cancellable(async move {
                        let res = me.call_async(&r).await;
                        c.respond(match res {
                            Ok(v) => Response::ok(r.id.clone(), v),
                            Err(e) => Response::err(r.id.clone(), e),
                        });
                    });
                Some(Handled::Later)
            }
            _ => None,
        }
    }

    async fn call_async(self: &Arc<Self>, r: &Request) -> Result<Value, RpcError> {
        match r.method.as_str() {
            n if n == m::ProviderLoginStart::NAME => {
                let p = params::<m::ProviderLoginStart>(r)?;
                to_result(&self.login_start(&p.provider, p.method).await?)
            }
            n if n == m::ProviderTest::NAME => {
                let p = params::<m::ProviderTest>(r)?;
                to_result(&self.test(&p.provider).await?)
            }
            n if n == m::ModelList::NAME => {
                let p = params::<m::ModelList>(r)?;
                to_result(&self.model_list(p.provider.as_deref()).await)
            }
            _ => to_result(&self.catalog_refresh().await?),
        }
    }

    fn unlock(&self, r: &Request) -> Result<Value, RpcError> {
        let p = params::<m::SecretsUnlock>(r)?;
        let pass = SecretString::from(p.passphrase);
        self.secrets.unlock(&pass).map_err(secrets_err)?;
        self.creds.reload();
        to_result(&OkResult { ok: true })
    }

    pub fn list(&self) -> ProviderListResult {
        let mut providers = vec![];
        for (id, kind) in self.provider_ids() {
            let active = self.creds.try_active(&id).ok().flatten();
            let mut list: Vec<CredentialInfo> = self
                .creds
                .try_list(&id)
                .unwrap_or_default()
                .into_iter()
                .map(|c| CredentialInfo {
                    active: active.as_deref() == Some(c.id.as_str()),
                    credential_id: c.id,
                    label: format!("{} ({})", c.label, c.source.label()),
                    kind: wire_kind(c.kind),
                    hint: c.hint,
                })
                .collect();
            for var in credentials::env_vars(&id) {
                if let Some(v) = (self.env)(var).filter(|v| !v.trim().is_empty()) {
                    list.push(CredentialInfo {
                        credential_id: format!("env:{var}"),
                        label: credentials::CredentialSource::Environment(var.to_string()).label(),
                        kind: WireKind::Key,
                        active: active.is_none() && list.iter().all(|c| !c.active),
                        hint: key_hint(&v),
                    });
                }
            }
            let status = self.status_of(&id);
            let s = self.settings(&id);
            let endpoint = s.base_url.clone().or_else(|| {
                (kind == "ollama").then(|| genie_providers::route::OLLAMA_DEFAULT_BASE.to_string())
            });
            providers.push(ProviderInfo {
                id,
                kind,
                status,
                credentials: list,
                endpoint,
            });
        }
        ProviderListResult { providers }
    }

    fn credential_add(&self, r: &Request) -> Result<Value, RpcError> {
        let p = params::<m::ProviderCredentialAdd>(r)?;
        let kind_s = self
            .kind_of(&p.provider)
            .ok_or_else(|| RpcError::invalid_params(format!("unknown provider {}", p.provider)))?;
        if !self.allowed(&p.provider) {
            return Err(policy_blocked(
                "providers.allowed",
                format!(
                    "{} is not allowed by your organization's policy",
                    p.provider
                ),
            ));
        }
        self.endpoint_check(&p.provider)
            .map_err(|e| policy_blocked("endpoints.allowed", e))?;
        let kind = match p.kind {
            WireKind::Key => CredentialKind::Key,
            WireKind::External => CredentialKind::External,
            WireKind::Oauth => {
                return Err(RpcError::invalid_params(
                    "OAuth credentials are added by provider.login.start",
                ));
            }
            WireKind::Unknown => return Err(RpcError::invalid_params("unknown credential kind")),
        };
        if !self.method_allowed(kind) {
            return Err(policy_blocked(
                "auth.methods",
                format!(
                    "{} is not allowed by your organization's policy",
                    method_name(kind)
                ),
            ));
        }
        let label = if p.label.trim().is_empty() {
            "Default".to_string()
        } else {
            p.label.trim().to_string()
        };
        let (secret, hint) = match kind {
            CredentialKind::Key => {
                let k = p
                    .secret
                    .as_deref()
                    .map(str::trim)
                    .filter(|k| !k.is_empty())
                    .ok_or_else(|| RpcError::invalid_params("an API key is required"))?;
                if k.chars().any(char::is_control) {
                    return Err(RpcError::invalid_params(
                        "the key contains control characters",
                    ));
                }
                (SecretString::from(k.to_string()), key_hint(k))
            }
            _ => {
                if kind_s != "google" {
                    return Err(RpcError::invalid_params(
                        "external credentials are only supported for Google (ADC)",
                    ));
                }
                let path = p.secret.filter(|s| !s.trim().is_empty());
                if let Some(pth) = &path
                    && !std::path::Path::new(pth).is_file()
                {
                    return Err(RpcError::invalid_params(format!("{pth} does not exist")));
                }
                let r = AdcRef {
                    adc_path: path.clone(),
                };
                let s = serde_json::to_string(&r).unwrap_or_default();
                (
                    SecretString::from(s),
                    Some(path.unwrap_or_else(|| "Application Default Credentials".into())),
                )
            }
        };
        let meta = self
            .creds
            .add(&p.provider, &label, kind, &secret, hint)
            .map_err(secrets_err)?;
        self.credential_changed(&p.provider, "added", Some(kind));
        to_result(&CredentialAddResult {
            credential_id: meta.id,
        })
    }

    fn stored(&self, id: &str) -> Result<CredentialMeta, RpcError> {
        if id.starts_with("env:") {
            return Err(RpcError::invalid_params(
                "this key comes from an environment variable; change it there",
            ));
        }
        self.creds
            .meta_of(id)
            .map_err(secrets_err)?
            .ok_or_else(|| not_found("credential"))
    }

    fn credential_remove(&self, r: &Request) -> Result<Value, RpcError> {
        let p = params::<m::ProviderCredentialRemove>(r)?;
        let meta = self.stored(&p.credential_id)?;
        self.creds.try_delete(&meta.id).map_err(secrets_err)?;
        self.forget_tokens(&meta.id);
        self.credential_changed(&meta.provider, "removed", Some(meta.kind));
        to_result(&Empty {})
    }

    fn credential_activate(&self, r: &Request) -> Result<Value, RpcError> {
        let p = params::<m::ProviderCredentialActivate>(r)?;
        let meta = self.stored(&p.credential_id)?;
        self.creds.try_set_active(&meta.id).map_err(secrets_err)?;
        self.credential_changed(&meta.provider, "activated", Some(meta.kind));
        to_result(&Empty {})
    }

    fn entra_config(&self, provider: &str) -> Result<EntraConfig, RpcError> {
        let s = self.settings(provider);
        let client_id = s.client_id.clone().or_else(|| self.entra_client_id.clone()).ok_or_else(|| {
            RpcError::new(
                ErrorKind::InvalidState,
                "Entra sign-in isn't configured in this build (no Genie Entra app id); use an API key or set providers.azure-foundry.client_id",
            )
        })?;
        let mut cfg = EntraConfig::new(client_id);
        if let Some(t) = s.tenant.filter(|t| !t.is_empty()) {
            cfg.tenant = t;
        }
        if let Some(a) = s.authority {
            let ok = a.starts_with("https://")
                || a.starts_with("http://127.0.0.1:")
                || a.starts_with("http://localhost:");
            if !ok {
                return Err(RpcError::invalid_params("authority must be an https URL"));
            }
            cfg.authority = a;
        }
        Ok(cfg)
    }

    async fn login_start(
        self: &Arc<Self>,
        provider: &str,
        method: LoginMethod,
    ) -> Result<LoginStartResult, RpcError> {
        if self.kind_of(provider).as_deref() != Some("azure-foundry") {
            return Err(RpcError::invalid_params(format!(
                "{provider} has no sign-in flow; add an API key instead"
            )));
        }
        if !self.allowed(provider) {
            return Err(policy_blocked(
                "providers.allowed",
                format!("{provider} is not allowed by your organization's policy"),
            ));
        }
        self.endpoint_check(provider)
            .map_err(|e| policy_blocked("endpoints.allowed", e))?;
        if !self.method_allowed(CredentialKind::OAuth) {
            return Err(policy_blocked(
                "auth.methods",
                "Entra sign-in is not allowed by your organization's policy".into(),
            ));
        }
        let cfg = self.entra_config(provider)?;
        let login_id = random_id("login");
        let (result, wait) = match method {
            LoginMethod::Pkce => {
                let l = oauth::start_pkce(&cfg, self.http.clone(), oauth::LOGIN_TIMEOUT)
                    .await
                    .map_err(|e| RpcError::new(ErrorKind::InternalError, e))?;
                (
                    LoginStartResult {
                        login_id: login_id.clone(),
                        auth_url: Some(l.auth_url),
                        user_code: None,
                        verification_uri: None,
                    },
                    l.wait,
                )
            }
            LoginMethod::DeviceCode => {
                let l = oauth::start_device_code(&cfg, self.http.clone())
                    .await
                    .map_err(|e| {
                        RpcError::new(ErrorKind::InternalError, format!("device code: {e}"))
                    })?;
                (
                    LoginStartResult {
                        login_id: login_id.clone(),
                        auth_url: None,
                        user_code: Some(l.user_code),
                        verification_uri: Some(l.verification_uri),
                    },
                    l.wait,
                )
            }
            LoginMethod::Unknown => return Err(RpcError::invalid_params("unknown login method")),
        };
        {
            let mut g = lock(&self.logins);
            g.retain(|_, l| {
                l.finished
                    .is_none_or(|f| f.elapsed() < Duration::from_secs(3600))
            });
            g.insert(
                login_id.clone(),
                Login {
                    provider: provider.into(),
                    state: LoginState::Pending,
                    error: None,
                    finished: None,
                },
            );
        }
        let me = self.clone();
        let id = login_id.clone();
        let tenant = cfg.tenant.clone();
        self.spawn(async move {
            let outcome = match wait.await {
                Ok(grant) => match grant.to_secret() {
                    Ok(secret) => me
                        .creds
                        .add(
                            &me.login_provider(&id),
                            &format!("Entra ({tenant})"),
                            CredentialKind::OAuth,
                            &secret,
                            Some(tenant.clone()),
                        )
                        .map(|m| {
                            // A fresh sign-in becomes the active credential.
                            let _ = me.creds.try_set_active(&m.id);
                        })
                        .map_err(|e| e.to_string()),
                    Err(e) => Err(e),
                },
                Err(e) => Err(e),
            };
            let provider = me.login_provider(&id);
            {
                let mut g = lock(&me.logins);
                if let Some(l) = g.get_mut(&id) {
                    l.finished = Some(Instant::now());
                    match &outcome {
                        Ok(()) => l.state = LoginState::Done,
                        Err(e) => {
                            l.state = LoginState::Failed;
                            l.error = Some(e.clone());
                        }
                    }
                }
            }
            if outcome.is_ok() {
                me.audit(
                    AuditEvent::SignedIn,
                    Attrs::new()
                        .with("provider", provider.as_str())
                        .with("method", "entra"),
                );
                me.credential_changed(&provider, "signed_in", Some(CredentialKind::OAuth));
            } else {
                tracing::warn!(provider = %provider, "sign-in failed");
            }
        });
        Ok(result)
    }

    fn login_provider(&self, id: &str) -> String {
        lock(&self.logins)
            .get(id)
            .map(|l| l.provider.clone())
            .unwrap_or_default()
    }

    fn login_status(&self, r: &Request) -> Result<Value, RpcError> {
        let p = params::<m::ProviderLoginStatus>(r)?;
        let g = lock(&self.logins);
        let l = g.get(&p.login_id).ok_or_else(|| not_found("login"))?;
        to_result(&LoginStatusResult {
            state: l.state,
            error: l.error.clone(),
        })
    }

    /// An authenticated GET (`auth` applied like on a model request).
    async fn get(
        &self,
        url: &str,
        auth: &Arc<dyn Auth>,
        extra: &[(&str, &str)],
    ) -> Result<Value, String> {
        let mut parts = http::Request::builder()
            .method(http::Method::GET)
            .uri(url)
            .body(())
            .map_err(|e| e.to_string())?
            .into_parts()
            .0;
        auth.apply(&mut parts, b"").await.map_err(|e| e.message)?;
        let mut req = self
            .http
            .get(url)
            .headers(parts.headers)
            .timeout(Duration::from_secs(10));
        for (k, v) in extra {
            req = req.header(*k, *v);
        }
        let resp = req.send().await.map_err(|e| e.to_string())?;
        let status = resp.status();
        if !status.is_success() {
            return Err(match status.as_u16() {
                401 | 403 => format!("{status}: the credential was rejected"),
                _ => format!("{status}"),
            });
        }
        let b = resp.bytes().await.map_err(|e| e.to_string())?;
        serde_json::from_slice(&b).map_err(|e| e.to_string())
    }

    /// The models endpoint used for discovery and `provider.test`.
    fn models_endpoint(
        &self,
        kind: &str,
        s: &ProviderSettings,
    ) -> Option<(String, Vec<(&'static str, &'static str)>)> {
        let base = |d: &str| s.base_url.clone().unwrap_or_else(|| d.to_string());
        let b = match kind {
            "anthropic" => {
                return Some((
                    format!(
                        "{}/v1/models",
                        base("https://api.anthropic.com").trim_end_matches('/')
                    ),
                    vec![("anthropic-version", "2023-06-01")],
                ));
            }
            "openai" => format!(
                "{}/v1/models",
                base("https://api.openai.com").trim_end_matches('/')
            ),
            "openrouter" => format!(
                "{}/v1/models",
                base("https://openrouter.ai/api").trim_end_matches('/')
            ),
            "google" => format!(
                "{}/v1beta/models",
                base("https://generativelanguage.googleapis.com").trim_end_matches('/')
            ),
            "ollama" => format!(
                "{}/api/tags",
                base(genie_providers::route::OLLAMA_DEFAULT_BASE).trim_end_matches('/')
            ),
            "azure-foundry" => format!(
                "{}/openai/v1/models",
                s.base_url.clone()?.trim_end_matches('/')
            ),
            _ => format!("{}/v1/models", s.base_url.clone()?.trim_end_matches('/')),
        };
        Some((b, vec![]))
    }

    async fn test(&self, provider: &str) -> Result<ProviderTestResult, RpcError> {
        let kind = self
            .kind_of(provider)
            .ok_or_else(|| RpcError::invalid_params(format!("unknown provider {provider}")))?;
        let started = Instant::now();
        let outcome: Result<(), String> = async {
            let (preset, auth, s, _) = self
                .auth_for(provider)?
                .ok_or_else(|| "no credential configured".to_string())?;
            if preset == Preset::Vertex {
                // ADC: getting a token is the test.
                let mut parts = http::Request::builder()
                    .uri("https://aiplatform.googleapis.com/")
                    .body(())
                    .map_err(|e| e.to_string())?
                    .into_parts()
                    .0;
                return auth.apply(&mut parts, b"").await.map_err(|e| e.message);
            }
            let (url, extra) = self
                .models_endpoint(&kind, &s)
                .ok_or_else(|| "set the provider's endpoint first".to_string())?;
            self.get(&url, &auth, &extra).await.map(|_| ())
        }
        .await;
        let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        match &outcome {
            Ok(()) => {
                lock(&self.errors).remove(provider);
            }
            Err(e) => {
                lock(&self.errors).insert(provider.to_string(), e.clone());
            }
        }
        self.refresh_statuses();
        Ok(ProviderTestResult {
            ok: outcome.is_ok(),
            latency_ms,
            error: outcome.err(),
        })
    }

    // ---- catalog -------------------------------------------------------------------

    async fn discover(&self, provider: &str, kind: &str) -> Vec<Discovered> {
        let Ok(Some((preset, auth, s, _))) = self.auth_for(provider) else {
            return vec![];
        };
        if matches!(preset, Preset::Vertex | Preset::AzureFoundry) || kind == "anthropic" {
            return vec![];
        }
        let Some((url, extra)) = self.models_endpoint(kind, &s) else {
            return vec![];
        };
        match self.get(&url, &auth, &extra).await {
            Ok(body) => match kind {
                "google" => discovery::gemini_models(provider, &body),
                "ollama" => discovery::ollama_tags(provider, &body),
                "openai" => discovery::openai_models(provider, Protocol::OpenaiResponses, &body),
                _ => discovery::openai_models(provider, Protocol::OpenaiChat, &body),
            },
            Err(e) => {
                tracing::debug!(provider, error = %e, "model discovery failed");
                vec![]
            }
        }
    }

    /// Merge base + discovery + Azure deployments + overrides + policy into the shared
    /// catalog the resolver reads.
    pub fn rebuild_catalog(&self) {
        let mut cat = {
            let g = lock(&self.cat);
            let mut c = g.base.clone();
            for (found, _) in g.discovered.values().map(|(_, f)| (f, ())) {
                c.merge_discovered(found);
            }
            c
        };
        for (id, kind) in self.provider_ids() {
            if kind != "azure-foundry" {
                continue;
            }
            for d in self.settings(&id).deployments {
                let proto = cat
                    .entries()
                    .find(|e| e.wire_model_id == d.model || e.id == d.model)
                    .map(|e| e.protocol)
                    .filter(|p| *p == Protocol::AnthropicMessages)
                    .unwrap_or(Protocol::OpenaiResponses);
                let e = discovery::azure_deployment(&cat, &id, &d.name, &d.model, proto);
                if let Ok(v) = serde_json::to_value(&e) {
                    let _ = cat.apply_override(&e.id, &v);
                }
            }
        }
        if let Some(Value::Object(over)) = self.setting("models") {
            for (id, patch) in over {
                if let Err(e) = cat.apply_override(&id, &patch) {
                    tracing::warn!(model = %id, error = %e, "model override ignored");
                }
            }
        }
        let mut pol = genie_catalog::Policy::default();
        if let Some(allowed) = self.policy_list("providers.allowed") {
            pol.hidden_providers = cat
                .entries()
                .map(|e| e.provider.clone())
                .filter(|p| !allowed.contains(p))
                .collect();
        }
        // Providers whose endpoint the policy refuses are hidden too (`endpoints.allowed`).
        if self.policy_list("endpoints.allowed").is_some() {
            let blocked: Vec<String> = cat
                .entries()
                .map(|e| e.provider.clone())
                .filter(|p| self.endpoint_check(p).is_err())
                .collect();
            for p in blocked {
                if !pol.hidden_providers.contains(&p) {
                    pol.hidden_providers.push(p);
                }
            }
        }
        let allowed_models = self.policy_list("models.allowed");
        let blocked = self.policy_list("models.blocked").unwrap_or_default();
        pol.hidden_models = cat
            .entries()
            .filter(|e| {
                matches_any(&e.id, &blocked)
                    || allowed_models
                        .as_ref()
                        .is_some_and(|a| !matches_any(&e.id, a))
            })
            .map(|e| e.id.clone())
            .collect();
        cat.apply_policy(&pol);
        *self.catalog.write().unwrap_or_else(|p| p.into_inner()) = cat;
    }

    pub async fn model_list(&self, provider: Option<&str>) -> ModelListResult {
        // Refresh stale discovery for providers that have credentials.
        let stale: Vec<(String, String)> = {
            let g = lock(&self.cat);
            self.provider_ids()
                .into_iter()
                .filter(|(id, _)| provider.is_none_or(|p| p == id))
                .filter(|(id, _)| {
                    g.discovered
                        .get(id)
                        .is_none_or(|(t, _)| t.elapsed() > DISCOVERY_TTL)
                })
                .collect()
        };
        if !stale.is_empty() {
            let found =
                futures::future::join_all(stale.iter().map(|(id, kind)| self.discover(id, kind)))
                    .await;
            let mut g = lock(&self.cat);
            for ((id, _), f) in stale.into_iter().zip(found) {
                g.discovered.insert(id, (Instant::now(), f));
            }
        }
        self.rebuild_catalog();
        let cat = self.catalog.read().unwrap_or_else(|p| p.into_inner());
        let models = cat
            .entries()
            .filter(|e| provider.is_none_or(|p| e.provider == p))
            .filter(|e| !e.is_deprecated())
            .map(model_info)
            .collect();
        ModelListResult { models }
    }

    pub fn catalog_status(&self) -> CatalogStatus {
        let g = lock(&self.cat);
        CatalogStatus {
            source: match g.base.source {
                Source::Bundled => CatalogSource::Bundled,
                Source::Cached => CatalogSource::Cached,
                Source::Fetched => CatalogSource::Fresh,
            },
            fetched_at: g.fetched_at.clone(),
            etag: self.cache.etag(),
        }
    }

    pub async fn catalog_refresh(&self) -> Result<CatalogStatus, RpcError> {
        let feed = match &self.feed {
            Some(f) => Some((f.url.clone(), f.fetcher.clone(), f.keys.clone())),
            None => self.default_feed().map(|f| (f.url, f.fetcher, f.keys)),
        };
        let Some((url, fetcher, keys)) = feed else {
            return Ok(self.catalog_status());
        };
        match self.cache.refresh(fetcher.as_ref(), &url, &keys).await {
            Ok(RefreshOutcome::Updated) => {
                let mut c = genie_catalog::refresh::load_best(&self.cache, &keys);
                if c.source == Source::Cached {
                    c.source = Source::Fetched;
                }
                {
                    let mut g = lock(&self.cat);
                    g.base = c;
                    g.fetched_at = rfc3339_now();
                }
                self.rebuild_catalog();
                let st = self.catalog_status();
                self.emit(
                    m::CatalogUpdated::NAME,
                    &CatalogUpdated {
                        source: st.source,
                        fetched_at: st.fetched_at.clone(),
                    },
                );
                Ok(st)
            }
            Ok(RefreshOutcome::NotModified) => {
                lock(&self.cat).fetched_at = rfc3339_now();
                Ok(self.catalog_status())
            }
            Err(e) => {
                // A failed refresh keeps the last good copy (03 §8.1).
                tracing::warn!(error = %e, "catalog refresh failed");
                Err(RpcError::new(
                    ErrorKind::InternalError,
                    format!("catalog refresh failed: {e}"),
                ))
            }
        }
    }

    // ---- usage ---------------------------------------------------------------------

    fn usage_query(&self, r: &Request) -> Result<Value, RpcError> {
        let p = params::<m::UsageQuery>(r)?;
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| RpcError::new(ErrorKind::InvalidState, "storage unavailable"))?;
        let day = |s: &str| -> Result<String, RpcError> {
            let d: String = s.chars().take(10).collect();
            let b = d.as_bytes();
            let ok = b.len() == 10
                && b[4] == b'-'
                && b[7] == b'-'
                && d.chars()
                    .enumerate()
                    .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit());
            ok.then_some(d)
                .ok_or_else(|| RpcError::invalid_params(format!("{s} is not an RFC 3339 date")))
        };
        let (from, to) = (day(&p.from)?, day(&p.to)?);
        if p.group_by.contains(&UsageGroup::Unknown) {
            return Err(RpcError::invalid_params("unknown group_by value"));
        }
        let rows = store
            .read(|c| genie_store::kv::usage_between(c, &from, &to))
            .map_err(|e| RpcError::new(ErrorKind::InternalError, e.to_string()))?;
        to_result(&aggregate(&rows, &p.group_by))
    }

    fn budget_set(&self, r: &Request) -> Result<Value, RpcError> {
        let p = params::<m::UsageBudgetSet>(r)?;
        if p.monthly_usd.is_some_and(|v| !v.is_finite() || v < 0.0) {
            return Err(RpcError::invalid_params(
                "monthly_usd must be a positive number",
            ));
        }
        let store = self
            .store
            .as_ref()
            .ok_or_else(|| RpcError::new(ErrorKind::InvalidState, "storage unavailable"))?;
        store
            .write(|tx| match p.monthly_usd {
                Some(v) if v > 0.0 => {
                    genie_store::kv::set(tx, BUDGET_KEY, &json!({"monthly_usd": v}))
                }
                _ => genie_store::kv::delete(tx, BUDGET_KEY),
            })
            .map_err(|e| RpcError::new(ErrorKind::InternalError, e.to_string()))?;
        self.check_budget();
        to_result(&Empty {})
    }

    /// Month-to-date spend vs the budget; one `notification` per month and level.
    pub fn check_budget(&self) -> Option<u8> {
        let store = self.store.as_ref()?;
        let today = genie_core::runtime::context::utc_day(genie_core::bus::now_ms());
        let month = today.get(..7)?.to_string();
        let (budget, warned) = store
            .read(|c| genie_store::kv::get(c, BUDGET_KEY))
            .ok()
            .flatten()
            .map(|v| {
                let w = v
                    .get("warned")
                    .filter(|w| w.get("month").and_then(Value::as_str) == Some(month.as_str()))
                    .and_then(|w| w.get("level"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                (v.get("monthly_usd").and_then(Value::as_f64), w)
            })?;
        let budget = budget.filter(|b| *b > 0.0)?;
        let spent: f64 = store
            .read(|c| {
                genie_store::kv::usage_between(c, &format!("{month}-01"), &format!("{month}-31"))
            })
            .ok()?
            .iter()
            .map(|u| u.cost_usd)
            .sum();
        let level: u8 = if spent >= budget {
            100
        } else if spent >= budget * 0.8 {
            80
        } else {
            0
        };
        if level == 0 || u64::from(level) <= warned {
            return None;
        }
        let _ = store.write(|tx| {
            genie_store::kv::set(
                tx,
                BUDGET_KEY,
                &json!({"monthly_usd": budget, "warned": {"month": month, "level": level}}),
            )
        });
        let body = if level >= 100 {
            format!(
                "You've spent ${spent:.2} this month, over your ${budget:.2} budget. Genie keeps working; this is only a warning."
            )
        } else {
            format!("You've spent ${spent:.2} of your ${budget:.2} monthly budget (80%).")
        };
        self.emit(
            m::UserNotification::NAME,
            &UserNotification {
                title: if level >= 100 {
                    "Monthly budget reached".into()
                } else {
                    "80% of monthly budget used".into()
                },
                body,
                session_id: None,
                ask_id: None,
                link: Some("genie://settings/usage".into()),
            },
        );
        Some(level)
    }

    /// Background work: statuses on settings/policy changes, budget checks (1 min),
    /// catalog refresh on start and every ~6 h.
    pub(crate) fn start(self: &Arc<Self>, state: &Arc<DaemonState>) {
        *lock(&self.state) = Arc::downgrade(state);
        let scope = state.root_scope().child("providers");
        *lock(&self.scope) = Some(scope.clone());
        self.refresh_statuses();
        if let Some(hub) = &self.hub {
            let mut rx = hub.subscribe();
            let me = self.clone();
            scope.spawn_cancellable(async move {
                use tokio::sync::broadcast::error::RecvError;
                while let Ok(_) | Err(RecvError::Lagged(_)) = rx.recv().await {
                    me.rebuild_catalog();
                    me.refresh_statuses();
                }
            });
        }
        if self.store.is_some() {
            let me = self.clone();
            scope.spawn_cancellable(async move {
                loop {
                    me.check_budget();
                    tokio::time::sleep(Duration::from_secs(60)).await;
                }
            });
        }
        if self.feed.is_some() || self.default_feed().is_some() {
            let me = self.clone();
            scope.spawn_cancellable(async move {
                loop {
                    let _ = me.catalog_refresh().await;
                    tokio::time::sleep(genie_catalog::refresh::REFRESH_INTERVAL).await;
                }
            });
        }
    }
}

fn model_info(e: &Entry) -> ModelInfo {
    ModelInfo {
        id: e.id.clone(),
        provider: e.provider.clone(),
        name: e.display_name.clone(),
        context_window: Some(e.context_window).filter(|c| *c > 0),
        effort_levels: e
            .effort_levels()
            .into_iter()
            .filter_map(|l| serde_json::to_value(l).ok()?.as_str().map(String::from))
            .collect(),
        extra: serde_json::to_value(e).unwrap_or(Value::Null),
    }
}

/// Group `usage_daily` rows (D34): one row per distinct key tuple, ordered by keys.
fn aggregate(rows: &[genie_store::kv::Usage], group_by: &[UsageGroup]) -> UsageQueryResult {
    let mut acc: BTreeMap<Vec<String>, UsageRow> = BTreeMap::new();
    for u in rows {
        let keys: Vec<String> = group_by
            .iter()
            .map(|g| match g {
                UsageGroup::Day => u.day.clone(),
                UsageGroup::Provider => u.provider.clone(),
                UsageGroup::Model => u.model.clone(),
                UsageGroup::Project => u.project_id.clone(),
                UsageGroup::Unknown => String::new(),
            })
            .collect();
        let row = acc.entry(keys.clone()).or_insert_with(|| UsageRow {
            keys,
            usage: Usage::default(),
            cost: Cost { usd: 0.0 },
            compaction_tokens: 0,
        });
        let n = |v: i64| u64::try_from(v).unwrap_or(0);
        row.usage.input_tokens += n(u.tokens_in);
        row.usage.output_tokens += n(u.tokens_out);
        row.usage.cache_read_tokens += n(u.cache_read);
        row.usage.cache_write_tokens += n(u.cache_write);
        row.cost.usd += u.cost_usd;
        if u.kind == "compaction" {
            row.compaction_tokens += n(u.tokens_in) + n(u.tokens_out);
        }
    }
    UsageQueryResult {
        rows: acc.into_values().collect(),
    }
}

impl ProviderBinder for ProviderHub {
    fn bind(&self, provider: &str) -> Option<Result<ProviderConfig, ResolveError>> {
        self.kind_of(provider)?;
        Some(match self.auth_for(provider) {
            Ok(Some((preset, auth, s, account))) => Ok(ProviderConfig {
                id: ProviderId::new(provider),
                preset,
                base_url: s.base_url,
                project: s.project,
                location: s.location,
                auth,
                account,
                compat_overlay: None,
                headers: vec![],
            }),
            Ok(None) => Err(ResolveError {
                class: "no_provider_configured",
                message: if self.secrets.is_locked() {
                    "the secrets file is locked".into()
                } else {
                    format!("no credential for {provider}")
                },
                fix_link: Some("genie://settings/providers".into()),
            }),
            Err(e) => Err(ResolveError {
                class: "no_provider_configured",
                message: e,
                fix_link: Some("genie://settings/providers".into()),
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(day: &str, provider: &str, kind: &str, cost: f64) -> genie_store::kv::Usage {
        genie_store::kv::Usage {
            day: day.into(),
            provider: provider.into(),
            model: "m".into(),
            project_id: String::new(),
            kind: kind.into(),
            tokens_in: 10,
            tokens_out: 5,
            cache_read: 3,
            cache_write: 1,
            calls: 1,
            cost_usd: cost,
        }
    }

    #[test]
    fn aggregates_by_groups_with_compaction_lines() {
        let rows = vec![
            u("2026-11-01", "openai", "chat", 1.0),
            u("2026-11-01", "openai", "compaction", 0.5),
            u("2026-11-02", "anthropic", "chat", 2.0),
        ];
        let r = aggregate(&rows, &[UsageGroup::Provider]);
        assert_eq!(r.rows.len(), 2);
        let openai = r.rows.iter().find(|x| x.keys == ["openai"]).unwrap();
        assert_eq!(openai.usage.input_tokens, 20);
        assert_eq!(openai.compaction_tokens, 15);
        assert!((openai.cost.usd - 1.5).abs() < 1e-9);
        let total = aggregate(&rows, &[]);
        assert_eq!(total.rows.len(), 1);
        assert!((total.rows[0].cost.usd - 3.5).abs() < 1e-9);
        let by_day = aggregate(&rows, &[UsageGroup::Day, UsageGroup::Provider]);
        assert_eq!(by_day.rows[0].keys, ["2026-11-01", "openai"]);
    }

    #[test]
    fn glob_matching() {
        let pats = vec!["openai/*".to_string(), "anthropic/x".to_string()];
        assert!(matches_any("openai/gpt", &pats));
        assert!(matches_any("anthropic/x", &pats));
        assert!(!matches_any("anthropic/y", &pats));
    }
}
