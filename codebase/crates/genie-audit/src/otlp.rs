//! OTLP audit exporter (08 §6, D38): audit records as OpenTelemetry log records, sent with
//! OTLP/HTTP JSON to `<audit.otlp.endpoint>/v1/logs`.
//!
//! [`OtlpSink`] is an [`AuditSink`] that only queues (bounded; the oldest records are
//! dropped and counted when the collector is unreachable for long). The daemon drains it
//! with [`flush`] through an [`OtlpTransport`] (its HTTP client, with headers from the
//! keychain). Encoding is [`logs_request`].

use std::collections::VecDeque;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};

use crate::audit::{AUDIT_SCHEMA, AuditRecord, AuditSink};

/// Records kept while the collector is unreachable.
pub const OTLP_QUEUE_MAX: usize = 10_000;
/// Records per request.
pub const OTLP_BATCH: usize = 500;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Sends one `ExportLogsServiceRequest` JSON body.
pub trait OtlpTransport: Send + Sync {
    fn send(&self, body: Value) -> BoxFuture<'_, Result<(), String>>;
}

/// Bounded in-memory queue in front of the collector.
pub struct OtlpSink {
    queue: Mutex<VecDeque<AuditRecord>>,
    max: usize,
    dropped: AtomicU64,
}

impl std::fmt::Debug for OtlpSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OtlpSink")
            .field("queued", &self.len())
            .field("dropped", &self.dropped())
            .finish()
    }
}

impl Default for OtlpSink {
    fn default() -> Self {
        OtlpSink::new(OTLP_QUEUE_MAX)
    }
}

