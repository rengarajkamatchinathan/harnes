//! OAuth flows against a fake local token endpoint (never real Microsoft / Google):
//! Entra auth code + PKCE over the loopback redirect, device code, refresh under the
//! cross-process lock (including "another process already rotated it"), Google ADC
//! authorized-user and service-account (RS256 verified by the fake).

#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use genie_providers::adc::AdcTokenSource;
use genie_providers::auth::{Auth, TokenAuth, TokenSource};
use genie_providers::oauth::{
    self, EntraConfig, GrantStore, OAuthGrant, OAuthTokenSource, start_device_code, start_pkce,
};
use genie_secrets::ExposeSecret;
use sha2::Digest;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Test-only RSA key (generated for these tests; not used anywhere else).
const TEST_KEY: &str = include_str!("fixtures/test-only-rsa-key.pem");

#[derive(Default)]
struct FakeState {
    /// Requests seen: (path, form).
    seen: Vec<(String, HashMap<String, String>)>,
    /// PKCE challenge from the authorize URL.
    challenge: Option<String>,
    device_polls: u32,
    refresh_n: u32,
}

struct Fake {
    base: String,
    state: Arc<Mutex<FakeState>>,
}

fn parse_form(body: &str) -> HashMap<String, String> {
    url::form_urlencoded::parse(body.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

async fn read_request(sock: &mut tokio::net::TcpStream) -> (String, String) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = sock.read(&mut chunk).await.unwrap();
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..pos]).to_string();
            let len = head
                .lines()
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.eq_ignore_ascii_case("content-length")
                        .then(|| v.trim().parse::<usize>().ok())?
                })
                .unwrap_or(0);
            while buf.len() < pos + 4 + len {
                let n = sock.read(&mut chunk).await.unwrap();
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            let path = head
                .lines()
                .next()
                .unwrap()
                .split(' ')
                .nth(1)
                .unwrap()
                .to_string();
            let body = String::from_utf8_lossy(&buf[pos + 4..pos + 4 + len]).to_string();
            return (path, body);
        }
    }
    (String::new(), String::new())
}

fn json_resp(status: u16, v: serde_json::Value) -> String {
    let b = v.to_string();
    format!(
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}",
        b.len()
    )
}

impl Fake {
    async fn start() -> Fake {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://127.0.0.1:{}", l.local_addr().unwrap().port());
        let state: Arc<Mutex<FakeState>> = Default::default();
        let st = state.clone();
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = l.accept().await.unwrap();
                let st = st.clone();
                tokio::spawn(async move {
                    let (path, body) = read_request(&mut sock).await;
                    let form = parse_form(&body);
                    let resp = {
                        let mut s = st.lock().unwrap();
                        s.seen.push((path.clone(), form.clone()));
                        respond(&mut s, &path, &form)
                    };
                    sock.write_all(resp.as_bytes()).await.unwrap();
                    let _ = sock.shutdown().await;
                });
            }
        });
        Fake { base, state }
    }

    fn entra(&self) -> EntraConfig {
        EntraConfig {
            client_id: "genie-test-client".into(),
            tenant: "organizations".into(),
            authority: self.base.clone(),
            scope: "https://ai.azure.com/.default".into(),
        }
    }
}

