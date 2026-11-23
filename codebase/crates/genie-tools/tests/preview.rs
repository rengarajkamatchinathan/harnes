//! Dev servers (ticket 47 phase 2): `DevServers` and the `preview_*` tools against real
//! processes. Skipped without `python3`.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use genie_protocol::api::browser::{PreviewServer, PreviewState};
use genie_protocol::common::SessionKind;
use genie_tools::preview::{DevServers, LaunchFile, ServerConfig};
use genie_tools::shell::FixedDir;
use genie_tools::{
    NullSink, PermissionGate, PermissionRequest, Tool, ToolCtx, ToolEnv, ToolError, ToolOutput,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// A server that honours `PORT` and prints a line per request.
const SERVE_PORT: &str = "import os, http.server\n\
H = type('H', (http.server.SimpleHTTPRequestHandler,), \
{'log_message': lambda self, *a: print('GET', self.path, flush=True)})\n\
print('listening', os.environ['PORT'], flush=True)\n\
http.server.HTTPServer(('127.0.0.1', int(os.environ['PORT'])), H).serve_forever()\n";

fn python() -> bool {
    let ok = std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_ok();
    if !ok {
        eprintln!("skipped: no python3");
    }
    ok
}

struct Fixture {
    dir: tempfile::TempDir,
    dev: Arc<DevServers>,
    changes: Arc<Mutex<Vec<Vec<PreviewServer>>>>,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let mut dev = DevServers::new(
        Arc::new(FixedDir(dir.path().to_path_buf())),
        TaskTracker::new(),
        CancellationToken::new(),
    );
    dev.ready_timeout = Duration::from_secs(20);
    let dev = Arc::new(dev);
    let changes: Arc<Mutex<Vec<Vec<PreviewServer>>>> = Arc::default();
    let c = changes.clone();
    dev.set_on_change(Arc::new(move |sid, list| {
        assert_eq!(sid, "ses");
        c.lock().unwrap().push(list);
    }));
    Fixture { dir, dev, changes }
}

fn py(name: &str, code: &str) -> ServerConfig {
    ServerConfig {
        name: name.into(),
        runtime_executable: Some("python3".into()),
        runtime_args: vec!["-u".into(), "-c".into(), code.into()],
        ..Default::default()
    }
}

async fn get(url: &str) -> bool {
    let addr = url
        .trim_start_matches("http://")
        .replace("localhost", "127.0.0.1");
    tokio::net::TcpStream::connect(addr.trim_end_matches('/'))
        .await
        .is_ok()
}

#[tokio::test]
async fn starts_waits_for_the_port_logs_and_stops() {
    if !python() {
        return;
    }
    let f = fixture();
    let mut cfg = py("web", SERVE_PORT);
    cfg.auto_port = Some(true);
    let s = f
        .dev
        .start_config("ses", cfg.clone(), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(s.state, PreviewState::Running, "{s:?}");
    let port = s.port.expect("a port was picked");
    assert_eq!(
        s.url.as_deref(),
        Some(format!("http://localhost:{port}").as_str())
    );
    assert!(s.command.as_deref().unwrap().starts_with("python3 -u -c "));
    assert!(get(&format!("localhost:{port}")).await);

    // Starting it again returns the running server.
    let again = f
        .dev
        .start_config("ses", cfg, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(again.pid, s.pid);
    assert_eq!(f.dev.list("ses").len(), 1);

    let logs = f.dev.logs("ses", "web", 10).unwrap();
    assert!(logs.contains(&format!("listening {port}")), "{logs}");

    let states: Vec<PreviewState> = f
        .changes
        .lock()
        .unwrap()
        .iter()
        .filter_map(|l| l.first().map(|s| s.state))
        .collect();
    assert_eq!(states, [PreviewState::Starting, PreviewState::Running]);

    assert!(f.dev.stop("ses", "web"));
    assert!(f.dev.list("ses").is_empty());
    assert_eq!(f.changes.lock().unwrap().last().unwrap().len(), 0);
    // The tree is killed: the port closes.
    let mut closed = false;
    for _ in 0..40 {
        if !get(&format!("localhost:{port}")).await {
            closed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(closed, "port {port} still open after stop");
    drop(f.dir);
}

#[tokio::test]
async fn finds_the_port_when_none_is_configured() {
    if !python() {
        return;
    }
    let f = fixture();
    let code = "import socket, time\n\
s = socket.socket(); s.bind(('127.0.0.1', 0)); s.listen()\n\
print('on', s.getsockname()[1], flush=True)\n\
time.sleep(60)\n";
    let s = f
        .dev
        .start_config("ses", py("api", code), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(s.state, PreviewState::Running, "{s:?}");
    let port = s.port.unwrap();
    let logs = f.dev.logs("ses", "api", 5).unwrap();
    assert!(logs.contains(&format!("on {port}")), "{logs} vs {port}");
    f.dev.end_session("ses");
    assert!(f.dev.list("ses").is_empty());
}

#[tokio::test]
async fn a_server_that_exits_early_fails_with_its_output() {
    if !python() {
        return;
    }
    let f = fixture();
    let err = f
        .dev
        .start_config(
            "ses",
            py(
                "bad",
                "print('Error: missing module x', flush=True)\nraise SystemExit(3)",
            ),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(err.contains("exited with code 3"), "{err}");
    assert!(err.contains("missing module x"), "{err}");
    let s = &f.dev.list("ses")[0];
    assert_eq!(s.state, PreviewState::Failed);
    assert_eq!(s.exit_code, Some(3));
}

#[tokio::test]
async fn a_taken_port_fails_unless_auto_port() {
    let f = fixture();
    let hold = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = hold.local_addr().unwrap().port();
    let mut cfg = py("web", SERVE_PORT);
    cfg.port = Some(port);
    let err = f
        .dev
        .start_config("ses", cfg.clone(), &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(
        err.contains(&format!("port {port} is already in use")),
        "{err}"
    );
    assert!(f.dev.list("ses").is_empty(), "nothing was started");

    // `url` alone attaches to what is there.
    let attach = ServerConfig {
        name: "existing".into(),
        url: Some(format!("http://localhost:{port}/")),
        ..Default::default()
    };
    let s = f
        .dev
        .start_config("ses", attach, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(s.state, PreviewState::Running);
    assert_eq!(s.port, Some(port));
    assert!(s.command.is_none());
    drop(hold);

    if !python() {
        return;
    }
    cfg.auto_port = Some(true);
    let hold = std::net::TcpListener::bind(("127.0.0.1", port));
    let s = f
        .dev
        .start_config("ses", cfg, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(s.state, PreviewState::Running);
    if hold.is_ok() {
        assert_ne!(s.port, Some(port), "a free port was picked instead");
    }
    f.dev.end_session("ses");
}

#[tokio::test]
async fn unknown_programs_and_entries_explain_themselves() {
    let f = fixture();
    let err = f
        .dev
        .start_config(
            "ses",
            ServerConfig {
                name: "x".into(),
                runtime_executable: Some("definitely-not-a-real-program-genie".into()),
                ..Default::default()
            },
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(err.contains("not found on PATH"), "{err}");
    let err = f
        .dev
        .start("ses", "web", &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(err.contains("has no entries"), "{err}");
    let err = f
        .dev
        .start_config(
            "ses",
            ServerConfig {
                name: "x".into(),
                runtime_executable: Some("python3".into()),
                cwd: Some("../..".into()),
                ..Default::default()
            },
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(err.contains("inside the project"), "{err}");
}

// ---------------------------------------------------------------- tools

#[derive(Default)]
struct Asks(Mutex<Vec<PermissionRequest>>);

#[async_trait]
impl PermissionGate for Asks {
    async fn assert(&self, req: PermissionRequest) -> Result<(), ToolError> {
        self.0.lock().unwrap().push(req);
        Ok(())
    }
}

fn ctx(asks: Arc<Asks>, kind: SessionKind) -> ToolCtx {
    ToolCtx {
        env: Arc::new(ToolEnv::new("ses", kind)),
        turn_id: "trn".into(),
        call_id: "call".into(),
        tool_name: "preview".into(),
        cancel: CancellationToken::new(),
        detach: CancellationToken::new(),
        permissions: asks,
        events: Arc::new(NullSink),
    }
}

async fn run(tool: &dyn Tool, input: Value, asks: &Arc<Asks>) -> Result<String, ToolError> {
    tool.execute(input, ctx(asks.clone(), SessionKind::Code))
        .await
        .map(|o| match o {
            ToolOutput::Text(t) => t,
            ToolOutput::Json(v) => v.to_string(),
        })
}

#[tokio::test]
async fn tools_save_start_read_and_stop() {
    if !python() {
        return;
    }
    let f = fixture();
    let tools = genie_tools::preview::tools(&f.dev);
    let [start, stop, logs] = [&tools[0], &tools[1], &tools[2]];
    assert_eq!(
        [start.name(), stop.name(), logs.name()],
        ["preview_start", "preview_stop", "preview_logs"]
    );
    assert!(
        !start
            .availability(&ToolEnv::new("ses", SessionKind::Chat))
            .is_available(),
        "Code only"
    );
    let asks = Arc::new(Asks::default());

    // No entries yet: the error says how to add one.
    let err = run(start.as_ref(), json!({}), &asks).await.unwrap_err();
    assert!(err.to_string().contains("has no dev servers yet"), "{err}");

    let out = run(
        start.as_ref(),
        json!({"name": "web", "runtimeExecutable": "python3",
               "runtimeArgs": ["-u", "-c", SERVE_PORT], "autoPort": true}),
        &asks,
    )
    .await
    .unwrap();
    assert!(out.contains("`web`: running at http://localhost:"), "{out}");
    assert!(out.contains("Saved to .genie/launch.json"), "{out}");
    // It asked like `shell`, with the command line.
    let req = asks.0.lock().unwrap()[0].clone();
    assert!(
        req.resource
            .as_deref()
            .unwrap()
            .starts_with("Bash(python3 -u -c ")
            || req
                .resource
                .as_deref()
                .unwrap()
                .starts_with("PowerShell(python3"),
        "{req:?}"
    );
    let file = LaunchFile::load(f.dir.path()).unwrap();
    assert_eq!(file.get("web").unwrap().auto_port, Some(true));

    // By name alone (the only entry), it is already running.
    let out = run(start.as_ref(), json!({}), &asks).await.unwrap();
    assert!(out.contains("already running"), "{out}");
    assert_eq!(asks.0.lock().unwrap().len(), 1, "no second ask");

    let out = run(logs.as_ref(), json!({"lines": 5}), &asks)
        .await
        .unwrap();
    assert!(
        out.contains("`web`: running") && out.contains("listening"),
        "{out}"
    );

    let out = run(stop.as_ref(), json!({}), &asks).await.unwrap();
    assert_eq!(out, "Stopped 1 dev server.");
    let err = run(stop.as_ref(), json!({"name": "web"}), &asks)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no dev server named `web`"));
    // The entry stays in launch.json for the Dev servers menu.
    assert!(LaunchFile::load(f.dir.path()).unwrap().get("web").is_some());
}
