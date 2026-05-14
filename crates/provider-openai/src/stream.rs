//! OpenAI SSE → SPP [`StreamEvent`] adapter.
//!
//! OpenAI's Chat Completions streaming endpoint emits `data:` SSE lines whose
//! payloads are JSON `ChatCompletionChunk` objects. The stream ends with the
//! sentinel line `data: [DONE]`.
//!
//! The translation strategy mirrors the Anthropic adapter:
//!
//! - The first chunk synthesises a [`StreamEvent::MessageStart`].
//! - Text deltas in `choices[0].delta.content` map to `ContentBlockDelta`.
//! - Tool-call deltas in `choices[0].delta.tool_calls` map to either
//!   `ContentBlockStart` (first occurrence of an index) + `InputJsonDelta`
//!   fragments (subsequent `arguments` fragments), or `ContentBlockStop` once
//!   all arguments have been received (at `finish_reason = "tool_calls"`).
//! - The `[DONE]` sentinel triggers a `MessageDelta` + `MessageStop`.
//! - When `stream_options.include_usage = true`, the final chunk before
//!   `[DONE]` carries `usage`; we emit a final `MessageDelta` with that.

use bytes::{Bytes, BytesMut};
use futures::StreamExt;
<<<<<<< Updated upstream
use savvagent_mcp::{EmitError, StreamEmitter};
use savvagent_protocol::{self as spp, BlockDelta, ContentBlock, StreamEvent, Usage, UsageDelta};
=======
use savvagent_mcp::StreamEmitter;
use savvagent_protocol::{
    self as spp, BlockDelta, ContentBlock, StreamEvent, Usage, UsageDelta,
};
>>>>>>> Stashed changes

use crate::api;
use crate::translate::{parse_tool_arguments, stop_reason_from_str, usage_from_openai};

/// Drive an OpenAI SSE streaming response to completion.
<<<<<<< Updated upstream
///
/// If the consumer disconnects (i.e. [`StreamEmitter::emit`] returns
/// [`EmitError::Disconnected`]) we abandon the call rather than continue
/// pulling chunks from upstream and burning tokens. Transport-level emit
/// errors are tolerated — those are typically transient hiccups in the MCP
/// progress channel and the caller will still get the final structured
/// response.
=======
>>>>>>> Stashed changes
pub async fn consume_sse(
    resp: reqwest::Response,
    emit: &dyn StreamEmitter,
) -> Result<spp::CompleteResponse, spp::ProviderError> {
    let mut acc = Accumulator::default();
    let mut sse = SseDecoder::new(resp);

    while let SseItem::Chunk(chunk) = sse.next().await? {
        for ev in acc.consume_chunk(chunk) {
<<<<<<< Updated upstream
            if let Err(EmitError::Disconnected) = emit.emit(ev).await {
                return Err(consumer_disconnected());
            }
=======
            let _ = emit.emit(ev).await;
>>>>>>> Stashed changes
        }
    }

    for ev in acc.flush() {
<<<<<<< Updated upstream
        if let Err(EmitError::Disconnected) = emit.emit(ev).await {
            return Err(consumer_disconnected());
        }
=======
        let _ = emit.emit(ev).await;
>>>>>>> Stashed changes
    }

    acc.finish()
}

<<<<<<< Updated upstream
fn consumer_disconnected() -> spp::ProviderError {
    spp::ProviderError {
        kind: spp::ErrorKind::Internal,
        message: "stream consumer disconnected".into(),
        retry_after_ms: None,
        provider_code: None,
    }
}

=======
>>>>>>> Stashed changes
#[derive(Default)]
struct Accumulator {
    started: bool,
    id: Option<String>,
    model: Option<String>,
    usage: Usage,
    stop_reason: Option<spp::StopReason>,
    /// Per-block accumulator state indexed by SPP block index.
    blocks: Vec<BlockState>,
    /// Next SPP block index to assign.
    next_block: u32,
    /// Whether the text block has been opened.
    text_block_open: bool,
    /// Per-OpenAI-tool-call-index → SPP block index mapping.
    tool_block_map: Vec<Option<u32>>,
}

#[derive(Debug)]
enum BlockState {
<<<<<<< Updated upstream
    Text {
        buf: String,
    },
    ToolUse {
        id: String,
        name: String,
        json_buf: String,
    },
=======
    Text { buf: String },
    ToolUse { id: String, name: String, json_buf: String },
>>>>>>> Stashed changes
}

