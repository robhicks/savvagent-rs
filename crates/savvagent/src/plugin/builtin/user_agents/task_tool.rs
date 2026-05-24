//! `task` in-process tool handler. Implemented in Task 20.

use std::sync::Arc;

use async_trait::async_trait;
use savvagent_plugin::{InProcessToolHandler, InProcessToolHandlerArc};
use savvagent_protocol::ToolDef;
use serde_json::Value;

use crate::plugin::builtin::user_agents::index::AgentIndex;

pub struct TaskToolHandler {
    _index: AgentIndex,
}

impl TaskToolHandler {
    pub fn new(index: AgentIndex) -> Self {
        Self { _index: index }
    }
}

#[async_trait]
impl InProcessToolHandler for TaskToolHandler {
    async fn call(
        &self,
        _input: Value,
        _ctx: Arc<dyn std::any::Any + Send + Sync>,
    ) -> Result<Value, String> {
        Err("task tool not yet implemented".into())
    }
}

pub async fn build_tool_def(index: &AgentIndex) -> ToolDef {
    let names = index.names_snapshot().await;
    ToolDef {
        name: "task".into(),
        description:
            "Spawn a subagent to handle a focused task. Returns the subagent's final response."
                .into(),
        input_schema: serde_json::json!({
            "type": "object",
            "required": ["description", "prompt", "subagent_type"],
            "properties": {
                "description": { "type": "string", "description": "3-5 word task label" },
                "prompt": { "type": "string", "description": "The task for the subagent" },
                "subagent_type": { "type": "string", "enum": names }
            }
        }),
    }
}

pub fn handler_arc(index: AgentIndex) -> InProcessToolHandlerArc {
    InProcessToolHandlerArc::new(TaskToolHandler::new(index))
}
