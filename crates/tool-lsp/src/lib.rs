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

use rmcp::{
    ServerHandler, ServiceExt,
    model::{Implementation, ProtocolVersion, ServerCapabilities, ServerInfo},
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

    let server = LspServer;
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

/// rmcp `ServerHandler` for tool-lsp. Currently advertises zero tools;
/// the tool surface is added incrementally in later tasks.
#[derive(Clone, Default)]
pub struct LspServer;

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
