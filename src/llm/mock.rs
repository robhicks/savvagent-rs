use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;
use crate::agent::{Message, Role, ToolCall};
use super::LlmClient;

pub struct MockLlmClient;

#[async_trait]
impl LlmClient for MockLlmClient {
    async fn completion(&self, messages: &[Message], _tools: &[serde_json::Value]) -> Result<Message> {
        tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
        
        let last_message = messages.last().unwrap();
        if last_message.role == Role::User && last_message.content.contains("read") {
            Ok(Message {
                role: Role::Assistant,
                content: "I'll read that file for you.".to_string(),
                tool_calls: Some(vec![ToolCall {
                    id: "mock_call_1".to_string(),
                    name: "read_file".to_string(),
                    arguments: json!({"path": "Cargo.toml"}).to_string(),
                }]),
                tool_call_id: None,
            })
        } else if last_message.role == Role::Tool {
            Ok(Message {
                role: Role::Assistant,
                content: "I've read the file. It looks like a standard Rust project.".to_string(),
                tool_calls: None,
                tool_call_id: None,
            })
        } else {
            Ok(Message {
                role: Role::Assistant,
                content: "I'm here to help!".to_string(),
                tool_calls: None,
                tool_call_id: None,
            })
        }
    }
}
