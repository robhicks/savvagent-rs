//! Transcript picker — Enter opens via `/view <path>` for v0.9.

use async_trait::async_trait;
use savvagent_plugin::{
    Effect, KeyCodePortable, KeyEventPortable, PluginError, Region, Screen, StyledLine, StyledSpan,
    TextMods, ThemeColor, TranscriptHandle,
};

/// Fullscreen-modal picker that lists saved transcripts and lets the user
/// open one via `/view <path>` (v0.9). Full transcript-replay into the log
/// is deferred to a later milestone.
#[derive(Debug)]
pub struct ResumePickerScreen {
    items: Vec<TranscriptHandle>,
    cursor: usize,
}

impl ResumePickerScreen {
    /// Construct a picker pre-loaded with the given transcript handles.
    pub fn new(items: Vec<TranscriptHandle>) -> Self {
        Self { items, cursor: 0 }
    }
}

#[async_trait]
impl Screen for ResumePickerScreen {
    fn id(&self) -> String {
        "resume.picker".to_string()
    }

    fn render(&self, _region: Region) -> Vec<StyledLine> {
        if self.items.is_empty() {
            return vec![StyledLine {
                spans: vec![StyledSpan {
                    text: "No saved transcripts found in this directory.".into(),
                    fg: Some(ThemeColor::Yellow),
                    bg: None,
                    modifiers: TextMods::default(),
                }],
            }];
        }
        self.items
            .iter()
            .enumerate()
            .map(|(i, h)| {
                let marker = if i == self.cursor { "▶ " } else { "  " };
                StyledLine::plain(format!("{marker}{}", h.label))
            })
            .collect()
    }

    async fn on_key(&mut self, key: KeyEventPortable) -> Result<Vec<Effect>, PluginError> {
        match key.code {
            KeyCodePortable::Esc => Ok(vec![Effect::CloseScreen]),
            KeyCodePortable::Up => {
                self.cursor = self.cursor.saturating_sub(1);
                Ok(vec![])
            }
            KeyCodePortable::Down => {
                let max = self.items.len().saturating_sub(1);
                if self.cursor < max {
                    self.cursor += 1;
                }
                Ok(vec![])
            }
            KeyCodePortable::Enter => {
                let Some(h) = self.items.get(self.cursor) else {
                    return Ok(vec![Effect::CloseScreen]);
                };
                Ok(vec![Effect::Stack(vec![
                    Effect::CloseScreen,
                    Effect::RunSlash {
                        name: "view".into(),
                        args: vec![h.id.clone()],
                    },
                ])])
            }
            _ => Ok(vec![]),
        }
    }

    fn tips(&self) -> Vec<StyledLine> {
        vec![StyledLine::plain("↑/↓ navigate · Enter open · Esc cancel")]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use savvagent_plugin::{KeyMods, Timestamp};

    fn key(c: KeyCodePortable) -> KeyEventPortable {
        KeyEventPortable {
            code: c,
            modifiers: KeyMods::default(),
        }
    }

    #[tokio::test]
    async fn empty_renders_helpful_message() {
        let s = ResumePickerScreen::new(vec![]);
        let lines = s.render(Region {
            x: 0,
            y: 0,
            width: 60,
            height: 10,
        });
        let joined: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.text.clone()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("No saved transcripts"));
    }

    #[tokio::test]
    async fn enter_routes_to_view_with_path() {
        let mut s = ResumePickerScreen::new(vec![TranscriptHandle {
            id: "transcript-x.json".into(),
            label: "transcript-x.json".into(),
            saved_at: Timestamp { secs: 0, nanos: 0 },
        }]);
        let effs = s.on_key(key(KeyCodePortable::Enter)).await.unwrap();
        match &effs[0] {
            Effect::Stack(children) => match &children[1] {
                Effect::RunSlash { name, args } => {
                    assert_eq!(name, "view");
                    assert_eq!(args[0], "transcript-x.json");
                }
                _ => panic!(),
            },
            _ => panic!(),
        }
    }
}
