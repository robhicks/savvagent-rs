//! Provider catalog.
//!
//! Each entry describes one supported LLM provider. The TUI links every
//! provider crate as a library; nothing is spawned.
//!
//! Adding a new provider is a one-entry change — implement `ProviderHandler`
//! in a new crate, expose a builder, and append to [`PROVIDERS`].

/// Static metadata for one provider.
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
    /// keyring read/write entirely.
    pub api_key_required: bool,
}

/// All providers the TUI offers in `/connect`.
pub const PROVIDERS: &[ProviderSpec] = &[
    ProviderSpec {
        id: "anthropic",
        display_name: "Anthropic (Claude)",
        api_key_env: "ANTHROPIC_API_KEY",
        default_model: "claude-haiku-4-5",
        api_key_required: true,
    },
    ProviderSpec {
        id: "gemini",
        display_name: "Google Gemini",
        api_key_env: "GEMINI_API_KEY",
        default_model: "gemini-2.5-flash",
        api_key_required: true,
    },
    ProviderSpec {
        id: "openai",
        display_name: "OpenAI",
        api_key_env: "OPENAI_API_KEY",
        default_model: "gpt-4o-mini",
        api_key_required: true,
    },
    ProviderSpec {
        id: "local",
        display_name: "Ollama (local)",
        api_key_env: "OLLAMA_HOST",
        default_model: "llama3.2",
        api_key_required: false,
    },
];
