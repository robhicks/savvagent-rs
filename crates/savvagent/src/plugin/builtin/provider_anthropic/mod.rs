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
pub struct ProviderAnthropicPlugin {
    client: Option<Box<dyn ProviderClient>>,
}

impl ProviderAnthropicPlugin {
    /// Construct a new shim with no client yet.
    pub fn new() -> Self {
        Self { client: None }
    }

    /// Try to read the API key from the keyring and, on success, build an
    /// in-process [`ProviderClient`]. Returns `Some(())` when a client was
    /// installed, `None` when credentials were unavailable.
    fn try_connect_from_keyring(&mut self) -> Option<()> {
        if self.client.is_some() {
            // Already connected; nothing to do.
            return Some(());
        }
        let key = match crate::creds::load(PROVIDER_ID) {
            Ok(Some(k)) => k,
            _ => return None,
        };
        let provider = provider_anthropic::AnthropicProvider::builder()
            .api_key(&key)
            .build()
            .ok()?;
        let client: Box<dyn ProviderClient> =
            Box::new(InProcessProviderClient::new(Arc::new(provider)));
        self.client = Some(client);
        Some(())
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
        if self.try_connect_from_keyring().is_some() {
            return Ok(vec![Effect::RegisterProvider {
                id: ProviderId::new(PROVIDER_ID).expect("valid"),
                display_name: DISPLAY_NAME.into(),
            }]);
        }
        Ok(vec![Effect::PushNote {
            line: StyledLine::plain(format!(
                "{DISPLAY_NAME} API key not found in keyring. Run `/connect` from the home view to enter one."
            )),
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
                    fg: Some(ThemeColor::Green),
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
    /// must emit a [`Effect::PushNote`] explaining the situation rather
    /// than a `RegisterProvider` effect with no client behind it.
    #[tokio::test]
    async fn no_creds_emits_pushnote() {
        // Best-effort: clear any existing entry so the keyring read returns
        // `Ok(None)` on platforms that have a backend. On CI (no backend),
        // load() returns Ok(None) regardless.
        let _ = keyring::Entry::new("savvagent", PROVIDER_ID).map(|e| e.delete_credential());

        let mut p = ProviderAnthropicPlugin::new();
        let effs = p.handle_slash("connect anthropic", vec![]).await.unwrap();
        assert!(
            matches!(effs[0], Effect::PushNote { .. }),
            "expected PushNote when no creds available, got {:?}",
            effs
        );
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
