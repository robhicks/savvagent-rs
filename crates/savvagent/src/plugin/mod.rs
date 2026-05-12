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

/// Re-export so callers don't have to reach into the registry submodule
/// for the type returned from [`register_builtins`].
pub use registry::BuiltinSet;

/// Returns the set of built-in plugin instances and provider-plugin shims.
///
/// PR 2 adds: home-footer, home-tips.
/// PR 3 adds: splash, command-palette.
/// PR 4 adds: view-file, edit-file.
/// PR 5 adds: connect, resume, model, save, clear.
/// PR 6 adds: themes + provider shims (task 6.2 ships anthropic; task 6.3
/// follows with openai/gemini/local).
/// PR 8 adds: plugins-manager.
///
/// Provider plugins are double-registered: once as `Box<dyn Plugin>` so
/// their manifest/slash/slot/event contributions flow through the normal
/// dispatch paths, and once as `Box<dyn BuiltinProviderPlugin>` so
/// `apply_effects` can call `take_client()` after seeing
/// [`savvagent_plugin::Effect::RegisterProvider`].
pub fn register_builtins() -> BuiltinSet {
    let providers: Vec<Box<dyn builtin::provider_common::BuiltinProviderPlugin>> = vec![Box::new(
        builtin::provider_anthropic::ProviderAnthropicPlugin::new(),
    )];

    let plugins: Vec<Box<dyn savvagent_plugin::Plugin>> = vec![
        Box::new(builtin::clear::ClearPlugin::new()),
        Box::new(builtin::command_palette::CommandPalettePlugin::new()),
        Box::new(builtin::connect::ConnectPlugin::new()),
        Box::new(builtin::edit_file::EditFilePlugin::new()),
        Box::new(builtin::home_footer::HomeFooterPlugin::new()),
        Box::new(builtin::home_tips::HomeTipsPlugin::new()),
        Box::new(builtin::model::ModelPlugin::new()),
        Box::new(builtin::resume::ResumePlugin::new()),
        Box::new(builtin::save::SavePlugin::new()),
        Box::new(builtin::splash::SplashPlugin::new()),
        Box::new(builtin::themes::ThemesPlugin::new()),
        Box::new(builtin::view_file::ViewFilePlugin::new()),
        // Provider plugins also live in the regular plugin registry — see
        // doc-comment above. The `providers` vec holds a separate set of
        // instances; each `Plugin`-side instance is independent state.
        Box::new(builtin::provider_anthropic::ProviderAnthropicPlugin::new()),
    ];

    BuiltinSet { plugins, providers }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn register_builtins_pr6_complete() {
        let set = register_builtins();
        let ids: Vec<_> = set
            .plugins
            .iter()
            .map(|p| p.manifest().id.as_str().to_string())
            .collect();
        // Original 12 PR-1..PR-5 + themes built-ins.
        assert!(ids.contains(&"internal:clear".to_string()));
        assert!(ids.contains(&"internal:command-palette".to_string()));
        assert!(ids.contains(&"internal:connect".to_string()));
        assert!(ids.contains(&"internal:edit-file".to_string()));
        assert!(ids.contains(&"internal:home-footer".to_string()));
        assert!(ids.contains(&"internal:home-tips".to_string()));
        assert!(ids.contains(&"internal:model".to_string()));
        assert!(ids.contains(&"internal:resume".to_string()));
        assert!(ids.contains(&"internal:save".to_string()));
        assert!(ids.contains(&"internal:splash".to_string()));
        assert!(ids.contains(&"internal:themes".to_string()));
        assert!(ids.contains(&"internal:view-file".to_string()));
        // PR 6 task 6.2 ships the anthropic provider shim.
        assert!(ids.contains(&"internal:provider-anthropic".to_string()));
        assert_eq!(set.plugins.len(), 13);

        // Provider shims are also indexed in the parallel provider vec so
        // `apply_effects` can call `take_client` on them.
        let provider_ids: Vec<_> = set
            .providers
            .iter()
            .map(|p| p.manifest().id.as_str().to_string())
            .collect();
        assert!(provider_ids.contains(&"internal:provider-anthropic".to_string()));
        assert_eq!(set.providers.len(), 1);
    }
}
