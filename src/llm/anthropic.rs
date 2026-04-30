use anyhow::{Result, anyhow};
use async_trait::async_trait;
use crate::agent::Message;
use super::LlmClient;
use std::env;

pub struct AnthropicClient {
    #[allow(dead_code)]
    api_key: String,
}

impl AnthropicClient {
    pub fn new() -> Self {
        let api_key = env::var("ANTHROPIC_API_KEY").unwrap_or_default();
        Self {
            api_key,
        }
    }
}

#[async_trait]
impl LlmClient for AnthropicClient {
    async fn completion(&self, _messages: &[Message], _tools: &[serde_json::Value]) -> Result<Message> {
        Err(anyhow!("Anthropic client not yet fully implemented"))
    }
}
