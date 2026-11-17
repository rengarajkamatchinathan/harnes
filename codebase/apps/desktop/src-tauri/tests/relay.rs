//! Relay contract tests against an in-memory fake daemon: batching, dedupe, server
//! requests and their cancellation, resume after reconnect.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use genie_client::{ClientInfo, Connection};
use genie_desktop::relay::{ConnState, HostMessage, Relay, Sink};
use genie_protocol::{ClientCapabilities, ClientKind};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};

#[derive(Default)]
struct Collect(Mutex<Vec<HostMessage>>);

impl Sink for Collect {
    fn send(&self, msg: HostMessage) {
        self.0.lock().unwrap().push(msg);
    }
}

impl Collect {
    fn take(&self) -> Vec<HostMessage> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

struct FakeDaemon {
    r: BufReader<ReadHalf<DuplexStream>>,
    w: WriteHalf<DuplexStream>,
}

impl FakeDaemon {
    async fn recv(&mut self) -> Value {
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(5), self.r.read_line(&mut line))
            .await
            .expect("daemon read timed out")
            .unwrap();
        serde_json::from_str(&line).unwrap()
    }
    async fn send(&mut self, v: Value) {
        let mut s = v.to_string();
        s.push('\n');
        self.w.write_all(s.as_bytes()).await.unwrap();
    }
    async fn reply(&mut self, id: &Value, result: Value) {
        self.send(json!({"jsonrpc": "2.0", "id": id, "result": result}))
            .await;
    }
    async fn event(&mut self, seq: Option<u64>, event: Value) {
        let mut p = json!({"session_id": "ses_1", "ts": "2026-01-01T00:00:00Z", "event": event});
        match seq {
            Some(s) => p["seq"] = json!(s),
            None => p["ref_seq"] = json!(1),
        }
        self.send(json!({"jsonrpc": "2.0", "method": "event", "params": p}))
            .await;
    }
}

async fn connect() -> (Connection, FakeDaemon) {
    let (client, server) = tokio::io::duplex(1 << 20);
    let (r, w) = tokio::io::split(server);
    let mut d = FakeDaemon {
        r: BufReader::new(r),
        w,
    };
    let info = ClientInfo {
        kind: ClientKind::Desktop,
        version: "0.1.0".into(),
        instance_id: "desktop_test".into(),
        capabilities: ClientCapabilities::default(),
    };
    let hs = tokio::spawn(async move {
        Connection::handshake(Box::new(client), &info, "tok".into())
            .await
            .unwrap()
    });
    let init = d.recv().await;
    assert_eq!(init["method"], "initialize");
    assert_eq!(init["params"]["client_kind"], "desktop");
    d.reply(
        &init["id"],
        json!({"protocol": "1.0.0", "engine_version": "0.1.0", "build": "x", "channel": "dev"}),
    )
    .await;
    (hs.await.unwrap(), d)
}

