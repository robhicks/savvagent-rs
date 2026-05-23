//! `internal:user-hooks` — discovers and dispatches Claude-Code-compatible
//! user shell hooks from `settings.json`. See
//! `docs/superpowers/specs/2026-05-22-user-hooks-design.md`.

mod config;
mod decision;
mod discovery;
mod matcher;
mod payload;
mod runner;

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Effect, Manifest, Plugin, PluginError, PluginId, PluginKind, SlashSpec,
};

/// Built-in plugin that exposes user-authored shell hooks.
pub struct UserHooksPlugin;

impl UserHooksPlugin {
    /// Construct a new [`UserHooksPlugin`].
    pub fn new() -> Self {
        Self
    }
}

impl Default for UserHooksPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for UserHooksPlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.slash_commands = vec![SlashSpec {
            name: "reload-hooks".into(),
            summary: "Rescan user-defined hooks (settings.json)".into(),
            args_hint: None,
            requires_arg: false,
        }];
        Manifest {
            id: PluginId::new("internal:user-hooks").expect("valid built-in id"),
            name: "User hooks".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: "Claude-Code-compatible settings.json hooks".into(),
            kind: PluginKind::Core,
            contributions,
        }
    }

    async fn handle_slash(
        &mut self,
        _name: &str,
        _args: Vec<String>,
    ) -> Result<Vec<Effect>, PluginError> {
        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_has_reload_hooks() {
        let p = UserHooksPlugin::new();
        let m = p.manifest();
        assert_eq!(m.id.as_str(), "internal:user-hooks");
        let names: Vec<_> = m
            .contributions
            .slash_commands
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert!(names.contains(&"reload-hooks"));
    }
}
