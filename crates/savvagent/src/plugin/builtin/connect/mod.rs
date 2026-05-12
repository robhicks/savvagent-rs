//! `internal:connect` — provider picker. With no args, opens the picker.
//! With one arg, routes to the named provider's connect slash.

pub mod screen;

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Effect, Manifest, Plugin, PluginError, PluginId, PluginKind, Screen, ScreenArgs,
    ScreenLayout, ScreenSpec, SlashSpec,
};

use screen::ConnectPickerScreen;

/// Core plugin that exposes `/connect [provider]`.
///
/// With no args, pushes the `connect.picker` screen so the user can choose
/// a provider interactively. With one arg, routes directly to the named
/// provider's connect slash (e.g. `/connect anthropic`).
pub struct ConnectPlugin;

impl ConnectPlugin {
    /// Construct a new `ConnectPlugin`.
    pub fn new() -> Self {
        Self
    }
}

impl Default for ConnectPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for ConnectPlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.slash_commands = vec![SlashSpec {
            name: "connect".into(),
            summary: "Pick a provider to connect".into(),
            args_hint: Some("[provider]".into()),
        }];
        contributions.screens = vec![ScreenSpec {
            id: "connect.picker".into(),
            layout: ScreenLayout::CenteredModal {
                width_pct: 50,
                height_pct: 50,
                title: Some("Connect to a provider".into()),
            },
        }];

        Manifest {
            id: PluginId::new("internal:connect").expect("valid built-in id"),
            name: "Connect".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: "Provider picker".into(),
            kind: PluginKind::Core,
            contributions,
        }
    }

    async fn handle_slash(
        &mut self,
        _: &str,
        args: Vec<String>,
    ) -> Result<Vec<Effect>, PluginError> {
        if let Some(provider) = args.into_iter().next() {
            // Direct route: /connect anthropic -> internal:provider-anthropic's connect slash.
            Ok(vec![Effect::RunSlash {
                name: format!("connect {provider}"),
                args: vec![],
            }])
        } else {
            Ok(vec![Effect::OpenScreen {
                id: "connect.picker".into(),
                args: ScreenArgs::ConnectPicker,
            }])
        }
    }

    fn create_screen(&self, id: &str, args: ScreenArgs) -> Result<Box<dyn Screen>, PluginError> {
        match (id, args) {
            ("connect.picker", ScreenArgs::ConnectPicker) => {
                Ok(Box::new(ConnectPickerScreen::new()))
            }
            (other, _) => Err(PluginError::ScreenNotFound(other.to_string())),
        }
    }
}
