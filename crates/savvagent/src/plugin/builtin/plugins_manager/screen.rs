//! `plugins.manager` Screen — list plugins with a per-row toggle.
//!
//! Rows are owned by the screen because the runtime injects them after
//! constructing the empty screen via the plugin's `create_screen`
//! callback (see `apply_effects::open_screen`). The screen itself never
//! reaches into `App` state; it only emits effects on key events.

use async_trait::async_trait;
use savvagent_plugin::{
    Effect, KeyCodePortable, KeyEventPortable, PluginError, PluginId, PluginKind, Region, Screen,
    StyledLine, StyledSpan, TextMods, ThemeColor,
};

/// Per-open instance of the plugins-manager modal.
pub(crate) struct PluginsManagerScreen {
    pub(crate) rows: Vec<PluginRow>,
    pub(crate) cursor: usize,
}

/// One row in [`PluginsManagerScreen`]. Cloned from the plugin's manifest
/// and the registry's enabled-set at open time; the screen mutates
/// `enabled` optimistically when the user toggles a row, then emits
/// [`Effect::TogglePlugin`] for the runtime to apply persistently.
pub(crate) struct PluginRow {
    pub(crate) id: PluginId,
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) kind: PluginKind,
    pub(crate) enabled: bool,
    pub(crate) contribution_summary: String,
}

impl PluginsManagerScreen {
    /// Construct an empty screen — used by the plugin's `create_screen`
    /// callback. `apply_effects::open_screen` then replaces the top of
    /// the stack with [`PluginsManagerScreen::with_rows`] once it has
    /// queried the registry.
    pub(crate) fn empty() -> Self {
        Self {
            rows: vec![],
            cursor: 0,
        }
    }

    /// Construct with a pre-populated row list. Used by the runtime after
    /// it builds rows from the registry + manifests.
    pub(crate) fn with_rows(rows: Vec<PluginRow>) -> Self {
        Self { rows, cursor: 0 }
    }
}

#[async_trait]
impl Screen for PluginsManagerScreen {
    fn id(&self) -> String {
        "plugins.manager".to_string()
    }

    fn render(&self, _region: Region) -> Vec<StyledLine> {
        let mut out = Vec::with_capacity(self.rows.len());
        if self.rows.is_empty() {
            out.push(StyledLine {
                spans: vec![StyledSpan {
                    text: "  (no plugins registered)".to_string(),
                    fg: Some(ThemeColor::Yellow),
                    bg: None,
                    modifiers: TextMods::default(),
                }],
            });
            return out;
        }
        for (i, row) in self.rows.iter().enumerate() {
            let marker = if i == self.cursor { "> " } else { "  " };
            let toggle = match (row.kind, row.enabled) {
                (PluginKind::Core, _) => "(core)",
                (PluginKind::Optional, true) => "[ on ]",
                (PluginKind::Optional, false) => "[ off]",
            };
            let color = if i == self.cursor {
                ThemeColor::Cyan
            } else {
                ThemeColor::White
            };
            // TextMods is not #[non_exhaustive], so FRU is safe here.
            let mods_active = TextMods {
                bold: i == self.cursor,
                ..Default::default()
            };
            out.push(StyledLine {
                spans: vec![
                    StyledSpan {
                        text: format!("{marker}{toggle} "),
                        fg: Some(color),
                        bg: None,
                        modifiers: mods_active,
                    },
                    StyledSpan {
                        text: format!("{:<28} v{}", row.name, row.version),
                        fg: Some(color),
                        bg: None,
                        modifiers: mods_active,
                    },
                    StyledSpan {
                        text: format!("  {}", row.contribution_summary),
                        fg: Some(ThemeColor::Gray),
                        bg: None,
                        modifiers: TextMods::default(),
                    },
                ],
            });
        }
        out
    }

    async fn on_key(&mut self, key: KeyEventPortable) -> Result<Vec<Effect>, PluginError> {
        match key.code {
            KeyCodePortable::Esc => Ok(vec![Effect::CloseScreen]),
            KeyCodePortable::Up => {
                self.cursor = self.cursor.saturating_sub(1);
                Ok(vec![])
            }
            KeyCodePortable::Down => {
                let max = self.rows.len().saturating_sub(1);
                if self.cursor < max {
                    self.cursor += 1;
                }
                Ok(vec![])
            }
            KeyCodePortable::Char(' ') | KeyCodePortable::Enter => {
                let Some(row) = self.rows.get_mut(self.cursor) else {
                    return Ok(vec![]);
                };
                if matches!(row.kind, PluginKind::Core) {
                    return Ok(vec![Effect::PushNote {
                        line: StyledLine {
                            spans: vec![StyledSpan {
                                text: "Core plugins cannot be disabled.".into(),
                                fg: Some(ThemeColor::Yellow),
                                bg: None,
                                modifiers: TextMods::default(),
                            }],
                        },
                    }]);
                }
                row.enabled = !row.enabled;
                Ok(vec![Effect::TogglePlugin {
                    id: row.id.clone(),
                    enabled: row.enabled,
                }])
            }
            _ => Ok(vec![]),
        }
    }

