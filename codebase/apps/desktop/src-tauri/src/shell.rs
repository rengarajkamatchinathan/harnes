//! The Tauri 2 shell: window, IPC commands, the IPC Channel sink and the artifact
//! scheme. Everything the webview can call is here; the webview reaches it only
//! through `src/platform/tauri/` (01 §1.3).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use serde_json::{Value, json};
use tauri::http::{Response, StatusCode};
use tauri::ipc::Channel;
use tauri::{Manager, State};

use crate::artifact;
use crate::batch::BATCH_MS;
use crate::host::{self, StateFile};
use crate::relay::{HostInfo, HostMessage, Relay, Sink, WireError};

struct ChannelSink(Channel<HostMessage>);

impl Sink for ChannelSink {
    fn send(&self, msg: HostMessage) {
        if let Err(e) = self.0.send(msg) {
            tracing::debug!(error = %e, "webview channel closed");
        }
    }
}

struct App {
    relay: Arc<Relay>,
    state: StateFile,
    artifacts: Mutex<HashMap<(String, u32), Arc<artifact::Rendered>>>,
}

/// Start (or re-attach after a reload) the stream of [`HostMessage`]s.
#[tauri::command]
fn host_connect(app: State<'_, Arc<App>>, on_message: Channel<HostMessage>) -> HostInfo {
    app.relay.attach(Arc::new(ChannelSink(on_message)))
}

/// One JSON-RPC request to the daemon.
#[tauri::command]
async fn host_request(
    app: State<'_, Arc<App>>,
    method: String,
    params: Option<Value>,
) -> Result<Value, WireError> {
    app.relay
        .request(&method, params.unwrap_or_else(|| json!({})))
        .await
}

/// The answer to a daemon → client request (`permission.ask`).
#[tauri::command]
fn host_respond(
    app: State<'_, Arc<App>>,
    req_id: u64,
    result: Option<Value>,
    error: Option<WireError>,
) -> Result<(), WireError> {
    let res = match error {
        Some(e) => Err(e),
        None => Ok(result.unwrap_or(Value::Null)),
    };
    app.relay.respond(req_id, res)
}

#[tauri::command]
fn host_retry(app: State<'_, Arc<App>>) {
    app.relay.retry_now();
}

#[tauri::command]
fn host_open_external(url: String) -> Result<(), String> {
    host::check_external_url(&url)?;
    host::os_open(&url)
}

/// "Open in editor" / reveal: an existing local path only.
#[tauri::command]
fn host_open_path(path: String) -> Result<(), String> {
    let p = std::path::Path::new(&path);
    if !p.is_absolute() || !p.exists() {
        return Err("not an existing absolute path".into());
    }
    host::os_open(&path)
}

#[tauri::command]
fn host_state_load(app: State<'_, Arc<App>>) -> Value {
    app.state.load()
}

#[tauri::command]
fn host_state_save(app: State<'_, Arc<App>>, state: Value) -> Result<(), String> {
    app.state.save(&state).map_err(|e| e.to_string())
}

/// Save an export (`text`, or `base64`) to Downloads; returns the path.
#[tauri::command]
fn host_save_file(
    name: String,
    text: Option<String>,
    base64: Option<String>,
) -> Result<String, String> {
    let bytes = match (text, base64) {
        (Some(t), _) => t.into_bytes(),
        (None, Some(b)) => base64::engine::general_purpose::STANDARD
            .decode(b)
            .map_err(|e| e.to_string())?,
        (None, None) => return Err("nothing to save".into()),
    };
    host::save_download(&name, &bytes)
        .map(|p| p.display().to_string())
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn host_log(level: String, message: String) {
    match level.as_str() {
        "error" => tracing::error!(target: "webview", "{message}"),
        "warn" => tracing::warn!(target: "webview", "{message}"),
        _ => tracing::info!(target: "webview", "{message}"),
    }
}

fn respond(status: StatusCode, mime: &str, body: Vec<u8>) -> Response<Vec<u8>> {
    Response::builder()
        .status(status)
        .header("Content-Type", mime)
        .header("Content-Security-Policy", artifact::CSP)
        .header("X-Content-Type-Options", "nosniff")
        .header("Cache-Control", "no-store")
        .body(body)
        .unwrap_or_else(|_| Response::new(Vec::new()))
}

async fn load_artifact(app: &App, id: &str, n: u32) -> Result<Arc<artifact::Rendered>, String> {
    if let Some(r) = app
        .artifacts
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&(id.to_string(), n))
    {
        return Ok(r.clone());
    }
    let got = app
        .relay
        .request("artifact.get", json!({"artifact_id": id, "n": n}))
        .await
        .map_err(|e| e.message)?;
    let kind = got["type"].as_str().unwrap_or("code").to_string();
    let title = got["title"].as_str().unwrap_or("").to_string();
    let content = match got["content"].as_str() {
        Some(c) => c.to_string(),
        None => match got["blob_id"].as_str() {
            Some(blob) => read_blob(&app.relay, blob).await?,
            None => String::new(),
        },
    };
    let rendered = Arc::new(artifact::render(&kind, &title, &content));
    let mut cache = app.artifacts.lock().unwrap_or_else(|e| e.into_inner());
    if cache.len() > 64 {
        cache.clear();
    }
    cache.insert((id.to_string(), n), rendered.clone());
    Ok(rendered)
}

