//! `internal:provider-anthropic` — thin Anthropic shim.
//!
//! On `HostStarting` (and on `/connect anthropic`) the plugin attempts to
//! read an API key from the keyring; on success it constructs an
//! [`provider_anthropic::AnthropicProvider`], wraps it in
//! [`savvagent_mcp::InProcessProviderClient`] so the runtime receives a
//! `Box<dyn ProviderClient>`, and emits
//! [`savvagent_plugin::Effect::RegisterProvider`].
//!
//! No keyring entry → emit a [`savvagent_plugin::Effect::PushNote`] so the
//! user knows credentials are missing.

use std::sync::Arc;

use async_trait::async_trait;
use savvagent_mcp::{InProcessProviderClient, ProviderClient};
use savvagent_plugin::{
    Contributions, Effect, HookKind, HostEvent, Manifest, Plugin, PluginError, PluginId,
    PluginKind, ProviderId, ProviderSpec, Region, SlashSpec, SlotSpec, StyledLine, StyledSpan,
    TextMods, ThemeColor,
};

use super::provider_common::BuiltinProviderPlugin;

/// Provider plugin id (used by `apply_effects` to look up the right shim
/// when a [`Effect::RegisterProvider`] arrives).
const PLUGIN_ID: &str = "internal:provider-anthropic";
/// Provider id this plugin registers.
const PROVIDER_ID: &str = "anthropic";
/// Human-readable label.
const DISPLAY_NAME: &str = "Anthropic";

/// Anthropic provider shim.
pub(crate) struct ProviderAnthropicPlugin {
    client: Option<Box<dyn ProviderClient>>,
}

impl ProviderAnthropicPlugin {
    /// Construct a new shim with no client yet.
    pub(crate) fn new() -> Self {
        Self { client: None }
    }

    /// Try to read the API key from the keyring and, on success, build an
    /// in-process [`ProviderClient`]. Returns `Some(())` when a client was
    /// installed, `None` when credentials were unavailable.
    ///
    /// Keyring backend errors (Secret Service daemon down, locked macOS
    /// Keychain, etc.) and provider client `BuildError`s (TLS init failure,
    /// broken proxy env vars, fd exhaustion) are surfaced via `tracing` so
    /// the user can distinguish "no credentials" from "credentials present
    /// but plumbing failed."
    fn try_connect_from_keyring(&mut self) -> Option<()> {
        if self.client.is_some() {
            // Already connected; nothing to do.
            return Some(());
        }
        let key = match crate::creds::load(PROVIDER_ID) {
            Ok(Some(k)) => k,
            Ok(None) => return None,
            Err(e) => {
                tracing::warn!(provider = PROVIDER_ID, error = %e,
                    "keyring read failed; treating as missing credentials");
                return None;
            }
        };
        let provider = match provider_anthropic::AnthropicProvider::builder()
            .api_key(&key)
            .build()
        {
            Ok(p) => p,
            Err(e) => {
                tracing::error!(provider = PROVIDER_ID, error = %e,
                    "provider client build failed despite credentials present");
                return None;
            }
        };
        let client: Box<dyn ProviderClient> =
            Box::new(InProcessProviderClient::new(Arc::new(provider)));
        self.client = Some(client);
        Some(())
    }

    /// Test-only helper that pre-installs a stub client without going
    /// through the keyring. Used by the end-to-end registry-wiring test
    /// to exercise the slash → register → take chain without touching
    /// the user's credential store.
    #[cfg(test)]
    pub(crate) fn with_test_client(client: Box<dyn ProviderClient>) -> Self {
        Self {
            client: Some(client),
        }
    }
}

