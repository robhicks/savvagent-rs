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
            // Validate provider id format: lowercase alphanumeric with optional
            // dashes/underscores. Reject anything with spaces or special chars
            // before constructing a slash name we can't route.
            if !provider
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
            {
                return Err(PluginError::InvalidArgs(format!(
                    "invalid provider id: {provider:?}; expected lowercase ASCII with optional - or _"
                )));
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn handle_slash_rejects_provider_with_spaces() {
        let mut p = ConnectPlugin::new();
        let err = p
            .handle_slash("connect", vec!["bad provider".into()])
            .await
            .unwrap_err();
        assert!(
            matches!(err, PluginError::InvalidArgs(_)),
            "expected InvalidArgs, got {err:?}"
        );
    }

    #[tokio::test]
    async fn handle_slash_rejects_provider_with_uppercase() {
        let mut p = ConnectPlugin::new();
        let err = p
            .handle_slash("connect", vec!["Anthropic".into()])
            .await
            .unwrap_err();
        assert!(matches!(err, PluginError::InvalidArgs(_)));
    }

    #[tokio::test]
    async fn handle_slash_accepts_valid_provider_ids() {
        let mut p = ConnectPlugin::new();
        for id in ["anthropic", "gemini-pro", "my_provider", "provider2"] {
            let effs = p.handle_slash("connect", vec![id.into()]).await.unwrap();
            assert!(
                matches!(&effs[0], Effect::RunSlash { name, .. } if name == &format!("connect {id}")),
                "unexpected effects for id={id}: {effs:?}"
            );
        }
    }
}