async fn read_blob(relay: &Relay, blob: &str) -> Result<String, String> {
    const CHUNK: u64 = 1 << 20;
    let mut out = Vec::new();
    let mut offset = 0u64;
    loop {
        let r = relay
            .request(
                "blob.get",
                json!({"blob_id": blob, "offset": offset, "len": CHUNK}),
            )
            .await
            .map_err(|e| e.message)?;
        let data = base64::engine::general_purpose::STANDARD
            .decode(r["data_b64"].as_str().unwrap_or(""))
            .map_err(|e| e.to_string())?;
        let size = r["size"].as_u64().unwrap_or(0);
        if data.is_empty() {
            break;
        }
        offset += data.len() as u64;
        out.extend_from_slice(&data);
        if offset >= size || out.len() > 32 << 20 {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

async fn serve_artifact(app: Arc<App>, path: String) -> Response<Vec<u8>> {
    if path == "/_genie/bridge.js" {
        return respond(
            StatusCode::OK,
            "text/javascript; charset=utf-8",
            artifact::BRIDGE_JS.as_bytes().to_vec(),
        );
    }
    let Some((id, n, file)) = artifact::parse_path(&path) else {
        return respond(StatusCode::NOT_FOUND, "text/plain", b"not found".to_vec());
    };
    let rendered = match load_artifact(&app, &id, n).await {
        Ok(r) => r,
        Err(e) => return respond(StatusCode::BAD_GATEWAY, "text/plain", e.into_bytes()),
    };
    if file == "index.html" {
        return respond(
            StatusCode::OK,
            rendered.mime,
            rendered.body.clone().into_bytes(),
        );
    }
    let script = file
        .strip_prefix("inline-")
        .and_then(|s| s.strip_suffix(".js"))
        .and_then(|k| k.parse::<usize>().ok())
        .and_then(|k| rendered.scripts.get(k));
    match script {
        Some(s) => respond(
            StatusCode::OK,
            "text/javascript; charset=utf-8",
            s.clone().into_bytes(),
        ),
        None => respond(StatusCode::NOT_FOUND, "text/plain", b"not found".to_vec()),
    }
}

/// Build and run the app. Returns when the last window closes.
pub fn run() -> Result<(), String> {
    crate::init_tracing();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("genie-desktop")
        .build()
        .map_err(|e| e.to_string())?;
    tauri::async_runtime::set(rt.handle().clone());

    let endpoint =
        genie_client::Endpoint::for_channel(host::channel()).map_err(|e| e.to_string())?;
    let dir = host::desktop_dir(&endpoint);
    let instance = host::instance_id(&dir);
    let relay = Relay::new(rt.handle().clone(), instance.clone());
    let batch_ms = std::env::var("GENIE_COALESCE_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(BATCH_MS);
    relay.start_flusher(Duration::from_millis(batch_ms.max(1)));
    relay.start(host::ensure_options(endpoint, &instance));

    let app = Arc::new(App {
        relay: relay.clone(),
        state: StateFile::new(&dir),
        artifacts: Mutex::new(HashMap::new()),
    });
    let scheme_app = app.clone();

    let result = tauri::Builder::default()
        .manage(app)
        .register_asynchronous_uri_scheme_protocol(
            "genie-artifact",
            move |_ctx, request, responder| {
                let app = scheme_app.clone();
                let path = request.uri().path().to_string();
                let relay = app.relay.clone();
                relay.spawn(async move {
                    responder.respond(serve_artifact(app, path).await);
                });
            },
        )
        .invoke_handler(tauri::generate_handler![
            host_connect,
            host_request,
            host_respond,
            host_retry,
            host_open_external,
            host_open_path,
            host_state_load,
            host_state_save,
            host_save_file,
            host_log
        ])
        .setup(|app| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.set_title("Genie");
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .map_err(|e| e.to_string());
    relay.shutdown();
    drop(rt);
    result
}
