//! Typed subset of Ollama's `/api/chat` request and response shapes.
//!
//! Ollama follows an OpenAI-compatible chat-completion format for tool-capable
//! models. Field names use snake_case with `#[serde(rename_all = "snake_case")]`
//! so the on-the-wire representation matches Ollama's docs verbatim.

#![allow(missing_docs)]

use serde::{Deserialize, Serialize};

// ── Request ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    /// Enable streaming NDJSON responses.
    pub stream: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Tool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Options>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<serde_json::Value>,
    /// Tool calls emitted by the model.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
}

impl Message {
    pub fn text(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: Some(serde_json::Value::String(content.into())),
            tool_calls: Vec::new(),
        }
    }

    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        // Ollama uses role "tool" for tool results, with the content carrying
        // the result text. The tool_call_id is embedded in the content because
        // Ollama's chat format doesn't have a dedicated id field on tool-result
        // messages (unlike OpenAI). We pass it as structured JSON so the model
        // can correlate it.
        let id = tool_call_id.into();
        let text = content.into();
        Self {
            role: "tool".into(),
            content: Some(serde_json::json!({ "tool_call_id": id, "content": text })),
            tool_calls: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Tool {
    #[serde(rename = "type")]
    pub kind: String,
    pub function: FunctionDef,
}

#[derive(Debug, Clone, Serialize)]
pub struct FunctionDef {
    pub name: String,
    pub description: String,
    /// OpenAPI-style JSON Schema for the function parameters.
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Options {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num_predict: Option<u32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub stop: Vec<String>,
}

// ── Response ───────────────────────────────────────────────────────────────

/// Final (non-streaming) or last NDJSON chunk from Ollama.
#[derive(Debug, Clone, Deserialize)]
pub struct ChatResponse {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub message: Option<Message>,
    /// `true` when this is the final chunk in a streaming response.
    #[serde(default)]
    pub done: bool,
    #[serde(default)]
    pub done_reason: Option<String>,
    /// Total tokens in the prompt (only on the final chunk).
    #[serde(default)]
    pub prompt_eval_count: Option<u32>,
    /// Tokens generated (only on the final chunk).
    #[serde(default)]
    pub eval_count: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// Ollama uses `id` on tool_calls for models that support it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    /// Arguments as a JSON object.
    #[serde(default)]
    pub arguments: serde_json::Value,
}
