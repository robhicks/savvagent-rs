//! Built-in plugin implementations. Each subdirectory hosts one plugin.

/// Clears the conversation log; registered as `/clear`.
pub mod clear;

/// Filterable slash-command picker; opened via `/` from the home view.
pub mod command_palette;

/// Provider connection picker; opened via `/connect` with no args.
pub mod connect;

/// Basic in-TUI file editor; opened via `/edit <path>`.
pub mod edit_file;

/// Footer widget: sandbox state + turn state + working dir + key reminder.
pub mod home_footer;

/// Tips widget: one-line hint above the prompt; switches text after Connect.
pub mod home_tips;

/// Cycles to the next model on the active provider; registered as `/model`.
pub mod model;

/// Transcript picker; opened via `/resume` with an in-memory cache backed
/// by the [`HookKind::TranscriptSaved`] hook.
pub mod resume;

/// Saves the active transcript to disk; registered as `/save [path]`.
pub mod save;

/// Startup HUD screen with connect status; responds to HostStarting + Connect.
pub mod splash;

/// Theme catalog + `/theme` slash + theme picker modal.
pub mod themes;

/// Fullscreen read-only file viewer; opened via `/view <path>`.
pub mod view_file;

/// Savvagent-internal [`provider_common::BuiltinProviderPlugin`] trait —
/// the explicit non-WIT-portable seam where `Box<dyn ProviderClient>` is
/// handed off from a provider plugin to the runtime.
pub mod provider_common;

/// Anthropic provider shim: keyring-backed `internal:provider-anthropic`.
pub mod provider_anthropic;

/// OpenAI provider shim: keyring-backed `internal:provider-openai`.
pub mod provider_openai;

/// Google Gemini provider shim: keyring-backed `internal:provider-gemini`.
pub mod provider_gemini;

/// Local (Ollama) provider shim: keyless `internal:provider-local`.
pub mod provider_local;
