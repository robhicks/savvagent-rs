//! Plugin runtime root. PR 1 ships only the empty `register_builtins()`
//! entry point; subsequent PRs add registry/screen stack/routers/effects.

/// Built-in plugin implementations shipped with the binary.
#[allow(dead_code)] // wired into register_builtins as each plugin lands
pub mod builtin;

/// In-memory registry of constructed plugin instances and their enabled-set.
#[allow(dead_code)] // wired into the event loop in Task 2.8
pub mod registry;

/// Derived indexes over enabled-plugin manifests (slash/slots/hooks/keybindings/screens).
#[allow(dead_code)] // consumed by the router/dispatcher in later PRs
pub mod manifests;

/// Slot routing: resolves a slot_id to its priority-ordered contributor list
/// and concatenates each contributor's rendered lines.
#[allow(dead_code)] // wired into ui.rs for the segmented footer + tips line in a later PR
pub mod slots;

/// Returns the set of built-in plugin instances.
///
/// PR 2 adds: home-footer, home-tips.
/// PR 3 adds: splash, command-palette.
/// PR 4 adds: view-file, edit-file.
/// PR 5 adds: connect, resume, model, save, clear.
/// PR 6 adds: themes + 4 providers.
/// PR 8 adds: plugins-manager.
#[allow(dead_code)] // wired into the event loop in Task 2.8
pub fn register_builtins() -> Vec<Box<dyn savvagent_plugin::Plugin>> {
    vec![
        Box::new(builtin::home_footer::HomeFooterPlugin::new()),
        Box::new(builtin::home_tips::HomeTipsPlugin::new()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use savvagent_plugin::PluginKind;

    #[tokio::test]
    async fn register_builtins_returns_pr2_pair() {
        let plugins = register_builtins();
        let ids: Vec<_> = plugins
            .iter()
            .map(|p| p.manifest().id.as_str().to_string())
            .collect();
        assert!(ids.contains(&"internal:home-footer".to_string()));
        assert!(ids.contains(&"internal:home-tips".to_string()));
        assert_eq!(plugins.len(), 2);

        // Both must be Core in PR 2.
        for p in &plugins {
            assert_eq!(p.manifest().kind, PluginKind::Core);
        }
    }
}
