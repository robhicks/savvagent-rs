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
    /// Optional embedder-supplied system-prompt layer. Composed (in
    /// order) after the built-in default prompt and before the
    /// `SAVVAGENT.md` body via [`crate::project::layered_prompt`].
    /// When `None`, only the default and `SAVVAGENT.md` layers
    /// contribute. The default layer can itself be suppressed via
    /// [`Self::with_default_prompt_disabled`].
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

    /// When true (the default), `Host::start` builds and prepends a
    /// default system prompt that introduces Savvagent's identity,
    /// environment, and tool affordances. Disabling this suppresses
    /// **only the built-in default layer** — the [`Self::system_prompt`]
    /// override and the parsed `SAVVAGENT.md` body still compose. See
    /// [`Self::with_default_prompt_disabled`].
    pub default_prompt_enabled: bool,

    /// Embedder-supplied app version, surfaced in the default prompt's
    /// Environment section. When `None`, the prompt falls back to the
    /// `savvagent-host` crate version with an explicit "host crate"
    /// label. Stored as an owned `String` so embedders that compute
    /// the version at runtime (config file, plugin host wrapper) can
    /// pass it without lifetime acrobatics. See [`Self::with_app_version`].
    pub app_version: Option<String>,
}

impl HostConfig {
    /// New config with sensible defaults: 4096 max tokens, 20 iteration cap,
    /// project root = current dir, no static tools, no system-prompt override,
    /// sandbox loaded from disk (`SandboxConfig::load`), default system
    /// prompt enabled (see [`Self::default_prompt_enabled`]), no embedder
    /// app version (see [`Self::with_app_version`]).
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
            default_prompt_enabled: true,
            app_version: None,
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

    /// Disable the built-in default-prompt layer. Does NOT disable
    /// the [`Self::system_prompt`] override or `SAVVAGENT.md` body —
    /// those still compose if present. To suppress every layer:
    /// call this AND leave `system_prompt` unset AND point
    /// `project_root` at a directory with no `SAVVAGENT.md`.
    pub fn with_default_prompt_disabled(mut self) -> Self {
        self.default_prompt_enabled = false;
        self
    }

    /// Set the app version label rendered in the default prompt's
    /// Environment section. Accepts anything convertible to `String`
    /// (`&'static str` literal, `String`, `&str` over a runtime value)
    /// so embedders that compute the version at runtime can pass it
    /// directly. Pass `env!("CARGO_PKG_VERSION")` from the binary the
    /// user actually launched so the version matches what they
    /// installed.
    pub fn with_app_version(mut self, version: impl Into<String>) -> Self {
        self.app_version = Some(version.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> HostConfig {
        HostConfig::new(
            ProviderEndpoint::StreamableHttp {
                url: "http://localhost/mcp".into(),
            },
            "test-model",
        )
    }

    #[test]
    fn default_prompt_enabled_defaults_to_true() {
        assert!(cfg().default_prompt_enabled);
    }

    #[test]
    fn with_default_prompt_disabled_flips_flag() {
        let c = cfg().with_default_prompt_disabled();
        assert!(!c.default_prompt_enabled);
    }

    #[test]
    fn app_version_defaults_to_none() {
        assert!(cfg().app_version.is_none());
    }

    #[test]
    fn with_app_version_accepts_static_str_literal() {
        let c = cfg().with_app_version("9.9.9");
        assert_eq!(c.app_version.as_deref(), Some("9.9.9"));
    }

    #[test]
    fn with_app_version_accepts_runtime_owned_string() {
        // Embedders that read the version from a config file at runtime
        // must be able to pass a `String` — the builder takes
        // `impl Into<String>`, which accepts owned values without
        // requiring a `'static` lifetime.
        let runtime_value: String = format!("{}.{}.{}", 1, 2, 3);
        let c = cfg().with_app_version(runtime_value);
        assert_eq!(c.app_version.as_deref(), Some("1.2.3"));
    }
}
