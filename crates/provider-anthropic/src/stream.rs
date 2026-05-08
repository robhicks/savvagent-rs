//! Anthropic SSE → SPP [`StreamEvent`] adapter.
//!
//! Anthropic streams the Messages API as Server-Sent Events whose `data:`
//! payloads are JSON objects with a `type` field. We parse them, translate
//! into [`spp::StreamEvent`], emit each over the [`StreamEmitter`], and
//! accumulate enough state to assemble the final [`spp::CompleteResponse`]
//! when the stream ends.

use bytes::{Bytes, BytesMut};
use futures::StreamExt;
use savvagent_mcp::StreamEmitter;
use savvagent_protocol::{
    self as spp, BlockDelta, ContentBlock, StopReason, StreamEvent, Usage, UsageDelta,
};
use serde::Deserialize;

use crate::translate::stop_reason_from_str;

/// Drive an Anthropic SSE response to completion, emitting SPP events along
/// the way and returning the assembled [`spp::CompleteResponse`].
pub async fn consume_sse(
    resp: reqwest::Response,
    emit: &dyn StreamEmitter,
) -> Result<spp::CompleteResponse, spp::ProviderError> {
    let mut acc = Accumulator::default();
    let mut sse = SseDecoder::new(resp);

    while let Some(frame) = sse.next().await? {
        let SseFrame { event, data } = frame;
        if event.as_deref() == Some("ping") {
            let _ = emit.emit(StreamEvent::Ping).await;
            continue;
        }

        let raw: AnthropicEvent = match serde_json::from_str(&data) {
            Ok(v) => v,
            Err(e) => {
                let _ = emit
                    .emit(StreamEvent::Warning {
                        message: format!("invalid SSE payload: {e}"),
                    })
                    .await;
                continue;
            }
        };

        for ev in acc.consume(raw) {
            // Best-effort: a disconnected emitter should not abort the call,
            // because the host can still want the final result.
            let _ = emit.emit(ev).await;
        }
    }

    acc.finish()
}

#[derive(Default)]
struct Accumulator {
    id: Option<String>,
    model: Option<String>,
    /// Block partial state, keyed by index.
    blocks: Vec<BlockState>,
    stop_reason: Option<StopReason>,
    stop_sequence: Option<String>,
    usage: Usage,
}

#[derive(Debug)]
enum BlockState {
    Text(String),
    ToolUse {
        id: String,
        name: String,
        partial_json: String,
    },
    Thinking {
        text: String,
        signature: Option<String>,
    },
    Image,
}

impl Accumulator {
    fn ensure_block(&mut self, idx: usize, init: BlockState) {
        while self.blocks.len() <= idx {
            self.blocks.push(BlockState::Text(String::new()));
        }
        self.blocks[idx] = init;
    }

