//! Provider errors and classification (03 §7).
//!
//! Classification order: protocol-specific codes → `x-should-retry` → status → the
//! context-overflow regex corpus (never on 429) → body heuristics → `Unknown`.

use std::sync::LazyLock;
use std::time::Duration;

use regex::{Regex, RegexSet, RegexSetBuilder};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportPhase {
    /// DNS / TCP / TLS; nothing was sent.
    Connect,
    Send,
    Headers,
    Body,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeoutPhase {
    Headers,
    ChunkGap,
    Total,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ErrorKind {
    ContextOverflow,
    PayloadTooLarge,
    ContentPolicy {
        category: Option<String>,
    },
    QuotaExceeded,
    Authentication,
    RateLimited,
    Overloaded,
    ProviderInternal,
    InvalidRequest,
    ReplayRejected,
    UnsupportedOnDeployment,
    /// `offline`: DNS failure / network unreachable → the separate offline retry budget.
    Transport {
        phase: TransportPhase,
        offline: bool,
    },
    Timeout {
        phase: TimeoutPhase,
    },
    IncompleteStream,
    InvalidProviderOutput,
    Cancelled,
    Unknown,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryHint {
    pub retryable: bool,
    /// From `retry-after-ms` / `retry-after`.
    pub after: Option<Duration>,
    /// `x-should-retry`: `Some(true)` forces a retry, `Some(false)` forbids it.
    pub forced: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[error("{kind:?}: {message}")]
pub struct ProviderError {
    pub kind: ErrorKind,
    pub status: Option<u16>,
    pub provider_code: Option<String>,
    /// `request-id`, `x-request-id`, `apim-request-id`, … (logged, shown in Developer view).
    pub request_ids: Vec<(String, String)>,
    pub retry: RetryHint,
    pub message: String,
    /// Scrubbed and truncated response body.
    pub body_excerpt: Option<String>,
}

impl ProviderError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        let retryable = default_retryable(&kind);
        Self {
            kind,
            status: None,
            provider_code: None,
            request_ids: vec![],
            retry: RetryHint {
                retryable,
                after: None,
                forced: None,
            },
            message: message.into(),
            body_excerpt: None,
        }
    }
    pub fn invalid_request(m: impl Into<String>) -> Self {
        Self::new(ErrorKind::InvalidRequest, m)
    }
    pub fn invalid_output(m: impl Into<String>) -> Self {
        Self::new(ErrorKind::InvalidProviderOutput, m)
    }
    pub fn incomplete(m: impl Into<String>) -> Self {
        Self::new(ErrorKind::IncompleteStream, m)
    }
    pub fn cancelled() -> Self {
        Self::new(ErrorKind::Cancelled, "cancelled")
    }
}

/// Whether a kind is retryable when nothing else (headers, policy) says otherwise.
pub fn default_retryable(kind: &ErrorKind) -> bool {
    matches!(
        kind,
        ErrorKind::RateLimited
            | ErrorKind::Overloaded
            | ErrorKind::ProviderInternal
            | ErrorKind::IncompleteStream
            | ErrorKind::Transport { .. }
            | ErrorKind::Timeout { .. }
    )
}

/// A non-2xx response (or an in-stream error object), as seen by `classify`.
#[derive(Clone, Debug, Default)]
pub struct HttpFailure {
    pub status: u16,
    /// Lower-case header names.
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl HttpFailure {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
    pub fn json(&self) -> Option<serde_json::Value> {
        serde_json::from_str(&self.body).ok()
    }
}

/// What a protocol recognised in its own error codes; anything left `None` falls through
/// to the generic rules.
#[derive(Clone, Debug, Default)]
pub struct ProtocolHint {
    pub kind: Option<ErrorKind>,
    pub code: Option<String>,
    pub message: Option<String>,
}

static OVERFLOW: LazyLock<RegexSet> = LazyLock::new(|| {
    let pats: Vec<&str> = include_str!("overflow_patterns.txt")
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    RegexSetBuilder::new(pats)
        .case_insensitive(true)
        .build()
        .unwrap_or_else(|_| RegexSet::empty())
});

static SECRETS: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(
        r"(sk-[A-Za-z0-9_\-]{8,}|AIza[0-9A-Za-z_\-]{20,}|(?i:bearer)\s+[A-Za-z0-9._\-]{16,})",
    )
    .ok()
});

/// Number of patterns in the overflow corpus (spec: ~40).
pub fn overflow_pattern_count() -> usize {
    OVERFLOW.len()
}

pub fn is_context_overflow(text: &str) -> bool {
    OVERFLOW.is_match(text)
}

/// Whether `text` contains something shaped like an API key or bearer token.
pub fn looks_like_secret(text: &str) -> bool {
    SECRETS.as_ref().is_some_and(|re| re.is_match(text))
}

/// Remove things that look like keys and cap the length (01 §7 excerpt scrubbing).
pub fn scrub_excerpt(body: &str) -> String {
    let s = match &*SECRETS {
        Some(re) => re.replace_all(body, "[REDACTED]").into_owned(),
        None => body.to_string(),
    };
    const MAX: usize = 512;
    if s.len() <= MAX {
        return s;
    }
    let mut end = MAX;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

const REQUEST_ID_HEADERS: &[&str] = &[
    "request-id",
    "x-request-id",
    "apim-request-id",
    "x-amzn-requestid",
    "x-goog-request-id",
];

pub fn parse_retry_after(f: &HttpFailure) -> Option<Duration> {
    if let Some(ms) = f
        .header("retry-after-ms")
        .and_then(|v| v.trim().parse::<f64>().ok())
        && ms >= 0.0
    {
        return Some(Duration::from_millis(ms as u64));
    }
    let v = f.header("retry-after")?.trim();
    v.parse::<f64>()
        .ok()
        .filter(|s| *s >= 0.0)
        .map(Duration::from_secs_f64)
}

/// Pull a human message and code out of the common error body shapes:
/// `{error:{type,code,message,status}}`, `{type:"error",error:{…}}`, `[{error:{…}}]` (Gemini).
pub fn extract_error_fields(body: &serde_json::Value) -> (Option<String>, Option<String>) {
    let err = match body {
        serde_json::Value::Array(a) => a.first().and_then(|v| v.get("error")),
        v => v.get("error"),
    };
    let Some(err) = err else {
        let msg = body
            .get("message")
            .and_then(|m| m.as_str())
            .map(String::from);
        return (None, msg);
    };
    if let Some(s) = err.as_str() {
        return (None, Some(s.to_string()));
    }
    let code = ["code", "type", "status"]
        .iter()
        .find_map(|k| err.get(*k).and_then(|v| v.as_str()).map(String::from));
    let msg = err
        .get("message")
        .and_then(|m| m.as_str())
        .map(String::from);
    (code, msg)
}

/// The generic classifier. `hint` carries what the protocol recognised first.
pub fn classify(f: &HttpFailure, hint: ProtocolHint) -> ProviderError {
    let json = f.json();
    let (code, msg) = json.as_ref().map(extract_error_fields).unwrap_or_default();
    let code = hint.code.or(code);
    let message = hint.message.or(msg).unwrap_or_else(|| {
        if f.body.is_empty() {
            format!("HTTP {}", f.status)
        } else {
            scrub_excerpt(&f.body)
        }
    });
    let text = format!("{} {}", message, f.body);
    let lower = text.to_ascii_lowercase();

    let forced = f.header("x-should-retry").and_then(|v| match v.trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    });

    let kind = hint.kind.unwrap_or_else(|| {
        let by_status = match f.status {
            401 | 403 => Some(ErrorKind::Authentication),
            402 => Some(ErrorKind::QuotaExceeded),
            408 | 409 => Some(ErrorKind::ProviderInternal),
            429 => Some(if is_quota_text(&lower) {
                ErrorKind::QuotaExceeded
            } else {
                ErrorKind::RateLimited
            }),
            503 | 529 => Some(ErrorKind::Overloaded),
            500..=599 => Some(ErrorKind::ProviderInternal),
            _ => None,
        };
        if let Some(k) = by_status {
            return k;
        }
        // 400 / 404 / 413 / 422 / other: overflow corpus first, then heuristics.
        if is_context_overflow(&text) {
            return ErrorKind::ContextOverflow;
        }
        if let Some(k) = body_heuristics(&lower) {
            return k;
        }
        match f.status {
            413 => ErrorKind::PayloadTooLarge,
            400..=499 => ErrorKind::InvalidRequest,
            _ => ErrorKind::Unknown,
        }
    });

    let mut retry = RetryHint {
        retryable: default_retryable(&kind),
        after: parse_retry_after(f),
        forced,
    };
    if let Some(force) = forced {
        retry.retryable = force;
    }
    let request_ids = REQUEST_ID_HEADERS
        .iter()
        .filter_map(|h| f.header(h).map(|v| (h.to_string(), v.to_string())))
        .collect();
    ProviderError {
        kind,
        status: Some(f.status),
        provider_code: code,
        request_ids,
        retry,
        message,
        body_excerpt: (!f.body.is_empty()).then(|| scrub_excerpt(&f.body)),
    }
}

fn is_quota_text(lower: &str) -> bool {
    [
        "insufficient_quota",
        "exceeded your current quota",
        "check your plan and billing",
        "billing",
        "credit balance is too low",
        "quota exceeded for this month",
    ]
    .iter()
    .any(|p| lower.contains(p))
}

fn body_heuristics(lower: &str) -> Option<ErrorKind> {
    if lower.contains("signature") && (lower.contains("thinking") || lower.contains("reasoning"))
        || lower.contains("bound to a different")
        || lower.contains("thinking blocks") && lower.contains("cannot be modified")
        || lower.contains("encrypted content") && lower.contains("could not be verified")
    {
        return Some(ErrorKind::ReplayRejected);
    }
    if lower.contains("model not supported")
        || lower.contains("not supported on this deployment")
        || lower.contains("hosted on azure")
        || lower.contains("operation is not supported")
        || lower.contains("unsupported for this model")
    {
        return Some(ErrorKind::UnsupportedOnDeployment);
    }
    if lower.contains("content_filter")
        || lower.contains("content management policy")
        || lower.contains("content_policy")
        || lower.contains("responsibleaipolicyviolation")
        || lower.contains("safety system")
    {
        return Some(ErrorKind::ContentPolicy { category: None });
    }
    if is_quota_text(lower) {
        return Some(ErrorKind::QuotaExceeded);
    }
    if lower.contains("request too large")
        || lower.contains("payload too large")
        || lower.contains("request_too_large")
    {
        return Some(ErrorKind::PayloadTooLarge);
    }
    if lower.contains("invalid api key")
        || lower.contains("invalid x-api-key")
        || lower.contains("incorrect api key")
    {
        return Some(ErrorKind::Authentication);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fail(status: u16, body: &str) -> HttpFailure {
        HttpFailure {
            status,
            headers: vec![],
            body: body.into(),
        }
    }

    #[test]
    fn corpus_has_about_forty_patterns_that_compile() {
        assert!(
            overflow_pattern_count() >= 40,
            "{}",
            overflow_pattern_count()
        );
    }

    #[test]
    fn overflow_samples_match() {
        let samples = [
            "prompt is too long: 213456 tokens > 200000 maximum",
            "This model's maximum context length is 128000 tokens. However, your messages resulted in 130000 tokens.",
            "The input token count (1200000) exceeds the maximum number of tokens allowed (1048576).",
            "Input is too long for requested model.",
            "the request exceeds the available context size, try increasing it",
            "context_length_exceeded",
            "Please reduce the length of the messages or completion.",
            "input is larger than the max context size",
            "model_context_window_exceeded",
        ];
        for s in samples {
            let e = classify(
                &fail(400, &format!(r#"{{"error":{{"message":"{s}"}}}}"#)),
                ProtocolHint::default(),
            );
            assert_eq!(e.kind, ErrorKind::ContextOverflow, "{s}");
        }
        assert!(!is_context_overflow(
            "rate limit exceeded, please slow down"
        ));
        assert!(!is_context_overflow("invalid tool schema"));
    }

    #[test]
    fn overflow_never_on_429() {
        let e = classify(&fail(429, "prompt is too long"), ProtocolHint::default());
        assert_eq!(e.kind, ErrorKind::RateLimited);
    }

    #[test]
    fn quota_vs_rate_limit() {
        let q = classify(
            &fail(
                429,
                r#"{"error":{"code":"insufficient_quota","message":"You exceeded your current quota"}}"#,
            ),
            ProtocolHint::default(),
        );
        assert_eq!(q.kind, ErrorKind::QuotaExceeded);
        assert!(!q.retry.retryable);
        assert_eq!(q.provider_code.as_deref(), Some("insufficient_quota"));
        let r = classify(
            &fail(429, r#"{"error":{"message":"slow down"}}"#),
            ProtocolHint::default(),
        );
        assert_eq!(r.kind, ErrorKind::RateLimited);
        assert!(r.retry.retryable);
    }

    #[test]
    fn retry_after_and_should_retry_and_request_ids() {
        let f = HttpFailure {
            status: 529,
            headers: vec![
                ("retry-after".into(), "7".into()),
                ("x-should-retry".into(), "false".into()),
                ("apim-request-id".into(), "abc".into()),
            ],
            body: String::new(),
        };
        let e = classify(&f, ProtocolHint::default());
        assert_eq!(e.kind, ErrorKind::Overloaded);
        assert_eq!(e.retry.after, Some(Duration::from_secs(7)));
        assert_eq!(e.retry.forced, Some(false));
        assert!(!e.retry.retryable);
        assert_eq!(
            e.request_ids,
            vec![("apim-request-id".to_string(), "abc".to_string())]
        );

        let f = HttpFailure {
            status: 400,
            headers: vec![
                ("retry-after-ms".into(), "1500".into()),
                ("x-should-retry".into(), "true".into()),
            ],
            body: "{}".into(),
        };
        let e = classify(&f, ProtocolHint::default());
        assert_eq!(e.retry.after, Some(Duration::from_millis(1500)));
        assert!(e.retry.retryable);
    }

    #[test]
    fn statuses() {
        let k = |s, b: &str| classify(&fail(s, b), ProtocolHint::default()).kind;
        assert_eq!(k(401, ""), ErrorKind::Authentication);
        assert_eq!(k(403, ""), ErrorKind::Authentication);
        assert_eq!(k(402, ""), ErrorKind::QuotaExceeded);
        assert_eq!(k(500, ""), ErrorKind::ProviderInternal);
        assert_eq!(k(503, ""), ErrorKind::Overloaded);
        assert_eq!(k(413, "too big"), ErrorKind::PayloadTooLarge);
        assert_eq!(
            k(400, "Model not supported"),
            ErrorKind::UnsupportedOnDeployment
        );
        assert_eq!(
            k(
                400,
                "messages.1.content.0: Invalid `signature` in `thinking` block"
            ),
            ErrorKind::ReplayRejected
        );
        assert_eq!(
            k(
                400,
                r#"{"error":{"code":"content_filter","message":"blocked"}}"#
            ),
            ErrorKind::ContentPolicy { category: None }
        );
        assert_eq!(k(400, "bad"), ErrorKind::InvalidRequest);
        assert_eq!(k(302, ""), ErrorKind::Unknown);
    }

    #[test]
    fn excerpt_scrubs_keys() {
        let s = scrub_excerpt("key sk-ant-api03-abcdefghijkl and AIzaSyA1234567890abcdefghijk end");
        assert!(!s.contains("sk-ant"));
        assert!(!s.contains("AIza"));
        assert!(scrub_excerpt(&"x".repeat(2000)).len() < 600);
    }
}
