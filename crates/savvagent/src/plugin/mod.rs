//! Plugin runtime root. PR 1 ships only the empty `register_builtins()`
//! entry point; subsequent PRs add registry/screen stack/routers/effects.

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

/// Returns the set of built-in plugin instances. Empty in PR 1.
///
/// PR 2 adds: home-footer, home-tips.
/// PR 3 adds: splash, command-palette.
/// PR 4 adds: view-file, edit-file.
/// PR 5 adds: connect, resume, model, save, clear.
/// PR 6 adds: themes + 4 providers.
/// PR 8 adds: plugins-manager.
#[allow(dead_code)] // wired into the event loop in a later PR
pub fn register_builtins() -> Vec<Box<dyn savvagent_plugin::Plugin>> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_builtins_is_empty_in_pr1() {
        assert!(register_builtins().is_empty());
    }
}
