//! `internal:edit-file` — basic in-TUI editor.

pub mod screen;

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Effect, Manifest, Plugin, PluginError, PluginId, PluginKind, Screen, ScreenArgs,
    ScreenLayout, ScreenSpec, SlashSpec,
};

use screen::EditFileScreen;

/// Built-in plugin that provides a basic in-TUI file editor screen.
///
/// Registers the `/edit <path>` slash command and the `edit-file` screen.
pub struct EditFilePlugin;

impl EditFilePlugin {
    /// Creates a new `EditFilePlugin` instance.
    pub fn new() -> Self {
        Self
    }
}

impl Default for EditFilePlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for EditFilePlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.slash_commands = vec![SlashSpec {
            name: "edit".into(),
            summary: "Open a file in the editor".into(),
            args_hint: Some("<path>".into()),
        }];
        contributions.screens = vec![ScreenSpec {
            id: "edit-file".into(),
            layout: ScreenLayout::Fullscreen { hide_chrome: false },
        }];

        Manifest {
            id: PluginId::new("internal:edit-file").expect("valid built-in id"),
            name: "Edit file".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: "Basic in-TUI file editor".into(),
            kind: PluginKind::Optional,
            contributions,
        }
    }

    async fn handle_slash(
        &mut self,
        _: &str,
        args: Vec<String>,
    ) -> Result<Vec<Effect>, PluginError> {
        let path = args
            .into_iter()
            .next()
            .ok_or_else(|| PluginError::InvalidArgs("usage: /edit <path>".into()))?;
        Ok(vec![Effect::OpenScreen {
            id: "edit-file".into(),
            args: ScreenArgs::EditFile { path },
        }])
    }

    fn create_screen(&self, id: &str, args: ScreenArgs) -> Result<Box<dyn Screen>, PluginError> {
        match (id, args) {
            ("edit-file", ScreenArgs::EditFile { path }) => {
                Ok(Box::new(EditFileScreen::open(path)?))
            }
            (other, _) => Err(PluginError::ScreenNotFound(other.to_string())),
        }
    }
}
