use anyhow::Result;
use async_trait::async_trait;
use crate::agent::Message;

pub mod openai;
pub mod anthropic;
pub mod gemini;
pub mod mock;

pub use openai::OpenAIClient;
pub use anthropic::AnthropicClient;
pub use gemini::GeminiClient;
pub use mock::MockLlmClient;

#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn completion(&self, messages: &[Message], tools: &[serde_json::Value]) -> Result<Message>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmProvider {
    OpenAI,
    Anthropic,
    Gemini,
    Mock,
}

pub fn get_client(provider: LlmProvider) -> Box<dyn LlmClient> {
    match provider {
        LlmProvider::OpenAI => Box::new(OpenAIClient::new()),
        LlmProvider::Anthropic => Box::new(AnthropicClient::new()),
        LlmProvider::Gemini => Box::new(GeminiClient::new()),
        LlmProvider::Mock => Box::new(MockLlmClient),
    }
}