fn batches(msgs: &[HostMessage]) -> Vec<Value> {
    msgs.iter()
        .filter_map(|m| match m {
            HostMessage::Batch { messages } => Some(messages.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

async fn settle() {
    tokio::time::sleep(Duration::from_millis(60)).await;
}

#[tokio::test]
async fn relays_batches_asks_and_resumes() {
    let relay = Relay::new(tokio::runtime::Handle::current(), "desktop_test");
    let sink = Arc::new(Collect::default());
    let info = relay.attach(sink.clone());
    assert_eq!(info.client_instance_id, "desktop_test");
    assert!(matches!(&sink.take()[0], HostMessage::Status(s) if s.state == ConnState::Connecting));

    // --- first connection ---
    let (conn, mut d) = connect().await;
    let r = relay.clone();
    let serving = tokio::spawn(async move { r.serve(conn, false).await });
    settle().await;
    assert!(
        sink.take()
            .iter()
            .any(|m| matches!(m, HostMessage::Status(s) if s.state == ConnState::Ready))
    );

    let r = relay.clone();
    let sub = tokio::spawn(async move {
        r.request("session.subscribe", json!({"session_id": "ses_1"}))
            .await
    });
    let req = d.recv().await;
    assert_eq!(req["method"], "session.subscribe");
    d.event(
        Some(1),
        json!({"type": "turn.started", "turn_id": "t", "message_id": "m"}),
    )
    .await;
    d.event(
        Some(2),
        json!({"type": "part.started", "kind": "text", "message_id": "m2", "part_id": "p"}),
    )
    .await;
    for t in ["He", "ll", "o"] {
        d.event(
            None,
            json!({"type": "part.delta", "part_id": "p", "text": t}),
        )
        .await;
    }
    d.event(
        Some(2),
        json!({"type": "part.started", "kind": "text", "message_id": "m2", "part_id": "p"}),
    )
    .await; // duplicate
    d.reply(&req["id"], json!({"head_seq": 2, "status": "running"}))
        .await;
    assert_eq!(sub.await.unwrap().unwrap()["head_seq"], 2);
    settle().await;
    relay.flush();
    let out = batches(&sink.take());
    let kinds: Vec<_> = out
        .iter()
        .map(|m| m["params"]["event"]["type"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(kinds, ["turn.started", "part.started", "part.delta"]);
    assert_eq!(out[2]["params"]["event"]["text"], "Hello");
    assert_eq!(relay.cursor("ses_1"), Some(2));

    // Daemon → client request, answered by the webview.
    d.send(json!({"jsonrpc": "2.0", "id": 77, "method": "permission.ask",
                  "params": {"ask": {"id": "ask_1", "kind": "permission", "session_id": "ses_1", "created_at": "t"}}}))
        .await;
    settle().await;
    let msgs = sink.take();
    let req_id = msgs
        .iter()
        .find_map(|m| match m {
            HostMessage::ServerRequest { req_id, method, .. } if method == "permission.ask" => {
                Some(*req_id)
            }
            _ => None,
        })
        .unwrap();
    relay
        .respond(req_id, Ok(json!({"decision": "once"})))
        .unwrap();
    let ans = d.recv().await;
    assert_eq!(ans["id"], 77);
    assert_eq!(ans["result"]["decision"], "once");
    assert!(
        relay.respond(req_id, Ok(json!({}))).is_err(),
        "answered once"
    );

    // A second ask, withdrawn by the daemon.
    d.send(json!({"jsonrpc": "2.0", "id": 78, "method": "permission.ask", "params": {}}))
        .await;
    d.send(json!({"jsonrpc": "2.0", "method": "$/cancelRequest", "params": {"id": 78}}))
        .await;
    settle().await;
    let msgs = sink.take();
    let asked = msgs.iter().find_map(|m| match m {
        HostMessage::ServerRequest { req_id, .. } => Some(*req_id),
        _ => None,
    });
    assert!(
        msgs.iter().any(
            |m| matches!(m, HostMessage::RequestCancelled { req_id } if Some(*req_id) == asked)
        )
    );

    // --- the daemon dies; a new connection resumes from seq 2 ---
    drop(d);
    serving.await.unwrap();
    assert!(relay.request("session.get", json!({})).await.is_err());

    let (conn, mut d) = connect().await;
    let r = relay.clone();
    let _serving = tokio::spawn(async move { r.serve(conn, false).await });
    let resub = d.recv().await;
    assert_eq!(resub["method"], "session.subscribe");
    assert_eq!(
        resub["params"],
        json!({"session_id": "ses_1", "after_seq": 2})
    );
    d.event(
        Some(2),
        json!({"type": "part.started", "kind": "text", "message_id": "m2", "part_id": "p"}),
    )
    .await; // replayed again: dropped
    d.event(Some(3), json!({"type": "part.ended", "part_id": "p", "part": {"type": "text", "id": "p", "text": "Hello"}}))
        .await;
    d.event(
        None,
        json!({"type": "part.delta", "part_id": "p", "text": "late"}),
    )
    .await; // stale after part.ended: dropped
    d.reply(&resub["id"], json!({"head_seq": 3, "status": "idle"}))
        .await;
    // Overflow: the relay re-subscribes from its cursor on its own.
    d.send(
        json!({"jsonrpc": "2.0", "method": "session.resync_required",
                  "params": {"session_id": "ses_1", "from_seq": 1}}),
    )
    .await;
    let again = d.recv().await;
    assert_eq!(again["params"]["after_seq"], 3);
    d.reply(&again["id"], json!({"head_seq": 3, "status": "idle"}))
        .await;
    // Below the floor: the webview is told to reload.
    d.send(
        json!({"jsonrpc": "2.0", "method": "session.resync_required",
                  "params": {"session_id": "ses_1", "from_seq": 0, "snapshot": true}}),
    )
    .await;
    settle().await;
    relay.flush();
    let msgs = sink.take();
    let out = batches(&msgs);
    let kinds: Vec<_> = out
        .iter()
        .filter_map(|m| m["params"]["event"]["type"].as_str())
        .collect();
    assert_eq!(kinds, ["part.ended"]);
    assert!(
        msgs.iter()
            .any(|m| matches!(m, HostMessage::Reload { session_id } if session_id == "ses_1"))
    );
}

#[tokio::test]
async fn reattach_replays_pending_asks() {
    let relay = Relay::new(tokio::runtime::Handle::current(), "desktop_test");
    let first = Arc::new(Collect::default());
    relay.attach(first.clone());
    let (conn, mut d) = connect().await;
    let r = relay.clone();
    let _serving = tokio::spawn(async move { r.serve(conn, false).await });
    d.send(json!({"jsonrpc": "2.0", "id": 5, "method": "permission.ask", "params": {"ask": {"id": "a"}}}))
        .await;
    settle().await;
    // The webview reloads.
    let second = Arc::new(Collect::default());
    relay.attach(second.clone());
    let msgs = second.take();
    let req_id = msgs
        .iter()
        .find_map(|m| match m {
            HostMessage::ServerRequest { req_id, params, .. } => {
                assert_eq!(params["ask"]["id"], "a");
                Some(*req_id)
            }
            _ => None,
        })
        .unwrap();
    relay
        .respond(
            req_id,
            Err(genie_desktop::relay::WireError::kind(
                genie_protocol::ErrorKind::Cancelled,
                "closed",
            )),
        )
        .unwrap();
    let ans = d.recv().await;
    assert_eq!(ans["id"], 5);
    assert!(ans["error"].is_object());
}
