//! Server-Sent Events parsing for the JMAP push channel (RFC 8620 §7.3).
//!
//! The wire format is [SSE]: `field: value` lines, frames separated by a blank
//! line. Only `event`, `data` and `id` carry meaning here — `id` is what a
//! reconnect replays from, via the `Last-Event-ID` header.
//!
//! [SSE]: https://html.spec.whatwg.org/multipage/server-sent-events.html

/// One complete SSE frame.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ServerEvent {
    pub event: Option<String>,
    pub data: String,
    pub id: Option<String>,
}

/// Reassembles frames from arbitrary byte chunks.
///
/// Chunk boundaries fall wherever the network puts them — mid-frame, mid-line,
/// even between the `\r` and `\n` of a line ending — so partial input is held
/// until the blank line that terminates a frame actually arrives.
pub struct EventParser {
    buf: Vec<u8>,
    skip_lf: bool,
    limit: usize,
}

impl Default for EventParser {
    fn default() -> Self {
        Self {
            buf: Vec::new(),
            skip_lf: false,
            limit: 1024 * 1024,
        }
    }
}

impl EventParser {
    /// Append a chunk and return every frame it completed.
    pub fn feed(&mut self, chunk: &str) -> Result<Vec<ServerEvent>, &'static str> {
        self.feed_bytes(chunk.as_bytes())
    }

    pub fn feed_bytes(&mut self, chunk: &[u8]) -> Result<Vec<ServerEvent>, &'static str> {
        let mut out = Vec::new();
        for &byte in chunk {
            if self.skip_lf && byte == b'\n' {
                self.skip_lf = false;
                continue;
            }
            self.skip_lf = byte == b'\r';
            self.buf.push(if byte == b'\r' { b'\n' } else { byte });
            if self.buf.len() > self.limit {
                self.buf.clear();
                return Err("Event stream frame exceeds its size limit");
            }
            if self.buf.ends_with(b"\n\n") {
                if let Some(event) = parse_frame(&String::from_utf8_lossy(&self.buf)) {
                    out.push(event);
                }
                self.buf.clear();
            }
        }
        Ok(out)
    }

    #[cfg(test)]
    pub(super) fn with_limit(limit: usize) -> Self {
        Self {
            limit,
            ..Self::default()
        }
    }
}

/// A frame with no recognised field is not an event — that is how keep-alive
/// comments (`: ping`) stay invisible to callers.
fn parse_frame(frame: &str) -> Option<ServerEvent> {
    let mut event = ServerEvent::default();
    let mut data = Vec::new();
    let mut recognised = false;

    for line in frame.lines() {
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        // A line with no colon is a field with an empty value.
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        match field {
            "event" => event.event = Some(value.to_string()),
            "data" => data.push(value),
            "id" if !value.contains('\0') => event.id = Some(value.to_string()),
            // `retry` and unknown fields are ignored: reconnect backoff is the
            // caller's, and it has better information than the server does.
            _ => continue,
        }
        recognised = true;
    }

    recognised.then(|| {
        event.data = data.join("\n");
        event
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_whole_frame() {
        let mut parser = EventParser::default();
        let events = parser
            .feed("event: state\ndata: {\"x\":1}\nid: abc\n\n")
            .unwrap();
        assert_eq!(
            events,
            vec![ServerEvent {
                event: Some("state".into()),
                data: "{\"x\":1}".into(),
                id: Some("abc".into()),
            }]
        );
    }

    #[test]
    fn holds_a_frame_split_across_chunks() {
        let mut parser = EventParser::default();
        assert!(parser.feed("event: sta").unwrap().is_empty());
        assert!(parser.feed("te\ndata: {\"x\":1}").unwrap().is_empty());
        let events = parser.feed("\n\n").unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data, "{\"x\":1}");
    }

    #[test]
    fn holds_a_frame_split_between_cr_and_lf() {
        let mut parser = EventParser::default();
        let mut events = parser.feed("data: hi\r\n\r").unwrap();
        events.extend(parser.feed("\ndata: there\r\n\r\n").unwrap());
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].data, "hi");
        assert_eq!(events[1].data, "there");
    }

    #[test]
    fn joins_repeated_data_lines() {
        let mut parser = EventParser::default();
        let events = parser.feed("data: one\ndata: two\n\n").unwrap();
        assert_eq!(events[0].data, "one\ntwo");
    }

    #[test]
    fn yields_several_frames_from_one_chunk() {
        let mut parser = EventParser::default();
        let events = parser.feed("data: a\n\ndata: b\n\ndata: c\n\n").unwrap();
        assert_eq!(events.len(), 3);
    }

    #[test]
    fn skips_comment_only_frames() {
        let mut parser = EventParser::default();
        assert!(parser.feed(": ping\n\n").unwrap().is_empty());
    }

    #[test]
    fn tolerates_a_missing_space_after_the_colon() {
        let mut parser = EventParser::default();
        let events = parser.feed("event:state\ndata:{}\n\n").unwrap();
        assert_eq!(events[0].event.as_deref(), Some("state"));
        assert_eq!(events[0].data, "{}");
    }

    #[test]
    fn unterminated_frames_cannot_grow_without_bound() {
        let mut parser = EventParser::with_limit(16);
        assert!(parser.feed(&"x".repeat(16)).unwrap().is_empty());
        assert!(parser.feed("x").is_err());
        assert!(parser.buf.is_empty());
    }

    #[test]
    fn frame_limit_is_independent_of_chunk_size() {
        let mut parser = EventParser::with_limit(16);
        assert_eq!(parser.feed(&"data: ok\n\n".repeat(10)).unwrap().len(), 10);
    }

    #[test]
    fn utf8_and_line_endings_can_cross_chunk_boundaries() {
        let input = "id: caf\u{e9}\rdata: hi\r\n\r\n";
        let mut parser = EventParser::default();
        let mut events = Vec::new();
        for byte in input.as_bytes() {
            events.extend(parser.feed_bytes(&[*byte]).unwrap());
        }
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id.as_deref(), Some("caf\u{e9}"));
        assert_eq!(events[0].data, "hi");
    }

    #[test]
    fn event_ids_reset_on_empty_values_but_ignore_nul_bytes() {
        let mut parser = EventParser::default();
        let events = parser
            .feed("id: first\nid: invalid\0value\ndata: one\n\nid\ndata: two\n\n")
            .unwrap();
        assert_eq!(events[0].id.as_deref(), Some("first"));
        assert_eq!(events[1].id.as_deref(), Some(""));
    }

    #[test]
    fn malformed_utf8_is_replaced_without_losing_the_next_frame() {
        let mut parser = EventParser::default();
        let events = parser
            .feed_bytes(b"data: a\xffb\n\ndata: next\n\n")
            .unwrap();
        assert_eq!(events[0].data, "a\u{fffd}b");
        assert_eq!(events[1].data, "next");
    }
}