    fn tips(&self) -> Vec<StyledLine> {
        vec![StyledLine::plain(
            "Up/Down navigate · Space/Enter toggle · Esc close",
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
    async fn toggling_core_emits_warning_push_note() {
        let rows = vec![PluginRow {
            id: PluginId::new("internal:home-footer").expect("valid"),
            name: "Home footer".into(),
            version: "0".into(),
            kind: PluginKind::Core,
            enabled: true,
            contribution_summary: "".into(),
        }];
        let mut s = PluginsManagerScreen::with_rows(rows);
        let effs = s.on_key(key(KeyCodePortable::Enter)).await.unwrap();
        match &effs[0] {
            Effect::PushNote { line } => {
                let joined: String = line.spans.iter().map(|s| s.text.clone()).collect();
                assert!(
                    joined.contains("Core"),
                    "expected Core warning, got {joined:?}"
                );
            }
            other => panic!("expected PushNote, got {other:?}"),
        }
        // The row stayed enabled — Core is not flipped optimistically.
        assert!(s.rows[0].enabled);
    }

    #[tokio::test]
    async fn toggling_optional_emits_toggleplugin_effect() {
        let rows = vec![PluginRow {
            id: PluginId::new("internal:provider-anthropic").expect("valid"),
            name: "Anthropic".into(),
            version: "0".into(),
            kind: PluginKind::Optional,
            enabled: true,
            contribution_summary: "".into(),
        }];
        let mut s = PluginsManagerScreen::with_rows(rows);
        let effs = s.on_key(key(KeyCodePortable::Enter)).await.unwrap();
        match &effs[0] {
            Effect::TogglePlugin { id, enabled } => {
                assert_eq!(id.as_str(), "internal:provider-anthropic");
                assert!(!*enabled);
            }
            other => panic!("expected TogglePlugin, got {other:?}"),
        }
        // Optimistic local flip: the row should now read disabled.
        assert!(!s.rows[0].enabled);
    }

    #[tokio::test]
    async fn space_also_toggles_optional() {
        let rows = vec![PluginRow {
            id: PluginId::new("internal:provider-openai").expect("valid"),
            name: "OpenAI".into(),
            version: "0".into(),
            kind: PluginKind::Optional,
            enabled: false,
            contribution_summary: "".into(),
        }];
        let mut s = PluginsManagerScreen::with_rows(rows);
        let effs = s.on_key(key(KeyCodePortable::Char(' '))).await.unwrap();
        match &effs[0] {
            Effect::TogglePlugin { enabled, .. } => assert!(*enabled),
            other => panic!("expected TogglePlugin, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn esc_emits_close_screen() {
        let mut s = PluginsManagerScreen::empty();
        let effs = s.on_key(key(KeyCodePortable::Esc)).await.unwrap();
        assert!(matches!(effs[0], Effect::CloseScreen));
    }

    #[tokio::test]
    async fn down_arrow_advances_cursor_up_to_last_row() {
        let rows = vec![
            PluginRow {
                id: PluginId::new("internal:a").expect("valid"),
                name: "A".into(),
                version: "0".into(),
                kind: PluginKind::Core,
                enabled: true,
                contribution_summary: "".into(),
            },
            PluginRow {
                id: PluginId::new("internal:b").expect("valid"),
                name: "B".into(),
                version: "0".into(),
                kind: PluginKind::Optional,
                enabled: true,
                contribution_summary: "".into(),
            },
        ];
        let mut s = PluginsManagerScreen::with_rows(rows);
        assert_eq!(s.cursor, 0);
        let _ = s.on_key(key(KeyCodePortable::Down)).await.unwrap();
        assert_eq!(s.cursor, 1);
        // Cursor saturates at the last row instead of wrapping.
        let _ = s.on_key(key(KeyCodePortable::Down)).await.unwrap();
        assert_eq!(s.cursor, 1);
    }

    #[test]
    fn id_is_plugins_manager() {
        let s = PluginsManagerScreen::empty();
        assert_eq!(s.id(), "plugins.manager");
    }
}
