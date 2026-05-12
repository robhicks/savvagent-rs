//! Basic in-TUI file editor screen.

use async_trait::async_trait;
use savvagent_plugin::{
    Effect, KeyCodePortable, KeyEventPortable, PluginError, Region, Screen, StyledLine, StyledSpan,
    TextMods, ThemeColor,
};

/// Convert a character index into the byte offset within `s`. Returns
/// `s.len()` (byte length) when `char_idx == s.chars().count()` — i.e., the
/// cursor is positioned just past the last character.
fn char_index_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(s.len())
}

/// Count chars in a line (cheap for short lines; this is an editor for
/// modest files in v0.9).
fn line_char_len(s: &str) -> usize {
    s.chars().count()
}

/// A fullscreen editor for a single file.
///
/// Supports arrow-key navigation, character insertion, Backspace deletion,
/// Enter to split lines, Ctrl-S to save, and Esc to close (discarding
/// unsaved changes for v0.9).
///
/// `cursor_col` is always a **character index** (not a byte offset). All
/// mutation sites convert it to a byte index via [`char_index_to_byte`]
/// before calling `String` methods, so files containing multibyte characters
/// (e.g. `é`, em-dash, smart quotes) are handled correctly.
#[derive(Debug)]
pub struct EditFileScreen {
    path: String,
    lines: Vec<String>,
    cursor_row: usize,
    cursor_col: usize,
    dirty: bool,
    last_error: Option<String>,
}

impl EditFileScreen {
    /// Reads `path` from disk and constructs a ready-to-edit screen.
    ///
    /// Returns [`PluginError::InvalidArgs`] if the file is not found, or
    /// [`PluginError::Internal`] for other I/O failures.
    pub fn open(path: String) -> Result<Self, PluginError> {
        let body = std::fs::read_to_string(&path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                PluginError::InvalidArgs(format!("{path}: file not found"))
            }
            _ => PluginError::Internal(format!("read {path}: {e}")),
        })?;
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
            last_error: None,
        })
    }

    /// Write the buffer to disk. Always appends a trailing newline to the
    /// joined buffer body — files that had no trailing newline on open will
    /// gain one after a save. A future PR may add a mode to preserve the
    /// original file's trailing-newline state.
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
                fg: Some(ThemeColor::Accent),
                bg: None,
                modifiers: TextMods {
                    bold: true,
                    ..Default::default()
                },
            }],
        }];

        if let Some(err) = &self.last_error {
            out.push(StyledLine {
                spans: vec![StyledSpan {
                    text: format!("Save failed: {err}"),
                    fg: Some(ThemeColor::Error),
                    bg: None,
                    modifiers: TextMods {
                        bold: true,
                        ..Default::default()
                    },
                }],
            });
        }

        let take = region.height.saturating_sub(out.len() as u16) as usize;
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
                self.cursor_col = self
                    .cursor_col
                    .min(line_char_len(&self.lines[self.cursor_row]));
                Ok(vec![])
            }
            KeyCodePortable::Down => {
                if self.cursor_row + 1 < self.lines.len() {
                    self.cursor_row += 1;
                    self.cursor_col = self
                        .cursor_col
                        .min(line_char_len(&self.lines[self.cursor_row]));
                }
                Ok(vec![])
            }
            KeyCodePortable::Left => {
                self.cursor_col = self.cursor_col.saturating_sub(1);
                Ok(vec![])
            }
            KeyCodePortable::Right => {
                let row_len = line_char_len(&self.lines[self.cursor_row]);
                self.cursor_col = (self.cursor_col + 1).min(row_len);
                Ok(vec![])
            }
            KeyCodePortable::Enter => {
                let byte_idx = char_index_to_byte(&self.lines[self.cursor_row], self.cursor_col);
                let split = self.lines[self.cursor_row].split_off(byte_idx);
                self.cursor_row += 1;
                self.lines.insert(self.cursor_row, split);
                self.cursor_col = 0;
                self.dirty = true;
                Ok(vec![])
            }
            KeyCodePortable::Backspace => {
                if self.cursor_col > 0 {
                    let row = &mut self.lines[self.cursor_row];
                    let prev_char_idx = self.cursor_col - 1;
                    let byte_idx = char_index_to_byte(row, prev_char_idx);
                    row.remove(byte_idx);
                    self.cursor_col -= 1;
                    self.dirty = true;
                }
                Ok(vec![])
            }
            KeyCodePortable::Char('s') if key.modifiers.ctrl => {
                match self.save() {
                    Ok(()) => {
                        self.last_error = None;
                    }
                    Err(e) => {
                        self.last_error = Some(e.to_string());
                    }
                }
                Ok(vec![])
            }
            KeyCodePortable::Char(c)
                if !c.is_control() && !key.modifiers.alt && !key.modifiers.meta =>
            {
                let row = &mut self.lines[self.cursor_row];
                let byte_idx = char_index_to_byte(row, self.cursor_col);
                row.insert(byte_idx, c);
                self.cursor_col += 1;
                self.dirty = true;
                Ok(vec![])
            }
            KeyCodePortable::Char(_) => {
                // Control chars (Tab, DEL on some terminals) and Alt/Meta-modified
                // chars are ignored to prevent corrupting the buffer with bytes the
                // user didn't intend.
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

    #[tokio::test]
    async fn multibyte_editing_does_not_panic() {
        let f = temp_file_with("café"); // 4 chars, 5 bytes (é is 2 bytes in UTF-8)
        let mut s = EditFileScreen::open(f.path().to_string_lossy().into()).unwrap();
        // Cursor at end (4 chars in).
        s.cursor_col = 4;
        // Press Right (should stay at 4, the end).
        s.on_key(key(KeyCodePortable::Right)).await.unwrap();
        assert_eq!(s.cursor_col, 4);
        // Press Backspace (deletes the é).
        s.on_key(key(KeyCodePortable::Backspace)).await.unwrap();
        assert_eq!(s.lines[0], "caf");
        assert_eq!(s.cursor_col, 3);
        // Insert é back.
        s.on_key(key(KeyCodePortable::Char('é'))).await.unwrap();
        assert_eq!(s.lines[0], "café");
        assert_eq!(s.cursor_col, 4);
        // Now position cursor at 2 (just before f) and split.
        s.cursor_col = 2;
        s.on_key(key(KeyCodePortable::Enter)).await.unwrap();
        assert_eq!(s.lines, vec!["ca", "fé"]);
    }

    #[cfg_attr(not(unix), ignore)]
    #[tokio::test]
    async fn save_to_unwritable_dir_sets_last_error() {
        // Use a tempdir, drop it so the path no longer exists, then try to
        // write into it — guarantees a failure on all Unix systems without
        // needing /proc.
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("x.txt");
        std::fs::write(&path, "ok").unwrap();
        let path_str: String = path.to_string_lossy().into();
        let mut s = EditFileScreen::open(path_str).unwrap();
        drop(dir); // directory (and file) are now gone
        s.dirty = true;
        s.on_key(ctrl_key(KeyCodePortable::Char('s')))
            .await
            .unwrap();
        assert!(
            s.last_error.is_some(),
            "expected last_error to be set on save failure"
        );
        assert!(s.dirty, "dirty should remain set when save failed");
    }
}
