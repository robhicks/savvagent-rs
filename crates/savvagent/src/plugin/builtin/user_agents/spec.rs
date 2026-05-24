//! `AgentSpec` — parsed representation of one agent definition file.

use std::collections::HashSet;

#[derive(Debug, Clone)]
#[allow(dead_code)] // Fields populated by Task 16 (frontmatter) + Task 17 (body).
pub struct AgentSpec {
    pub name: String,
    pub description: String,
    pub tools: ToolsScope,
    pub model: Option<String>,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // Consumed by ScopedToolRegistry filter in Task 20.
pub enum ToolsScope {
    /// `tools:` key absent — inherit parent's full tool set.
    Inherit,
    /// `tools: []` — only the `task` tool available.
    Empty,
    /// Explicit allowlist.
    Allowed(HashSet<String>),
}
