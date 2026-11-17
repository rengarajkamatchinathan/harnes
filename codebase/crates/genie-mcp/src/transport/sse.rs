//! Minimal `text/event-stream` parser for Streamable HTTP (only `data`, `event`, `id`).

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
    pub id: Option<String>,
}

/// Incremental parser: feed bytes, get complete events.
#[derive(Default)]
pub struct SseParser {
    buf: Vec<u8>,
    data: Vec<String>,
    event: Option<String>,
    id: Option<String>,
}

/// One event's data may not exceed this.
pub const MAX_EVENT_BYTES: usize = 16 * 1024 * 1024;

impl SseParser {
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<SseEvent>, String> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(i) = self.buf.iter().position(|b| *b == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..=i).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let line = String::from_utf8_lossy(&line).into_owned();
            if line.is_empty() {
                if !self.data.is_empty() {
                    out.push(SseEvent {
                        event: self.event.take(),
                        data: self.data.join("\n"),
                        id: self.id.take(),
                    });
                }
                self.data.clear();
                self.event = None;
                continue;
            }
            if line.starts_with(':') {
                continue;
            }
            let (field, value) = match line.split_once(':') {
                Some((f, v)) => (f.to_string(), v.strip_prefix(' ').unwrap_or(v).to_string()),
                None => (line.clone(), String::new()),
            };
            match field.as_str() {
                "data" => self.data.push(value),
                "event" => self.event = Some(value),
                "id" => self.id = Some(value),
                _ => {}
            }
        }
        if self.buf.len() + self.data.iter().map(String::len).sum::<usize>() > MAX_EVENT_BYTES {
            return Err("SSE event exceeds 16 MiB".into());
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_split_events() {
        let mut p = SseParser::default();
        assert!(p.push(b"event: message\r\nda").unwrap().is_empty());
        let ev = p
            .push(b"ta: {\"a\":1}\r\n\r\n: comment\n\ndata: x\ndata: y\nid: 7\n\n")
            .unwrap();
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0].event.as_deref(), Some("message"));
        assert_eq!(ev[0].data, "{\"a\":1}");
        assert_eq!(ev[1].data, "x\ny");
        assert_eq!(ev[1].id.as_deref(), Some("7"));
    }
}
