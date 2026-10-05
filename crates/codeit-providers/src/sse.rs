//! Server-sent events, as streamed by Copilot's three endpoints.

use anyhow::Result;
use futures_util::StreamExt;

#[derive(Debug, Default, PartialEq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

/// Incremental parser: feed it bytes, get back complete events.
#[derive(Default)]
pub struct SseParser {
    buf: String,
    pending: Vec<u8>,
    event: Option<String>,
    data: Vec<String>,
}

impl SseParser {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        // Keep a partial UTF-8 sequence until the rest of it arrives.
        self.pending.extend_from_slice(chunk);
        let valid = match std::str::from_utf8(&self.pending) {
            Ok(s) => s.len(),
            Err(e) => e.valid_up_to(),
        };
        self.buf.push_str(std::str::from_utf8(&self.pending[..valid]).unwrap_or_default());
        self.pending.drain(..valid);

        let mut out = Vec::new();
        while let Some(nl) = self.buf.find('\n') {
            let line: String = self.buf.drain(..=nl).collect();
            let line = line.trim_end_matches(['\n', '\r']);
            if line.is_empty() {
                if !self.data.is_empty() || self.event.is_some() {
                    out.push(SseEvent { event: self.event.take(), data: self.data.join("\n") });
                    self.data.clear();
                }
            } else if let Some(v) = line.strip_prefix("data:") {
                self.data.push(v.strip_prefix(' ').unwrap_or(v).to_string());
            } else if let Some(v) = line.strip_prefix("event:") {
                self.event = Some(v.trim().to_string());
            }
        }
        out
    }
}

/// Reads an SSE response to the end, handing each event's JSON to `on_event`.
/// `on_event` returns false to stop early (e.g. after a terminal event).
pub async fn for_each_json(
    res: reqwest::Response,
    mut on_event: impl FnMut(Option<&str>, serde_json::Value) -> Result<bool>,
) -> Result<()> {
    let mut parser = SseParser::default();
    let mut stream = res.bytes_stream();
    while let Some(chunk) = stream.next().await {
        for ev in parser.push(&chunk?) {
            if ev.data == "[DONE]" {
                return Ok(());
            }
            let Ok(json) = serde_json::from_str(&ev.data) else { continue };
            if !on_event(ev.event.as_deref(), json)? {
                return Ok(());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_events_across_chunks() {
        let mut p = SseParser::default();
        assert!(p.push(b"event: a\ndata: {\"x\"").is_empty());
        let evs = p.push(b":1}\n\ndata: [DONE]\n\n");
        assert_eq!(evs[0], SseEvent { event: Some("a".into()), data: "{\"x\":1}".into() });
        assert_eq!(evs[1].data, "[DONE]");
    }

    #[test]
    fn keeps_split_utf8() {
        let mut p = SseParser::default();
        let bytes = "data: é\n\n".as_bytes();
        assert!(p.push(&bytes[..7]).is_empty());
        assert_eq!(p.push(&bytes[7..])[0].data, "é");
    }
}
