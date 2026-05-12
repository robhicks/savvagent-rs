//! Filterable list-of-commands modal.

use async_trait::async_trait;
use savvagent_plugin::{
    Effect, KeyCodePortable, KeyEventPortable, PluginError, Region, Screen, StyledLine, StyledSpan,
    TextMods, ThemeColor,
};

/// Static list of commands shown by default. PR 8's manager screen builds
/// the same list at open-time from the runtime's slash index for accuracy.
///
/// Each tuple is `(name, description, needs_arg)`. Commands flagged
/// `needs_arg == true` prefill the textarea with `"/cmd "` on Enter
/// instead of firing immediately — so the user can supply the missing
/// argument (typically via the `@` file picker) before submitting. Without
/// this, `/view` and `/edit` would error out on entry with their "usage:"
/// `PluginError::InvalidArgs`.
const VISIBLE_COMMANDS: &[(&str, &str, bool)] = &[
    ("theme", "Switch color theme", false),
    ("clear", "Clear conversation log", false),
    ("save", "Save transcript", false),
    ("model", "Switch active model", false),
    ("connect", "Pick a provider to connect", false),
    ("resume", "Resume an earlier transcript", false),
    ("view", "Open a file in the viewer", true),
    ("edit", "Open a file in the editor", true),
    ("plugins", "Open the plugin manager", false),
    ("splash", "Show the splash screen", false),
    ("quit", "Quit savvagent", false),
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

    fn filtered(&self) -> Vec<(usize, &'static str, &'static str, bool)> {
        let f = self.filter.to_ascii_lowercase();
        VISIBLE_COMMANDS
            .iter()
            .enumerate()
            .filter(|(_, (name, _, _))| name.contains(&f))
            .map(|(i, (n, d, a))| (i, *n, *d, *a))
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
        for (i, (_, name, desc, _)) in self.filtered().iter().enumerate() {
            let marker = if i == self.cursor { "▶ " } else { "  " };
            lines.push(StyledLine {
                spans: vec![
                    StyledSpan {
                        text: format!("{marker}/{name:<12}"),
                        fg: Some(if i == self.cursor {
                            ThemeColor::Accent
                        } else {
                            ThemeColor::Fg
                        }),
                        bg: None,
                        modifiers: TextMods {
                            bold: i == self.cursor,
                            ..Default::default()
                        },
                    },
                    StyledSpan {
                        text: desc.to_string(),
                        fg: Some(ThemeColor::Muted),
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
                let Some((_, name, _, needs_arg)) = self.filtered().get(self.cursor).cloned()
                else {
                    return Ok(vec![Effect::CloseScreen]);
                };
                if needs_arg {
                    // Don't fire the slash with empty args (which would
                    // error with "usage: /<cmd> <path>"). Instead, close
                    // the palette and seed the textarea so the user can
                    // complete the line — typically via the `@` file
                    // picker — before pressing Enter.
                    Ok(vec![Effect::Stack(vec![
                        Effect::CloseScreen,
                        Effect::PrefillInput {
                            text: format!("/{name} "),
                        },
                    ])])
                } else {
                    Ok(vec![Effect::Stack(vec![
                        Effect::CloseScreen,
                        Effect::RunSlash {
                            name: name.to_string(),
                            args: vec![],
                        },
                    ])])
                }
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
        assert!(filtered.iter().all(|(_, n, _, _)| n.contains('m')));
        assert_eq!(p.cursor, 0);
    }

    /// Selecting a `needs_arg` command (e.g. `/view`) must seed the
    /// textarea with `"/view "` rather than firing the slash with empty
    /// args (which would error out with "usage: /view <path>"). Regression
    /// test for hotfix bug #1.
    #[tokio::test]
    async fn enter_on_needs_arg_command_emits_prefill_not_runslash() {
        let mut p = PaletteScreen::new();
        // Filter down to `view` so the cursor sits on a needs_arg entry.
        for ch in "view".chars() {
            p.on_key(key(KeyCodePortable::Char(ch))).await.unwrap();
        }
        let effs = p.on_key(key(KeyCodePortable::Enter)).await.unwrap();
        match effs.first() {
            Some(Effect::Stack(children)) => {
                assert!(matches!(children[0], Effect::CloseScreen));
                match &children[1] {
                    Effect::PrefillInput { text } => assert_eq!(text, "/view "),
                    other => panic!("expected PrefillInput, got {other:?}"),
                }
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// `quit` is reachable from the palette (post-v0.9 regression).
    #[tokio::test]
    async fn quit_is_listed_and_runs_via_runslash() {
        let mut p = PaletteScreen::new();
        for ch in "quit".chars() {
            p.on_key(key(KeyCodePortable::Char(ch))).await.unwrap();
        }
        // After typing "quit", at least one entry must match.
        assert!(!p.filtered().is_empty(), "palette should list /quit");
        let effs = p.on_key(key(KeyCodePortable::Enter)).await.unwrap();
        match effs.first() {
            Some(Effect::Stack(children)) => {
                assert!(matches!(children[0], Effect::CloseScreen));
                match &children[1] {
                    Effect::RunSlash { name, args } => {
                        assert_eq!(name, "quit");
                        assert!(args.is_empty());
                    }
                    other => panic!("expected RunSlash, got {other:?}"),
                }
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn esc_closes() {
        let mut p = PaletteScreen::new();
        let effs = p.on_key(key(KeyCodePortable::Esc)).await.unwrap();
        assert!(matches!(effs[0], Effect::CloseScreen));
    }
}