fn respond(s: &mut FakeState, path: &str, f: &HashMap<String, String>) -> String {
    let gt = f.get("grant_type").map(String::as_str).unwrap_or("");
    match (path, gt) {
        (p, _) if p.ends_with("/devicecode") => {
            assert_eq!(f["client_id"], "genie-test-client");
            assert!(f["scope"].contains("offline_access"));
            json_resp(
                200,
                serde_json::json!({
                    "device_code": "dev-123", "user_code": "ABCD-EFGH",
                    "verification_uri": "https://microsoft.com/devicelogin",
                    "expires_in": 900, "interval": 1
                }),
            )
        }
        (p, "authorization_code") if p.ends_with("/token") => {
            let verifier = &f["code_verifier"];
            let expect = URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(verifier.as_bytes()));
            assert_eq!(Some(expect), s.challenge.clone(), "PKCE S256 must match");
            assert_eq!(f["code"], "the-code");
            json_resp(
                200,
                serde_json::json!({"access_token": "at-pkce", "refresh_token": "rt-1",
                    "token_type": "Bearer", "expires_in": 3600}),
            )
        }
        (p, "urn:ietf:params:oauth:grant-type:device_code") if p.ends_with("/token") => {
            s.device_polls += 1;
            if s.device_polls == 1 {
                json_resp(400, serde_json::json!({"error": "authorization_pending"}))
            } else {
                json_resp(
                    200,
                    serde_json::json!({"access_token": "at-device", "refresh_token": "rt-d",
                        "token_type": "Bearer", "expires_in": 3600}),
                )
            }
        }
        (p, "refresh_token") if p.ends_with("/oauth2/v2.0/token") => {
            s.refresh_n += 1;
            json_resp(
                200,
                serde_json::json!({"access_token": format!("at-r{}", s.refresh_n),
                    "refresh_token": format!("rt-r{}", s.refresh_n),
                    "token_type": "Bearer", "expires_in": 3600}),
            )
        }
        ("/google/token", "refresh_token") => {
            assert_eq!(f["client_secret"], "csecret");
            json_resp(
                200,
                serde_json::json!({"access_token": "ya29.user", "expires_in": 3599}),
            )
        }
        ("/google/token", "urn:ietf:params:oauth:grant-type:jwt-bearer") => {
            let jwt = &f["assertion"];
            let mut parts = jwt.split('.');
            let (h, c, sig) = (
                parts.next().unwrap(),
                parts.next().unwrap(),
                parts.next().unwrap(),
            );
            let claims: serde_json::Value =
                serde_json::from_slice(&URL_SAFE_NO_PAD.decode(c).unwrap()).unwrap();
            assert_eq!(claims["iss"], "sa@test.iam.gserviceaccount.com");
            assert!(claims["aud"].as_str().unwrap().ends_with("/google/token"));
            // Verify RS256 with the fixture's public key.
            let der = base64::engine::general_purpose::STANDARD
                .decode(
                    TEST_KEY
                        .lines()
                        .filter(|l| !l.starts_with("-----"))
                        .collect::<String>(),
                )
                .unwrap();
            let kp = ring::signature::RsaKeyPair::from_pkcs8(&der).unwrap();
            let pk = ring::signature::UnparsedPublicKey::new(
                &ring::signature::RSA_PKCS1_2048_8192_SHA256,
                kp.public().as_ref().to_vec(),
            );
            pk.verify(
                format!("{h}.{c}").as_bytes(),
                &URL_SAFE_NO_PAD.decode(sig).unwrap(),
            )
            .expect("valid RS256 signature");
            json_resp(
                200,
                serde_json::json!({"access_token": "ya29.sa", "expires_in": 3599}),
            )
        }
        _ => json_resp(404, serde_json::json!({"error": "not_found", "path": path})),
    }
}

