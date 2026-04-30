use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde_json::json;
use crate::agent::{Message, Role, ToolCall};
use super::LlmClient;
use std::env;

pub struct GeminiClient {
    api_key: String,
    model: String,
    client: reqwest::Client,
}

impl GeminiClient {
    pub fn new() -> Self {
        let api_key = env::var("GEMINI_API_KEY").unwrap_or_default();
        Self {
            api_key,
            model: "gemini-1.5-flash".to_string(),
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl LlmClient for GeminiClient {
    async fn completion(&self, messages: &[Message], tools: &[serde_json::Value]) -> Result<Message> {
        if self.api_key.is_empty() {
            return Err(anyhow!("GEMINI_API_KEY not set"));
        }

        let mut contents = Vec::new();
        for m in messages {
            let role = match m.role {
                Role::User => "user",
                Role::Assistant => "model",
                Role::Tool => "function_response",
            };

            let mut parts = Vec::new();
            
            if role == "function_response" {
                // Gemini function response format
                parts.push(json!({
                    "functionResponse": {
                        "name": messages.iter().find(|msg| msg.tool_call_id == m.tool_call_id && msg.role == Role::Assistant)
                            .and_then(|msg| msg.tool_calls.as_ref())
                            .and_then(|calls| calls.iter().find(|c| Some(c.id.clone()) == m.tool_call_id))
                            .map(|c| c.name.clone())
                            .unwrap_or_else(|| "unknown".to_string()),
                        "response": { "result": m.content }
                    }
                }));
            } else {
                if !m.content.is_empty() {
                    parts.push(json!({ "text": m.content }));
                }

                if let Some(calls) = &m.tool_calls {
                    for c in calls {
                        let args: serde_json::Value = serde_json::from_str(&c.arguments).unwrap_or(json!({}));
                        parts.push(json!({
                            "functionCall": {
                                "name": c.name,
                                "args": args
                            }
                        }));
                    }
                }
            }

            contents.push(json!({
                "role": if role == "function_response" { "user" } else { role }, // Gemini wants tool results as 'user' role but with functionResponse part
                "parts": parts
            }));
        }

        let mut body = json!({
            "contents": contents,
        });

        if !tools.is_empty() {
            // Convert OpenAI-style tool definitions to Gemini function declarations
            let function_declarations: Vec<_> = tools.iter().map(|t| {
                let func = &t["function"];
                json!({
                    "name": func["name"],
                    "description": func["description"],
                    "parameters": func["parameters"]
                })
            }).collect();

            body.as_object_mut().unwrap().insert("tools".to_string(), json!([{
                "function_declarations": function_declarations
            }]));
        }

        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
            self.model, self.api_key
        );

        let res = self.client.post(url)
            .json(&body)
            .send()
            .await?;

        if !res.status().is_success() {
            let error = res.text().await?;
            return Err(anyhow!("Gemini API error: {}", error));
        }

        let response_json: serde_json::Value = res.json().await?;
        
        let candidate = &response_json["candidates"][0];
        let content_obj = &candidate["content"];
        let parts = content_obj["parts"].as_array().ok_or_else(|| anyhow!("No parts in Gemini response"))?;
        
        let mut content = String::new();
        let mut tool_calls = Vec::new();

        for part in parts {
            if let Some(text) = part["text"].as_str() {
                content.push_str(text);
            }
            if let Some(call) = part["functionCall"].as_object() {
                let name = call["name"].as_str().unwrap_or_default().to_string();
                let args = call["args"].to_string();
                tool_calls.push(ToolCall {
                    id: format!("call_{}", uuid::Uuid::new_v4().simple()),
                    name,
                    arguments: args,
                });
            }
        }

        Ok(Message {
            role: Role::Assistant,
            content,
            tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
            tool_call_id: None,
        })
    }
}
