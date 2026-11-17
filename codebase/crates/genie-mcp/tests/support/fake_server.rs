//! Fake MCP server used by genie-mcp tests: in-process (duplex streams / fake HTTP) and as
//! the `genie-mcp-fake-server` binary over a real stdio pipe. Plain JSON values only.
#![allow(dead_code, clippy::unwrap_used)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Protocol version to answer with (default: echo the client's).
    pub version: Option<String>,
    /// Write a line to stderr on start and for the `log` tool.
    pub stderr: bool,
}

impl Options {
    pub fn from_args(args: impl Iterator<Item = String>) -> Options {
        let mut o = Options::default();
        let mut args = args.peekable();
        while let Some(a) = args.next() {
            match a.as_str() {
                "--version" => o.version = args.next(),
                "--stderr" => o.stderr = true,
                _ => {}
            }
        }
        o
    }
}

pub type Out = mpsc::UnboundedSender<Value>;

pub struct Fake {
    pub opts: Options,
    pub tasks: TaskTracker,
    pub crash: CancellationToken,
    pub cancelled: AtomicU64,
    pub initialized: AtomicBool,
    pub extra_tool: AtomicBool,
    next_id: AtomicU64,
    cancels: Mutex<HashMap<String, CancellationToken>>,
    client_reqs: Mutex<HashMap<u64, oneshot::Sender<Value>>>,
}

fn key(v: &Value) -> String {
    v.to_string()
}

impl Fake {
    pub fn new(opts: Options) -> Arc<Fake> {
        Arc::new(Fake {
            opts,
            tasks: TaskTracker::new(),
            crash: CancellationToken::new(),
            cancelled: AtomicU64::new(0),
            initialized: AtomicBool::new(false),
            extra_tool: AtomicBool::new(false),
            next_id: AtomicU64::new(1000),
            cancels: Mutex::new(HashMap::new()),
            client_reqs: Mutex::new(HashMap::new()),
        })
    }

    /// Handle one inbound message; requests run concurrently on `tasks`.
    pub fn on_message(self: &Arc<Self>, msg: Value, out: Out) {
        let has_id = msg.get("id").is_some();
        match msg.get("method").and_then(Value::as_str) {
            None if has_id => {
                let id = msg["id"].as_u64().unwrap_or(0);
                if let Some(tx) = self.client_reqs.lock().unwrap().remove(&id) {
                    let _ = tx.send(msg);
                }
            }
            None => {}
            Some("notifications/cancelled") => {
                let k = key(&msg["params"]["requestId"]);
                if let Some(t) = self.cancels.lock().unwrap().remove(&k) {
                    t.cancel();
                }
            }
            Some("notifications/initialized") => self.initialized.store(true, Ordering::SeqCst),
            Some(_) if !has_id => {}
            Some(_) => {
                let me = self.clone();
                let cancel = CancellationToken::new();
                me.cancels
                    .lock()
                    .unwrap()
                    .insert(key(&msg["id"]), cancel.clone());
                self.tasks.spawn(async move {
                    let id = msg["id"].clone();
                    let res = me.request(&msg, &out, &cancel).await;
                    me.cancels.lock().unwrap().remove(&key(&id));
                    if cancel.is_cancelled() {
                        me.cancelled.fetch_add(1, Ordering::SeqCst);
                        return;
                    }
                    let resp = match res {
                        Ok(v) => json!({"jsonrpc": "2.0", "id": id, "result": v}),
                        Err((code, m)) => {
                            json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": m}})
                        }
                    };
                    let _ = out.send(resp);
                });
            }
        }
    }

    async fn client_request(&self, out: &Out, method: &str, params: Value) -> Value {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.client_reqs.lock().unwrap().insert(id, tx);
        let _ = out.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        rx.await.unwrap_or(Value::Null)
    }

