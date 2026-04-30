use crate::llm::LlmClient;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use anyhow::Result;
use async_trait::async_trait;
use crate::tools::ReadFile;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Role {
    User,
    Assistant,
    Tool,
}


#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> String;
    fn description(&self) -> String;
    fn parameters(&self) -> serde_json::Value;
    async fn call(&self, arguments: &str) -> Result<String>;
}

pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.insert(tool.name(), tool);
    }

    pub async fn call(&self, name: &str, arguments: &str) -> Result<String> {
        let tool = self.tools.get(name).ok_or_else(|| anyhow::anyhow!("Tool not found"))?;
        tool.call(arguments).await
    }

    pub fn get_definitions(&self) -> Vec<serde_json::Value> {
        self.tools.values().map(|t| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": t.name(),
                    "description": t.description(),
                    "parameters": t.parameters(),
                }
            })
        }).collect()
    }
}

pub struct Agent {
    pub conversation: Vec<Message>,
    pub registry: ToolRegistry,
}

impl Agent {
    pub fn new() -> Self {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(ReadFile));
        
        Self {
            conversation: Vec::new(),
            registry,
        }
    }

    pub fn add_message(&mut self, message: Message) {
        self.conversation.push(message);
    }

    pub async fn step(&mut self, client: &dyn LlmClient) -> Result<Option<Message>> {
        let tools = self.registry.get_definitions();
        let response = client.completion(&self.conversation, &tools).await?;
        
        self.add_message(response.clone());
        
        if let Some(ref tool_calls) = response.tool_calls {
            for tc in tool_calls {
                let output = self.registry.call(&tc.name, &tc.arguments).await?;
                self.add_message(Message {
                    role: Role::Tool,
                    content: output,
                    tool_calls: None,
                    tool_call_id: Some(tc.id.clone()),
                });
            }
            // Return None to indicate we should continue the loop (step again)
            Ok(None)
        } else {
            // Return the final response
            Ok(Some(response))
        }
    }
}
