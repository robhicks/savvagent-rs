//! `internal:user-slash-commands` — discovers and dispatches user-defined
//! slash commands from `.savvagent/commands/` and `.claude/commands/`.
//!
//! See `docs/superpowers/specs/2026-05-21-user-slash-commands-design.md`.

mod discovery;
mod frontmatter;
mod name;
mod template;
mod trust;
mod trust_modal;

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Effect, Manifest, Plugin, PluginError, PluginId, PluginKind, ScreenArgs,
    ScreenLayout, ScreenSpec, SlashSpec,
};

/// Built-in plugin that exposes user-authored slash commands.
pub struct UserSlashCommandsPlugin;

impl UserSlashCommandsPlugin {
    /// Construct a new [`UserSlashCommandsPlugin`].
    pub fn new() -> Self {
        Self
    }
}

impl Default for UserSlashCommandsPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for UserSlashCommandsPlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.slash_commands = vec![SlashSpec {
            name: "reload-commands".into(),
            summary: "Rescan user-defined slash command directories".into(),
            args_hint: None,
            requires_arg: false,
        }];
        contributions.screens = vec![ScreenSpec {
            id: "trust.modal".into(),
            layout: ScreenLayout::CenteredModal {
                width_pct: 60,
                height_pct: 30,
                title: Some("Trust project commands?".into()),
            },
        }];
        Manifest {
            id: PluginId::new("internal:user-slash-commands").expect("valid built-in id"),
            name: "User slash commands".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: "User-defined commands from .savvagent/commands/ and .claude/commands/"
                .into(),
            kind: PluginKind::Core,
            contributions,
        }
    }

    fn create_screen(
        &self,
        id: &str,
        args: ScreenArgs,
    ) -> Result<Box<dyn savvagent_plugin::Screen>, PluginError> {
        match id {
            "trust.modal" => Ok(Box::new(trust_modal::TrustModal::from_args(args)?)),
            _ => Err(PluginError::ScreenNotFound(id.into())),
        }
    }

    async fn handle_slash(
        &mut self,
        _name: &str,
        _args: Vec<String>,
    ) -> Result<Vec<Effect>, PluginError> {
        // Implemented in Task 18.
        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_has_reload_commands() {
        let p = UserSlashCommandsPlugin::new();
        let m = p.manifest();
        assert_eq!(m.id.as_str(), "internal:user-slash-commands");
        let names: Vec<_> = m
            .contributions
            .slash_commands
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert!(names.contains(&"reload-commands"));
    }

    #[test]
    fn manifest_registers_trust_modal_screen() {
        let p = UserSlashCommandsPlugin::new();
        let m = p.manifest();
        assert_eq!(
            m.contributions.screens.len(),
            1,
            "expected exactly one screen contribution"
        );
        assert_eq!(m.contributions.screens[0].id, "trust.modal");
    }
}