    fn tools(&self) -> Vec<Value> {
        let t = |name: &str, desc: &str| {
            json!({"name": name, "description": desc,
                   "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}})
        };
        let mut v = vec![
            t("echo", "Echo the arguments"),
            t("slow", "Report progress, then finish"),
            t("hang", "Never finishes"),
            t("crash", "Exit the server"),
            t("add_tool", "Add a tool and send list_changed"),
            t("ask", "Elicit input from the user"),
            t("sample", "Ask the client for a completion"),
            t("stats", "Server-side counters"),
            t("log", "Write a log line"),
            json!({"name": "bad.name", "description": "Odd schema", "inputSchema": {"type": "array"},
                   "annotations": {"readOnlyHint": true}}),
        ];
        if self.extra_tool.load(Ordering::SeqCst) {
            v.push(t("extra", "Added at runtime"));
        }
        v
    }

    async fn request(
        &self,
        msg: &Value,
        out: &Out,
        cancel: &CancellationToken,
    ) -> Result<Value, (i64, String)> {
        let p = &msg["params"];
        let method = msg["method"].as_str().unwrap_or_default();
        match method {
            "initialize" => {
                if self.opts.stderr {
                    eprintln!("fake server starting");
                }
                let version = self.opts.version.clone().unwrap_or_else(|| {
                    p["protocolVersion"]
                        .as_str()
                        .unwrap_or("2025-06-18")
                        .to_string()
                });
                Ok(json!({
                    "protocolVersion": version,
                    "capabilities": {
                        "tools": {"listChanged": true},
                        "resources": {"subscribe": true, "listChanged": true},
                        "prompts": {"listChanged": true},
                        "logging": {}
                    },
                    "serverInfo": {"name": "fake", "version": "1.0"},
                    "instructions": "Use echo to echo."
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => {
                let all = self.tools();
                if p.get("cursor").and_then(Value::as_str) == Some("p2") {
                    Ok(json!({"tools": all[3..]}))
                } else {
                    Ok(json!({"tools": all[..3], "nextCursor": "p2"}))
                }
            }
            "tools/call" => self.call(p, out, cancel).await,
            "resources/list" => Ok(json!({"resources": [
                {"uri": "mem://a", "name": "a", "mimeType": "text/plain"},
                {"uri": "mem://b", "name": "b", "mimeType": "image/png"}
            ]})),
            "resources/templates/list" => Ok(json!({"resourceTemplates": [
                {"uriTemplate": "mem://{name}", "name": "mem"}
            ]})),
            "resources/read" => match p["uri"].as_str() {
                Some("mem://a") => Ok(
                    json!({"contents": [{"uri": "mem://a", "mimeType": "text/plain", "text": "hello from a"}]}),
                ),
                Some("mem://b") => Ok(
                    json!({"contents": [{"uri": "mem://b", "mimeType": "image/png", "blob": "iVBORw=="}]}),
                ),
                _ => Err((-32002, "resource not found".into())),
            },
            "resources/subscribe" => {
                let _ = out.send(
                    json!({"jsonrpc": "2.0", "method": "notifications/resources/updated",
                                        "params": {"uri": p["uri"]}}),
                );
                Ok(json!({}))
            }
            "prompts/list" => Ok(json!({"prompts": [
                {"name": "greet", "description": "Say hello", "arguments": [{"name": "who", "required": true}]}
            ]})),
            "prompts/get" => {
                let who = p["arguments"]["who"].as_str().unwrap_or("world");
                Ok(json!({"description": "greeting", "messages": [
                    {"role": "user", "content": {"type": "text", "text": format!("Hello, {who}")}}
                ]}))
            }
            _ => Err((-32601, format!("method not found: {method}"))),
        }
    }

    async fn call(
        &self,
        p: &Value,
        out: &Out,
        cancel: &CancellationToken,
    ) -> Result<Value, (i64, String)> {
        let args = p.get("arguments").cloned().unwrap_or(json!({}));
        let text = |t: String| json!({"content": [{"type": "text", "text": t}]});
        match p["name"].as_str().unwrap_or_default() {
            "echo" => Ok(json!({
                "content": [{"type": "text", "text": args.to_string()}],
                "structuredContent": args
            })),
            "slow" => {
                let steps = args["steps"].as_u64().unwrap_or(3);
                let token = p.pointer("/_meta/progressToken").cloned();
                for i in 1..=steps {
                    tokio::select! {
                        () = cancel.cancelled() => return Err((0, "cancelled".into())),
                        () = tokio::time::sleep(Duration::from_millis(30)) => {}
                    }
                    if let Some(t) = &token {
                        let _ = out.send(json!({"jsonrpc": "2.0", "method": "notifications/progress",
                            "params": {"progressToken": t, "progress": i, "total": steps, "message": format!("step {i}")}}));
                    }
                }
                Ok(text("slow done".into()))
            }
            "hang" => {
                cancel.cancelled().await;
                Err((0, "cancelled".into()))
            }
            "crash" => {
                self.crash.cancel();
                std::future::pending().await
            }
            "add_tool" => {
                self.extra_tool.store(true, Ordering::SeqCst);
                let _ = out
                    .send(json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"}));
                Ok(text("added".into()))
            }
            "ask" => {
                let r = self
                    .client_request(
                        out,
                        "elicitation/create",
                        json!({
                            "message": "Need your token",
                            "requestedSchema": {"type": "object", "properties": {
                                "name": {"type": "string"},
                                "token": {"type": "string"}
                            }}
                        }),
                    )
                    .await;
                Ok(text(r.get("result").cloned().unwrap_or(r).to_string()))
            }
            "sample" => {
                let max = args["max"].as_u64().unwrap_or(50);
                let r = self
                    .client_request(out, "sampling/createMessage", json!({
                        "messages": [{"role": "user", "content": {"type": "text", "text": "Summarise"}}],
                        "maxTokens": max
                    }))
                    .await;
                Ok(text(r.to_string()))
            }
            "stats" => Ok(json!({"content": [], "structuredContent": {
                "cancelled": self.cancelled.load(Ordering::SeqCst),
                "initialized": self.initialized.load(Ordering::SeqCst)
            }})),
            "log" => {
                if self.opts.stderr {
                    eprintln!("log tool called");
                }
                let _ = out.send(json!({"jsonrpc": "2.0", "method": "notifications/message",
                    "params": {"level": "warning", "data": "careful"}}));
                Ok(text("logged".into()))
            }
            other => Ok(
                json!({"content": [{"type": "text", "text": format!("unknown tool {other}")}], "isError": true}),
            ),
        }
    }
}

/// Serve newline-delimited JSON-RPC until EOF or the `crash` tool. Returns true on crash.
pub async fn serve<R, W>(reader: R, writer: W, opts: Options) -> bool
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let fake = Fake::new(opts);
    serve_with(fake, reader, writer).await
}

pub async fn serve_with<R, W>(fake: Arc<Fake>, reader: R, writer: W) -> bool
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
    let stop = CancellationToken::new();
    let wstop = stop.clone();
    fake.tasks.spawn(async move {
        let mut w = writer;
        loop {
            tokio::select! {
                () = wstop.cancelled() => break,
                m = rx.recv() => match m {
                    Some(m) => {
                        let mut line = m.to_string();
                        line.push('\n');
                        if w.write_all(line.as_bytes()).await.is_err() || w.flush().await.is_err() {
                            break;
                        }
                    }
                    None => break,
                },
            }
        }
        let _ = w.shutdown().await;
    });
    let mut lines = BufReader::new(reader).lines();
    let crashed = loop {
        tokio::select! {
            () = fake.crash.cancelled() => break true,
            l = lines.next_line() => match l {
                Ok(Some(l)) => {
                    if let Ok(v) = serde_json::from_str::<Value>(&l) {
                        fake.on_message(v, tx.clone());
                    }
                }
                _ => break false,
            },
        }
    };
    stop.cancel();
    crashed
}
