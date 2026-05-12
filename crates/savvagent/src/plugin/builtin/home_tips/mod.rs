//! `internal:home-tips` — static "Press / for commands" tip when home
//! view is active and no screen is on top of the stack.
//!
//! Contributes to slot `home.tips`. Subscribes to `Connect` so the
//! splash-warming hint can be replaced once a provider is online.

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Effect, HookKind, HostEvent, Manifest, Plugin, PluginError, PluginId,
    PluginKind, Region, SlotSpec, StyledLine, StyledSpan, TextMods, ThemeColor,
};

/// TUI home-screen tips plugin.
///
/// Renders a one-line hint above the prompt in the `home.tips` slot.
/// Shows `"Connecting…  Press / for commands"` until the first `Connect`
/// host event fires, then switches to `"Press / for commands"`.
#[allow(dead_code)] // wired into register_builtins in a later task
pub struct HomeTipsPlugin {
    connected: bool,
}

impl HomeTipsPlugin {
    /// Construct a new `HomeTipsPlugin` in the pre-connection state.
    pub fn new() -> Self {
        Self { connected: false }
    }
}

impl Default for HomeTipsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for HomeTipsPlugin {
    fn manifest(&self) -> Manifest {
        // Contributions is #[non_exhaustive] — build via field mutation.
        let mut contributions = Contributions::default();
        contributions.slots = vec![SlotSpec {
            slot_id: "home.tips".into(),
            priority: 100,
        }];
        contributions.hooks = vec![HookKind::Connect];

        Manifest {
            id: PluginId::new("internal:home-tips").expect("valid built-in id"),
            name: "Home tips".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: "One-line tips above the prompt".into(),
            kind: PluginKind::Core,
            contributions,
        }
    }

    fn render_slot(&self, slot_id: &str, _region: Region) -> Vec<StyledLine> {
        if slot_id != "home.tips" {
            return vec![];
        }
        let text = if self.connected {
            "Press / for commands".to_string()
        } else {
            "Connecting…  Press / for commands".to_string()
        };
        vec![StyledLine {
            spans: vec![StyledSpan {
                text,
                fg: Some(ThemeColor::DarkGray),
                bg: None,
                modifiers: TextMods::default(),
            }],
        }]
    }

    async fn on_event(&mut self, event: HostEvent) -> Result<Vec<Effect>, PluginError> {
        if let HostEvent::Connect { .. } = event {
            self.connected = true;
        }
        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use savvagent_plugin::ProviderId;

    #[test]
    fn renders_connecting_before_first_connect() {
        let p = HomeTipsPlugin::new();
        let lines = p.render_slot(
            "home.tips",
            Region {
                x: 0,
                y: 0,
                width: 80,
                height: 1,
            },
        );
        assert!(lines[0].spans[0].text.starts_with("Connecting"));
    }

    #[tokio::test]
    async fn renders_press_slash_after_connect() {
        let mut p = HomeTipsPlugin::new();
        p.on_event(HostEvent::Connect {
            provider_id: ProviderId::new("anthropic").expect("valid"),
        })
        .await
        .unwrap();
        let lines = p.render_slot(
            "home.tips",
            Region {
                x: 0,
                y: 0,
                width: 80,
                height: 1,
            },
        );
        assert_eq!(lines[0].spans[0].text, "Press / for commands");
    }
}
