use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde_json::json;
use crate::agent::{Message, Role, ToolCall};
use super::LlmClient;
use std::env;

pub struct OpenAIClient {
    api_key: String,
    model: String,
    client: reqwest::Client,
}

impl OpenAIClient {
    pub fn new() -> Self {
        let api_key = env::var("OPENAI_API_KEY").unwrap_or_default();
        Self {
            api_key,
            model: "gpt-4o".to_string(),
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl LlmClient for OpenAIClient {
    async fn completion(&self, messages: &[Message], tools: &[serde_json::Value]) -> Result<Message> {
        if self.api_key.is_empty() {
            return Err(anyhow!("OPENAI_API_KEY not set"));
        }

        let mut body = json!({
            "model": self.model,
            "messages": messages.iter().map(|m| {
                let mut map = serde_json::Map::new();
                map.insert("role".to_string(), match m.role {
                    Role::User => json!("user"),
                    Role::Assistant => json!("assistant"),
                    Role::Tool => json!("tool"),
                });
                map.insert("content".to_string(), json!(m.content));
                if let Some(id) = &m.tool_call_id {
                    map.insert("tool_call_id".to_string(), json!(id));
                }
                if let Some(calls) = &m.tool_calls {
                    map.insert("tool_calls".to_string(), json!(calls.iter().map(|c| {
                        json!({
                            "id": c.id,
                            "type": "function",
                            "function": {
                                "name": c.name,
                                "arguments": c.arguments,
                            }
                        })
                    }).collect::<Vec<_>>()));
                }
                json!(map)
            }).collect::<Vec<_>>(),
        });

        if !tools.is_empty() {
            body.as_object_mut().unwrap().insert("tools".to_string(), json!(tools));
        }

        let res = self.client.post("https://api.openai.com/v1/chat/completions")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .await?;

        if !res.status().is_success() {
            let error = res.text().await?;
            return Err(anyhow!("OpenAI API error: {}", error));
        }

        let response_json: serde_json::Value = res.json().await?;
        let choice = &response_json["choices"][0]["message"];
        
        let content = choice["content"].as_str().unwrap_or("").to_string();
        let tool_calls = choice["tool_calls"].as_array().map(|calls| {
            calls.iter().map(|c| {
                ToolCall {
                    id: c["id"].as_str().unwrap_or_default().to_string(),
                    name: c["function"]["name"].as_str().unwrap_or_default().to_string(),
                    arguments: c["function"]["arguments"].as_str().unwrap_or_default().to_string(),
                }
            }).collect()
        });

        Ok(Message {
            role: Role::Assistant,
            content,
            tool_calls,
            tool_call_id: None,
        })
    }
}
