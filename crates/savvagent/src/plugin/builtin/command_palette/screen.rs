//! Filterable list-of-commands modal.

use async_trait::async_trait;
use savvagent_plugin::{
    Effect, KeyCodePortable, KeyEventPortable, PluginError, Region, Screen, StyledLine, StyledSpan,
    TextMods, ThemeColor,
};

/// One row in the palette: a slash command name, its summary, and whether
/// it requires an argument (selecting a `needs_arg == true` row prefills
/// the textarea with `"/cmd "` instead of dispatching the slash immediately).
#[derive(Debug, Clone)]
pub struct PaletteCommand {
    /// Slash command name without the leading `/`.
    pub name: String,
    /// One-line summary from the plugin's `SlashSpec.summary`.
    pub description: String,
    /// `true` if the command's `SlashSpec.args_hint` is `Some(_)`.
    pub needs_arg: bool,
}

/// Modal screen that lets the user filter and run slash commands by name.
///
/// The command list is populated by `apply_effects::open_screen` from the
/// runtime's [`crate::plugin::manifests::Indexes::slash`] map and each
/// owning plugin's manifest — so disabled plugins' slashes don't appear,
/// and new plugins are picked up without touching this file.
///
/// On `Enter`:
/// - `needs_arg == true` rows emit `Stack([CloseScreen, PrefillInput])`
///   so the user can complete the slash (typically via the `@` file picker).
/// - Other rows emit `Stack([CloseScreen, RunSlash])`.
pub struct PaletteScreen {
    filter: String,
    cursor: usize,
    commands: Vec<PaletteCommand>,
}

impl PaletteScreen {
    /// Empty palette with no rows; only useful before
    /// `apply_effects::open_screen` replaces it with a populated screen
    /// via [`Self::with_commands`].
    pub fn empty() -> Self {
        Self {
            filter: String::new(),
            cursor: 0,
            commands: Vec::new(),
        }
    }

    /// Populate the palette with `commands` (already sorted by the caller).
    pub fn with_commands(commands: Vec<PaletteCommand>) -> Self {
        Self {
            filter: String::new(),
            cursor: 0,
            commands,
        }
    }

    fn filtered(&self) -> Vec<(usize, &PaletteCommand)> {
        let f = self.filter.to_ascii_lowercase();
        self.commands
            .iter()
            .enumerate()
            .filter(|(_, c)| c.name.contains(&f))
            .collect()
    }
}

impl Default for PaletteScreen {
    fn default() -> Self {
        Self::empty()
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
        if self.commands.is_empty() {
            lines.push(StyledLine {
                spans: vec![StyledSpan {
                    text: rust_i18n::t!("picker.command-palette.no-commands").to_string(),
                    fg: Some(ThemeColor::Muted),
                    bg: None,
                    modifiers: TextMods::default(),
                }],
            });
            return lines;
        }
        for (visual_idx, (_, cmd)) in self.filtered().iter().enumerate() {
            let marker = if visual_idx == self.cursor {
                "▶ "
            } else {
                "  "
            };
            lines.push(StyledLine {
                spans: vec![
                    StyledSpan {
                        text: format!("{marker}/{:<12}", cmd.name),
                        fg: Some(if visual_idx == self.cursor {
                            ThemeColor::Accent
                        } else {
                            ThemeColor::Fg
                        }),
                        bg: None,
                        modifiers: TextMods {
                            bold: visual_idx == self.cursor,
                            ..Default::default()
                        },
                    },
                    StyledSpan {
                        text: cmd.description.clone(),
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
                let filtered = self.filtered();
                let Some((_, cmd)) = filtered.get(self.cursor).cloned() else {
                    return Ok(vec![Effect::CloseScreen]);
                };
                let name = cmd.name.clone();
                if cmd.needs_arg {
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
                        Effect::RunSlash { name, args: vec![] },
                    ])])
                }
            }
            _ => Ok(vec![]),
        }
    }

    fn tips(&self) -> Vec<StyledLine> {
        vec![StyledLine::plain(
            rust_i18n::t!("picker.command-palette.tips").to_string(),
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

    fn cmd(name: &str, needs_arg: bool) -> PaletteCommand {
        PaletteCommand {
            name: name.into(),
            description: format!("{name} description"),
            needs_arg,
        }
    }

    fn fixture() -> PaletteScreen {
        // Alphabetically sorted — matches apply_effects::open_screen ordering.
        PaletteScreen::with_commands(vec![
            cmd("clear", false),
            cmd("edit", true),
            cmd("quit", false),
            cmd("theme", false),
            cmd("view", true),
        ])
    }

    #[tokio::test]
    async fn enter_emits_close_then_runslash_for_first_match() {
        let mut p = fixture();
        let effs = p.on_key(key(KeyCodePortable::Enter)).await.unwrap();
        match effs.first() {
            Some(Effect::Stack(children)) => {
                assert!(matches!(children[0], Effect::CloseScreen));
                match &children[1] {
                    Effect::RunSlash { name, .. } => assert_eq!(name, "clear"),
                    other => panic!("expected RunSlash, got {other:?}"),
                }
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn typing_filters_and_resets_cursor() {
        let mut p = fixture();
        p.on_key(key(KeyCodePortable::Char('e'))).await.unwrap();
        let filtered = p.filtered();
        assert!(filtered.iter().all(|(_, c)| c.name.contains('e')));
        assert_eq!(p.cursor, 0);
    }

    /// Selecting a `needs_arg` command (e.g. `/view`) must seed the
    /// textarea with `"/view "` rather than firing the slash with empty
    /// args (which would error out with "usage: /view <path>"). Regression
    /// test for hotfix bug #1.
    #[tokio::test]
    async fn enter_on_needs_arg_command_emits_prefill_not_runslash() {
        let mut p = fixture();
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
        let mut p = fixture();
        for ch in "quit".chars() {
            p.on_key(key(KeyCodePortable::Char(ch))).await.unwrap();
        }
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

    /// Empty command set renders a placeholder and Enter is a no-op-ish close.
    #[tokio::test]
    async fn empty_palette_renders_placeholder_and_enter_closes() {
        let mut p = PaletteScreen::empty();
        let lines = p.render(Region {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        });
        let joined: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.text.clone()))
            .collect();
        assert!(
            joined.contains(rust_i18n::t!("picker.command-palette.no-commands").as_ref()),
            "empty render should show placeholder, got: {joined}"
        );
        let effs = p.on_key(key(KeyCodePortable::Enter)).await.unwrap();
        assert!(matches!(effs[0], Effect::CloseScreen));
    }

    #[tokio::test]
    async fn esc_closes() {
        let mut p = fixture();
        let effs = p.on_key(key(KeyCodePortable::Esc)).await.unwrap();
        assert!(matches!(effs[0], Effect::CloseScreen));
    }
}
