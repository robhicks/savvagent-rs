//! Ollama HTTP API as a Savvagent SPP [`ProviderHandler`].
//!
//! Crate layout:
//!
//! - [`api`] — typed subset of Ollama's `/api/chat` request/response shapes.
//! - [`translate`] — pure functions converting between SPP and [`api`] types.
//! - [`stream`] — Ollama NDJSON stream → SPP [`StreamEvent`] adapter.
//!
//! # Example
//!
//! ```no_run
//! use provider_local::OllamaProvider;
//!
//! let provider = OllamaProvider::builder()
//!     .base_url("http://localhost:11434")
//!     .build()
//!     .expect("failed to build Ollama provider");
//! ```
//!
//! The provider is keyless — no API key is required. The `OLLAMA_HOST` env
//! var overrides the base URL at runtime (Ollama's own convention).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod api;
pub mod stream;
pub mod translate;

use std::time::Duration;

use async_trait::async_trait;
use savvagent_mcp::{ProviderHandler, StreamEmitter};
use savvagent_protocol::{CompleteRequest, CompleteResponse, ErrorKind, ProviderError, StreamEvent};

/// Default Ollama base URL. Override via [`OllamaProviderBuilder::base_url`]
/// or the `OLLAMA_HOST` environment variable.
pub const DEFAULT_BASE_URL: &str = "http://localhost:11434";

/// Default model used when none is specified in the request.
pub const DEFAULT_MODEL: &str = "llama3.2";

/// SPP provider backed by Ollama's `/api/chat` endpoint.
pub struct OllamaProvider {
    http: reqwest::Client,
    base_url: String,
}

/// Builder for [`OllamaProvider`]. Use [`OllamaProvider::builder`].
pub struct OllamaProviderBuilder {
    base_url: String,
    timeout: Duration,
}

impl OllamaProvider {
    /// Start configuring an [`OllamaProvider`].
    pub fn builder() -> OllamaProviderBuilder {
        OllamaProviderBuilder {
            base_url: DEFAULT_BASE_URL.to_string(),
            timeout: Duration::from_secs(300),
        }
    }
}

impl OllamaProviderBuilder {
    /// Override the Ollama base URL (no trailing slash). If unset,
    /// [`build`](Self::build) reads `OLLAMA_HOST` from the environment,
    /// falling back to `http://localhost:11434`.
    pub fn base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    /// Override the HTTP request timeout. Default is 300 s (local inference
    /// can be slow on first load).
    pub fn timeout(mut self, t: Duration) -> Self {
        self.timeout = t;
        self
    }

    /// Build the provider. Reads `OLLAMA_HOST` from the environment when no
    /// explicit base URL was set via [`base_url`](Self::base_url).
    pub fn build(mut self) -> Result<OllamaProvider, BuildError> {
        // Honor OLLAMA_HOST if the caller did not set an explicit URL. Ollama's
        // own tooling uses this env var.
        if self.base_url == DEFAULT_BASE_URL {
            if let Ok(host) = std::env::var("OLLAMA_HOST") {
                if !host.is_empty() {
                    self.base_url = host;
                }
            }
        }
        let http = reqwest::Client::builder()
            .timeout(self.timeout)
            .build()
            .map_err(|e| BuildError::HttpClient(e.to_string()))?;
        Ok(OllamaProvider {
            http,
            base_url: self.base_url,
        })
    }
}

/// [`OllamaProviderBuilder::build`] failures.
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    /// The reqwest client failed to construct.
    #[error("failed to build HTTP client: {0}")]
    HttpClient(String),
}

#[async_trait]
impl ProviderHandler for OllamaProvider {
    async fn complete(
        &self,
        req: CompleteRequest,
        emit: Option<&dyn StreamEmitter>,
    ) -> Result<CompleteResponse, ProviderError> {
        let want_stream = req.stream && emit.is_some();
        let body = translate::request_to_ollama(&req, want_stream);
        let url = format!("{}/api/chat", self.base_url);

        tracing::trace!(%url, want_stream, model = %req.model, "POSTing to Ollama");
        let resp = self
            .http
            .post(&url)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(map_reqwest_error)?;
        tracing::trace!(status = %resp.status(), "Ollama responded");

        if !resp.status().is_success() {
            return Err(parse_error_response(resp).await);
        }

        if want_stream {
            tracing::trace!("entering NDJSON consumer");
            let out = stream::consume_ndjson(resp, emit.unwrap()).await;
            tracing::trace!(ok = out.is_ok(), "NDJSON consumer returned");
            out
        } else {
            let raw: api::ChatResponse = resp.json().await.map_err(|e| ProviderError {
                kind: ErrorKind::Internal,
                message: format!("failed to parse response body: {e}"),
                retry_after_ms: None,
                provider_code: None,
            })?;
            Ok(translate::response_from_ollama(raw))
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
    let kind = match status.as_u16() {
        400 => ErrorKind::InvalidRequest,
        401 => ErrorKind::Authentication,
        403 => ErrorKind::PermissionDenied,
        404 => ErrorKind::ModelNotFound,
        429 => ErrorKind::RateLimited,
        500 | 502 | 503 | 504 => ErrorKind::Overloaded,
        _ => ErrorKind::Internal,
    };
    let body = resp.text().await.unwrap_or_default();
    // Ollama error shape: { "error": "message" }
    let message = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("error")?.as_str().map(String::from))
        .unwrap_or(body);
    ProviderError {
        kind,
        message,
        retry_after_ms: None,
        provider_code: None,
    }
}

/// Useful for tests: an [`OllamaProvider`] aimed at a local mock server.
#[doc(hidden)]
pub fn provider_for_tests(base_url: impl Into<String>) -> OllamaProvider {
    OllamaProvider::builder()
        .base_url(base_url)
        .build()
        .expect("test provider should build")
}

#[doc(hidden)]
pub fn _events_phantom(_: StreamEvent) {}