impl Accumulator {
    fn consume_chunk(&mut self, chunk: api::ChatCompletionChunk) -> Vec<StreamEvent> {
        let mut events = Vec::new();

        if !self.started {
            self.started = true;
            self.id = Some(chunk.id.clone());
            self.model = Some(chunk.model.clone());
            events.push(StreamEvent::MessageStart {
                id: chunk.id.clone(),
                model: chunk.model.clone(),
                usage: self.usage.clone(),
            });
        }

        // Capture usage from a chunk (typically the last one when
        // `include_usage = true`).
        if let Some(u) = chunk.usage {
            let spp_usage = usage_from_openai(u);
            self.usage = spp_usage;
        }

        let choice = chunk.choices.into_iter().next();
        let Some(choice) = choice else {
            return events;
        };

        if let Some(reason) = choice.finish_reason.as_deref() {
            self.stop_reason = Some(stop_reason_from_str(Some(reason)));
        }

        let delta = choice.delta;

        // Text delta.
        if let Some(text) = delta.content {
            if !text.is_empty() {
                if !self.text_block_open {
                    self.text_block_open = true;
                    let idx = self.alloc_block(BlockState::Text { buf: String::new() });
                    events.push(StreamEvent::ContentBlockStart {
                        index: idx,
                        block: ContentBlock::Text {
                            text: String::new(),
                        },
                    });
                }
                // Append to the text block (always index 0 unless tool blocks
                // precede it, which OpenAI doesn't do in practice).
                let idx = self.find_text_block_index();
<<<<<<< Updated upstream
                if let Some(BlockState::Text { buf }) =
                    idx.and_then(|i| self.blocks.get_mut(i as usize))
                {
=======
                if let Some(BlockState::Text { buf }) = idx.and_then(|i| self.blocks.get_mut(i as usize)) {
>>>>>>> Stashed changes
                    buf.push_str(&text);
                }
                if let Some(idx) = idx {
                    events.push(StreamEvent::ContentBlockDelta {
                        index: idx,
                        delta: BlockDelta::TextDelta { text },
                    });
                }
            }
        }

        // Tool-call deltas.
        for tc in delta.tool_calls {
            let oi = tc.index as usize;
            // Grow the map to cover this index.
            while self.tool_block_map.len() <= oi {
                self.tool_block_map.push(None);
            }

            if self.tool_block_map[oi].is_none() {
                // First delta for this tool-call: allocate an SPP block.
                let id = tc.id.unwrap_or_default();
<<<<<<< Updated upstream
                let name = tc
                    .function
                    .as_ref()
                    .and_then(|f| f.name.clone())
                    .unwrap_or_default();
=======
                let name = tc.function.as_ref().and_then(|f| f.name.clone()).unwrap_or_default();
>>>>>>> Stashed changes
                let block_idx = self.alloc_block(BlockState::ToolUse {
                    id: id.clone(),
                    name: name.clone(),
                    json_buf: String::new(),
                });
                self.tool_block_map[oi] = Some(block_idx);
                events.push(StreamEvent::ContentBlockStart {
                    index: block_idx,
                    block: ContentBlock::ToolUse {
                        id,
                        name,
                        input: serde_json::json!({}),
                    },
                });
            }

            let block_idx = self.tool_block_map[oi].expect("just inserted");
            if let Some(func) = tc.function {
                if let Some(args_frag) = func.arguments {
                    if !args_frag.is_empty() {
                        if let Some(BlockState::ToolUse { json_buf, .. }) =
                            self.blocks.get_mut(block_idx as usize)
                        {
                            json_buf.push_str(&args_frag);
                        }
                        events.push(StreamEvent::ContentBlockDelta {
                            index: block_idx,
                            delta: BlockDelta::InputJsonDelta {
                                partial_json: args_frag,
                            },
                        });
                    }
                }
            }
        }

        events
    }

    fn flush(&mut self) -> Vec<StreamEvent> {
        let mut events = Vec::new();

        // Close open blocks.
        for i in 0..self.blocks.len() {
            events.push(StreamEvent::ContentBlockStop { index: i as u32 });
        }

        if self.stop_reason.is_none() {
            self.stop_reason = Some(spp::StopReason::EndTurn);
        }
        events.push(StreamEvent::MessageDelta {
            stop_reason: self.stop_reason,
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
            return Err(stream_decode_error("stream produced no chunks"));
        }
        let mut content = Vec::new();
        for block in self.blocks {
            match block {
                BlockState::Text { buf } => {
                    content.push(ContentBlock::Text { text: buf });
                }
                BlockState::ToolUse { id, name, json_buf } => {
                    let input = parse_tool_arguments(&json_buf);
                    content.push(ContentBlock::ToolUse { id, name, input });
                }
            }
        }
        Ok(spp::CompleteResponse {
            id: self.id.unwrap_or_default(),
            model: self.model.unwrap_or_default(),
            content,
            stop_reason: self.stop_reason.unwrap_or(spp::StopReason::EndTurn),
            stop_sequence: None,
            usage: self.usage,
        })
    }

    fn alloc_block(&mut self, state: BlockState) -> u32 {
        let idx = self.next_block;
        self.blocks.push(state);
        self.next_block += 1;
        idx
    }

    fn find_text_block_index(&self) -> Option<u32> {
        for (i, b) in self.blocks.iter().enumerate() {
            if matches!(b, BlockState::Text { .. }) {
                return Some(i as u32);
            }
        }
        None
    }
}

fn stream_decode_error(msg: &str) -> spp::ProviderError {
    spp::ProviderError {
        kind: spp::ErrorKind::Internal,
        message: format!("stream decode error: {msg}"),
        retry_after_ms: None,
        provider_code: None,
    }
}

// ---- SSE decoder ----

<<<<<<< Updated upstream
#[derive(Debug)]
=======
>>>>>>> Stashed changes
enum SseItem {
    Chunk(api::ChatCompletionChunk),
    Done,
}

struct SseDecoder {
    inner: futures::stream::BoxStream<'static, reqwest::Result<Bytes>>,
    buf: BytesMut,
}

impl SseDecoder {
    fn new(resp: reqwest::Response) -> Self {
        Self {
            inner: resp.bytes_stream().boxed(),
            buf: BytesMut::with_capacity(8 * 1024),
        }
    }

<<<<<<< Updated upstream
    /// Build a decoder from a raw byte-chunk stream. For tests where we don't
    /// want to spin up an HTTP server just to feed bytes into the decoder.
    #[cfg(test)]
    fn from_stream(s: futures::stream::BoxStream<'static, reqwest::Result<Bytes>>) -> Self {
        Self {
            inner: s,
            buf: BytesMut::with_capacity(8 * 1024),
        }
    }

=======
>>>>>>> Stashed changes
    async fn next(&mut self) -> Result<SseItem, spp::ProviderError> {
        loop {
            if let Some(item) = self.try_pop()? {
                return Ok(item);
            }
            match self.inner.next().await {
                Some(Ok(chunk)) => self.buf.extend_from_slice(&chunk),
                Some(Err(e)) => {
                    return Err(spp::ProviderError {
                        kind: spp::ErrorKind::Network,
                        message: e.to_string(),
                        retry_after_ms: None,
                        provider_code: None,
                    });
                }
                None => {
<<<<<<< Updated upstream
                    // Stream ended. If we still have buffered bytes that
                    // didn't form a complete `\n\n`-terminated frame, the
                    // upstream truncated mid-frame — surface that as a
                    // network error rather than silently report success.
                    if !self.buf.is_empty() {
                        return Err(spp::ProviderError {
                            kind: spp::ErrorKind::Network,
                            message: "stream truncated mid-frame".into(),
                            retry_after_ms: None,
                            provider_code: None,
                        });
                    }
=======
                    // Stream ended; treat as done.
>>>>>>> Stashed changes
                    return Ok(SseItem::Done);
                }
            }
        }
    }

    fn try_pop(&mut self) -> Result<Option<SseItem>, spp::ProviderError> {
        let end = {
            let bytes = &self.buf[..];
            let len = bytes.len();
            let mut i = 0;
            let mut sep = None;
            while i + 1 < len {
                if bytes[i] == b'\n' && bytes[i + 1] == b'\n' {
                    sep = Some(i + 2);
                    break;
                }
                if i + 3 < len
                    && bytes[i] == b'\r'
                    && bytes[i + 1] == b'\n'
                    && bytes[i + 2] == b'\r'
                    && bytes[i + 3] == b'\n'
                {
                    sep = Some(i + 4);
                    break;
                }
                i += 1;
            }
            match sep {
                Some(s) => s,
                None => return Ok(None),
            }
        };

        let frame_bytes = self.buf.split_to(end);
        let text = match std::str::from_utf8(&frame_bytes) {
            Ok(t) => t,
            Err(_) => return Ok(None),
        };

        let mut data_lines: Vec<&str> = Vec::new();
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("data:") {
                data_lines.push(rest.strip_prefix(' ').unwrap_or(rest));
            }
        }

        if data_lines.is_empty() {
            return self.try_pop();
        }

        let data = data_lines.join("");
        if data.trim() == "[DONE]" {
            return Ok(Some(SseItem::Done));
        }

        let chunk: api::ChatCompletionChunk = match serde_json::from_str(&data) {
            Ok(c) => c,
            Err(_) => {
                // Silently skip unparseable frames (e.g. ping lines).
                return self.try_pop();
            }
        };
        Ok(Some(SseItem::Chunk(chunk)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn chunk(v: serde_json::Value) -> api::ChatCompletionChunk {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn accumulator_assembles_text_across_chunks() {
        let mut acc = Accumulator::default();
        let _ = acc.consume_chunk(chunk(json!({
            "id": "c1",
            "model": "gpt-4o-mini",
            "choices": [{"delta": {"content": "hel"}, "finish_reason": null}]
        })));
        let _ = acc.consume_chunk(chunk(json!({
            "id": "c1",
            "model": "gpt-4o-mini",
            "choices": [{"delta": {"content": "lo"}, "finish_reason": null}]
        })));
        let _ = acc.consume_chunk(chunk(json!({
            "id": "c1",
            "model": "gpt-4o-mini",
            "choices": [{"delta": {}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 5, "completion_tokens": 3, "total_tokens": 8}
        })));
        let _ = acc.flush();
        let out = acc.finish().unwrap();
        assert_eq!(out.id, "c1");
        assert_eq!(out.stop_reason, spp::StopReason::EndTurn);
        assert_eq!(out.usage.output_tokens, 3);
        match &out.content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "hello"),
            _ => panic!("expected text"),
        }
    }

    #[test]
    fn accumulator_assembles_tool_call() {
        let mut acc = Accumulator::default();
        // First chunk: tool call opens with id + name
        let _ = acc.consume_chunk(chunk(json!({
            "id": "c2",
            "model": "gpt-4o",
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_abc",
                        "type": "function",
                        "function": {"name": "ls", "arguments": ""}
                    }]
                },
                "finish_reason": null
            }]
        })));
        // Second chunk: arguments fragment
        let _ = acc.consume_chunk(chunk(json!({
            "id": "c2",
            "model": "gpt-4o",
            "choices": [{
                "delta": {
                    "tool_calls": [{"index": 0, "function": {"arguments": "{\"path\":"}}]
                },
                "finish_reason": null
            }]
        })));
        // Third chunk: finish the arguments
        let _ = acc.consume_chunk(chunk(json!({
            "id": "c2",
            "model": "gpt-4o",
            "choices": [{
                "delta": {
                    "tool_calls": [{"index": 0, "function": {"arguments": "\"/tmp\"}"}}]
                },
                "finish_reason": "tool_calls"
            }]
        })));
        let _ = acc.flush();
        let out = acc.finish().unwrap();
        assert_eq!(out.stop_reason, spp::StopReason::ToolUse);
        match &out.content[0] {
            ContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "call_abc");
                assert_eq!(name, "ls");
                assert_eq!(input["path"], "/tmp");
            }
            _ => panic!("expected tool_use, got {:?}", &out.content[0]),
        }
    }

    #[test]
    fn accumulator_handles_empty_stream() {
        let acc = Accumulator::default();
        let result = acc.finish();
        assert!(result.is_err(), "empty stream must return an error");
    }
