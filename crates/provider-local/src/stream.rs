//! Ollama NDJSON stream → SPP [`StreamEvent`] adapter.
//!
//! Ollama's streaming `/api/chat` endpoint emits newline-delimited JSON.
//! Each line is a [`ChatResponse`] with:
//!
//! - `done: false` — an incremental chunk carrying partial `message.content`
//!   and/or `message.tool_calls`.
//! - `done: true` — the final chunk carrying usage metadata and `done_reason`.
//!
//! Behaviour:
//!
//! - The first chunk synthesizes a [`StreamEvent::MessageStart`].
//! - Text delta chunks produce [`StreamEvent::ContentBlockDelta`] events
//!   on an open text block, opening it on first delta.
//! - Tool calls arrive atomically on any chunk (usually the last before
//!   `done: true`). Each produces a [`StreamEvent::ContentBlockStart`]
//!   immediately followed by [`StreamEvent::ContentBlockStop`].
//! - When `done: true` arrives, any open text block is closed and a
//!   [`StreamEvent::MessageDelta`] + [`StreamEvent::MessageStop`] are emitted.

use bytes::Bytes;
use futures::StreamExt;
use savvagent_mcp::StreamEmitter;
use savvagent_protocol::{self as spp, BlockDelta, ContentBlock, StreamEvent, Usage, UsageDelta};

use crate::api;
use crate::translate::{message_text, stop_reason_from_ollama};

/// Drive the Ollama NDJSON response stream to completion, forwarding SPP
/// events to `emit` as they arrive.
pub async fn consume_ndjson(
    resp: reqwest::Response,
    emit: &dyn StreamEmitter,
) -> Result<spp::CompleteResponse, spp::ProviderError> {
    let mut acc = Accumulator::default();
    let mut decoder = NdjsonDecoder::new(resp);

    while let Some(chunk) = decoder.next().await? {
        for ev in acc.consume_chunk(chunk) {
            let _ = emit.emit(ev).await;
        }
    }

    for ev in acc.flush() {
        let _ = emit.emit(ev).await;
    }

    acc.finish()
}

#[derive(Default)]
struct Accumulator {
    started: bool,
    model: Option<String>,
    usage: Usage,
    stop_reason: Option<spp::StopReason>,
    /// Index of the currently-open text block, if any. The text block is not
    /// always at `0` — a `tool_call` chunk arriving before any text chunk
    /// advances `next_index`, so we must remember which index the text block
    /// actually opened on and reuse it for every subsequent delta and the
    /// final stop event.
    text_index: Option<u32>,
    text_buf: String,
    /// Completed content blocks to put in the final response.
    final_blocks: Vec<ContentBlock>,
    /// Running index for the next block to open.
    next_index: u32,
}

impl Accumulator {
    fn consume_chunk(&mut self, chunk: api::ChatResponse) -> Vec<StreamEvent> {
        let mut events = Vec::new();

        if !self.started {
            self.started = true;
            self.model = chunk.model.clone();
            events.push(StreamEvent::MessageStart {
                id: "ollama-stream".to_string(),
                model: self.model.clone().unwrap_or_default(),
                usage: self.usage.clone(),
            });
        }

        let msg = chunk.message.as_ref();

        // Text delta.
        if let Some(text) = msg.and_then(message_text) {
            if !text.is_empty() {
                let idx = match self.text_index {
                    Some(i) => i,
                    None => {
                        // Open the text block on first delta at the current
                        // next_index (which may be > 0 if tool_calls have
                        // already advanced it).
                        let i = self.next_index;
                        events.push(StreamEvent::ContentBlockStart {
                            index: i,
                            block: ContentBlock::Text {
                                text: String::new(),
                            },
                        });
                        self.text_index = Some(i);
                        i
                    }
                };
                self.text_buf.push_str(&text);
                events.push(StreamEvent::ContentBlockDelta {
                    index: idx,
                    delta: BlockDelta::TextDelta { text },
                });
            }
        }

        // Tool calls arrive atomically.
        if let Some(m) = msg {
            for (idx, tc) in m.tool_calls.iter().enumerate() {
                // Close text block first if open.
                if let Some(text_idx) = self.text_index.take() {
                    events.push(StreamEvent::ContentBlockStop { index: text_idx });
                    self.final_blocks.push(ContentBlock::Text {
                        text: std::mem::take(&mut self.text_buf),
                    });
                    self.next_index += 1;
                }

                let block_idx = self.next_index + idx as u32;
                let id = tc
                    .id
                    .clone()
                    .unwrap_or_else(|| format!("ollama-{}-{idx}", tc.function.name));
                let block = ContentBlock::ToolUse {
                    id: id.clone(),
                    name: tc.function.name.clone(),
                    input: tc.function.arguments.clone(),
                };
                events.push(StreamEvent::ContentBlockStart {
                    index: block_idx,
                    block: block.clone(),
                });
                events.push(StreamEvent::ContentBlockStop { index: block_idx });
                self.final_blocks.push(block);
            }
            if !m.tool_calls.is_empty() {
                self.next_index += m.tool_calls.len() as u32;
            }
        }

        if chunk.done {
            self.stop_reason = Some(stop_reason_from_ollama(chunk.done_reason.as_deref()));
            self.usage.input_tokens = chunk.prompt_eval_count.unwrap_or(0);
            self.usage.output_tokens = chunk.eval_count.unwrap_or(0);
        }

        events
    }

