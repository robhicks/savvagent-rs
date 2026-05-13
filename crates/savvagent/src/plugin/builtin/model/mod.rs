//! `internal:model` — toggles the active provider's model.
//!
//! v0.9 ships a placeholder PushNote; the rotate-to-next-model behavior
//! wires up in PR 6 with the provider plugins.

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Effect, Manifest, Plugin, PluginError, PluginId, PluginKind, SlashSpec,
    StyledLine,
};

/// Plugin that registers the `/model` slash command.
///
/// In v0.9 this emits a [`Effect::PushNote`] explaining that the rotate
/// behavior becomes functional in PR 6 when the provider plugins land. The
/// manifest is registered now so the command palette lists `/model` across
/// the PR 5–PR 6 work window.
pub struct ModelPlugin;

impl ModelPlugin {
    /// Construct a new [`ModelPlugin`].
    pub fn new() -> Self {
        Self
    }
}

impl Default for ModelPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for ModelPlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.slash_commands = vec![SlashSpec {
            name: "model".into(),
            summary: rust_i18n::t!("slash.model-summary").to_string(),
            args_hint: None,
        }];
        Manifest {
            id: PluginId::new("internal:model").expect("valid built-in id"),
            name: "Switch model".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: rust_i18n::t!("plugin.model-description").to_string(),
            kind: PluginKind::Optional,
            contributions,
        }
    }

    async fn handle_slash(&mut self, _: &str, _: Vec<String>) -> Result<Vec<Effect>, PluginError> {
        Ok(vec![Effect::PushNote {
            line: StyledLine::plain("(switch-model wired in PR 6 with the provider plugins)"),
        }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn slash_returns_a_note() {
        let mut p = ModelPlugin::new();
        let effs = p.handle_slash("model", vec![]).await.unwrap();
        assert!(matches!(effs[0], Effect::PushNote { .. }));
    }
}