#[tokio::test]
async fn pkce_loopback_sign_in() {
    let fake = Fake::start().await;
    let login = start_pkce(
        &fake.entra(),
        oauth::http_client().unwrap(),
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    let url = url::Url::parse(&login.auth_url).unwrap();
    assert!(login.auth_url.starts_with(&format!(
        "{}/organizations/oauth2/v2.0/authorize?",
        fake.base
    )));
    let q: HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(q["code_challenge_method"], "S256");
    assert_eq!(q["response_type"], "code");
    assert!(q["scope"].contains("https://ai.azure.com/.default"));
    assert!(q["redirect_uri"].starts_with("http://localhost:"));
    fake.state.lock().unwrap().challenge = Some(q["code_challenge"].clone());

    let wait = tokio::spawn(login.wait);
    // Be the browser: an unrelated request first, then a wrong state is rejected
    // only for the matching request — so send the right one.
    let port = q["redirect_uri"].rsplit(':').next().unwrap().to_string();
    let http = reqwest::Client::new();
    let r = http
        .get(format!("http://127.0.0.1:{port}/favicon.ico"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    let page = http
        .get(format!(
            "http://127.0.0.1:{port}/?code=the-code&state={}",
            q["state"]
        ))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(page.contains("signed in"));
    let grant = wait.await.unwrap().unwrap();
    assert_eq!(grant.access_token.as_deref(), Some("at-pkce"));
    assert_eq!(grant.refresh_token.as_deref(), Some("rt-1"));
    assert!(grant.valid_for(Duration::from_secs(60)));
}

#[tokio::test]
async fn pkce_wrong_state_fails() {
    let fake = Fake::start().await;
    let login = start_pkce(
        &fake.entra(),
        oauth::http_client().unwrap(),
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    let port = login.redirect_uri.rsplit(':').next().unwrap().to_string();
    let wait = tokio::spawn(login.wait);
    let _ = reqwest::get(format!("http://127.0.0.1:{port}/?code=x&state=forged")).await;
    let err = wait.await.unwrap().unwrap_err();
    assert!(err.contains("state"), "{err}");
}

#[tokio::test]
async fn device_code_sign_in() {
    let fake = Fake::start().await;
    let login = start_device_code(&fake.entra(), oauth::http_client().unwrap())
        .await
        .unwrap();
    assert_eq!(login.user_code, "ABCD-EFGH");
    assert_eq!(login.verification_uri, "https://microsoft.com/devicelogin");
    let grant = tokio::time::timeout(Duration::from_secs(20), login.wait)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(grant.access_token.as_deref(), Some("at-device"));
    assert_eq!(fake.state.lock().unwrap().device_polls, 2);
}

/// A grant store shared by two "processes" (two token sources).
#[derive(Default)]
struct SharedGrant(Mutex<Option<OAuthGrant>>);
impl GrantStore for SharedGrant {
    fn load(&self) -> Result<Option<OAuthGrant>, String> {
        Ok(self.0.lock().unwrap().clone())
    }
    fn save(&self, g: &OAuthGrant) -> Result<(), String> {
        *self.0.lock().unwrap() = Some(g.clone());
        Ok(())
    }
}

#[tokio::test]
async fn refresh_under_lock_and_rotation_by_another_process() {
    let fake = Fake::start().await;
    let dir = tempfile::tempdir().unwrap();
    let lock = dir.path().join("cred.lock");
    let store = Arc::new(SharedGrant::default());
    store
        .save(&OAuthGrant {
            issuer: "entra".into(),
            config: fake.entra(),
            refresh_token: Some("rt-0".into()),
            access_token: Some("expired".into()),
            expires_at: Some(1),
        })
        .unwrap();
    let a = OAuthTokenSource::new(store.clone(), oauth::http_client().unwrap(), lock.clone());
    let b = OAuthTokenSource::new(store.clone(), oauth::http_client().unwrap(), lock.clone());

    // Expired → refresh once; the new refresh token is persisted.
    let t = a.fetch(false).await.unwrap();
    assert_eq!(t.token.expose_secret(), "at-r1");
    assert_eq!(
        store.load().unwrap().unwrap().refresh_token.as_deref(),
        Some("rt-r1")
    );
    // Valid → no network.
    assert_eq!(b.fetch(false).await.unwrap().token.expose_secret(), "at-r1");
    assert_eq!(fake.state.lock().unwrap().refresh_n, 1);

    // Forced refresh while another process holds the lock and rotates meanwhile:
    // we must pick up its token instead of refreshing again.
    let held = genie_secrets::lock::FileLock::try_acquire(&lock)
        .unwrap()
        .unwrap();
    let st = store.clone();
    let rotate = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mut g = st.load().unwrap().unwrap();
        g.access_token = Some("at-other-process".into());
        g.expires_at = Some(u64::MAX / 2);
        st.save(&g).unwrap();
        drop(held);
    });
    let t = b.fetch(true).await.unwrap();
    rotate.await.unwrap();
    assert_eq!(t.token.expose_secret(), "at-other-process");
    assert_eq!(fake.state.lock().unwrap().refresh_n, 1);

    // Through TokenAuth: 401 → invalidate → forced refresh (now really refreshes).
    store
        .save(&OAuthGrant {
            access_token: Some("x".into()),
            expires_at: Some(u64::MAX / 2),
            ..store.load().unwrap().unwrap()
        })
        .unwrap();
    let auth = TokenAuth::new(Arc::new(OAuthTokenSource::new(
        store.clone(),
        oauth::http_client().unwrap(),
        lock,
    )));
    let mut parts = http::Request::builder()
        .uri("https://x/")
        .body(())
        .unwrap()
        .into_parts()
        .0;
    auth.apply(&mut parts, b"").await.unwrap();
    assert_eq!(parts.headers["authorization"], "Bearer x");
    assert!(auth.invalidate());
    auth.apply(&mut parts, b"").await.unwrap();
    assert_eq!(parts.headers["authorization"], "Bearer at-r2");
}

#[tokio::test]
async fn google_adc_authorized_user_and_service_account() {
    let fake = Fake::start().await;
    let dir = tempfile::tempdir().unwrap();
    let user = dir.path().join("adc-user.json");
    std::fs::write(
        &user,
        serde_json::json!({"type": "authorized_user", "client_id": "cid",
            "client_secret": "csecret", "refresh_token": "1//rt",
            "token_uri": format!("{}/google/token", fake.base)})
        .to_string(),
    )
    .unwrap();
    let src = AdcTokenSource::new(user, oauth::http_client().unwrap());
    assert_eq!(
        src.fetch(false).await.unwrap().token.expose_secret(),
        "ya29.user"
    );

    let sa = dir.path().join("sa.json");
    std::fs::write(
        &sa,
        serde_json::json!({"type": "service_account",
            "client_email": "sa@test.iam.gserviceaccount.com",
            "private_key": TEST_KEY,
            "token_uri": format!("{}/google/token", fake.base)})
        .to_string(),
    )
    .unwrap();
    let src = AdcTokenSource::new(sa, oauth::http_client().unwrap());
    assert_eq!(
        src.fetch(false).await.unwrap().token.expose_secret(),
        "ya29.sa"
    );

    let missing = AdcTokenSource::new(dir.path().join("nope.json"), oauth::http_client().unwrap());
    let e = missing.fetch(false).await.unwrap_err();
    assert!(e.message.contains("gcloud auth application-default login"));
}