    fn flush(&mut self) -> Vec<StreamEvent> {
        let mut events = Vec::new();

        // Close text block if open, using the index it actually opened on.
        if let Some(text_idx) = self.text_index.take() {
            events.push(StreamEvent::ContentBlockStop { index: text_idx });
            self.final_blocks.push(ContentBlock::Text {
                text: std::mem::take(&mut self.text_buf),
            });
        }

        let stop_reason = self.stop_reason.get_or_insert(spp::StopReason::EndTurn);
        events.push(StreamEvent::MessageDelta {
            stop_reason: Some(*stop_reason),
            stop_sequence: None,
            usage_delta: UsageDelta {
                output_tokens: Some(self.usage.output_tokens),
                cache_read_input_tokens: None,
            },
        });
        events.push(StreamEvent::MessageStop);
        events
    }

    fn finish(self) -> Result<spp::CompleteResponse, spp::ProviderError> {
        if !self.started {
            return Err(spp::ProviderError {
                kind: spp::ErrorKind::Internal,
                message: "stream produced no chunks".into(),
                retry_after_ms: None,
                provider_code: None,
            });
        }
        Ok(spp::CompleteResponse {
            id: "ollama-stream".into(),
            model: self.model.unwrap_or_default(),
            content: self.final_blocks,
            stop_reason: self.stop_reason.unwrap_or(spp::StopReason::EndTurn),
            stop_sequence: None,
            usage: self.usage,
        })
    }
}

// ── NDJSON byte-stream decoder ─────────────────────────────────────────────

struct NdjsonDecoder {
    inner: futures::stream::BoxStream<'static, reqwest::Result<Bytes>>,
    buf: Vec<u8>,
}

impl NdjsonDecoder {
    fn new(resp: reqwest::Response) -> Self {
        Self {
            inner: resp.bytes_stream().boxed(),
            buf: Vec::with_capacity(4 * 1024),
        }
    }

    /// Test-only constructor wrapping an arbitrary byte-chunk stream so the
    /// decoder can be exercised without spinning up an HTTP server.
    #[cfg(test)]
    fn from_stream(s: futures::stream::BoxStream<'static, reqwest::Result<Bytes>>) -> Self {
        Self {
            inner: s,
            buf: Vec::with_capacity(4 * 1024),
        }
    }

    async fn next(&mut self) -> Result<Option<api::ChatResponse>, spp::ProviderError> {
        loop {
            // Try to pop a complete line from the buffer.
            if let Some(line) = self.pop_line() {
                if line.is_empty() {
                    continue;
                }
                return Ok(Some(
                    serde_json::from_str::<api::ChatResponse>(&line).map_err(|e| {
                        spp::ProviderError {
                            kind: spp::ErrorKind::Internal,
                            message: format!("NDJSON decode error: {e} (line: {line:?})"),
                            retry_after_ms: None,
                            provider_code: None,
                        }
                    })?,
                ));
            }

            match self.inner.next().await {
                Some(Ok(bytes)) => self.buf.extend_from_slice(&bytes),
                Some(Err(e)) => {
                    return Err(spp::ProviderError {
                        kind: spp::ErrorKind::Network,
                        message: e.to_string(),
                        retry_after_ms: None,
                        provider_code: None,
                    });
                }
                None => {
                    // Drain any partial line left in the buffer.
                    if !self.buf.is_empty() {
                        let line = std::str::from_utf8(&self.buf)
                            .unwrap_or("")
                            .trim()
                            .to_string();
                        self.buf.clear();
                        if !line.is_empty() {
                            return Ok(Some(
                                serde_json::from_str::<api::ChatResponse>(&line).map_err(|e| {
                                    spp::ProviderError {
                                        kind: spp::ErrorKind::Internal,
                                        message: format!("NDJSON final-chunk decode error: {e}"),
                                        retry_after_ms: None,
                                        provider_code: None,
                                    }
                                })?,
                            ));
                        }
                    }
                    return Ok(None);
                }
            }
        }
    }

