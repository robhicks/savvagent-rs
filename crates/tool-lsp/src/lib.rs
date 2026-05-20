//! LSP-server bridge as a stdio MCP server.
//!
//! Wraps user-configured LSP servers (rust-analyzer, typescript-language-server,
//! pyright, gopls, …) behind a small MCP tool surface and publishes diagnostics
//! as MCP resources (`lsp://diagnostics/<absolute-path>`).
//!
//! Language servers are configured in `~/.savvagent/lsp.toml` (global) and
//! optionally overridden per repo at `<repo>/.savvagent/lsp.toml`. No
//! languages are hardcoded; see the README for example entries.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod config;
pub use config::{LanguageEntry, LspConfig};

mod language;
pub use language::{LanguageId, extension_of, workspace_root_for};

mod session;
pub use session::LspSession;

mod pool;
pub use pool::{IDLE_TIMEOUT, LspPool};

mod convert;
pub use convert::{DiagnosticOut, FileEditOut, LocationOut, PositionOut, RangeOut, TextEditOut};

mod resources;
mod tools;

use std::sync::Arc;

use rmcp::{
    ErrorData, ServerHandler, ServiceExt,
    handler::server::{
        router::tool::ToolRouter,
        wrapper::{Json, Parameters},
    },
    model::{Implementation, ProtocolVersion, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
    transport::stdio,
};

/// Entrypoint used by the `savvagent-tool-lsp` shim binary. Reads the
/// configured `lsp.toml` files, starts an rmcp stdio server, and serves
/// until stdin closes.
pub async fn run() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let server = LspServer::new()?;
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

/// rmcp `ServerHandler` for tool-lsp. Owns the shared configuration,
/// session pool, root, diagnostics callback, and the macro-generated
/// tool router that dispatches to per-tool modules in `tools/`.
pub struct LspServer {
    #[allow(dead_code)] // Read by tool dispatch handlers.
    config: Arc<config::LspConfig>,
    #[allow(dead_code)] // Read by tool dispatch handlers.
    pool: Arc<pool::LspPool>,
    #[allow(dead_code)] // Read by tool dispatch handlers.
    root: Arc<std::path::PathBuf>,
    /// Callback that fires after every publishDiagnostics arrives.
    /// Set by `resources::diagnostics` to publish MCP resource updates;
    /// stubbed to no-op until that module lands.
    #[allow(dead_code)] // Wired in Task 15.
    on_diagnostics: Arc<dyn Fn(&str) + Send + Sync>,
    #[allow(dead_code)] // Read by the `#[tool_handler]` macro expansion.
    tool_router: ToolRouter<Self>,
}

impl LspServer {
    /// Construct a new server: loads global + per-repo `lsp.toml`,
    /// pins the SAVVAGENT_TOOL_LSP_ROOT (defaulting to the process CWD),
    /// and initializes an empty session pool.
    pub fn new() -> anyhow::Result<Self> {
        let home = std::env::var("HOME").map(std::path::PathBuf::from).ok();
        let global = home
            .map(|h| h.join(".savvagent/lsp.toml"))
            .unwrap_or_else(|| std::path::PathBuf::from("/dev/null"));
        let cwd = std::env::current_dir()?;
        let repo = cwd.join(".savvagent/lsp.toml");
        let config = config::LspConfig::load(&global, Some(&repo))?;
        let root = std::env::var("SAVVAGENT_TOOL_LSP_ROOT")
            .map(std::path::PathBuf::from)
            .unwrap_or(cwd);
        Ok(Self {
            config: Arc::new(config),
            pool: Arc::new(pool::LspPool::default()),
            root: Arc::new(root),
            on_diagnostics: Arc::new(|_| {}),
            tool_router: Self::tool_router(),
        })
    }
}

#[tool_router]
impl LspServer {
    /// Jump to the definition of the symbol at the given position.
    #[tool(description = "Jump to the definition of the symbol at the given position.")]
    pub async fn lsp_definition(
        &self,
        Parameters(input): Parameters<tools::definition::LspDefinitionInput>,
    ) -> Result<Json<tools::definition::LspDefinitionOutput>, ErrorData> {
        tools::definition::dispatch(
            input,
            &self.config,
            &self.pool,
            &self.root,
            Arc::clone(&self.on_diagnostics),
        )
        .await
        .map(Json)
        .map_err(|e| ErrorData::internal_error(e.to_string(), None))
    }

    /// Find all references to the symbol at the given position.
    #[tool(description = "Find all references to the symbol at the given position.")]
    pub async fn lsp_references(
        &self,
        Parameters(input): Parameters<tools::references::LspReferencesInput>,
    ) -> Result<Json<tools::references::LspReferencesOutput>, ErrorData> {
        tools::references::dispatch(
            input,
            &self.config,
            &self.pool,
            &self.root,
            Arc::clone(&self.on_diagnostics),
        )
        .await
        .map(Json)
        .map_err(|e| ErrorData::internal_error(e.to_string(), None))
    }

    /// Get hover information for the symbol at the given position.
    #[tool(description = "Get hover information for the symbol at the given position.")]
    pub async fn lsp_hover(
        &self,
        Parameters(input): Parameters<tools::hover::LspHoverInput>,
    ) -> Result<Json<tools::hover::LspHoverOutput>, ErrorData> {
        tools::hover::dispatch(
            input,
            &self.config,
            &self.pool,
            &self.root,
            Arc::clone(&self.on_diagnostics),
        )
        .await
        .map(Json)
        .map_err(|e| ErrorData::internal_error(e.to_string(), None))
    }
}

#[tool_handler]
impl ServerHandler for LspServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_protocol_version(ProtocolVersion::default())
        .with_server_info(Implementation::new(
            env!("CARGO_PKG_NAME"),
            env!("CARGO_PKG_VERSION"),
        ))
    }
}
