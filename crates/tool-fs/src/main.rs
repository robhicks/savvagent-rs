//! `savvagent-tool-fs` — Savvagent filesystem MCP server (stdio).
//!
//! The host spawns this binary and speaks MCP over its stdin/stdout pipes.
//! All logging goes to stderr so it cannot collide with the JSON-RPC frames on
//! stdout.

use rmcp::{ServiceExt, transport::stdio};
use tool_fs::FsTools;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    tracing::info!(
        "savvagent-tool-fs {} starting on stdio",
        env!("CARGO_PKG_VERSION")
    );

    let service = FsTools::new().serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