impl Default for ProviderAnthropicPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for ProviderAnthropicPlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.providers = vec![ProviderSpec {
            id: ProviderId::new(PROVIDER_ID).expect("valid provider id"),
            display_name: DISPLAY_NAME.into(),
            requires_credential: true,
            in_process: true,
        }];
        contributions.slash_commands = vec![SlashSpec {
            name: format!("connect {PROVIDER_ID}"),
            summary: format!("Connect to {DISPLAY_NAME}"),
            args_hint: None,
            requires_arg: false,
        }];
        contributions.slots = vec![SlotSpec {
            slot_id: "home.footer.left".into(),
            priority: 100,
        }];
        contributions.hooks = vec![HookKind::HostStarting];

        Manifest {
            id: PluginId::new(PLUGIN_ID).expect("valid built-in id"),
            name: DISPLAY_NAME.into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: "Anthropic provider (Claude family)".into(),
            kind: PluginKind::Optional,
            contributions,
        }
    }

    async fn handle_slash(&mut self, _: &str, _: Vec<String>) -> Result<Vec<Effect>, PluginError> {
        // `/connect <provider>` (via the picker or directly) always
        // opens the API-key entry modal so the user can re-key — even
        // if a credential is already in the keyring. Pressing Enter on
        // an empty input falls back to the stored key (see the modal's
        // submit handler), so the stored-key path remains one keystroke.
        // Silent auto-connect on startup happens in `on_event(HostStarting)`,
        // not here.
        Ok(vec![Effect::PromptApiKey {
            provider_id: ProviderId::new(PROVIDER_ID).expect("valid"),
        }])
    }

    async fn on_event(&mut self, event: HostEvent) -> Result<Vec<Effect>, PluginError> {
        if matches!(event, HostEvent::HostStarting) && self.try_connect_from_keyring().is_some() {
            return Ok(vec![Effect::RegisterProvider {
                id: ProviderId::new(PROVIDER_ID).expect("valid"),
                display_name: DISPLAY_NAME.into(),
            }]);
        }
        Ok(vec![])
    }

    fn render_slot(&self, slot_id: &str, _: Region) -> Vec<StyledLine> {
        if slot_id != "home.footer.left" {
            return vec![];
        }
        if self.client.is_some() {
            let mods = TextMods {
                bold: true,
                ..TextMods::default()
            };
            vec![StyledLine {
                spans: vec![StyledSpan {
                    text: DISPLAY_NAME.into(),
                    fg: Some(ThemeColor::Success),
                    bg: None,
                    modifiers: mods,
                }],
            }]
        } else {
            vec![]
        }
    }
}

impl BuiltinProviderPlugin for ProviderAnthropicPlugin {
    fn take_client(&mut self) -> Option<Box<dyn ProviderClient>> {
        self.client.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With no keyring entry available (CI default), `/connect anthropic`
    /// must emit [`Effect::PromptApiKey`] so the runtime opens the
    /// masked input — not a dead-end note telling the user to do
    /// what they just did.
    #[tokio::test]
    async fn no_creds_emits_prompt_api_key() {
        // Best-effort: clear any existing entry so the keyring read returns
        // `Ok(None)` on platforms that have a backend. On CI (no backend),
        // load() returns Ok(None) regardless.
        let _ = keyring::Entry::new("savvagent", PROVIDER_ID).map(|e| e.delete_credential());

        let mut p = ProviderAnthropicPlugin::new();
        let effs = p.handle_slash("connect anthropic", vec![]).await.unwrap();
        match &effs[0] {
            Effect::PromptApiKey { provider_id } => {
                assert_eq!(provider_id.as_str(), PROVIDER_ID);
            }
            other => panic!("expected PromptApiKey, got {other:?}"),
        }
    }

    /// `/connect <provider>` must open the API-key modal *even when the
    /// plugin already has a constructed client* — letting the user
    /// re-key from the picker. Pre-fix behavior short-circuited and
    /// silently emitted `RegisterProvider`, denying re-key.
    #[tokio::test]
    async fn handle_slash_with_existing_client_still_prompts() {
        use async_trait::async_trait;
        use savvagent_mcp::ProviderClient;
        use savvagent_protocol::{
            CompleteRequest, CompleteResponse, ListModelsResponse, ProviderError, StreamEvent,
        };
        use tokio::sync::mpsc;

        struct StubClient;
        #[async_trait]
        impl ProviderClient for StubClient {
            async fn complete(
                &self,
                _: CompleteRequest,
                _: Option<mpsc::Sender<StreamEvent>>,
            ) -> Result<CompleteResponse, ProviderError> {
                unreachable!()
            }
            async fn list_models(&self) -> Result<ListModelsResponse, ProviderError> {
                unreachable!()
            }
        }

        let mut p = ProviderAnthropicPlugin::with_test_client(Box::new(StubClient));
        let effs = p.handle_slash("connect anthropic", vec![]).await.unwrap();
        match &effs[0] {
            Effect::PromptApiKey { provider_id } => {
                assert_eq!(provider_id.as_str(), PROVIDER_ID);
            }
            other => panic!(
                "expected PromptApiKey even with pre-installed client; got {other:?}"
            ),
        }
    }

    #[test]
    fn manifest_declares_provider_and_slash() {
        let p = ProviderAnthropicPlugin::new();
        let m = p.manifest();
        assert_eq!(m.id.as_str(), PLUGIN_ID);
        assert_eq!(m.contributions.providers.len(), 1);
        assert_eq!(m.contributions.providers[0].id.as_str(), PROVIDER_ID);
        assert!(
            m.contributions
                .slash_commands
                .iter()
                .any(|s| s.name == format!("connect {PROVIDER_ID}"))
        );
        assert!(m.contributions.hooks.contains(&HookKind::HostStarting));
    }

    #[test]
    fn render_slot_returns_empty_when_disconnected() {
        let p = ProviderAnthropicPlugin::new();
        let lines = p.render_slot(
            "home.footer.left",
            Region {
                x: 0,
                y: 0,
                width: 80,
                height: 1,
            },
        );
        assert!(lines.is_empty());
    }
}