<<<<<<< Updated upstream

    /// SSE byte streams that end without a terminating `\n\n` for the final
    /// frame must surface as a `Network` error, not silently report `Done`
    /// (which previously masked truncation and lost the partial chunk).
    #[tokio::test]
    async fn sse_decoder_errors_on_truncated_trailing_frame() {
        // Valid first frame, then a partial second frame missing `\n\n`.
        let bytes = bytes::Bytes::from_static(
            b"data: {\"id\":\"c1\",\"model\":\"gpt-4o-mini\",\"choices\":[{\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\ndata: {\"id\":\"c1\",\"model\":\"gpt-4o-mini\""
        );
        let s = futures::stream::iter(vec![Ok::<_, reqwest::Error>(bytes)]).boxed();
        let mut dec = SseDecoder::from_stream(s);

        // First call yields the well-formed chunk.
        let first = dec.next().await.expect("first chunk");
        assert!(matches!(first, SseItem::Chunk(_)));

        // Second call sees buffered bytes with no terminator and the inner
        // stream exhausted — must error.
        let err = dec
            .next()
            .await
            .expect_err("truncated trailing frame must error");
        assert_eq!(err.kind, spp::ErrorKind::Network);
        assert!(
            err.message.contains("truncated"),
            "unexpected message: {}",
            err.message
        );
    }
=======
>>>>>>> Stashed changes
}
