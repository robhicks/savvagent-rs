//! Provider catalog.
//!
//! Each entry describes one supported LLM provider and carries a function
//! pointer that builds an in-process [`ProviderHandler`] for it given an API
//! key. The TUI links every provider crate as a library; nothing is spawned.
//!
//! Adding a new provider is a one-entry change — implement `ProviderHandler`
//! in a new crate, expose a builder, and append to [`PROVIDERS`].

use std::sync::Arc;

use anyhow::Result;
use savvagent_mcp::ProviderHandler;

/// Static metadata + factory for one provider.
#[derive(Clone, Copy)]
pub struct ProviderSpec {
    /// Stable identifier — keyring account name and `/connect` selector key.
    pub id: &'static str,
    /// Pretty name shown in the selector.
    pub display_name: &'static str,
    /// The env var the underlying SDK conventionally reads. Used only as a
    /// hint in the API-key prompt; we never actually read or set it.
    pub api_key_env: &'static str,
    /// Default model id passed to the host when this provider connects.
    pub default_model: &'static str,
    /// Build an in-process handler bound to `api_key`.
    pub build: fn(api_key: &str) -> Result<Arc<dyn ProviderHandler>>,
}

/// All providers the TUI offers in `/connect`.
pub const PROVIDERS: &[ProviderSpec] = &[
    ProviderSpec {
        id: "anthropic",
        display_name: "Anthropic (Claude)",
        api_key_env: "ANTHROPIC_API_KEY",
        default_model: "claude-haiku-4-5",
        build: build_anthropic,
    },
    ProviderSpec {
        id: "gemini",
        display_name: "Google Gemini",
        api_key_env: "GEMINI_API_KEY",
        default_model: "gemini-1.5-flash",
        build: build_gemini,
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
