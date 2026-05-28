//! GUI editor state + paint pass for the `view-file` / `edit-file`
//! marker screens. The buffer lives on `SavvagentApp` (not on `App`,
//! which still owns the ratatui editor for the TUI path); it is
//! lazy-loaded from `App::active_file_path` the first frame after a
//! marker screen opens and cleared when the screen pops.

use std::path::{Path, PathBuf};

/// Per-open file state for the GUI editor. Owns the text the
/// `egui_code_editor::CodeEditor` widget mutates in-place, plus the path
/// it came from so save knows where to write.
#[derive(Debug, Clone)]
#[allow(dead_code)] // Task 4 wires this into the paint pass; remove then.
pub struct EditorBuffer {
    /// Disk path of the open file. Set on load.
    pub path: PathBuf,
    /// In-memory buffer. The widget mutates this on every keystroke
    /// when the screen is `edit-file`. Untouched for `view-file`.
    pub text: String,
    /// Whether the buffer has unsaved changes since load. Bumped to
    /// true by `mark_dirty` whenever the widget reports a text change;
    /// reset to false on successful save.
    pub dirty: bool,
}

#[allow(dead_code)] // Task 4 wires these into the paint pass; remove then.
impl EditorBuffer {
    /// Load `path` from disk into a fresh buffer.
    pub fn load(path: &Path) -> std::io::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            text,
            dirty: false,
        })
    }

    /// Mark dirty after a widget edit reported text changed.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Write the buffer text to disk and clear `dirty` on success.
    pub fn save_to_disk(&mut self) -> std::io::Result<()> {
        std::fs::write(&self.path, &self.text)?;
        self.dirty = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn load_reads_file_contents() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "hello").unwrap();
        let buf = EditorBuffer::load(tmp.path()).unwrap();
        assert_eq!(buf.text, "hello\n");
        assert_eq!(buf.path, tmp.path());
        assert!(!buf.dirty);
    }

    #[test]
    fn mark_dirty_flips_flag() {
        let mut buf = EditorBuffer {
            path: PathBuf::from("/tmp/x"),
            text: String::new(),
            dirty: false,
        };
        buf.mark_dirty();
        assert!(buf.dirty);
    }

    #[test]
    fn save_writes_file_and_clears_dirty() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let mut buf = EditorBuffer {
            path: tmp.path().to_path_buf(),
            text: "new content\n".to_string(),
            dirty: true,
        };
        buf.save_to_disk().unwrap();
        assert!(!buf.dirty);
        assert_eq!(
            std::fs::read_to_string(tmp.path()).unwrap(),
            "new content\n"
        );
    }
}