    fn pop_line(&mut self) -> Option<String> {
        let pos = self.buf.iter().position(|&b| b == b'\n')?;
        let line_bytes = self.buf.drain(..=pos).collect::<Vec<_>>();
        let s = std::str::from_utf8(&line_bytes).ok()?;
        Some(s.trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_chunk(v: serde_json::Value) -> api::ChatResponse {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn accumulator_assembles_text_stream() {
        let mut acc = Accumulator::default();
        let evs1 = acc.consume_chunk(make_chunk(json!({
            "model": "llama3.2",
            "message": { "role": "assistant", "content": "hel" },
            "done": false
        })));
        let evs2 = acc.consume_chunk(make_chunk(json!({
            "model": "llama3.2",
            "message": { "role": "assistant", "content": "lo" },
            "done": false
        })));
        let evs3 = acc.consume_chunk(make_chunk(json!({
            "model": "llama3.2",
            "message": { "role": "assistant", "content": "" },
            "done": true,
            "done_reason": "stop",
            "prompt_eval_count": 5,
            "eval_count": 3
        })));

        // First chunk must have MessageStart.
        assert!(
            evs1.iter()
                .any(|e| matches!(e, StreamEvent::MessageStart { .. }))
        );
        // Text deltas should be present.
        let has_delta = |evs: &[StreamEvent]| {
            evs.iter()
                .any(|e| matches!(e, StreamEvent::ContentBlockDelta { .. }))
        };
        assert!(has_delta(&evs1));
        assert!(has_delta(&evs2));
        // Final chunk with done=true: no delta if empty content.
        drop(evs3);

        let _ = acc.flush();
        let out = acc.finish().unwrap();
        assert_eq!(out.model, "llama3.2");
        assert_eq!(out.usage.input_tokens, 5);
        assert_eq!(out.usage.output_tokens, 3);
        match &out.content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "hello"),
            _ => panic!("expected text block"),
        }
    }

    #[test]
    fn accumulator_emits_tool_call_atomically() {
        let mut acc = Accumulator::default();
        let evs = acc.consume_chunk(make_chunk(json!({
            "model": "llama3.1",
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "tc-1",
                    "function": { "name": "ls", "arguments": { "path": "/tmp" } }
                }]
            },
            "done": true,
            "done_reason": "tool_calls"
        })));

        let has_tool_start = evs.iter().any(|e| {
            matches!(
                e,
                StreamEvent::ContentBlockStart {
                    block: ContentBlock::ToolUse { name, .. },
                    ..
                } if name == "ls"
            )
        });
        assert!(has_tool_start, "expected tool_use start: {evs:#?}");

        let _ = acc.flush();
        let out = acc.finish().unwrap();
        assert_eq!(out.stop_reason, spp::StopReason::ToolUse);
        match &out.content[0] {
            ContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "tc-1");
                assert_eq!(name, "ls");
                assert_eq!(input["path"], "/tmp");
            }
            _ => panic!("expected tool_use"),
        }
    }

    #[test]
    fn empty_stream_returns_error() {
        let acc = Accumulator::default();
        assert!(acc.finish().is_err());
    }

    #[test]
    fn accumulator_emits_text_after_tool_call_uses_correct_index() {
        // Regression: Ollama can stream a tool_call chunk before any text
        // chunk on tool-using turns that subsequently emit narration. The
        // text block must open at the next available index (1, after the
        // tool_use at 0) and every related event — Start, Delta(s), Stop —
        // must reference that same index.
        let mut acc = Accumulator::default();

        // 1) tool_call first (no text on this chunk).
        let _ = acc.consume_chunk(make_chunk(json!({
            "model": "llama3.1",
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "tc-1",
                    "function": { "name": "ls", "arguments": { "path": "/tmp" } }
                }]
            },
            "done": false
        })));

        // 2) text chunk arrives on a later message.
        let evs2 = acc.consume_chunk(make_chunk(json!({
            "model": "llama3.1",
            "message": { "role": "assistant", "content": "ok " },
            "done": false
        })));

        // 3) more text on a follow-up chunk to exercise multiple deltas at
        //    the same index.
        let evs3 = acc.consume_chunk(make_chunk(json!({
            "model": "llama3.1",
            "message": { "role": "assistant", "content": "done" },
            "done": false
        })));

        // 4) terminal chunk closes the stream.
        let _ = acc.consume_chunk(make_chunk(json!({
            "model": "llama3.1",
            "message": { "role": "assistant", "content": "" },
            "done": true,
            "done_reason": "stop",
            "prompt_eval_count": 4,
            "eval_count": 6
        })));

        let final_evs = acc.flush();

        // Locate the ContentBlockStart for the text block — must be at
        // index 1 (tool_use occupied 0).
        let text_start_index = evs2
            .iter()
            .find_map(|e| match e {
                StreamEvent::ContentBlockStart {
                    index,
                    block: ContentBlock::Text { .. },
                } => Some(*index),
                _ => None,
            })
            .expect("text block start should appear on the text chunk");
        assert_eq!(
            text_start_index, 1,
            "text block must open at index 1 after a tool_call at index 0"
        );

        // Every text delta in evs2/evs3 must carry the same index.
        for ev in evs2.iter().chain(evs3.iter()) {
            if let StreamEvent::ContentBlockDelta { index, .. } = ev {
                assert_eq!(
                    *index, text_start_index,
                    "text delta must reference the text block index, not 0"
                );
            }
        }

        // The final ContentBlockStop produced by flush() must close the same
        // index.
        let stop_index = final_evs
            .iter()
            .find_map(|e| match e {
                StreamEvent::ContentBlockStop { index } => Some(*index),
                _ => None,
            })
            .expect("flush should emit a ContentBlockStop for the open text block");
        assert_eq!(
            stop_index, text_start_index,
            "ContentBlockStop on flush must close the text block, not index 0"
        );
    }

    // ── NDJSON decoder unit tests ─────────────────────────────────────────
    //
    // These exercise the decoder buffering directly without HTTP. The PR
    // claims the decoder handles JSON documents that are split across
    // multiple `Bytes` chunks (e.g. when the `\n` delimiter only arrives in
    // the second chunk). These tests pin that behaviour down.

    fn make_byte_stream(
        chunks: Vec<&'static [u8]>,
    ) -> futures::stream::BoxStream<'static, reqwest::Result<Bytes>> {
        let v: Vec<reqwest::Result<Bytes>> = chunks
            .into_iter()
            .map(|c| Ok(Bytes::from_static(c)))
            .collect();
        futures::stream::iter(v).boxed()
    }

    #[tokio::test]
    async fn ndjson_decoder_buffers_split_chunks() {
        // A single JSON document split such that the trailing `\n` arrives
        // only in the second chunk. The decoder must not yield the first
        // chunk on its own.
        let part1 =
            br#"{"model":"llama3.2","message":{"role":"assistant","content":"he"# as &[u8];
        let part2 = br#"llo"},"done":false}
"# as &[u8];

        let mut dec = NdjsonDecoder::from_stream(make_byte_stream(vec![part1, part2]));
        let first = dec.next().await.unwrap().expect("expected one chunk");
        let txt = first
            .message
            .as_ref()
            .and_then(message_text)
            .expect("decoded chunk should carry text");
        assert_eq!(txt, "hello");

        // No more lines — stream ends cleanly.
        assert!(dec.next().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn ndjson_decoder_handles_multi_line_chunk() {
        // A single byte chunk carrying TWO complete NDJSON lines. The
        // decoder must yield both before pulling another chunk from the
        // underlying stream.
        let chunk = b"{\"model\":\"m\",\"message\":{\"role\":\"assistant\",\"content\":\"a\"},\"done\":false}\n{\"model\":\"m\",\"message\":{\"role\":\"assistant\",\"content\":\"b\"},\"done\":true,\"done_reason\":\"stop\"}\n" as &[u8];

        let mut dec = NdjsonDecoder::from_stream(make_byte_stream(vec![chunk]));
        let one = dec.next().await.unwrap().expect("first line");
        let two = dec.next().await.unwrap().expect("second line");
        assert_eq!(
            one.message.as_ref().and_then(message_text).as_deref(),
            Some("a")
        );
        assert!(two.done);
        assert_eq!(
            two.message.as_ref().and_then(message_text).as_deref(),
            Some("b")
        );
        assert!(dec.next().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn ndjson_decoder_drains_final_partial_line_without_newline() {
        // Underlying stream ends WITHOUT a trailing newline. The decoder
        // is documented to drain the remaining buffer as one final line.
        let chunk = br#"{"model":"m","message":{"role":"assistant","content":"x"},"done":true,"done_reason":"stop"}"#
            as &[u8];
        let mut dec = NdjsonDecoder::from_stream(make_byte_stream(vec![chunk]));
        let line = dec.next().await.unwrap().expect("final partial line");
        assert!(line.done);
        assert!(dec.next().await.unwrap().is_none());
    }

    #[test]
    fn pop_line_returns_none_until_newline_arrives() {
        let mut dec = NdjsonDecoder {
            inner: futures::stream::empty().boxed(),
            buf: Vec::new(),
        };
        dec.buf.extend_from_slice(b"{\"a\":1");
        assert!(dec.pop_line().is_none(), "no \\n yet — must buffer");
        dec.buf.extend_from_slice(b"}\nremainder");
        assert_eq!(dec.pop_line().as_deref(), Some("{\"a\":1}"));
        // Remainder stays in the buffer until the next \n arrives.
        assert!(dec.pop_line().is_none());
        assert_eq!(dec.buf, b"remainder");
    }

    // Integration test: mock Ollama server via axum.
    mod mock_server {
        use super::*;
        use axum::{Router, body::Body, http::StatusCode, response::Response, routing::post};

        fn ndjson_response(lines: Vec<serde_json::Value>) -> Response<Body> {
            let body = lines
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join("\n");
            Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "application/x-ndjson")
                .body(Body::from(body))
                .unwrap()
        }

        #[tokio::test]
        async fn streaming_text_round_trip() {
            let app = Router::new().route(
                "/api/chat",
                post(|| async {
                    ndjson_response(vec![
                        json!({ "model": "llama3.2", "message": {"role":"assistant","content":"hi"}, "done": false }),
                        json!({ "model": "llama3.2", "message": {"role":"assistant","content":""}, "done": true, "done_reason": "stop", "prompt_eval_count": 3, "eval_count": 1 }),
                    ])
                }),
            );

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });

            let provider = crate::provider_for_tests(format!("http://{addr}"));
            let req = savvagent_protocol::CompleteRequest {
                model: "llama3.2".into(),
                messages: vec![savvagent_protocol::Message {
                    role: savvagent_protocol::Role::User,
                    content: vec![savvagent_protocol::ContentBlock::Text { text: "hi".into() }],
                }],
                system: None,
                tools: vec![],
                temperature: None,
                top_p: None,
                max_tokens: 32,
                stop_sequences: vec![],
                stream: true,
                thinking: None,
                metadata: None,
            };

            let (tx, mut rx) = tokio::sync::mpsc::channel(64);
            let emitter = savvagent_mcp::ChannelEmitter::new(tx);
            let emitter_ref: &dyn savvagent_mcp::StreamEmitter = &emitter;

            use savvagent_mcp::ProviderHandler;
            let resp = provider.complete(req, Some(emitter_ref)).await.unwrap();
            drop(emitter);

            assert_eq!(resp.stop_reason, savvagent_protocol::StopReason::EndTurn);
            assert!(!resp.content.is_empty());

            let mut events = Vec::new();
            while let Some(e) = rx.recv().await {
                events.push(e);
            }
            assert!(
                events
                    .iter()
                    .any(|e| matches!(e, StreamEvent::MessageStart { .. }))
            );
            assert!(events.iter().any(|e| matches!(e, StreamEvent::MessageStop)));
        }

        #[tokio::test]
        async fn tool_call_round_trip() {
            use savvagent_protocol::ToolDef;
            let app = Router::new().route(
                "/api/chat",
                post(|| async {
                    ndjson_response(vec![json!({
                        "model": "llama3.1",
                        "message": {
                            "role": "assistant",
                            "content": null,
                            "tool_calls": [{
                                "id": "tc-abc",
                                "function": { "name": "ls", "arguments": { "path": "/home" } }
                            }]
                        },
                        "done": true,
                        "done_reason": "tool_calls"
                    })])
                }),
            );

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });

            let provider = crate::provider_for_tests(format!("http://{addr}"));
            let req = savvagent_protocol::CompleteRequest {
                model: "llama3.1".into(),
                messages: vec![savvagent_protocol::Message {
                    role: savvagent_protocol::Role::User,
                    content: vec![savvagent_protocol::ContentBlock::Text {
                        text: "list home".into(),
                    }],
                }],
                system: None,
                tools: vec![ToolDef {
                    name: "ls".into(),
                    description: "list directory".into(),
                    input_schema: serde_json::json!({ "type": "object", "properties": { "path": { "type": "string" } } }),
                }],
                temperature: None,
                top_p: None,
                max_tokens: 64,
                stop_sequences: vec![],
                stream: true,
                thinking: None,
                metadata: None,
            };

            use savvagent_mcp::ProviderHandler;
            let resp = provider.complete(req, None).await.unwrap();
            match &resp.content[0] {
                savvagent_protocol::ContentBlock::ToolUse { name, input, .. } => {
                    assert_eq!(name, "ls");
                    assert_eq!(input["path"], "/home");
                }
                _ => panic!("expected tool_use"),
            }
        }
    }
}
