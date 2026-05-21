//! `internal:lsp-installer` — `/lsp` slash command, multi-select picker,
//! and one-shot LSP-binary installer.
//!
//! See `docs/superpowers/specs/2026-05-20-lsp-installer-design.md` and
//! `docs/superpowers/plans/2026-05-20-lsp-installer.md`.

pub mod catalog;
pub mod config_writer;
pub mod installer;
pub mod picker;
pub mod screen;

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Effect, Manifest, Plugin, PluginError, PluginId, PluginKind, Screen,
    ScreenArgs, ScreenLayout, ScreenSpec, SlashSpec, StyledLine,
};

use screen::LspPickerScreen;

/// Plugin instance exposing `/lsp` and the picker screen.
pub struct LspInstallerPlugin;

impl LspInstallerPlugin {
    /// Construct a new `LspInstallerPlugin`. Stateless; multiple
    /// instances would behave identically (the catalog is `'static`).
    pub fn new() -> Self {
        Self
    }
}

impl Default for LspInstallerPlugin {
    fn default() -> Self {
        Self::new()
    }
}

const PLUGIN_ID: &str = "internal:lsp-installer";

#[async_trait]
impl Plugin for LspInstallerPlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.slash_commands = vec![SlashSpec {
            name: "lsp".into(),
            summary: "Install language servers".into(),
            args_hint: None,
            requires_arg: false,
        }];
        contributions.screens = vec![ScreenSpec {
            id: "lsp_installer.picker".into(),
            layout: ScreenLayout::CenteredModal {
                width_pct: 80,
                height_pct: 80,
                title: Some("Install language servers".into()),
            },
        }];

        Manifest {
            id: PluginId::new(PLUGIN_ID).expect("valid built-in id"),
            name: "LSP installer".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: "Install curated language-server binaries via /lsp".into(),
            kind: PluginKind::Optional,
            contributions,
        }
    }

    async fn handle_slash(
        &mut self,
        name: &str,
        args: Vec<String>,
    ) -> Result<Vec<Effect>, PluginError> {
        if name != "lsp" {
            return Err(PluginError::SlashNotHandled(name.into()));
        }
        match args.first().map(String::as_str) {
            None => Ok(vec![Effect::OpenScreen {
                id: "lsp_installer.picker".into(),
                args: ScreenArgs::None,
            }]),
            Some("__install") => Ok(vec![]), // Task 20 wires the real install path.
            Some(other) => Ok(vec![Effect::PushNote {
                line: StyledLine::plain(format!(
                    "/lsp: unknown sub-command `{other}` — run `/lsp` with no args to open the picker"
                )),
            }]),
        }
    }

    fn create_screen(
        &self,
        id: &str,
        _args: ScreenArgs,
    ) -> Result<Box<dyn Screen>, PluginError> {
        match id {
            "lsp_installer.picker" => Ok(Box::new(LspPickerScreen::new())),
            other => Err(PluginError::ScreenNotFound(other.into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lsp_with_no_args_opens_picker() {
        let mut p = LspInstallerPlugin::new();
        let effs = p.handle_slash("lsp", vec![]).await.unwrap();
        match &effs[..] {
            [Effect::OpenScreen { id, .. }] => assert_eq!(id, "lsp_installer.picker"),
            other => panic!("expected OpenScreen, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unknown_subcommand_pushes_help_note() {
        let mut p = LspInstallerPlugin::new();
        let effs = p.handle_slash("lsp", vec!["bogus".into()]).await.unwrap();
        assert!(matches!(effs.as_slice(), [Effect::PushNote { .. }]));
    }

    #[tokio::test]
    async fn unrelated_slash_returns_slash_not_handled() {
        let mut p = LspInstallerPlugin::new();
        let err = p.handle_slash("not-lsp", vec![]).await.unwrap_err();
        assert!(matches!(err, PluginError::SlashNotHandled(_)));
    }

    #[test]
    fn manifest_advertises_slash_and_screen() {
        let p = LspInstallerPlugin::new();
        let m = p.manifest();
        assert!(m.contributions.slash_commands.iter().any(|s| s.name == "lsp"));
        assert!(
            m.contributions
                .screens
                .iter()
                .any(|s| s.id == "lsp_installer.picker")
        );
    }
}
