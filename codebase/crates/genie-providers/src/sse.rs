//! Our own incremental Server-Sent Events parser (D18; WHATWG "event stream interpretation").
//!
//! - Accepts `\n`, `\r\n` and `\r` line endings, split anywhere across chunks (including
//!   inside a UTF-8 sequence or between `\r` and `\n`).
//! - Ignores comment lines (`: keepalive`) and a leading BOM.
//! - Joins multi-line `data:` fields with `\n`; dispatches on a blank line.
//! - Leaves `[DONE]` and `data: null` to the protocol decoder, so raw passthrough sees them.

use crate::protocol::RawFrame;

#[derive(Debug, Default)]
pub struct SseParser {
    buf: Vec<u8>,
    event: Option<String>,
    data: Option<String>,
    id: Option<String>,
    started: bool,
    /// Last byte consumed was `\r`; a following `\n` belongs to the same line ending.
    after_cr: bool,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a chunk; complete events are appended to `out`.
    pub fn push(&mut self, chunk: &[u8], out: &mut Vec<RawFrame>) {
        let mut chunk = chunk;
        if self.after_cr && chunk.first() == Some(&b'\n') {
            chunk = &chunk[1..];
        }
        self.after_cr = false;
        self.buf.extend_from_slice(chunk);
        let mut start = 0;
        let mut i = 0;
        while i < self.buf.len() {
            let b = self.buf[i];
            if b == b'\n' || b == b'\r' {
                let line = self.buf[start..i].to_vec();
                let mut next = i + 1;
                if b == b'\r' {
                    if next < self.buf.len() {
                        if self.buf[next] == b'\n' {
                            next += 1;
                        }
                    } else {
                        self.after_cr = true;
                    }
                }
                self.line(&line, out);
                start = next;
                i = next;
            } else {
                i += 1;
            }
        }
        self.buf.drain(..start);
    }

    /// End of stream: an event not terminated by a blank line is dispatched anyway, which
    /// is what providers that close right after the last `data:` line expect.
    pub fn finish(&mut self, out: &mut Vec<RawFrame>) {
        if !self.buf.is_empty() {
            let line = std::mem::take(&mut self.buf);
            self.line(&line, out);
        }
        self.dispatch(out);
    }

    fn line(&mut self, raw: &[u8], out: &mut Vec<RawFrame>) {
        let mut line = String::from_utf8_lossy(raw).into_owned();
        if !self.started {
            self.started = true;
            if let Some(s) = line.strip_prefix('\u{feff}') {
                line = s.to_string();
            }
        }
        if line.is_empty() {
            self.dispatch(out);
            return;
        }
        if line.starts_with(':') {
            return;
        }
        let (field, value) = match line.find(':') {
            Some(p) => {
                let v = &line[p + 1..];
                (&line[..p], v.strip_prefix(' ').unwrap_or(v))
            }
            None => (line.as_str(), ""),
        };
        match field {
            "event" => self.event = Some(value.to_string()),
            "data" => match &mut self.data {
                Some(d) => {
                    d.push('\n');
                    d.push_str(value);
                }
                None => self.data = Some(value.to_string()),
            },
            "id" => self.id = Some(value.to_string()),
            _ => {} // `retry` and unknown fields are ignored
        }
    }

    fn dispatch(&mut self, out: &mut Vec<RawFrame>) {
        let event = self.event.take();
        if let Some(data) = self.data.take() {
            out.push(RawFrame {
                event,
                data,
                id: self.id.clone(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_chunks(chunks: &[&[u8]]) -> Vec<RawFrame> {
        let mut p = SseParser::new();
        let mut out = vec![];
        for c in chunks {
            p.push(c, &mut out);
        }
        p.finish(&mut out);
        out
    }

    #[test]
    fn basic_events_and_comments() {
        let f = parse_chunks(&[b": ping\n\nevent: a\ndata: {\"x\":1}\n\ndata: b\n\n"]);
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].event.as_deref(), Some("a"));
        assert_eq!(f[0].data, "{\"x\":1}");
        assert_eq!(f[1].event, None);
        assert_eq!(f[1].data, "b");
    }

    #[test]
    fn multiline_data_and_no_space() {
        let f = parse_chunks(&[b"data:one\ndata: two\n\n"]);
        assert_eq!(f[0].data, "one\ntwo");
    }

    #[test]
    fn crlf_split_across_chunks_and_bom() {
        let f = parse_chunks(&[b"\xEF\xBB\xBFdata: a\r", b"\n\r", b"\ndata: b\r\r"]);
        assert_eq!(
            f.iter().map(|f| f.data.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
    }

    #[test]
    fn utf8_split_mid_sequence() {
        let s = "data: héllo\n\n".as_bytes();
        let f = parse_chunks(&[&s[..8], &s[8..]]);
        assert_eq!(f[0].data, "héllo");
    }

    #[test]
    fn byte_by_byte_equals_whole() {
        let text = b"event: message_start\ndata: {\"a\":1}\n\n: keep\ndata: [DONE]\n\n";
        let whole = parse_chunks(&[text]);
        let mut p = SseParser::new();
        let mut out = vec![];
        for b in text.iter() {
            p.push(std::slice::from_ref(b), &mut out);
        }
        p.finish(&mut out);
        assert_eq!(whole, out);
        assert_eq!(out[1].data, "[DONE]");
    }

    #[test]
    fn unterminated_last_event_is_dispatched() {
        let f = parse_chunks(&[b"data: tail"]);
        assert_eq!(f[0].data, "tail");
    }

    #[test]
    fn event_without_data_is_dropped() {
        let f = parse_chunks(&[b"event: x\n\n"]);
        assert!(f.is_empty());
    }
}
