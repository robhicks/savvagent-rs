//! Provider catalog.
//!
//! Each entry describes one supported LLM provider and carries a function
//! pointer that builds an in-process [`ProviderHandler`] for it given an API
//! key (or an empty string for keyless providers). The TUI links every
//! provider crate as a library; nothing is spawned.
//!
//! Adding a new provider is a one-entry change — implement `ProviderHandler`
//! in a new crate, expose a builder, and append to [`PROVIDERS`].

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use anyhow::Result;
use savvagent_mcp::ProviderHandler;

/// Future returned by a provider's connect-time readiness probe.
pub type HealthCheckFuture = Pin<Box<dyn Future<Output = Result<()>> + Send>>;
/// Function pointer to a provider's connect-time readiness probe.
pub type HealthCheckFn = fn() -> HealthCheckFuture;

/// Static metadata + factory for one provider.
#[derive(Clone, Copy)]
pub struct ProviderSpec {
    /// Stable identifier — keyring account name and `/connect` selector key.
    pub id: &'static str,
    /// Pretty name shown in the selector.
    pub display_name: &'static str,
    /// The env var the underlying SDK conventionally reads. Used only as a
    /// hint in the API-key prompt; we never actually read or set it.
    /// For keyless providers (see [`api_key_required`]) this is the URL
    /// override env var instead.
    pub api_key_env: &'static str,
    /// Default model id passed to the host when this provider connects.
    pub default_model: &'static str,
    /// When `false`, the `/connect` flow skips the API-key prompt and the
    /// keyring read/write entirely, passing an empty string to `build`.
    pub api_key_required: bool,
    /// Build an in-process handler bound to `api_key`. For keyless providers
    /// `api_key` is always an empty string.
    ///
    /// Used by the provider plugins (`perform_connect` delegates to each
    /// plugin's `try_build_registration`). Retained here as the canonical
    /// source of the fn pointer; the plugin wrappers call it indirectly.
    #[allow(dead_code)]
    pub build: fn(api_key: &str) -> Result<Arc<dyn ProviderHandler>>,
    /// Optional connect-time readiness probe. Run after [`build`] and
    /// before swapping the host slot so reachability problems surface as
    /// "Connect failed" rather than a silent first-turn timeout. Returns
    /// `Ok(())` when the provider is reachable.
    #[allow(dead_code)]
    pub health_check: Option<HealthCheckFn>,
}

/// All providers the TUI offers in `/connect`.
pub const PROVIDERS: &[ProviderSpec] = &[
    ProviderSpec {
        id: "anthropic",
        display_name: "Anthropic (Claude)",
        api_key_env: "ANTHROPIC_API_KEY",
        default_model: "claude-haiku-4-5",
        api_key_required: true,
        build: build_anthropic,
        health_check: None,
    },
    ProviderSpec {
        id: "gemini",
        display_name: "Google Gemini",
        api_key_env: "GEMINI_API_KEY",
        default_model: "gemini-2.5-flash",
        api_key_required: true,
        build: build_gemini,
        health_check: None,
    },
    ProviderSpec {
        id: "openai",
        display_name: "OpenAI",
        api_key_env: "OPENAI_API_KEY",
        default_model: "gpt-4o-mini",
        api_key_required: true,
        build: build_openai,
        health_check: None,
    },
    ProviderSpec {
        id: "local",
        display_name: "Ollama (local)",
        api_key_env: "OLLAMA_HOST",
        default_model: "llama3.2",
        api_key_required: false,
        build: build_ollama,
        health_check: Some(check_ollama),
    },
];

fn build_anthropic(api_key: &str) -> Result<Arc<dyn ProviderHandler>> {
    let p = provider_anthropic::AnthropicProvider::builder()
        .api_key(api_key)
        .build()?;
    Ok(Arc::new(p))
}

fn build_gemini(api_key: &str) -> Result<Arc<dyn ProviderHandler>> {
    let p = provider_gemini::GeminiProvider::builder()
        .api_key(api_key)
        .build()?;
    Ok(Arc::new(p))
}

fn build_openai(api_key: &str) -> Result<Arc<dyn ProviderHandler>> {
    let p = provider_openai::OpenAiProvider::builder()
        .api_key(api_key)
        .build()?;
    Ok(Arc::new(p))
}

fn build_ollama(_api_key: &str) -> Result<Arc<dyn ProviderHandler>> {
    let p = provider_local::OllamaProvider::builder().build()?;
    Ok(Arc::new(p))
}

/// Probe Ollama's `/api/tags` endpoint with a short timeout so the user
/// gets an actionable error when `ollama serve` isn't running.
fn check_ollama() -> HealthCheckFuture {
    Box::pin(async {
        let p = provider_local::OllamaProvider::builder().build()?;
        p.ready().await.map_err(|e| anyhow::anyhow!(e.message))?;
        Ok(())
    })
}
