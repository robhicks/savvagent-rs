//! Host configuration types.

use std::path::PathBuf;

/// Where the host should reach the LLM provider.
#[derive(Debug, Clone)]
pub enum ProviderEndpoint {
    /// An SPP provider running as an MCP Streamable HTTP server (e.g.
    /// `provider-anthropic`'s `savvagent-anthropic` binary listening on a
    /// loopback port).
    StreamableHttp {
        /// Full URL ending in the MCP path, e.g. `http://127.0.0.1:8787/mcp`.
        url: String,
    },
}

/// How to launch a tool MCP server.
#[derive(Debug, Clone)]
pub enum ToolEndpoint {
    /// Spawn `command` with `args` as a child process and speak MCP over its
    /// stdin/stdout pipes.
    Stdio {
        /// Path to the binary (anything `tokio::process::Command::new` accepts).
        command: PathBuf,
        /// Arguments forwarded verbatim.
        args: Vec<String>,
    },
}

/// Top-level host configuration.
#[derive(Debug, Clone)]
pub struct HostConfig {
    /// Provider to route `complete` calls to.
    pub provider: ProviderEndpoint,
    /// Tool MCP servers to spawn at startup.
    pub tools: Vec<ToolEndpoint>,
    /// Provider-specific model identifier (forwarded in every request).
    pub model: String,
    /// Hard cap on response tokens for each `complete` call.
    pub max_tokens: u32,
    /// Project root, used to locate `SAVVAGENT.md` and as default cwd context.
    pub project_root: PathBuf,
    /// Optional override for the system prompt. When `None`, the host
    /// auto-generates one from `SAVVAGENT.md` if it exists.
    pub system_prompt: Option<String>,
    /// Hard cap on tool-use iterations within a single turn. Guards against
    /// pathological loops.
    pub max_iterations: u32,
}

impl HostConfig {
    /// New config with sensible defaults: 4096 max tokens, 20 iteration cap,
    /// project root = current dir, no static tools, no system-prompt override.
    pub fn new(provider: ProviderEndpoint, model: impl Into<String>) -> Self {
        Self {
            provider,
            tools: Vec::new(),
            model: model.into(),
            max_tokens: 4096,
            project_root: PathBuf::from("."),
            system_prompt: None,
            max_iterations: 20,
        }
    }

    /// Add a tool endpoint to the config.
    pub fn with_tool(mut self, tool: ToolEndpoint) -> Self {
        self.tools.push(tool);
        self
    }

    /// Override the project root.
    pub fn with_project_root(mut self, path: impl Into<PathBuf>) -> Self {
        self.project_root = path.into();
        self
    }

    /// Override the system prompt.
    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    /// Override the max-tokens cap.
    pub fn with_max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = n;
        self
    }

    /// Override the iteration cap.
    pub fn with_max_iterations(mut self, n: u32) -> Self {
        self.max_iterations = n;
        self
    }
}
