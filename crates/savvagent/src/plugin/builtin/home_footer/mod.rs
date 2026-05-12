//! `internal:home-footer` — sandbox state + turn state + working dir + key reminder.
//!
//! Contributes to slots `home.footer.center` and `home.footer.right`.
//! Subscribes to `TurnStart` / `TurnEnd` for the turn-state span (PR 7
//! wires the host to actually emit those events; until then the plugin
//! shows the idle state).

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Effect, HookKind, HostEvent, Manifest, Plugin, PluginError, PluginId,
    PluginKind, Region, SlotSpec, StyledLine, StyledSpan, TextMods, ThemeColor,
};

/// TUI home-screen footer plugin.
///
/// Renders sandbox + turn state in `home.footer.center` and the working
/// directory + key reminder in `home.footer.right`. Tracks the active turn
/// by listening to `TurnStart` / `TurnEnd` host events.
pub struct HomeFooterPlugin {
    turn_active: Option<u32>,
    working_dir: String,
}

impl HomeFooterPlugin {
    /// Construct a new `HomeFooterPlugin`, snapshotting `$PWD` at creation time.
    ///
    /// Falls back to `"?"` if the working directory cannot be determined.
    pub fn new() -> Self {
        let wd = std::env::current_dir()
            .ok()
            .and_then(|p| p.to_str().map(|s| s.to_string()))
            .unwrap_or_else(|| "?".into());
        Self {
            turn_active: None,
            working_dir: wd,
        }
    }
}

impl Default for HomeFooterPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for HomeFooterPlugin {
    fn manifest(&self) -> Manifest {
        // Contributions is #[non_exhaustive] — build via field mutation
        // from default rather than struct-literal FRU syntax.
        let mut contributions = Contributions::default();
        contributions.slots = vec![
            SlotSpec {
                slot_id: "home.footer.center".into(),
                priority: 100,
            },
            SlotSpec {
                slot_id: "home.footer.right".into(),
                priority: 100,
            },
        ];
        contributions.hooks = vec![HookKind::TurnStart, HookKind::TurnEnd];

        Manifest {
            id: PluginId::new("internal:home-footer").expect("valid built-in id"),
            name: "Home footer".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: "Sandbox + turn state + working dir + key reminder".into(),
            kind: PluginKind::Core,
            contributions,
        }
    }

    fn render_slot(&self, slot_id: &str, _region: Region) -> Vec<StyledLine> {
        match slot_id {
            "home.footer.center" => {
                let turn = match self.turn_active {
                    Some(id) => format!("turn #{id} working"),
                    None => "idle".to_string(),
                };
                vec![StyledLine {
                    spans: vec![StyledSpan {
                        text: turn,
                        fg: Some(ThemeColor::Accent),
                        bg: None,
                        modifiers: TextMods::default(),
                    }],
                }]
            }
            "home.footer.right" => {
                vec![StyledLine {
                    spans: vec![
                        StyledSpan {
                            text: self.working_dir.clone(),
                            fg: Some(ThemeColor::Muted),
                            bg: None,
                            modifiers: TextMods::default(),
                        },
                        StyledSpan {
                            text: "  ? for help".into(),
                            fg: Some(ThemeColor::Muted),
                            bg: None,
                            modifiers: TextMods::default(),
                        },
                    ],
                }]
            }
            _ => vec![],
        }
    }

    async fn on_event(&mut self, event: HostEvent) -> Result<Vec<Effect>, PluginError> {
        match event {
            HostEvent::TurnStart { turn_id } => self.turn_active = Some(turn_id),
            HostEvent::TurnEnd { .. } => self.turn_active = None,
            _ => {}
        }
        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_center_renders_idle() {
        let p = HomeFooterPlugin::new();
        let lines = p.render_slot(
            "home.footer.center",
            Region {
                x: 0,
                y: 0,
                width: 40,
                height: 1,
            },
        );
        assert_eq!(lines[0].spans[0].text, "idle");
    }

    #[tokio::test]
    async fn turn_start_flips_to_working() {
        let mut p = HomeFooterPlugin::new();
        p.on_event(HostEvent::TurnStart { turn_id: 3 })
            .await
            .unwrap();
        let lines = p.render_slot(
            "home.footer.center",
            Region {
                x: 0,
                y: 0,
                width: 40,
                height: 1,
            },
        );
        assert_eq!(lines[0].spans[0].text, "turn #3 working");
    }

    #[tokio::test]
    async fn turn_end_returns_to_idle() {
        let mut p = HomeFooterPlugin::new();
        p.on_event(HostEvent::TurnStart { turn_id: 3 })
            .await
            .unwrap();
        p.on_event(HostEvent::TurnEnd {
            turn_id: 3,
            success: true,
        })
        .await
        .unwrap();
        let lines = p.render_slot(
            "home.footer.center",
            Region {
                x: 0,
                y: 0,
                width: 40,
                height: 1,
            },
        );
        assert_eq!(lines[0].spans[0].text, "idle");
    }

    #[test]
    fn right_slot_includes_working_dir_and_hint() {
        let p = HomeFooterPlugin::new();
        let lines = p.render_slot(
            "home.footer.right",
            Region {
                x: 0,
                y: 0,
                width: 80,
                height: 1,
            },
        );
        let joined: String = lines[0].spans.iter().map(|s| s.text.clone()).collect();
        assert!(joined.contains("? for help"));
    }
}
