//! `internal:save` — save the active transcript.

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Effect, Manifest, Plugin, PluginError, PluginId, PluginKind, SlashSpec,
    StyledLine,
};

/// Plugin that registers the `/save` slash command.
///
/// `/save [path]` emits a [`Effect::Stack`] containing
/// [`Effect::SaveTranscript`] followed by a [`Effect::PushNote`] confirming
/// the path. When no path argument is given, [`default_transcript_path`]
/// generates a timestamped filename in the current directory.
pub struct SavePlugin;

impl SavePlugin {
    /// Construct a new [`SavePlugin`].
    pub fn new() -> Self {
        Self
    }
}

impl Default for SavePlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for SavePlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.slash_commands = vec![SlashSpec {
            name: "save".into(),
            summary: "Save transcript to a path".into(),
            args_hint: Some("[path]".into()),
        }];
        Manifest {
            id: PluginId::new("internal:save").expect("valid built-in id"),
            name: "Save transcript".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: "Save the active conversation to disk".into(),
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
            .unwrap_or_else(default_transcript_path);
        Ok(vec![Effect::Stack(vec![
            Effect::SaveTranscript { path: path.clone() },
            Effect::PushNote {
                line: StyledLine::plain(format!("Transcript saved to {path}")),
            },
        ])])
    }
}

/// Returns a timestamped default transcript filename in the current directory.
///
/// Falls back to `./transcript-0.json` if the system clock is unavailable.
fn default_transcript_path() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("./transcript-{secs}.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn save_with_explicit_path_uses_it() {
        let mut p = SavePlugin::new();
        let effs = p
            .handle_slash("save", vec!["/tmp/x.json".into()])
            .await
            .unwrap();
        match &effs[0] {
            Effect::Stack(children) => {
                assert!(
                    matches!(&children[0], Effect::SaveTranscript { path } if path == "/tmp/x.json")
                );
            }
            _ => panic!(),
        }
    }
}
