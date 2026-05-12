//! Basic in-TUI file editor screen.

use async_trait::async_trait;
use savvagent_plugin::{
    Effect, KeyCodePortable, KeyEventPortable, PluginError, Region, Screen, StyledLine, StyledSpan,
    TextMods, ThemeColor,
};

/// A fullscreen editor for a single file.
///
/// Supports arrow-key navigation, character insertion, Backspace deletion,
/// Enter to split lines, Ctrl-S to save, and Esc to close (discarding
/// unsaved changes for v0.9).
#[derive(Debug)]
pub struct EditFileScreen {
    path: String,
    lines: Vec<String>,
    cursor_row: usize,
    cursor_col: usize,
    dirty: bool,
}

impl EditFileScreen {
    /// Reads `path` from disk and constructs a ready-to-edit screen.
    ///
    /// Returns [`PluginError::InvalidArgs`] if the file cannot be read.
    pub fn open(path: String) -> Result<Self, PluginError> {
        let body = std::fs::read_to_string(&path)
            .map_err(|e| PluginError::InvalidArgs(format!("{path}: {e}")))?;
        let lines = if body.is_empty() {
            vec![String::new()]
        } else {
            body.lines().map(|l| l.to_string()).collect()
        };
        Ok(Self {
            path,
            lines,
            cursor_row: 0,
            cursor_col: 0,
            dirty: false,
        })
    }

    fn save(&mut self) -> Result<(), PluginError> {
        let body = self.lines.join("\n") + "\n";
        std::fs::write(&self.path, body)
            .map_err(|e| PluginError::Internal(format!("write {}: {e}", self.path)))?;
        self.dirty = false;
        Ok(())
    }
}

#[async_trait]
impl Screen for EditFileScreen {
    fn id(&self) -> String {
        "edit-file".to_string()
    }

    fn render(&self, region: Region) -> Vec<StyledLine> {
        let dirty_marker = if self.dirty { " [+]" } else { "" };
        let mut out = vec![StyledLine {
            spans: vec![StyledSpan {
                text: format!("{}{}", self.path, dirty_marker),
                fg: Some(ThemeColor::Cyan),
                bg: None,
                modifiers: TextMods {
                    bold: true,
                    ..Default::default()
                },
            }],
        }];
        let take = region.height.saturating_sub(1) as usize;
        for (i, line) in self.lines.iter().take(take).enumerate() {
            let marker = if i == self.cursor_row { "→ " } else { "  " };
            out.push(StyledLine::plain(format!("{marker}{line}")));
        }
        out
    }

    async fn on_key(&mut self, key: KeyEventPortable) -> Result<Vec<Effect>, PluginError> {
        match key.code {
            KeyCodePortable::Esc => {
                // v0.9 discards unsaved changes on Esc. Prompt-on-dirty is future scope.
                Ok(vec![Effect::CloseScreen])
            }
            KeyCodePortable::Up => {
                self.cursor_row = self.cursor_row.saturating_sub(1);
                self.cursor_col = self.cursor_col.min(self.lines[self.cursor_row].len());
                Ok(vec![])
            }
            KeyCodePortable::Down => {
                if self.cursor_row + 1 < self.lines.len() {
                    self.cursor_row += 1;
                    self.cursor_col = self.cursor_col.min(self.lines[self.cursor_row].len());
                }
                Ok(vec![])
            }
            KeyCodePortable::Left => {
                self.cursor_col = self.cursor_col.saturating_sub(1);
                Ok(vec![])
            }
            KeyCodePortable::Right => {
                let row_len = self.lines[self.cursor_row].len();
                self.cursor_col = (self.cursor_col + 1).min(row_len);
                Ok(vec![])
            }
            KeyCodePortable::Enter => {
                let split = self.lines[self.cursor_row].split_off(self.cursor_col);
                self.cursor_row += 1;
                self.lines.insert(self.cursor_row, split);
                self.cursor_col = 0;
                self.dirty = true;
                Ok(vec![])
            }
            KeyCodePortable::Backspace => {
                if self.cursor_col > 0 {
                    self.lines[self.cursor_row].remove(self.cursor_col - 1);
                    self.cursor_col -= 1;
                    self.dirty = true;
                }
                Ok(vec![])
            }
            KeyCodePortable::Char('s') if key.modifiers.ctrl => {
                self.save()?;
                Ok(vec![])
            }
            KeyCodePortable::Char(c) => {
                self.lines[self.cursor_row].insert(self.cursor_col, c);
                self.cursor_col += 1;
                self.dirty = true;
                Ok(vec![])
            }
            _ => Ok(vec![]),
        }
    }

    fn tips(&self) -> Vec<StyledLine> {
        vec![StyledLine::plain(
            "←/→/↑/↓ move · type to insert · Ctrl-S save · Esc close",
        )]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use savvagent_plugin::KeyMods;
    use std::io::Write;

    fn temp_file_with(content: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "{content}").unwrap();
        f
    }

    fn key(code: KeyCodePortable) -> KeyEventPortable {
        KeyEventPortable {
            code,
            modifiers: KeyMods::default(),
        }
    }

    fn ctrl_key(code: KeyCodePortable) -> KeyEventPortable {
        KeyEventPortable {
            code,
            modifiers: KeyMods {
                ctrl: true,
                alt: false,
                shift: false,
                meta: false,
            },
        }
    }

    #[tokio::test]
    async fn typing_inserts_at_cursor_and_marks_dirty() {
        let f = temp_file_with("");
        let mut s = EditFileScreen::open(f.path().to_string_lossy().into()).unwrap();
        s.on_key(key(KeyCodePortable::Char('h'))).await.unwrap();
        s.on_key(key(KeyCodePortable::Char('i'))).await.unwrap();
        assert_eq!(s.lines[0], "hi");
        assert!(s.dirty);
    }

    #[tokio::test]
    async fn ctrl_s_saves_and_clears_dirty() {
        let f = temp_file_with("");
        let mut s = EditFileScreen::open(f.path().to_string_lossy().into()).unwrap();
        s.on_key(key(KeyCodePortable::Char('x'))).await.unwrap();
        assert!(s.dirty);
        s.on_key(ctrl_key(KeyCodePortable::Char('s')))
            .await
            .unwrap();
        assert!(!s.dirty);
        let on_disk = std::fs::read_to_string(f.path()).unwrap();
        assert_eq!(on_disk, "x\n");
    }

    #[tokio::test]
    async fn enter_splits_the_line() {
        let f = temp_file_with("abcd");
        let mut s = EditFileScreen::open(f.path().to_string_lossy().into()).unwrap();
        s.cursor_col = 2;
        s.on_key(key(KeyCodePortable::Enter)).await.unwrap();
        assert_eq!(s.lines, vec!["ab", "cd"]);
        assert_eq!(s.cursor_row, 1);
        assert_eq!(s.cursor_col, 0);
    }
}
