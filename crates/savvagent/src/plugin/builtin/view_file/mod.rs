//! `internal:view-file` — read-only file viewer.

pub mod screen;

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Effect, Manifest, Plugin, PluginError, PluginId, PluginKind, Screen, ScreenArgs,
    ScreenLayout, ScreenSpec, SlashSpec,
};

use screen::ViewFileScreen;

/// Built-in plugin that provides a read-only file viewer screen.
///
/// Registers the `/view <path>` slash command and the `view-file` screen.
pub struct ViewFilePlugin;

impl ViewFilePlugin {
    /// Creates a new `ViewFilePlugin` instance.
    pub fn new() -> Self {
        Self
    }
}

impl Default for ViewFilePlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for ViewFilePlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.slash_commands = vec![SlashSpec {
            name: "view".into(),
            summary: "Open a file in the viewer".into(),
            args_hint: Some("<path>".into()),
        }];
        contributions.screens = vec![ScreenSpec {
            id: "view-file".into(),
            layout: ScreenLayout::Fullscreen { hide_chrome: false },
        }];

        Manifest {
            id: PluginId::new("internal:view-file").expect("valid built-in id"),
            name: "View file".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: "Read-only file viewer".into(),
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
            .ok_or_else(|| PluginError::InvalidArgs("usage: /view <path>".into()))?;
        Ok(vec![Effect::OpenScreen {
            id: "view-file".into(),
            args: ScreenArgs::ViewFile { path },
        }])
    }

    fn create_screen(&self, id: &str, args: ScreenArgs) -> Result<Box<dyn Screen>, PluginError> {
        match (id, args) {
            ("view-file", ScreenArgs::ViewFile { path }) => {
                Ok(Box::new(ViewFileScreen::open(path)?))
            }
            (other, _) => Err(PluginError::ScreenNotFound(other.to_string())),
        }
    }
}
