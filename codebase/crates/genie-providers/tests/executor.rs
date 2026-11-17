//! Executor behaviour against replayed cassettes (03 §3, §7, §11 layer 1): retries,
//! `retry-after`, forced token refresh, cancellation, mid-stream failures, chunking.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::result_large_err)] // test helpers

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::StreamExt;
use genie_providers::auth::{AccessToken, ApiKeyHeader, NoAuth, TokenAuth, TokenSource};
use genie_providers::cassette::{CassetteTransport, RecordedResponse};
use genie_providers::compat::ModelSpec;
use genie_providers::error::TransportPhase;
use genie_providers::event::StreamEvent;
use genie_providers::executor::TransportError;
use genie_providers::model::*;
use genie_providers::route::{Preset, PresetProvider, ProviderConfig, Route};
use genie_providers::{Auth, ErrorKind, Executor, LlmClient, Provider, ProviderError, RetryPolicy};
use genie_secrets::SecretString;
use tokio_util::sync::CancellationToken;

fn cassette(name: &str) -> String {
    std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/cassettes")
            .join(name),
    )
    .unwrap()
}

fn route(preset: Preset, protocol: ProtocolKind, auth: Arc<dyn Auth>) -> Route {
    PresetProvider(ProviderConfig {
        id: ProviderId::new("anthropic"),
        preset,
        base_url: None,
        project: None,
        location: None,
        auth,
        account: None,
        compat_overlay: None,
        headers: vec![],
    })
    .route(&ModelSpec::new("anthropic", "claude-opus-5-5", protocol))
    .unwrap()
}

fn anthropic(auth: Arc<dyn Auth>) -> Route {
    route(Preset::Anthropic, ProtocolKind::AnthropicMessages, auth)
}

fn req() -> LlmRequest {
    LlmRequest::new(
        ModelSpec::new(
            "anthropic",
            "claude-opus-5-5",
            ProtocolKind::AnthropicMessages,
        ),
        vec![Message::user_text("u1", "hi")],
    )
}

fn exec(t: Arc<CassetteTransport>) -> Executor {
    Executor::new(
        t,
        RetryPolicy {
            jitter: false,
            ..Default::default()
        },
    )
}

async fn collect(
    s: genie_providers::executor::EventStream,
) -> (Vec<StreamEvent>, Option<ProviderError>) {
    let mut ev = vec![];
    let mut s = s;
    while let Some(item) = s.next().await {
        match item {
            Ok(e) => ev.push(e),
            Err(e) => return (ev, Some(e)),
        }
    }
    (ev, None)
}

fn key_auth() -> Arc<dyn Auth> {
    Arc::new(ApiKeyHeader {
        header: http::HeaderName::from_static("x-api-key"),
        key: SecretString::from("sk-ant-test-0123456789"),
    })
}

#[tokio::test(start_paused = true)]
async fn happy_path_any_chunking_and_scrubbed_request() {
    for chunk in [1usize, 7, 64, 100_000] {
        let t = Arc::new(CassetteTransport::new([RecordedResponse::sse(
            200,
            &cassette("anthropic_thinking_tool.sse"),
            chunk,
        )]));
        let (ev, err) = collect(exec(t.clone()).stream(
            &anthropic(key_auth()),
            req(),
            CancellationToken::new(),
        ))
        .await;
        assert!(err.is_none(), "chunk {chunk}: {err:?}");
        assert!(
            matches!(ev.first(), Some(StreamEvent::WireRecorded { records }) if records.len() == 1)
        );
        assert!(matches!(
            ev.last(),
            Some(StreamEvent::Finish {
                reason: FinishReason::ToolCalls,
                ..
            })
        ));
        let reqs = t.recorded();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].url, "https://api.anthropic.com/v1/messages");
        let h = |n: &str| {
            reqs[0]
                .headers
                .iter()
                .find(|(k, _)| k == n)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(
            h("x-api-key").as_deref(),
            Some("[REDACTED]"),
            "recorder scrubs keys"
        );
        assert_eq!(h("anthropic-version").as_deref(), Some("2023-06-01"));
        assert_eq!(h("accept").as_deref(), Some("text/event-stream"));
        assert!(reqs[0].body.contains("\"stream\":true"));
    }
}

