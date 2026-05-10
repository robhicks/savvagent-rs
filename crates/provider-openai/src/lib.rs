//! OpenAI Chat Completions API as a Savvagent SPP [`ProviderHandler`].
//!
//! Crate layout:
//!
//! - [`api`] — typed mirror of the relevant subset of OpenAI's
//!   `POST /v1/chat/completions` request/response shapes.
//! - [`translate`] — pure functions converting between SPP and
//!   [`api`] types.
//! - [`stream`] — OpenAI SSE → SPP [`StreamEvent`](savvagent_protocol::StreamEvent)
//!   adapter.
//! - [`mcp`] — [`ProviderHandler`] MCP server wrapper.
//! - [`OpenAiProvider`] — [`ProviderHandler`] impl that wires the pieces
//!   together over an HTTP client.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod api;
pub mod mcp;
pub mod stream;
pub mod translate;

pub use mcp::OpenAiMcpServer;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use savvagent_mcp::{ProviderHandler, StreamEmitter};
use savvagent_protocol::{
    CompleteRequest, CompleteResponse, ErrorKind, ProviderError, StreamEvent,
};

/// Default OpenAI API base URL. Override via [`OpenAiProviderBuilder::base_url`]
/// to point at a proxy, a local mock, or an OpenAI-compatible endpoint.
pub const DEFAULT_BASE_URL: &str = "https://api.openai.com";

/// Chat Completions endpoint path.
pub const CHAT_COMPLETIONS_PATH: &str = "/v1/chat/completions";

/// Default model when none is specified by the host.
pub const DEFAULT_MODEL: &str = "gpt-4o-mini";

/// SPP provider backed by OpenAI's Chat Completions endpoint.
pub struct OpenAiProvider {
    http: reqwest::Client,
    api_key: String,
    base_url: String,
}

/// Builder for [`OpenAiProvider`]. Use [`OpenAiProvider::builder`].
pub struct OpenAiProviderBuilder {
    api_key: Option<String>,
    base_url: String,
    timeout: Duration,
}

impl OpenAiProvider {
    /// Start configuring an [`OpenAiProvider`].
    pub fn builder() -> OpenAiProviderBuilder {
        OpenAiProviderBuilder {
            api_key: None,
            base_url: DEFAULT_BASE_URL.to_string(),
            timeout: Duration::from_secs(120),
        }
    }
}

impl OpenAiProviderBuilder {
    /// Set the API key. If unset, [`build`](Self::build) reads `OPENAI_API_KEY`
    /// from the environment.
    pub fn api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }

    /// Override the API base URL (no trailing slash). Defaults to
    /// [`DEFAULT_BASE_URL`]. Set this to point at OpenAI-compatible endpoints
    /// such as Azure OpenAI, Ollama, or a local test server.
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    /// Override the HTTP request timeout. Default is 120 s.
    pub fn timeout(mut self, t: Duration) -> Self {
        self.timeout = t;
        self
    }

    /// Build the provider, validating that an API key is available.
    pub fn build(self) -> Result<OpenAiProvider, BuildError> {
        let api_key = self
            .api_key
            .or_else(|| std::env::var("OPENAI_API_KEY").ok())
            .filter(|k| !k.is_empty())
            .ok_or(BuildError::MissingApiKey)?;
        let http = reqwest::Client::builder()
            .timeout(self.timeout)
            .https_only(self.base_url.starts_with("https://"))
            .build()
            .map_err(|e| BuildError::HttpClient(e.to_string()))?;
        Ok(OpenAiProvider {
            http,
            api_key,
            base_url: self.base_url,
        })
    }
}

/// [`OpenAiProviderBuilder::build`] failures.
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    /// Neither the builder nor `OPENAI_API_KEY` provided an API key.
    #[error("OPENAI_API_KEY is not set and no api_key was provided")]
    MissingApiKey,
    /// The reqwest client failed to construct.
    #[error("failed to build HTTP client: {0}")]
    HttpClient(String),
}

