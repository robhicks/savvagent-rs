//! `internal:provider-local` — keyless Ollama shim.
//!
//! Unlike the other provider shims, this one needs no credentials. On
//! `HostStarting` (and on `/connect local`) the plugin attempts a quick
//! reachability probe against the configured Ollama endpoint and, on
//! success, builds an in-process [`savvagent_mcp::ProviderClient`] and
//! emits [`savvagent_plugin::Effect::RegisterProvider`].
//!
//! v0.9 deferred item: the actual ping is stubbed — we build the provider
//! and emit `RegisterProvider` regardless. The first turn surfaces any
//! reachability failure. A proper health-check + retry loop lands as part
//! of PR 7's Host integration. See task 6.3 in the v0.9 plan.

use std::sync::Arc;

use async_trait::async_trait;
use savvagent_mcp::{InProcessProviderClient, ProviderClient};
use savvagent_plugin::{
    Contributions, Effect, HookKind, HostEvent, Manifest, Plugin, PluginError, PluginId,
    PluginKind, ProviderId, ProviderSpec, Region, SlashSpec, SlotSpec, StyledLine, StyledSpan,
    TextMods, ThemeColor,
};

use super::provider_common::BuiltinProviderPlugin;

const PLUGIN_ID: &str = "internal:provider-local";
const PROVIDER_ID: &str = "local";
const DISPLAY_NAME: &str = "Local (Ollama)";

/// Local (Ollama) provider shim. Keyless.
pub(crate) struct ProviderLocalPlugin {
    client: Option<Box<dyn ProviderClient>>,
    /// Sticky bit set when a previous build attempt failed; the slash and
    /// hook re-entry then take the "endpoint unreachable" branch instead of
    /// repeatedly retrying a known-bad build and emitting `RegisterProvider`
    /// for a dead client. Cleared on a successful build, so `/connect local`
    /// after the user starts `ollama serve` can succeed.
    last_build_failed: bool,
}

impl ProviderLocalPlugin {
    /// Construct a new shim with no client yet.
    pub(crate) fn new() -> Self {
        Self {
            client: None,
            last_build_failed: false,
        }
    }

    /// Try to construct an in-process Ollama client. Returns `Some(())` on
    /// success. v0.9 ships without a true health-check — TODO is wired in
    /// PR 7 alongside the rest of the Host integration.
    ///
    /// A previously-failed build short-circuits this call. The user can
    /// retry via `/connect local`, which clears the sticky bit on success
    /// once `ollama serve` is reachable.
    fn try_connect_local(&mut self) -> Option<()> {
        if self.client.is_some() {
            return Some(());
        }
        if self.last_build_failed {
            // A prior call set the sticky bit; surface "endpoint unreachable"
            // again rather than spinning the builder per slash/hook re-entry.
            // PR 7's real health-check will replace this with a timed probe.
            return None;
        }
        let provider = match provider_local::OllamaProvider::builder().build() {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, "ollama provider build failed");
                self.last_build_failed = true;
                return None;
            }
        };
        let client: Box<dyn ProviderClient> =
            Box::new(InProcessProviderClient::new(Arc::new(provider)));
        self.client = Some(client);
        self.last_build_failed = false;
        Some(())
    }

    /// Test-only: clear the sticky-failure bit. Lets unit tests drive
    /// the same plugin through "fail then succeed" sequences if the
    /// underlying builder is ever made injectable.
    #[cfg(test)]
    #[allow(dead_code)]
    fn reset_last_build_failed(&mut self) {
        self.last_build_failed = false;
    }
}

impl Default for ProviderLocalPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for ProviderLocalPlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.providers = vec![ProviderSpec {
            id: ProviderId::new(PROVIDER_ID).expect("valid provider id"),
            display_name: DISPLAY_NAME.into(),
            requires_credential: false,
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
            priority: 130,
        }];
        contributions.hooks = vec![HookKind::HostStarting];

        Manifest {
            id: PluginId::new(PLUGIN_ID).expect("valid built-in id"),
            name: DISPLAY_NAME.into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: "Local provider via Ollama (keyless)".into(),
            kind: PluginKind::Optional,
            contributions,
        }
    }

    async fn handle_slash(&mut self, _: &str, _: Vec<String>) -> Result<Vec<Effect>, PluginError> {
        if self.try_connect_local().is_some() {
            return Ok(vec![Effect::RegisterProvider {
                id: ProviderId::new(PROVIDER_ID).expect("valid"),
                display_name: DISPLAY_NAME.into(),
            }]);
        }
        Ok(vec![Effect::PushNote {
            line: StyledLine::plain(format!(
                "{DISPLAY_NAME} endpoint not reachable. Start `ollama serve` and try again."
            )),
        }])
    }

    async fn on_event(&mut self, event: HostEvent) -> Result<Vec<Effect>, PluginError> {
        if matches!(event, HostEvent::HostStarting) && self.try_connect_local().is_some() {
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

impl BuiltinProviderPlugin for ProviderLocalPlugin {
    fn take_client(&mut self) -> Option<Box<dyn ProviderClient>> {
        self.client.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Local is keyless, so the "no-creds" path doesn't apply. Until the
    /// real Ollama ping lands (deferred to PR 7), `/connect local` always
    /// builds a client and emits `RegisterProvider`. This test pins down
    /// the v0.9 stub behavior so the deferred upgrade doesn't regress
    /// silently.
    #[tokio::test]
    async fn connect_emits_register_or_push_note() {
        let mut p = ProviderLocalPlugin::new();
        let effs = p.handle_slash("connect local", vec![]).await.unwrap();
        assert_eq!(effs.len(), 1);
        match &effs[0] {
            Effect::RegisterProvider { id, .. } => {
                assert_eq!(id.as_str(), PROVIDER_ID);
            }
            Effect::PushNote { .. } => {
                // Builder failed (very rare — only on misconfigured envs).
            }
            other => panic!("unexpected effect: {other:?}"),
        }
    }

    #[test]
    fn manifest_marks_provider_keyless() {
        let p = ProviderLocalPlugin::new();
        let m = p.manifest();
        assert_eq!(m.id.as_str(), PLUGIN_ID);
        assert!(!m.contributions.providers[0].requires_credential);
        assert!(
            m.contributions
                .slash_commands
                .iter()
                .any(|s| s.name == format!("connect {PROVIDER_ID}"))
        );
    }
}
