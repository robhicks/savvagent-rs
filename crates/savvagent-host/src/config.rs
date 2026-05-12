//! Host configuration types.

use std::path::PathBuf;

use crate::permissions::PermissionPolicy;
use crate::sandbox::SandboxConfig;

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
    /// Permission policy override. `None` means the host builds a default
    /// policy ([`PermissionPolicy::default_for`]) from `project_root`.
    pub policy: Option<PermissionPolicy>,
    /// OS-level sandbox configuration for tool spawns (Layer 3).
    ///
    /// When `None`, the host loads `~/.savvagent/sandbox.toml` via
    /// [`SandboxConfig::load`]. Sandboxing is enabled by default on Linux and
    /// macOS as of v0.7; set the mode to [`SandboxMode::Off`] to disable, or
    /// run `/sandbox off` in the TUI.
    ///
    /// [`SandboxMode::Off`]: crate::SandboxMode::Off
    pub sandbox: Option<SandboxConfig>,
}

impl HostConfig {
    /// New config with sensible defaults: 4096 max tokens, 20 iteration cap,
    /// project root = current dir, no static tools, no system-prompt override,
    /// sandbox loaded from disk (`SandboxConfig::load`).
    pub fn new(provider: ProviderEndpoint, model: impl Into<String>) -> Self {
        Self {
            provider,
            tools: Vec::new(),
            model: model.into(),
            max_tokens: 4096,
            project_root: PathBuf::from("."),
            system_prompt: None,
            max_iterations: 20,
            policy: None,
            sandbox: None,
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

    /// Override the permission policy. When unset, the host builds
    /// [`PermissionPolicy::default_for(project_root)`] at startup.
    pub fn with_policy(mut self, policy: PermissionPolicy) -> Self {
        self.policy = Some(policy);
        self
    }

    /// Override the sandbox configuration. When unset, the host loads
    /// `~/.savvagent/sandbox.toml` at startup. Sandboxing is enabled by
    /// default on Linux and macOS as of v0.7.
    pub fn with_sandbox(mut self, sandbox: SandboxConfig) -> Self {
        self.sandbox = Some(sandbox);
        self
    }
}