    fn consume(&mut self, ev: AnthropicEvent) -> Vec<StreamEvent> {
        match ev {
            AnthropicEvent::MessageStart { message } => {
                self.id = Some(message.id.clone());
                self.model = Some(message.model.clone());
                self.usage.input_tokens = message.usage.input_tokens;
                self.usage.cache_creation_input_tokens = message.usage.cache_creation_input_tokens;
                self.usage.cache_read_input_tokens = message.usage.cache_read_input_tokens;
                vec![StreamEvent::MessageStart {
                    id: message.id,
                    model: message.model,
                    usage: self.usage.clone(),
                }]
            }
            AnthropicEvent::ContentBlockStart {
                index,
                content_block,
            } => {
                let (state, emitted) = match content_block {
                    SseContentBlock::Text { text } => {
                        (BlockState::Text(text.clone()), ContentBlock::Text { text })
                    }
                    SseContentBlock::ToolUse { id, name, input } => (
                        BlockState::ToolUse {
                            id: id.clone(),
                            name: name.clone(),
                            partial_json: String::new(),
                        },
                        ContentBlock::ToolUse {
                            id,
                            name,
                            input: input.unwrap_or(serde_json::json!({})),
                        },
                    ),
                    SseContentBlock::Thinking {
                        thinking,
                        signature,
                    } => (
                        BlockState::Thinking {
                            text: thinking.clone(),
                            signature: signature.clone(),
                        },
                        ContentBlock::Thinking {
                            text: thinking,
                            signature,
                        },
                    ),
                    SseContentBlock::Image => (
                        BlockState::Image,
                        ContentBlock::Text {
                            text: String::new(),
                        },
                    ),
                };
                self.ensure_block(index as usize, state);
                vec![StreamEvent::ContentBlockStart {
                    index,
                    block: emitted,
                }]
            }
            AnthropicEvent::ContentBlockDelta { index, delta } => {
                let (delta_event, _) = match delta {
                    SseDelta::TextDelta { text } => {
                        if let Some(BlockState::Text(buf)) = self.blocks.get_mut(index as usize) {
                            buf.push_str(&text);
                        }
                        (BlockDelta::TextDelta { text }, ())
                    }
                    SseDelta::InputJsonDelta { partial_json } => {
                        if let Some(BlockState::ToolUse {
                            partial_json: buf, ..
                        }) = self.blocks.get_mut(index as usize)
                        {
                            buf.push_str(&partial_json);
                        }
                        (BlockDelta::InputJsonDelta { partial_json }, ())
                    }
                    SseDelta::ThinkingDelta { thinking } => {
                        if let Some(BlockState::Thinking { text, .. }) =
                            self.blocks.get_mut(index as usize)
                        {
                            text.push_str(&thinking);
                        }
                        (BlockDelta::ThinkingDelta { text: thinking }, ())
                    }
                    SseDelta::SignatureDelta { signature } => {
                        if let Some(BlockState::Thinking { signature: sig, .. }) =
                            self.blocks.get_mut(index as usize)
                        {
                            *sig = Some(signature.clone());
                        }
                        (BlockDelta::SignatureDelta { signature }, ())
                    }
                };
                vec![StreamEvent::ContentBlockDelta {
                    index,
                    delta: delta_event,
                }]
            }
            AnthropicEvent::ContentBlockStop { index } => {
                vec![StreamEvent::ContentBlockStop { index }]
            }
            AnthropicEvent::MessageDelta { delta, usage } => {
                if let Some(reason) = delta.stop_reason.as_deref() {
                    self.stop_reason = Some(stop_reason_from_str(reason));
                }
                if delta.stop_sequence.is_some() {
                    self.stop_sequence = delta.stop_sequence.clone();
                }
                if let Some(out) = usage.output_tokens {
                    self.usage.output_tokens = self.usage.output_tokens.saturating_add(out);
                }
                vec![StreamEvent::MessageDelta {
                    stop_reason: self.stop_reason,
                    stop_sequence: self.stop_sequence.clone(),
                    usage_delta: UsageDelta {
                        output_tokens: usage.output_tokens,
                        cache_read_input_tokens: usage.cache_read_input_tokens,
                    },
                }]
            }
            AnthropicEvent::MessageStop => vec![StreamEvent::MessageStop],
            AnthropicEvent::Ping => vec![StreamEvent::Ping],
            AnthropicEvent::Error { error } => vec![StreamEvent::Warning {
                message: format!("{}: {}", error.kind, error.message),
            }],
        }
    }

