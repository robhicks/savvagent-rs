//! `internal:command-palette` — filterable list of all visible slash
//! commands, opened via the `/` keybinding from the home view.

pub mod screen;

use async_trait::async_trait;
use savvagent_plugin::{
    BoundAction, ChordPortable, Contributions, Effect, KeyCodePortable, KeyEventPortable, KeyMods,
    KeyScope, KeybindingSpec, Manifest, Plugin, PluginError, PluginId, PluginKind, Region, Screen,
    ScreenArgs, ScreenLayout, ScreenSpec, SlotSpec, StyledLine,
};

use screen::PaletteScreen;

/// Plugin wrapper for the filterable slash-command picker.
///
/// Registers the `palette` screen, the `OnHome` `/` keybinding, and a
/// `home.tips` slot contribution so future palette hints can ship without
/// manifest changes.
pub struct CommandPalettePlugin;

impl CommandPalettePlugin {
    /// Create a new `CommandPalettePlugin`.
    pub fn new() -> Self {
        Self
    }
}

impl Default for CommandPalettePlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for CommandPalettePlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.screens = vec![ScreenSpec {
            id: "palette".into(),
            layout: ScreenLayout::CenteredModal {
                width_pct: 60,
                height_pct: 60,
                title: Some("Commands".into()),
            },
        }];
        let open_palette = BoundAction::EmitEffect(Effect::OpenScreen {
            id: "palette".into(),
            args: ScreenArgs::None,
        });
        contributions.keybindings = vec![
            KeybindingSpec {
                chord: ChordPortable::new(KeyEventPortable {
                    code: KeyCodePortable::Char('/'),
                    modifiers: KeyMods::default(),
                }),
                scope: KeyScope::OnHome,
                action: open_palette.clone(),
            },
            // Ctrl-P is the v0.8 muscle-memory shortcut for the palette.
            // The empty-state splash message advertises it.
            KeybindingSpec {
                chord: ChordPortable::new(KeyEventPortable {
                    code: KeyCodePortable::Char('p'),
                    modifiers: KeyMods {
                        ctrl: true,
                        ..KeyMods::default()
                    },
                }),
                scope: KeyScope::OnHome,
                action: open_palette,
            },
        ];
        contributions.slots = vec![SlotSpec {
            slot_id: "home.tips".into(),
            priority: 200,
        }];

        Manifest {
            id: PluginId::new("internal:command-palette").expect("valid built-in id"),
            name: "Command palette".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: "/-prefixed command picker".into(),
            kind: PluginKind::Core,
            contributions,
        }
    }

    fn create_screen(&self, id: &str, _args: ScreenArgs) -> Result<Box<dyn Screen>, PluginError> {
        if id != "palette" {
            return Err(PluginError::ScreenNotFound(id.to_string()));
        }
        Ok(Box::new(PaletteScreen::new()))
    }

    fn render_slot(&self, slot_id: &str, _region: Region) -> Vec<StyledLine> {
        // Lower-priority slot contribution; intentionally empty until
        // PR 4+ adds palette-specific tips. The slot reservation is here
        // so future palette hints (e.g., "Ctrl-K to clear filter") can
        // ship without manifest changes.
        if slot_id != "home.tips" {
            return vec![];
        }
        vec![]
    }
}