#[tokio::test(start_paused = true)]
async fn rate_limit_with_retry_after_then_success() {
    let t = Arc::new(CassetteTransport::new([
        RecordedResponse::json(
            429,
            r#"{"type":"error","error":{"type":"rate_limit_error","message":"slow down"}}"#,
            vec![
                ("retry-after".into(), "3".into()),
                ("request-id".into(), "req_1".into()),
            ],
        ),
        RecordedResponse::sse(200, &cassette("anthropic_thinking_tool.sse"), 50),
    ]));
    let start = tokio::time::Instant::now();
    let (ev, err) = collect(exec(t.clone()).stream(
        &anthropic(Arc::new(NoAuth)),
        req(),
        CancellationToken::new(),
    ))
    .await;
    assert!(err.is_none(), "{err:?}");
    let retry = ev.iter().find_map(|e| {
        if let StreamEvent::RetryScheduled {
            attempt,
            delay_ms,
            kind,
            ..
        } = e
        {
            Some((*attempt, *delay_ms, kind.clone()))
        } else {
            None
        }
    });
    assert_eq!(retry, Some((1, 3000, ErrorKind::RateLimited)));
    assert!(start.elapsed() >= Duration::from_secs(3));
    assert_eq!(t.recorded().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn overloaded_in_stream_before_output_is_retried() {
    let overloaded = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"m\",\"usage\":{}}}\n\n\
                      event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n";
    let t = Arc::new(CassetteTransport::new([
        RecordedResponse::sse(200, overloaded, 30),
        RecordedResponse::sse(200, &cassette("anthropic_thinking_tool.sse"), 30),
    ]));
    let (ev, err) = collect(exec(t.clone()).stream(
        &anthropic(Arc::new(NoAuth)),
        req(),
        CancellationToken::new(),
    ))
    .await;
    assert!(err.is_none(), "{err:?}");
    // StepStart from the failed attempt was held back; only one StepStart reaches the caller.
    assert_eq!(
        ev.iter()
            .filter(|e| matches!(e, StreamEvent::StepStart { .. }))
            .count(),
        1
    );
    assert_eq!(t.recorded().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn failure_after_output_is_not_retried() {
    let full = cassette("anthropic_thinking_tool.sse");
    let cut = &full[..full
        .find("event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1")
        .unwrap()];
    let mut resp = RecordedResponse::sse(200, cut, 40);
    resp.disconnect = true;
    let t = Arc::new(CassetteTransport::new([resp]));
    let (ev, err) = collect(exec(t.clone()).stream(
        &anthropic(Arc::new(NoAuth)),
        req(),
        CancellationToken::new(),
    ))
    .await;
    let err = err.unwrap();
    assert_eq!(
        err.kind,
        ErrorKind::Transport {
            phase: TransportPhase::Body,
            offline: false
        }
    );
    assert!(
        ev.iter()
            .any(|e| matches!(e, StreamEvent::ReasoningEnd { .. })),
        "partial output was delivered"
    );
    assert_eq!(t.recorded().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn non_retryable_errors_surface_once() {
    for (status, body, kind) in [
        (
            400,
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 300000 tokens > 200000 maximum"}}"#,
            ErrorKind::ContextOverflow,
        ),
        (
            400,
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"Your credit balance is too low to access the Anthropic API."}}"#,
            ErrorKind::QuotaExceeded,
        ),
        (
            402,
            r#"{"type":"error","error":{"type":"billing_error","message":"billing"}}"#,
            ErrorKind::QuotaExceeded,
        ),
        (
            403,
            r#"{"type":"error","error":{"type":"permission_error","message":"no"}}"#,
            ErrorKind::Authentication,
        ),
    ] {
        let t = Arc::new(CassetteTransport::new([RecordedResponse::json(
            status,
            body,
            vec![],
        )]));
        let (_, err) = collect(exec(t.clone()).stream(
            &anthropic(Arc::new(NoAuth)),
            req(),
            CancellationToken::new(),
        ))
        .await;
        let err = err.unwrap();
        assert_eq!(err.kind, kind);
        assert_eq!(err.status, Some(status));
        assert_eq!(t.recorded().len(), 1, "{kind:?} not retried");
    }
}

#[tokio::test(start_paused = true)]
async fn should_retry_false_wins_over_503() {
    let t = Arc::new(CassetteTransport::new([RecordedResponse::json(
        503,
        "busy",
        vec![("x-should-retry".into(), "false".into())],
    )]));
    let (_, err) = collect(exec(t.clone()).stream(
        &anthropic(Arc::new(NoAuth)),
        req(),
        CancellationToken::new(),
    ))
    .await;
    assert_eq!(err.unwrap().kind, ErrorKind::Overloaded);
    assert_eq!(t.recorded().len(), 1);
}

struct Tokens(AtomicU32);
#[async_trait]
impl TokenSource for Tokens {
    async fn fetch(&self, _force: bool) -> Result<AccessToken, ProviderError> {
        let n = self.0.fetch_add(1, Ordering::SeqCst);
        Ok(AccessToken {
            token: SecretString::from(format!("entra-{n}")),
            expires_at: Some(Instant::now() + Duration::from_secs(3600)),
        })
    }
}

#[tokio::test(start_paused = true)]
async fn expired_entra_token_forces_one_refresh() {
    let src = Arc::new(Tokens(AtomicU32::new(0)));
    let auth: Arc<dyn Auth> = Arc::new(TokenAuth::new(src.clone()));
    let unauthorized = || {
        RecordedResponse::json(
            401,
            r#"{"error":{"code":"401","message":"token expired"}}"#,
            vec![],
        )
    };
    let t = Arc::new(CassetteTransport::new([
        unauthorized(),
        RecordedResponse::sse(200, &cassette("anthropic_thinking_tool.sse"), 99),
    ]));
    let (_, err) =
        collect(exec(t.clone()).stream(&anthropic(auth.clone()), req(), CancellationToken::new()))
            .await;
    assert!(err.is_none(), "{err:?}");
    assert_eq!(src.0.load(Ordering::SeqCst), 2);

    // A second 401 after the refresh is final.
    let t = Arc::new(CassetteTransport::new([
        unauthorized(),
        unauthorized(),
        unauthorized(),
    ]));
    let (_, err) =
        collect(exec(t.clone()).stream(&anthropic(auth), req(), CancellationToken::new())).await;
    assert_eq!(err.unwrap().kind, ErrorKind::Authentication);
    assert_eq!(t.recorded().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn offline_uses_separate_budget_and_cancel_stops_it() {
    let t = Arc::new(CassetteTransport::new([]));
    for _ in 0..30 {
        t.push_error(TransportError {
            phase: TransportPhase::Connect,
            offline: true,
            message: "dns error: failed to lookup".into(),
        });
    }
    let cancel = CancellationToken::new();
    let stream = exec(t.clone()).stream(&anthropic(Arc::new(NoAuth)), req(), cancel.clone());
    let c2 = cancel.clone();
    let handle = async move {
        tokio::time::sleep(Duration::from_secs(600)).await;
        c2.cancel();
    };
    let ((ev, err), ()) = tokio::join!(collect(stream), handle);
    assert_eq!(err.unwrap().kind, ErrorKind::Cancelled);
    let retries = ev
        .iter()
        .filter(|e| matches!(e, StreamEvent::RetryScheduled { .. }))
        .count();
    assert!(
        retries > 10,
        "offline retries are not capped at 10 ({retries})"
    );
}

#[tokio::test(start_paused = true)]
async fn chunk_gap_timeout() {
    struct Stall;
    #[async_trait]
    impl genie_providers::Transport for Stall {
        async fn send(
            &self,
            _r: genie_providers::executor::HttpRequest,
        ) -> Result<genie_providers::executor::HttpResponse, TransportError> {
            let body = futures::stream::once(async {
                Ok(bytes::Bytes::from_static(
                    b"event: ping\ndata: {\"type\":\"ping\"}\n\n",
                ))
            })
            .chain(futures::stream::pending())
            .boxed();
            Ok(genie_providers::executor::HttpResponse {
                status: 200,
                headers: Default::default(),
                body,
            })
        }
    }
    let ex = Executor::new(
        Arc::new(Stall),
        RetryPolicy {
            jitter: false,
            max_timeout_retries: 0,
            ..Default::default()
        },
    );
    let mut r = req();
    r.timeouts.chunk_gap = Duration::from_secs(5);
    let (_, err) =
        collect(ex.stream(&anthropic(Arc::new(NoAuth)), r, CancellationToken::new())).await;
    assert_eq!(
        err.unwrap().kind,
        ErrorKind::Timeout {
            phase: genie_providers::error::TimeoutPhase::ChunkGap
        }
    );
}

#[tokio::test(start_paused = true)]
async fn other_protocols_through_the_executor() {
    let cases = [
        (
            Preset::OpenAi,
            ProtocolKind::OpenaiResponses,
            "responses_reasoning_tool.sse",
            "https://api.openai.com/v1/responses",
        ),
        (
            Preset::Ollama,
            ProtocolKind::OpenaiChat,
            "chat_ollama_tools.sse",
            "http://127.0.0.1:11434/v1/chat/completions",
        ),
        (
            Preset::GeminiApi,
            ProtocolKind::Gemini,
            "gemini_thinking_call.sse",
            "https://generativelanguage.googleapis.com/v1beta/models/claude-opus-5-5:streamGenerateContent?alt=sse",
        ),
    ];
    for (preset, protocol, file, url) in cases {
        let t = Arc::new(CassetteTransport::new([RecordedResponse::sse(
            200,
            &cassette(file),
            13,
        )]));
        let r = route(preset, protocol, Arc::new(NoAuth));
        let mut q = req();
        q.model = ModelSpec::new("anthropic", "claude-opus-5-5", protocol);
        let (ev, err) = collect(exec(t.clone()).stream(&r, q, CancellationToken::new())).await;
        assert!(err.is_none(), "{file}: {err:?}");
        assert!(
            matches!(ev.last(), Some(StreamEvent::Finish { .. })),
            "{file}"
        );
        assert_eq!(t.recorded()[0].url, url);
    }
}
