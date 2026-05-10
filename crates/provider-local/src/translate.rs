//! Pure translation between SPP and Ollama `/api/chat` types.
//!
//! Ollama's tool-call schema follows the OpenAI convention:
//! - Request carries `tools: [{ type: "function", function: { name, description, parameters } }]`
//! - Response message carries `tool_calls: [{ id?, function: { name, arguments } }]`
//! - Tool results come back as `role: "tool"` messages
//!
//! For models that don't support tool calling, the `tools` array is simply
//! omitted and any tool-result messages are rendered as user text so the
//! conversation stays valid.

use savvagent_protocol::{self as spp};

use crate::api;

/// Translate an SPP request into an Ollama `/api/chat` body.
pub fn request_to_ollama(req: &spp::CompleteRequest, stream: bool) -> api::ChatRequest {
    let has_tool_support = !req.tools.is_empty();

    let mut messages: Vec<api::Message> = Vec::new();

    // Ollama expects a system message first when a system prompt is present.
    if let Some(sys) = &req.system {
        messages.push(api::Message::text("system", sys.clone()));
    }

    for m in &req.messages {
        push_messages_for_spp(m, &mut messages, has_tool_support);
    }

    let tools: Vec<api::Tool> = req.tools.iter().map(tool_to_ollama).collect();

    let options = build_options(req);

    api::ChatRequest {
        model: req.model.clone(),
        messages,
        stream,
        tools,
        options,
    }
}

/// Translate a final (non-streaming) Ollama response into SPP.
pub fn response_from_ollama(r: api::ChatResponse) -> spp::CompleteResponse {
    let model = r.model.unwrap_or_default();
    let usage = spp::Usage {
        input_tokens: r.prompt_eval_count.unwrap_or(0),
        output_tokens: r.eval_count.unwrap_or(0),
        cache_creation_input_tokens: None,
        cache_read_input_tokens: None,
    };
    let stop_reason = stop_reason_from_ollama(r.done_reason.as_deref());

    let content = r
        .message
        .map(|m| message_content_to_spp(&m))
        .unwrap_or_default();

    spp::CompleteResponse {
        id: format!("ollama-{}", uuid_v4_simple()),
        model,
        content,
        stop_reason,
        stop_sequence: None,
        usage,
    }
}

pub(crate) fn stop_reason_from_ollama(reason: Option<&str>) -> spp::StopReason {
    match reason {
        Some("stop") => spp::StopReason::EndTurn,
        Some("length") => spp::StopReason::MaxTokens,
        Some("tool_calls") => spp::StopReason::ToolUse,
        _ => spp::StopReason::EndTurn,
    }
}

/// Extract SPP content blocks from an Ollama response message.
pub(crate) fn message_content_to_spp(m: &api::Message) -> Vec<spp::ContentBlock> {
    let mut blocks = Vec::new();

    // Text content.
    if let Some(text) = message_text(m) {
        if !text.is_empty() {
            blocks.push(spp::ContentBlock::Text { text });
        }
    }

    // Tool calls.
    for (idx, tc) in m.tool_calls.iter().enumerate() {
        let id = tc
            .id
            .clone()
            .unwrap_or_else(|| format!("ollama-{}-{idx}", tc.function.name));
        blocks.push(spp::ContentBlock::ToolUse {
            id,
            name: tc.function.name.clone(),
            input: tc.function.arguments.clone(),
        });
    }

    blocks
}

/// Extract the text string from an Ollama message `content` field.
///
/// Ollama sends content as either a plain JSON string or sometimes as an
/// array of content parts (rare in current versions). We handle both.
pub(crate) fn message_text(m: &api::Message) -> Option<String> {
    match &m.content {
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        Some(serde_json::Value::Array(parts)) => {
            // If the model ever sends content parts, concatenate text fields.
            let text: String = parts
                .iter()
                .filter_map(|p| p.get("text")?.as_str().map(String::from))
                .collect::<Vec<_>>()
                .join("");
            if text.is_empty() { None } else { Some(text) }
        }
        Some(serde_json::Value::Null) | None => None,
        // Fallback: serialize whatever we got.
        Some(other) => Some(other.to_string()),
    }
}

