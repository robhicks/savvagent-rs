use crate::agent::Tool;
use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;
use std::fs;

pub struct ReadFile;

#[async_trait]
impl Tool for ReadFile {
    fn name(&self) -> String {
        "read_file".to_string()
    }

    fn description(&self) -> String {
        "Read the contents of a file".to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The absolute path to the file to read"
                }
            },
            "required": ["path"]
        })
    }

    async fn call(&self, arguments: &str) -> Result<String> {
        let args: serde_json::Value = serde_json::from_str(arguments)?;
        let path = args["path"].as_str().ok_or_else(|| anyhow::anyhow!("Missing path argument"))?;
        let content = fs::read_to_string(path)?;
        Ok(content)
    }
}
