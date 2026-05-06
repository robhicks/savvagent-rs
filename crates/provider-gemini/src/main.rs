//! `savvagent-gemini` — SPP-conformant Gemini provider as an MCP
//! Streamable HTTP server.
//!
//! Configuration:
//! - `GEMINI_API_KEY` — required, forwarded to Gemini. `GOOGLE_API_KEY` is
//!   honored as a fallback. Loaded from the process environment, or from a
//!   `.env` file walking up from the current directory.
//! - `SAVVAGENT_GEMINI_LISTEN` — bind address (default `127.0.0.1:8788`).
//! - `GEMINI_BASE_URL` — override the upstream API base URL (mainly for
//!   testing; default `https://generativelanguage.googleapis.com`).

use std::env;
use std::process::ExitCode;
use std::sync::Arc;

use provider_gemini::{DEFAULT_BASE_URL, DEFAULT_MCP_PATH, GeminiProvider, router};

const DEFAULT_LISTEN: &str = "127.0.0.1:8788";

#[tokio::main]
async fn main() -> ExitCode {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let listen =
        env::var("SAVVAGENT_GEMINI_LISTEN").unwrap_or_else(|_| DEFAULT_LISTEN.to_string());
    let base_url = env::var("GEMINI_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());

    let provider = match GeminiProvider::builder().base_url(base_url).build() {
        Ok(p) => Arc::new(p),
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let app = router(provider);

    let listener = match tokio::net::TcpListener::bind(&listen).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error binding {listen}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let local = listener.local_addr().expect("local_addr");
    tracing::info!(
        "savvagent-gemini {} listening on http://{local}{DEFAULT_MCP_PATH}",
        env!("CARGO_PKG_VERSION")
    );

    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("ctrl-c received, shutting down");
    };
    if let Err(e) = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
    {
        eprintln!("server error: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
