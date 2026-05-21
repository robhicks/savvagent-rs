//! `internal:user-slash-commands` — discovers and dispatches user-defined
//! slash commands from `.savvagent/commands/` and `.claude/commands/`.
//!
//! See `docs/superpowers/specs/2026-05-21-user-slash-commands-design.md`.

mod discovery;
mod frontmatter;
mod name;
mod template;
pub(crate) mod trust;
mod trust_modal;

use std::path::PathBuf;
use std::sync::Mutex;

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Effect, Manifest, Plugin, PluginError, PluginId, PluginKind, ScreenArgs,
    ScreenLayout, ScreenSpec, SlashSpec,
};

use crate::plugin::builtin::user_slash_commands::discovery::{walk_all, Index};

/// Built-in plugin that exposes user-authored slash commands.
pub struct UserSlashCommandsPlugin {
    project_root: PathBuf,
    home: PathBuf,
    cache: Mutex<Option<Index>>,
}

impl UserSlashCommandsPlugin {
    /// Default constructor used by `register_builtins`: resolves
    /// `project_root` from cwd and `home` from `dirs::home_dir()`.
    pub fn new() -> Self {
        let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        Self {
            project_root,
            home,
            cache: Mutex::new(None),
        }
    }

    /// Override the search roots; used by tests and `/reload-commands` (Task 20).
    #[allow(dead_code)]
    pub fn with_roots(project_root: PathBuf, home: PathBuf) -> Self {
        Self {
            project_root,
            home,
            cache: Mutex::new(None),
        }
    }

    /// Snapshot the cached Index, populating the cache on first access.
    fn index_snapshot(&self) -> Index {
        let mut g = self.cache.lock().unwrap();
        if g.is_none() {
            *g = Some(walk_all(&self.project_root, &self.home));
        }
        g.as_ref().unwrap().clone()
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
        contributions.slash_commands.push(SlashSpec {
            name: "reload-commands".into(),
            summary: "Rescan user-defined slash command directories".into(),
            args_hint: None,
            requires_arg: false,
        });
        let idx = self.index_snapshot();
        for d in idx.commands.values() {
            let summary = d
                .frontmatter
                .description
                .clone()
                .unwrap_or_else(|| d.path.display().to_string());
            contributions.slash_commands.push(SlashSpec {
                name: d.name.clone(),
                summary,
                args_hint: d.frontmatter.argument_hint.clone(),
                requires_arg: false,
            });
        }
        // Keep the trust.modal screen contribution from Task 16.
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
        // Implemented in Task 19.
        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

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

    #[test]
    fn manifest_includes_discovered_commands() {
        let proj = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let dir = proj.path().join(".savvagent/commands");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("review.md"),
            "---\ndescription: Review the diff\n---\nbody",
        )
        .unwrap();

        let p = UserSlashCommandsPlugin::with_roots(
            proj.path().to_path_buf(),
            home.path().to_path_buf(),
        );
        let m = p.manifest();
        let names: Vec<_> = m
            .contributions
            .slash_commands
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert!(names.contains(&"reload-commands"));
        assert!(names.contains(&"review"));
        let review = m
            .contributions
            .slash_commands
            .iter()
            .find(|s| s.name == "review")
            .unwrap();
        assert_eq!(review.summary, "Review the diff");
    }
}