impl OtlpSink {
    pub fn new(max: usize) -> OtlpSink {
        OtlpSink {
            queue: Mutex::new(VecDeque::new()),
            max: max.max(1),
            dropped: AtomicU64::new(0),
        }
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Records dropped because the queue was full.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<AuditRecord>> {
        self.queue.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn take(&self, n: usize) -> Vec<AuditRecord> {
        let mut q = self.lock();
        let n = n.min(q.len());
        q.drain(..n).collect()
    }

    fn put_back(&self, batch: Vec<AuditRecord>) {
        let mut q = self.lock();
        for r in batch.into_iter().rev() {
            q.push_front(r);
        }
        while q.len() > self.max {
            q.pop_front();
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

impl AuditSink for OtlpSink {
    fn write(&self, record: &AuditRecord) -> io::Result<()> {
        let mut q = self.lock();
        if q.len() >= self.max {
            q.pop_front();
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
        q.push_back(record.clone());
        Ok(())
    }
}

/// Send everything queued, batch by batch. A failed batch goes back to the front of the
/// queue and the error is returned (the caller retries later). Returns records sent.
pub async fn flush(
    sink: &OtlpSink,
    transport: &dyn OtlpTransport,
    version: &str,
) -> Result<usize, String> {
    let mut sent = 0;
    loop {
        let batch = sink.take(OTLP_BATCH);
        if batch.is_empty() {
            return Ok(sent);
        }
        let body = logs_request(&batch, version);
        match transport.send(body).await {
            Ok(()) => sent += batch.len(),
            Err(e) => {
                sink.put_back(batch);
                return Err(e);
            }
        }
    }
}

/// OTLP `AnyValue` JSON for a JSON value.
pub fn any_value(v: &Value) -> Value {
    match v {
        Value::Null => json!({}),
        Value::Bool(b) => json!({ "boolValue": b }),
        Value::Number(n) => match n.as_i64() {
            Some(i) => json!({ "intValue": i.to_string() }),
            None => json!({ "doubleValue": n.as_f64().unwrap_or_default() }),
        },
        Value::String(s) => json!({ "stringValue": s }),
        Value::Array(a) => {
            json!({ "arrayValue": { "values": a.iter().map(any_value).collect::<Vec<_>>() } })
        }
        Value::Object(m) => {
            json!({ "kvlistValue": { "values": m.iter().map(|(k, v)| kv(k, any_value(v))).collect::<Vec<_>>() } })
        }
    }
}

fn kv(key: &str, value: Value) -> Value {
    json!({ "key": key, "value": value })
}

fn s(v: &str) -> Value {
    json!({ "stringValue": v })
}

/// `2026-11-17T09:30:00.123Z` → Unix nanoseconds (0 if unparsable).
pub fn rfc3339_nanos(ts: &str) -> u128 {
    let parse = || -> Option<u128> {
        let b = ts.as_bytes();
        let num = |r: std::ops::Range<usize>| ts.get(r)?.parse::<i64>().ok();
        if b.len() < 20 {
            return None;
        }
        let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
        let (h, mi, se) = (num(11..13)?, num(14..16)?, num(17..19)?);
        let millis = if b.get(19) == Some(&b'.') {
            let end = ts[20..].find(|c: char| !c.is_ascii_digit())? + 20;
            let frac = &ts[20..end];
            let padded = format!("{frac:0<9}");
            padded.get(..9)?.parse::<i64>().ok()?
        } else {
            0
        };
        // days from civil (Howard Hinnant).
        let y = if mo <= 2 { y - 1 } else { y };
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let mp = (mo + 9) % 12;
        let doy = (153 * mp + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        let days = era * 146_097 + doe - 719_468;
        let secs = days * 86_400 + h * 3600 + mi * 60 + se;
        u128::try_from(secs)
            .ok()
            .map(|s| s * 1_000_000_000 + millis as u128)
    };
    parse().unwrap_or(0)
}

/// An `ExportLogsServiceRequest` (OTLP/HTTP JSON) for audit records.
pub fn logs_request(records: &[AuditRecord], version: &str) -> Value {
    let logs: Vec<Value> = records
        .iter()
        .map(|r| {
            let mut attrs = vec![
                kv(
                    "audit_schema",
                    json!({ "intValue": AUDIT_SCHEMA.to_string() }),
                ),
                kv("event.name", s(&r.event)),
                kv("genie.install_id", s(&r.install_id)),
                kv("user.name", s(&r.user)),
            ];
            if let Some(o) = &r.org {
                attrs.push(kv("genie.org", s(o)));
            }
            if let Some(sid) = &r.session_id {
                attrs.push(kv("session.id", s(sid)));
            }
            for (k, v) in &r.attrs {
                if !v.is_null() {
                    attrs.push(kv(&format!("genie.{k}"), any_value(v)));
                }
            }
            let t = rfc3339_nanos(&r.ts).to_string();
            json!({
                "timeUnixNano": t,
                "observedTimeUnixNano": t,
                "severityNumber": 9,
                "severityText": "INFO",
                "eventName": r.event,
                "body": s(&r.event),
                "attributes": attrs,
            })
        })
        .collect();
    json!({
        "resourceLogs": [{
            "resource": { "attributes": [
                kv("service.name", s("genie")),
                kv("service.version", s(version)),
            ]},
            "scopeLogs": [{
                "scope": { "name": "genie.audit", "version": AUDIT_SCHEMA.to_string() },
                "logRecords": logs,
            }],
        }],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Map;
    use std::sync::Arc;

    fn rec(n: i64) -> AuditRecord {
        let mut attrs = Map::new();
        attrs.insert("tool".into(), json!("shell"));
        attrs.insert("duration_ms".into(), json!(n));
        attrs.insert("ok".into(), json!(true));
        attrs.insert("cost_usd".into(), json!(0.25));
        AuditRecord {
            audit_schema: 1,
            ts: "2026-11-18T11:06:40.250Z".into(),
            install_id: "i".into(),
            user: "alice".into(),
            org: Some("Org".into()),
            session_id: Some("s1".into()),
            event: "tool.called".into(),
            attrs,
        }
    }

    #[test]
    fn encodes_log_records() {
        assert_eq!(
            rfc3339_nanos("2026-11-18T11:06:40.250Z"),
            1_795_000_000_250_000_000
        );
        assert_eq!(rfc3339_nanos("1970-01-01T00:00:01Z"), 1_000_000_000);
        assert_eq!(rfc3339_nanos("garbage"), 0);
        let body = logs_request(&[rec(5)], "0.1.0");
        let lr = &body["resourceLogs"][0]["scopeLogs"][0]["logRecords"][0];
        assert_eq!(lr["timeUnixNano"], "1795000000250000000");
        assert_eq!(lr["eventName"], "tool.called");
        let attrs = lr["attributes"].as_array().unwrap();
        let get = |k: &str| {
            attrs
                .iter()
                .find(|a| a["key"] == k)
                .map(|a| a["value"].clone())
        };
        assert_eq!(get("genie.tool"), Some(json!({"stringValue": "shell"})));
        assert_eq!(get("genie.duration_ms"), Some(json!({"intValue": "5"})));
        assert_eq!(get("genie.ok"), Some(json!({"boolValue": true})));
        assert_eq!(get("genie.cost_usd"), Some(json!({"doubleValue": 0.25})));
        assert_eq!(get("session.id"), Some(json!({"stringValue": "s1"})));
        assert_eq!(
            body["resourceLogs"][0]["resource"]["attributes"][1]["value"]["stringValue"],
            "0.1.0"
        );
    }

    struct Fake {
        fail: Mutex<bool>,
        got: Mutex<Vec<Value>>,
    }
    impl OtlpTransport for Fake {
        fn send(&self, body: Value) -> BoxFuture<'_, Result<(), String>> {
            Box::pin(async move {
                if *self.fail.lock().unwrap() {
                    return Err("down".into());
                }
                self.got.lock().unwrap().push(body);
                Ok(())
            })
        }
    }

    fn block_on<F: Future>(f: F) -> F::Output {
        // Minimal executor: the fake transport never pends.
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        let mut f = std::pin::pin!(f);
        loop {
            if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
                return v;
            }
        }
    }

    #[test]
    fn queue_is_bounded_and_retried() {
        let sink = Arc::new(OtlpSink::new(3));
        for i in 0..5 {
            sink.write(&rec(i)).unwrap();
        }
        assert_eq!((sink.len(), sink.dropped()), (3, 2));
        let t = Fake {
            fail: Mutex::new(true),
            got: Mutex::new(vec![]),
        };
        assert!(block_on(flush(&sink, &t, "v")).is_err());
        assert_eq!(sink.len(), 3, "failed batch requeued");
        *t.fail.lock().unwrap() = false;
        assert_eq!(block_on(flush(&sink, &t, "v")).unwrap(), 3);
        assert!(sink.is_empty());
        let got = t.got.lock().unwrap();
        let first = &got[0]["resourceLogs"][0]["scopeLogs"][0]["logRecords"][0]["attributes"];
        assert!(
            first.to_string().contains("\"intValue\":\"2\""),
            "oldest dropped first"
        );
    }
}