#[async_trait]
impl ProviderHandler for OpenAiProvider {
    async fn complete(
        &self,
        req: CompleteRequest,
        emit: Option<&dyn StreamEmitter>,
    ) -> Result<CompleteResponse, ProviderError> {
        let want_stream = req.stream && emit.is_some();
        let body = translate::request_to_openai(&req, want_stream);
        let url = format!("{}{CHAT_COMPLETIONS_PATH}", self.base_url);

        tracing::trace!(%url, want_stream, "POSTing to OpenAI");
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(map_reqwest_error)?;
        tracing::trace!(status = %resp.status(), "OpenAI responded");

        if !resp.status().is_success() {
            return Err(parse_error_response(resp).await);
        }

        if want_stream {
            tracing::trace!("entering SSE consumer");
            let out = stream::consume_sse(resp, emit.unwrap()).await;
            tracing::trace!(ok = out.is_ok(), "SSE consumer returned");
            out
        } else {
            let raw: api::ChatCompletionResponse =
                resp.json().await.map_err(|e| ProviderError {
                    kind: ErrorKind::Internal,
                    message: format!("failed to parse response body: {e}"),
                    retry_after_ms: None,
                    provider_code: None,
                })?;
            Ok(translate::response_from_openai(raw))
        }
    }
}

fn map_reqwest_error(e: reqwest::Error) -> ProviderError {
    let kind = if e.is_timeout() || e.is_connect() {
        ErrorKind::Network
    } else {
        ErrorKind::Internal
    };
    ProviderError {
        kind,
        message: e.to_string(),
        retry_after_ms: None,
        provider_code: None,
    }
}

async fn parse_error_response(resp: reqwest::Response) -> ProviderError {
    let status = resp.status();
    let retry_after_ms = resp
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .map(|s| s * 1000);

    let kind = match status.as_u16() {
        400 => ErrorKind::InvalidRequest,
        401 => ErrorKind::Authentication,
        403 => ErrorKind::PermissionDenied,
        404 => ErrorKind::ModelNotFound,
        413 => ErrorKind::ContextLengthExceeded,
        429 => ErrorKind::RateLimited,
        500 | 502 | 503 | 504 => ErrorKind::Overloaded,
        _ => ErrorKind::Internal,
    };

    let body = resp.text().await.unwrap_or_default();
    let (message, provider_code) = if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
        let msg = v
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
            .map(String::from)
            .unwrap_or_else(|| body.clone());
        let code = v
            .get("error")
            .and_then(|e| e.get("code"))
            .and_then(|t| t.as_str())
            .map(String::from);
        (msg, code)
    } else {
        (body, None)
    };

    ProviderError {
        kind,
        message,
        retry_after_ms,
        provider_code,
    }
}

/// Default endpoint path the Streamable HTTP server is mounted at.
pub const DEFAULT_MCP_PATH: &str = "/mcp";

/// Build the `axum::Router` that serves [`OpenAiMcpServer`] over MCP
/// Streamable HTTP at [`DEFAULT_MCP_PATH`].
pub fn router(provider: Arc<OpenAiProvider>) -> axum::Router {
    let provider_for_factory = provider.clone();
    let service = StreamableHttpService::new(
        move || Ok(OpenAiMcpServer::from_shared(provider_for_factory.clone())),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    axum::Router::new().nest_service(DEFAULT_MCP_PATH, service)
}

/// Default bind address for the standalone `savvagent-openai` binary.
pub const DEFAULT_LISTEN: &str = "127.0.0.1:8789";

/// Run the standalone OpenAI MCP HTTP server. Reads `OPENAI_API_KEY`,
/// `SAVVAGENT_OPENAI_LISTEN`, and `OPENAI_BASE_URL` from the environment (a
/// `.env` file walking up from the CWD is honored).
pub async fn run() -> std::process::ExitCode {
    use std::env;
    use std::process::ExitCode;

    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let listen = env::var("SAVVAGENT_OPENAI_LISTEN").unwrap_or_else(|_| DEFAULT_LISTEN.to_string());
    let base_url = env::var("OPENAI_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());

    let provider = match OpenAiProvider::builder().base_url(base_url).build() {
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
        "savvagent-openai {} listening on http://{local}{DEFAULT_MCP_PATH}",
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

/// Build an [`OpenAiProvider`] aimed at a local mock server; for tests.
#[doc(hidden)]
pub fn provider_for_tests(base_url: impl Into<String>) -> OpenAiProvider {
    OpenAiProvider::builder()
        .api_key("test-key")
        .base_url(base_url)
        .build()
        .expect("test provider should build")
}

#[doc(hidden)]
pub fn _events_phantom(_: StreamEvent) {}