    fn finish(self) -> Result<spp::CompleteResponse, spp::ProviderError> {
        let id = self
            .id
            .ok_or_else(|| stream_decode_error("missing message_start"))?;
        let model = self.model.unwrap_or_default();
        let mut content = Vec::with_capacity(self.blocks.len());
        for b in self.blocks {
            match b {
                BlockState::Text(text) => content.push(ContentBlock::Text { text }),
                BlockState::ToolUse {
                    id,
                    name,
                    partial_json,
                } => {
                    let input = if partial_json.is_empty() {
                        serde_json::json!({})
                    } else {
                        serde_json::from_str(&partial_json).map_err(|e| {
                            stream_decode_error(&format!("tool_use partial_json invalid: {e}"))
                        })?
                    };
                    content.push(ContentBlock::ToolUse { id, name, input });
                }
                BlockState::Thinking { text, signature } => {
                    content.push(ContentBlock::Thinking { text, signature });
                }
                BlockState::Image => {}
            }
        }
        Ok(spp::CompleteResponse {
            id,
            model,
            content,
            stop_reason: self.stop_reason.unwrap_or(StopReason::EndTurn),
            stop_sequence: self.stop_sequence,
            usage: self.usage,
        })
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

// ---- Anthropic SSE event JSON shapes ----

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicEvent {
    MessageStart {
        message: SseMessage,
    },
    ContentBlockStart {
        index: u32,
        content_block: SseContentBlock,
    },
    ContentBlockDelta {
        index: u32,
        delta: SseDelta,
    },
    ContentBlockStop {
        index: u32,
    },
    MessageDelta {
        delta: SseMessageDelta,
        usage: SseUsageDelta,
    },
    MessageStop,
    Ping,
    Error {
        error: SseError,
    },
}

#[derive(Debug, Deserialize)]
struct SseMessage {
    id: String,
    model: String,
    #[serde(default)]
    usage: SseInitialUsage,
}

#[derive(Debug, Default, Deserialize)]
struct SseInitialUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    cache_creation_input_tokens: Option<u32>,
    #[serde(default)]
    cache_read_input_tokens: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SseContentBlock {
    Text {
        #[serde(default)]
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: Option<serde_json::Value>,
    },
    Thinking {
        #[serde(default)]
        thinking: String,
        #[serde(default)]
        signature: Option<String>,
    },
    #[serde(other)]
    Image,
}

// Variants mirror Anthropic's `delta.type` wire field (`text_delta`,
// `input_json_delta`, …) one-for-one, so the shared `Delta` postfix is
// required by the protocol — not a naming smell.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SseDelta {
    TextDelta { text: String },
    InputJsonDelta { partial_json: String },
    ThinkingDelta { thinking: String },
    SignatureDelta { signature: String },
}

#[derive(Debug, Deserialize)]
struct SseMessageDelta {
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    stop_sequence: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct SseUsageDelta {
    #[serde(default)]
    output_tokens: Option<u32>,
    #[serde(default)]
    cache_read_input_tokens: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct SseError {
    #[serde(rename = "type")]
    kind: String,
    message: String,
}

// ---- Tiny SSE byte-stream decoder ----

struct SseDecoder {
    inner: futures::stream::BoxStream<'static, reqwest::Result<Bytes>>,
    buf: BytesMut,
}

#[derive(Debug)]
struct SseFrame {
    event: Option<String>,
    data: String,
}

impl SseDecoder {
    fn new(resp: reqwest::Response) -> Self {
        Self {
            inner: resp.bytes_stream().boxed(),
            buf: BytesMut::with_capacity(8 * 1024),
        }
    }

    async fn next(&mut self) -> Result<Option<SseFrame>, spp::ProviderError> {
        loop {
            if let Some(frame) = self.try_pop_frame() {
                return Ok(Some(frame));
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
                None => return Ok(self.try_pop_frame()),
            }
        }
    }

    fn try_pop_frame(&mut self) -> Option<SseFrame> {
        let end = {
            let bytes = &self.buf[..];
            let mut sep_idx = None;
            let len = bytes.len();
            let mut i = 0;
            while i + 1 < len {
                if bytes[i] == b'\n' && bytes[i + 1] == b'\n' {
                    sep_idx = Some(i + 2);
                    break;
                }
                if i + 3 < len
                    && bytes[i] == b'\r'
                    && bytes[i + 1] == b'\n'
                    && bytes[i + 2] == b'\r'
                    && bytes[i + 3] == b'\n'
                {
                    sep_idx = Some(i + 4);
                    break;
                }
                i += 1;
            }
            sep_idx?
        };
        let frame_bytes = self.buf.split_to(end);
        let text = std::str::from_utf8(&frame_bytes).ok()?;
        let mut event = None;
        let mut data_lines = Vec::new();
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("event:") {
                event = Some(rest.trim().to_string());
            } else if let Some(rest) = line.strip_prefix("data:") {
                data_lines.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
            }
            // ignore comment lines (starting with ':') and id:/retry: fields.
        }
        // empty frames (e.g. from a stray separator) are ignored
        if data_lines.is_empty() && event.is_none() {
            return self.try_pop_frame();
        }
        Some(SseFrame {
            event,
            data: data_lines.join("\n"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accumulator_assembles_text() {
        let mut acc = Accumulator::default();
        acc.consume(AnthropicEvent::MessageStart {
            message: SseMessage {
                id: "m1".into(),
                model: "claude-x".into(),
                usage: SseInitialUsage {
                    input_tokens: 5,
                    ..Default::default()
                },
            },
        });
        acc.consume(AnthropicEvent::ContentBlockStart {
            index: 0,
            content_block: SseContentBlock::Text {
                text: String::new(),
            },
        });
        acc.consume(AnthropicEvent::ContentBlockDelta {
            index: 0,
            delta: SseDelta::TextDelta { text: "hi".into() },
        });
        acc.consume(AnthropicEvent::ContentBlockDelta {
            index: 0,
            delta: SseDelta::TextDelta {
                text: " there".into(),
            },
        });
        acc.consume(AnthropicEvent::ContentBlockStop { index: 0 });
        acc.consume(AnthropicEvent::MessageDelta {
            delta: SseMessageDelta {
                stop_reason: Some("end_turn".into()),
                stop_sequence: None,
            },
            usage: SseUsageDelta {
                output_tokens: Some(2),
                ..Default::default()
            },
        });
        acc.consume(AnthropicEvent::MessageStop);

        let out = acc.finish().unwrap();
        assert_eq!(out.id, "m1");
        assert_eq!(out.usage.output_tokens, 2);
        assert_eq!(out.stop_reason, StopReason::EndTurn);
        match &out.content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "hi there"),
            _ => panic!("expected text"),
        }
    }

    #[test]
    fn accumulator_assembles_tool_use_input() {
        let mut acc = Accumulator::default();
        acc.consume(AnthropicEvent::MessageStart {
            message: SseMessage {
                id: "m2".into(),
                model: "claude-x".into(),
                usage: Default::default(),
            },
        });
        acc.consume(AnthropicEvent::ContentBlockStart {
            index: 0,
            content_block: SseContentBlock::ToolUse {
                id: "toolu_1".into(),
                name: "ls".into(),
                input: None,
            },
        });
        acc.consume(AnthropicEvent::ContentBlockDelta {
            index: 0,
            delta: SseDelta::InputJsonDelta {
                partial_json: "{\"path\":\"".into(),
            },
        });
        acc.consume(AnthropicEvent::ContentBlockDelta {
            index: 0,
            delta: SseDelta::InputJsonDelta {
                partial_json: "/tmp\"}".into(),
            },
        });
        acc.consume(AnthropicEvent::ContentBlockStop { index: 0 });
        acc.consume(AnthropicEvent::MessageDelta {
            delta: SseMessageDelta {
                stop_reason: Some("tool_use".into()),
                stop_sequence: None,
            },
            usage: SseUsageDelta {
                output_tokens: Some(7),
                ..Default::default()
            },
        });
        acc.consume(AnthropicEvent::MessageStop);

        let out = acc.finish().unwrap();
        assert_eq!(out.stop_reason, StopReason::ToolUse);
        match &out.content[0] {
            ContentBlock::ToolUse { name, input, .. } => {
                assert_eq!(name, "ls");
                assert_eq!(input["path"], "/tmp");
            }
            _ => panic!("expected tool_use"),
        }
    }
}
