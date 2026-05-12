//! Filterable list-of-commands modal.

use async_trait::async_trait;
use savvagent_plugin::{
    Effect, KeyCodePortable, KeyEventPortable, PluginError, Region, Screen, StyledLine, StyledSpan,
    TextMods, ThemeColor,
};

/// Static list of commands shown by default. PR 8's manager screen builds
/// the same list at open-time from the runtime's slash index for accuracy.
const VISIBLE_COMMANDS: &[(&str, &str)] = &[
    ("theme", "Switch color theme"),
    ("clear", "Clear conversation log"),
    ("save", "Save transcript"),
    ("model", "Switch active model"),
    ("connect", "Pick a provider to connect"),
    ("resume", "Resume an earlier transcript"),
    ("view", "Open a file in the viewer"),
    ("edit", "Open a file in the editor"),
    ("plugins", "Open the plugin manager"),
    ("splash", "Show the splash screen"),
];

/// Modal screen that lets the user filter and run slash commands by name.
///
/// Maintains a text `filter` and a `cursor` index into the filtered list.
/// On `Enter`, emits `Effect::Stack([CloseScreen, RunSlash])` so the palette
/// closes before the target slash command's screen is pushed.
pub struct PaletteScreen {
    filter: String,
    cursor: usize,
}

impl PaletteScreen {
    /// Create a new `PaletteScreen` with an empty filter and cursor at the top.
    pub fn new() -> Self {
        Self {
            filter: String::new(),
            cursor: 0,
        }
    }

    fn filtered(&self) -> Vec<(usize, &'static str, &'static str)> {
        let f = self.filter.to_ascii_lowercase();
        VISIBLE_COMMANDS
            .iter()
            .enumerate()
            .filter(|(_, (name, _))| name.contains(&f))
            .map(|(i, (n, d))| (i, *n, *d))
            .collect()
    }
}

impl Default for PaletteScreen {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Screen for PaletteScreen {
    fn id(&self) -> String {
        "palette".to_string()
    }

    fn render(&self, _region: Region) -> Vec<StyledLine> {
        let mut lines = vec![
            StyledLine::plain(format!("> {}", self.filter)),
            StyledLine::plain(""),
        ];
        for (i, (_, name, desc)) in self.filtered().iter().enumerate() {
            let marker = if i == self.cursor { "▶ " } else { "  " };
            lines.push(StyledLine {
                spans: vec![
                    StyledSpan {
                        text: format!("{marker}/{name:<12}"),
                        fg: Some(if i == self.cursor {
                            ThemeColor::Cyan
                        } else {
                            ThemeColor::White
                        }),
                        bg: None,
                        modifiers: TextMods {
                            bold: i == self.cursor,
                            ..Default::default()
                        },
                    },
                    StyledSpan {
                        text: desc.to_string(),
                        fg: Some(ThemeColor::Gray),
                        bg: None,
                        modifiers: TextMods::default(),
                    },
                ],
            });
        }
        lines
    }

    async fn on_key(&mut self, key: KeyEventPortable) -> Result<Vec<Effect>, PluginError> {
        match key.code {
            KeyCodePortable::Esc => Ok(vec![Effect::CloseScreen]),
            KeyCodePortable::Up => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                }
                Ok(vec![])
            }
            KeyCodePortable::Down => {
                let max = self.filtered().len().saturating_sub(1);
                if self.cursor < max {
                    self.cursor += 1;
                }
                Ok(vec![])
            }
            KeyCodePortable::Backspace => {
                self.filter.pop();
                self.cursor = 0;
                Ok(vec![])
            }
            KeyCodePortable::Char(c) => {
                self.filter.push(c);
                self.cursor = 0;
                Ok(vec![])
            }
            KeyCodePortable::Enter => {
                let Some((_, name, _)) = self.filtered().get(self.cursor).cloned() else {
                    return Ok(vec![Effect::CloseScreen]);
                };
                Ok(vec![Effect::Stack(vec![
                    Effect::CloseScreen,
                    Effect::RunSlash {
                        name: name.to_string(),
                        args: vec![],
                    },
                ])])
            }
            _ => Ok(vec![]),
        }
    }

    fn tips(&self) -> Vec<StyledLine> {
        vec![StyledLine::plain(
            "↑/↓ navigate · type to filter · Enter run · Esc cancel",
        )]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use savvagent_plugin::KeyMods;

    fn key(c: KeyCodePortable) -> KeyEventPortable {
        KeyEventPortable {
            code: c,
            modifiers: KeyMods::default(),
        }
    }

    #[tokio::test]
    async fn enter_emits_close_then_runslash_for_first_match() {
        let mut p = PaletteScreen::new();
        let effs = p.on_key(key(KeyCodePortable::Enter)).await.unwrap();
        match effs.first() {
            Some(Effect::Stack(children)) => {
                assert!(matches!(children[0], Effect::CloseScreen));
                match &children[1] {
                    Effect::RunSlash { name, .. } => assert_eq!(name, "theme"),
                    _ => panic!(),
                }
            }
            other => panic!("unexpected: {:?}", other),
        }
    }

    #[tokio::test]
    async fn typing_filters_and_resets_cursor() {
        let mut p = PaletteScreen::new();
        p.on_key(key(KeyCodePortable::Char('m'))).await.unwrap();
        let filtered = p.filtered();
        assert!(filtered.iter().all(|(_, n, _)| n.contains('m')));
        assert_eq!(p.cursor, 0);
    }

    #[tokio::test]
    async fn esc_closes() {
        let mut p = PaletteScreen::new();
        let effs = p.on_key(key(KeyCodePortable::Esc)).await.unwrap();
        assert!(matches!(effs[0], Effect::CloseScreen));
    }
}
