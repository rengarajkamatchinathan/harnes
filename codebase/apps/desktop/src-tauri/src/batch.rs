//! Delta coalescing before IPC (05 §7, D57): daemon notifications are queued in order
//! and flushed to the webview as one Channel message every ~8 ms. Adjacent `part.delta`
//! events for the same part are merged into one (text and JSON fragments appended);
//! adjacent `tool.progress` events for the same call keep only the latest.

use serde_json::{Map, Value, json};

/// Flush interval (D57). `GENIE_COALESCE_MS` overrides it for benchmarks.
pub const BATCH_MS: u64 = 8;

/// One queued notification: `{method, params}` as JSON.
#[derive(Debug, Default)]
pub struct Batcher {
    items: Vec<Value>,
    /// Daemon notifications received (before merging), for diagnostics.
    received: u64,
}

impl Batcher {
    pub fn push(&mut self, method: &str, params: Value) {
        self.received += 1;
        if method == "event" && self.try_merge(&params) {
            return;
        }
        self.items
            .push(json!({ "method": method, "params": params }));
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn received(&self) -> u64 {
        self.received
    }

    /// Everything queued since the last flush, in arrival order.
    pub fn take(&mut self) -> Vec<Value> {
        std::mem::take(&mut self.items)
    }

    fn try_merge(&mut self, params: &Value) -> bool {
        let Some(last) = self.items.last_mut() else {
            return false;
        };
        if last["method"] != "event" || last["params"]["session_id"] != params["session_id"] {
            return false;
        }
        let ev = &params["event"];
        let prev = &mut last["params"]["event"];
        match ev["type"].as_str() {
            Some("part.delta")
                if prev["type"] == "part.delta" && prev["part_id"] == ev["part_id"] =>
            {
                if let Value::Object(p) = prev {
                    append(p, "text", &ev["text"]);
                    append(p, "json", &ev["json"]);
                }
                true
            }
            Some("tool.progress")
                if prev["type"] == "tool.progress" && prev["call_id"] == ev["call_id"] =>
            {
                *prev = ev.clone();
                true
            }
            _ => false,
        }
    }
}

fn append(obj: &mut Map<String, Value>, key: &str, add: &Value) {
    let Some(add) = add.as_str() else { return };
    match obj.get_mut(key) {
        Some(Value::String(s)) => s.push_str(add),
        _ => {
            obj.insert(key.to_string(), Value::String(add.to_string()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn delta(session: &str, part: &str, text: &str) -> Value {
        json!({"session_id": session, "ref_seq": 3, "ts": "t",
               "event": {"type": "part.delta", "part_id": part, "text": text}})
    }

    #[test]
    fn merges_adjacent_deltas_of_one_part() {
        let mut b = Batcher::default();
        b.push("event", delta("s", "p1", "Hel"));
        b.push("event", delta("s", "p1", "lo"));
        b.push("event", delta("s", "p1", " world"));
        assert_eq!(b.len(), 1);
        assert_eq!(b.received(), 3);
        let out = b.take();
        assert_eq!(out[0]["params"]["event"]["text"], "Hello world");
        assert!(b.is_empty());
    }

    #[test]
    fn keeps_order_across_parts_and_other_events() {
        let mut b = Batcher::default();
        b.push("event", delta("s", "p1", "a"));
        b.push("event", delta("s", "p2", "b"));
        b.push("event", delta("s", "p1", "c"));
        b.push(
            "event",
            json!({"session_id": "s", "seq": 9, "ts": "t",
                   "event": {"type": "part.ended", "part_id": "p1", "part": {"type": "text", "id": "p1", "text": "ac"}}}),
        );
        b.push("event", delta("s", "p2", "d"));
        b.push("event", delta("other", "p2", "e"));
        b.push("session.synced", json!({"session_id": "s", "seq": 9}));
        let out = b.take();
        let texts: Vec<_> = out
            .iter()
            .map(|v| {
                v["params"]["event"]["text"]
                    .as_str()
                    .unwrap_or("-")
                    .to_string()
            })
            .collect();
        assert_eq!(texts, ["a", "b", "c", "-", "d", "e", "-"]);
    }

    #[test]
    fn merges_tool_input_json_and_progress() {
        let mut b = Batcher::default();
        let j = |s: &str| json!({"session_id": "s", "ts": "t", "event": {"type": "part.delta", "part_id": "p", "json": s}});
        b.push("event", j("{\"a\":"));
        b.push("event", j("1}"));
        let p = |m: &str| json!({"session_id": "s", "ts": "t", "event": {"type": "tool.progress", "call_id": "c", "message": m}});
        b.push("event", p("1/3"));
        b.push("event", p("2/3"));
        let out = b.take();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["params"]["event"]["json"], "{\"a\":1}");
        assert_eq!(out[1]["params"]["event"]["message"], "2/3");
    }

    #[test]
    fn terminal_output_is_never_merged_or_dropped() {
        // Offsets must stay gap-free for the Terminal tab (05 §10).
        let mut b = Batcher::default();
        let o = |start: u64, data: &str| json!({"session_id": "s", "ts": "t", "event": {"type": "terminal.output", "terminal_id": "term_1", "start": start, "end": start + 1, "data_b64": data}});
        b.push("event", o(0, "YQ=="));
        b.push("event", o(1, "Yg=="));
        b.push(
            "terminal.exited",
            json!({"terminal_id": "term_1", "code": 0}),
        );
        let out = b.take();
        assert_eq!(out.len(), 3);
        assert_eq!(out[1]["params"]["event"]["start"], 1);
    }
}
