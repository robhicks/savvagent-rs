//! Plugin runtime root. PR 1 ships only the empty `register_builtins()`
//! entry point; subsequent PRs add registry/screen stack/routers/effects.

/// Built-in plugin implementations shipped with the binary.
pub mod builtin;

/// Conversion helpers between `savvagent-plugin` types and ratatui types.
pub mod convert;

/// In-memory registry of constructed plugin instances and their enabled-set.
pub mod registry;

/// Derived indexes over enabled-plugin manifests (slash/slots/hooks/keybindings/screens).
pub mod manifests;

/// Slot routing: resolves a slot_id to its priority-ordered contributor list
/// and concatenates each contributor's rendered lines.
pub mod slots;

/// Slash command routing: resolves bare command names to their owning plugin
/// and dispatches `handle_slash`, with a re-entrancy depth cap.
#[allow(dead_code)]
pub mod slash;

/// Keybinding routing: resolves a portable key event to its [`savvagent_plugin::BoundAction`]
/// using scope precedence `OnScreen` > `OnHome` > `Global`.
#[allow(dead_code)]
pub mod keybindings;

/// LIFO stack of `(Box<dyn Screen>, ScreenLayout)` pairs driven by
/// `Effect::OpenScreen` / `Effect::CloseScreen`; replaces the v0.8
/// `InputMode` flat-field state machine.
#[allow(dead_code)]
pub mod screen_stack;

/// Single mutation surface: maps each `Effect` variant to the corresponding
/// `App` method. The event loop calls this after dispatching key events, hook
/// events, or slash commands.
#[allow(dead_code)]
pub mod effects;

/// Returns the set of built-in plugin instances.
///
/// PR 2 adds: home-footer, home-tips.
/// PR 3 adds: splash, command-palette.
/// PR 4 adds: view-file, edit-file.
/// PR 5 adds: connect, resume, model, save, clear.
/// PR 6 adds: themes + 4 providers.
/// PR 8 adds: plugins-manager.
pub fn register_builtins() -> Vec<Box<dyn savvagent_plugin::Plugin>> {
    vec![
        Box::new(builtin::command_palette::CommandPalettePlugin::new()),
        Box::new(builtin::edit_file::EditFilePlugin::new()),
        Box::new(builtin::home_footer::HomeFooterPlugin::new()),
        Box::new(builtin::home_tips::HomeTipsPlugin::new()),
        Box::new(builtin::splash::SplashPlugin::new()),
        Box::new(builtin::view_file::ViewFilePlugin::new()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn register_builtins_pr4_complete() {
        let plugins = register_builtins();
        let ids: Vec<_> = plugins
            .iter()
            .map(|p| p.manifest().id.as_str().to_string())
            .collect();
        assert!(ids.contains(&"internal:command-palette".to_string()));
        assert!(ids.contains(&"internal:edit-file".to_string()));
        assert!(ids.contains(&"internal:home-footer".to_string()));
        assert!(ids.contains(&"internal:home-tips".to_string()));
        assert!(ids.contains(&"internal:splash".to_string()));
        assert!(ids.contains(&"internal:view-file".to_string()));
        assert_eq!(plugins.len(), 6);
    }
}