fn push_messages_for_spp(
    m: &spp::Message,
    out: &mut Vec<api::Message>,
    has_tool_support: bool,
) {
    match m.role {
        spp::Role::User => {
            // Collect tool results and text separately.
            let mut tool_results: Vec<(&str, String)> = Vec::new();
            let mut text_parts: Vec<&str> = Vec::new();

            for b in &m.content {
                match b {
                    spp::ContentBlock::Text { text } => text_parts.push(text.as_str()),
                    spp::ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => {
                        let result_text = flatten_tool_result_text(content);
                        let result = if *is_error {
                            format!("[error] {result_text}")
                        } else {
                            result_text
                        };
                        tool_results.push((tool_use_id.as_str(), result));
                    }
                    spp::ContentBlock::Image { .. } => {
                        // Ollama's vision support is model-dependent; skip
                        // images rather than crashing.
                    }
                    _ => {}
                }
            }

            if has_tool_support {
                // Emit tool results as "tool" role messages, one per call.
                for (id, result) in tool_results {
                    out.push(api::Message::tool_result(id, result));
                }
            } else {
                // Models without tool support: render tool results as
                // "user" text so the conversation round-trips.
                for (id, result) in &tool_results {
                    out.push(api::Message::text(
                        "user",
                        format!("[tool result for {id}]: {result}"),
                    ));
                }
            }

            if !text_parts.is_empty() {
                let combined = text_parts.join("\n");
                out.push(api::Message::text("user", combined));
            }
        }
        spp::Role::Assistant => {
            // SPP assistant messages can contain text and/or tool_use blocks.
            // Reconstruct an Ollama assistant message with both.
            let mut text_buf = String::new();
            let mut tool_calls: Vec<api::ToolCall> = Vec::new();

            for b in &m.content {
                match b {
                    spp::ContentBlock::Text { text } => {
                        if !text_buf.is_empty() {
                            text_buf.push('\n');
                        }
                        text_buf.push_str(text.as_str());
                    }
                    spp::ContentBlock::ToolUse { id, name, input } => {
                        tool_calls.push(api::ToolCall {
                            id: Some(id.clone()),
                            function: api::FunctionCall {
                                name: name.clone(),
                                arguments: input.clone(),
                            },
                        });
                    }
                    _ => {}
                }
            }

            let content = if text_buf.is_empty() {
                None
            } else {
                Some(serde_json::Value::String(text_buf))
            };

            out.push(api::Message {
                role: "assistant".into(),
                content,
                tool_calls,
            });
        }
    }
}

