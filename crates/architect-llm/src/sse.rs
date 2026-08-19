//! Server-Sent Events decoding.
//!
//! Both providers stream SSE, so the framing is decoded once here and the
//! adapters only deal with parsed events. Doing this properly matters more than
//! it looks: chunk boundaries fall anywhere, including the middle of a
//! multi-byte character, so bytes are buffered and only split into lines on
//! `\n` — never decoded as UTF-8 per chunk.

use std::collections::VecDeque;

use bytes::Bytes;
use futures_util::{Stream, StreamExt};

use crate::error::LlmError;

/// One dispatched SSE event.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SseEvent {
    /// The `event:` field. Anthropic sets it; OpenAI does not.
    pub event: Option<String>,
    /// The `data:` field, with multiple lines joined by `\n` per the spec.
    pub data: String,
}

#[derive(Default)]
struct Decoder {
    buffer: Vec<u8>,
    event: Option<String>,
    data: Vec<String>,
    ready: VecDeque<SseEvent>,
}

impl Decoder {
    /// Feed a chunk and pull out whatever complete events it completed.
    fn push(&mut self, chunk: &[u8]) {
        self.buffer.extend_from_slice(chunk);

        while let Some(newline) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=newline).collect();
            // The line is whole, so it is safe to decode here.
            let line = String::from_utf8_lossy(&line);
            self.line(line.trim_end_matches(['\n', '\r']));
        }
    }

    fn line(&mut self, line: &str) {
        if line.is_empty() {
            self.dispatch();
            return;
        }

        // Comments (`: keep-alive`) are ignored.
        if let Some(stripped) = line.strip_prefix(':') {
            let _ = stripped;
            return;
        }

        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };

        match field {
            "event" => self.event = Some(value.to_owned()),
            "data" => self.data.push(value.to_owned()),
            // `id` and `retry` are irrelevant here: neither provider resumes a
            // dropped stream by last-event-id.
            _ => {}
        }
    }

    fn dispatch(&mut self) {
        if self.data.is_empty() && self.event.is_none() {
            return;
        }

        self.ready.push_back(SseEvent {
            event: self.event.take(),
            data: self.data.join("\n"),
        });
        self.data.clear();
    }

    /// Flush a trailing event that arrived without its blank line.
    fn finish(&mut self) {
        if !self.buffer.is_empty() {
            let line = String::from_utf8_lossy(&self.buffer)
                .trim_end_matches(['\n', '\r'])
                .to_owned();
            self.buffer.clear();
            self.line(&line);
        }
        self.dispatch();
    }
}

/// Decode a byte stream into SSE events.
pub fn decode<S>(stream: S) -> impl Stream<Item = Result<SseEvent, LlmError>>
where
    S: Stream<Item = reqwest::Result<Bytes>> + Send + 'static,
{
    futures_util::stream::unfold(
        (stream.boxed(), Decoder::default(), false),
        |(mut stream, mut decoder, mut ended)| async move {
            loop {
                if let Some(event) = decoder.ready.pop_front() {
                    return Some((Ok(event), (stream, decoder, ended)));
                }

                if ended {
                    return None;
                }

                match stream.next().await {
                    Some(Ok(chunk)) => decoder.push(&chunk),
                    Some(Err(error)) => {
                        ended = true;
                        return Some((Err(LlmError::Transport(error)), (stream, decoder, ended)));
                    }
                    None => {
                        ended = true;
                        decoder.finish();
                    }
                }
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(chunks: &[&[u8]]) -> Vec<SseEvent> {
        let mut decoder = Decoder::default();
        for chunk in chunks {
            decoder.push(chunk);
        }
        decoder.finish();
        decoder.ready.into_iter().collect()
    }

    #[test]
    fn parses_named_events() {
        let parsed = events(&[b"event: message_start\ndata: {\"a\":1}\n\ndata: [DONE]\n\n"]);

        assert_eq!(
            parsed,
            vec![
                SseEvent {
                    event: Some("message_start".into()),
                    data: "{\"a\":1}".into()
                },
                SseEvent {
                    event: None,
                    data: "[DONE]".into()
                },
            ]
        );
    }

    #[test]
    fn survives_chunk_boundaries_anywhere() {
        let whole = b"event: delta\ndata: {\"text\":\"hi\"}\n\n";

        for split in 1..whole.len() {
            let parsed = events(&[&whole[..split], &whole[split..]]);
            assert_eq!(parsed.len(), 1, "split at {split}");
            assert_eq!(parsed[0].data, "{\"text\":\"hi\"}", "split at {split}");
        }
    }

    #[test]
    fn survives_a_split_multibyte_character() {
        // "é" is two bytes; split between them.
        let whole = "data: {\"text\":\"é\"}\n\n".as_bytes();
        let boundary = whole.iter().position(|byte| *byte == 0xC3).unwrap() + 1;

        let parsed = events(&[&whole[..boundary], &whole[boundary..]]);

        assert_eq!(parsed[0].data, "{\"text\":\"é\"}");
    }

    #[test]
    fn joins_multiline_data_and_ignores_comments() {
        let parsed = events(&[b": keep-alive\ndata: one\ndata: two\n\n"]);

        assert_eq!(
            parsed,
            vec![SseEvent {
                event: None,
                data: "one\ntwo".into()
            }]
        );
    }

    #[test]
    fn handles_crlf_line_endings() {
        let parsed = events(&[b"event: ping\r\ndata: {}\r\n\r\n"]);

        assert_eq!(
            parsed,
            vec![SseEvent {
                event: Some("ping".into()),
                data: "{}".into()
            }]
        );
    }
}