fn flatten_tool_result_text(blocks: &[spp::ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|b| {
            if let spp::ContentBlock::Text { text } = b {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn tool_to_ollama(t: &spp::ToolDef) -> api::Tool {
    api::Tool {
        kind: "function".into(),
        function: api::FunctionDef {
            name: t.name.clone(),
            description: t.description.clone(),
            parameters: t.input_schema.clone(),
        },
    }
}

fn build_options(req: &spp::CompleteRequest) -> Option<api::Options> {
    let opts = api::Options {
        temperature: req.temperature,
        top_p: req.top_p,
        num_predict: Some(req.max_tokens),
        stop: req.stop_sequences.clone(),
    };
    // Return None only when everything is default to keep the payload clean.
    if opts.temperature.is_none()
        && opts.top_p.is_none()
        && opts.num_predict == Some(0)
        && opts.stop.is_empty()
    {
        return None;
    }
    Some(opts)
}

/// Cheap pseudo-unique id using timestamp + a counter — avoids pulling uuid
/// into every response path.
fn uuid_v4_simple() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static CTR: AtomicU64 = AtomicU64::new(0);
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let n = CTR.fetch_add(1, Ordering::Relaxed);
    format!("{ts:x}-{n:x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn simple_req() -> spp::CompleteRequest {
        spp::CompleteRequest::text("llama3.2", "hello", 64)
    }

    #[test]
    fn text_message_translates_to_user_role() {
        let req = simple_req();
        let body = request_to_ollama(&req, false);
        assert_eq!(body.model, "llama3.2");
        assert!(!body.stream);
        // One user message, no system.
        assert_eq!(body.messages.len(), 1);
        assert_eq!(body.messages[0].role, "user");
        let content = body.messages[0].content.as_ref().unwrap();
        assert_eq!(content, &json!("hello"));
    }

    #[test]
    fn system_prompt_becomes_first_message() {
        let req = spp::CompleteRequest {
            model: "llama3.2".into(),
            messages: vec![spp::Message {
                role: spp::Role::User,
                content: vec![spp::ContentBlock::Text { text: "hi".into() }],
            }],
            system: Some("You are helpful.".into()),
            tools: vec![],
            temperature: None,
            top_p: None,
            max_tokens: 32,
            stop_sequences: vec![],
            stream: false,
            thinking: None,
            metadata: None,
        };
        let body = request_to_ollama(&req, false);
        assert_eq!(body.messages[0].role, "system");
        assert_eq!(body.messages[1].role, "user");
    }

    #[test]
    fn assistant_turn_preserves_text() {
        let req = spp::CompleteRequest {
            model: "x".into(),
            messages: vec![spp::Message {
                role: spp::Role::Assistant,
                content: vec![spp::ContentBlock::Text { text: "sure".into() }],
            }],
            system: None,
            tools: vec![],
            temperature: None,
            top_p: None,
            max_tokens: 16,
            stop_sequences: vec![],
            stream: false,
            thinking: None,
            metadata: None,
        };
        let body = request_to_ollama(&req, false);
        assert_eq!(body.messages[0].role, "assistant");
        assert_eq!(body.messages[0].content, Some(json!("sure")));
    }

    #[test]
    fn tool_def_becomes_function_tool() {
        use savvagent_protocol::ToolDef;
        let req = spp::CompleteRequest {
            model: "llama3.1".into(),
            messages: vec![spp::Message {
                role: spp::Role::User,
                content: vec![spp::ContentBlock::Text { text: "call ls".into() }],
            }],
            system: None,
            tools: vec![ToolDef {
                name: "ls".into(),
                description: "list dir".into(),
                input_schema: json!({ "type": "object", "properties": { "path": { "type": "string" } } }),
            }],
            temperature: None,
            top_p: None,
            max_tokens: 32,
            stop_sequences: vec![],
            stream: false,
            thinking: None,
            metadata: None,
        };
        let body = request_to_ollama(&req, false);
        assert_eq!(body.tools.len(), 1);
        assert_eq!(body.tools[0].kind, "function");
        assert_eq!(body.tools[0].function.name, "ls");
    }

    #[test]
    fn tool_result_with_tool_support_uses_tool_role() {
        use savvagent_protocol::ToolDef;
        let req = spp::CompleteRequest {
            model: "llama3.1".into(),
            messages: vec![
                spp::Message {
                    role: spp::Role::Assistant,
                    content: vec![spp::ContentBlock::ToolUse {
                        id: "call-1".into(),
                        name: "ls".into(),
                        input: json!({"path": "/tmp"}),
                    }],
                },
                spp::Message {
                    role: spp::Role::User,
                    content: vec![spp::ContentBlock::ToolResult {
                        tool_use_id: "call-1".into(),
                        content: vec![spp::ContentBlock::Text {
                            text: "a\nb".into(),
                        }],
                        is_error: false,
                    }],
                },
            ],
            system: None,
            tools: vec![ToolDef {
                name: "ls".into(),
                description: "list".into(),
                input_schema: json!({}),
            }],
            temperature: None,
            top_p: None,
            max_tokens: 32,
            stop_sequences: vec![],
            stream: false,
            thinking: None,
            metadata: None,
        };
        let body = request_to_ollama(&req, false);
        // assistant + tool result
        let tool_msg = body.messages.iter().find(|m| m.role == "tool").unwrap();
        let content = tool_msg.content.as_ref().unwrap();
        assert_eq!(content["tool_call_id"], "call-1");
        assert_eq!(content["content"], "a\nb");
    }

    #[test]
    fn tool_result_without_tool_support_uses_user_role() {
        // When no tools are defined, tool results should be sent as user messages.
        let req = spp::CompleteRequest {
            model: "llama3.2".into(),
            messages: vec![spp::Message {
                role: spp::Role::User,
                content: vec![spp::ContentBlock::ToolResult {
                    tool_use_id: "call-1".into(),
                    content: vec![spp::ContentBlock::Text {
                        text: "result".into(),
                    }],
                    is_error: false,
                }],
            }],
            system: None,
            tools: vec![],
            temperature: None,
            top_p: None,
            max_tokens: 32,
            stop_sequences: vec![],
            stream: false,
            thinking: None,
            metadata: None,
        };
        let body = request_to_ollama(&req, false);
        assert!(body.messages.iter().any(|m| m.role == "user"));
        assert!(!body.messages.iter().any(|m| m.role == "tool"));
    }

    #[test]
    fn response_from_ollama_parses_text() {
        let raw = serde_json::from_value::<crate::api::ChatResponse>(json!({
            "model": "llama3.2",
            "message": { "role": "assistant", "content": "hello back" },
            "done": true,
            "done_reason": "stop",
            "prompt_eval_count": 5,
            "eval_count": 2
        }))
        .unwrap();
        let resp = response_from_ollama(raw);
        assert_eq!(resp.model, "llama3.2");
        assert_eq!(resp.stop_reason, spp::StopReason::EndTurn);
        assert_eq!(resp.usage.input_tokens, 5);
        assert_eq!(resp.usage.output_tokens, 2);
        match &resp.content[0] {
            spp::ContentBlock::Text { text } => assert_eq!(text, "hello back"),
            _ => panic!("expected text"),
        }
    }

    #[test]
    fn response_from_ollama_parses_tool_call() {
        let raw = serde_json::from_value::<crate::api::ChatResponse>(json!({
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
        }))
        .unwrap();
        let resp = response_from_ollama(raw);
        match &resp.content[0] {
            spp::ContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "tc-1");
                assert_eq!(name, "ls");
                assert_eq!(input["path"], "/tmp");
            }
            _ => panic!("expected tool_use"),
        }
        assert_eq!(resp.stop_reason, spp::StopReason::ToolUse);
    }

    #[test]
    fn options_include_max_tokens() {
        let req = spp::CompleteRequest {
            model: "llama3.2".into(),
            messages: vec![spp::Message {
                role: spp::Role::User,
                content: vec![spp::ContentBlock::Text {
                    text: "hi".into(),
                }],
            }],
            system: None,
            tools: vec![],
            temperature: Some(0.7),
            top_p: None,
            max_tokens: 256,
            stop_sequences: vec![],
            stream: false,
            thinking: None,
            metadata: None,
        };
        let body = request_to_ollama(&req, false);
        let opts = body.options.unwrap();
        assert_eq!(opts.num_predict, Some(256));
        assert_eq!(opts.temperature, Some(0.7));
    }
}
