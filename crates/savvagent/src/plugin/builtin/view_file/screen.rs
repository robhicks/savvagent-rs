//! Read-only file viewer screen.

use async_trait::async_trait;
use savvagent_plugin::{
    Effect, KeyCodePortable, KeyEventPortable, PluginError, Region, Screen, StyledLine, StyledSpan,
    TextMods, ThemeColor,
};

/// A fullscreen read-only viewer for a single file.
///
/// Renders the file contents with a header bar and supports line-by-line
/// and page-level scrolling. Pressing Esc emits [`Effect::CloseScreen`].
#[derive(Debug)]
pub struct ViewFileScreen {
    path: String,
    lines: Vec<String>,
    scroll: usize,
}

impl ViewFileScreen {
    /// Reads `path` from disk and constructs a ready-to-render screen.
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
        let lines = body.lines().map(|l| l.to_string()).collect();
        Ok(Self {
            path,
            lines,
            scroll: 0,
        })
    }
}

#[async_trait]
impl Screen for ViewFileScreen {
    fn id(&self) -> String {
        "view-file".to_string()
    }

    fn render(&self, region: Region) -> Vec<StyledLine> {
        let mut out = vec![StyledLine {
            spans: vec![StyledSpan {
                text: format!("{}  (read-only)", self.path),
                fg: Some(ThemeColor::Cyan),
                bg: None,
                modifiers: TextMods {
                    bold: true,
                    ..Default::default()
                },
            }],
        }];
        let take = region.height.saturating_sub(1) as usize;
        for line in self.lines.iter().skip(self.scroll).take(take) {
            out.push(StyledLine::plain(line.clone()));
        }
        out
    }

    async fn on_key(&mut self, key: KeyEventPortable) -> Result<Vec<Effect>, PluginError> {
        match key.code {
            KeyCodePortable::Esc => Ok(vec![Effect::CloseScreen]),
            KeyCodePortable::Down => {
                if self.scroll + 1 < self.lines.len() {
                    self.scroll += 1;
                }
                Ok(vec![])
            }
            KeyCodePortable::Up => {
                self.scroll = self.scroll.saturating_sub(1);
                Ok(vec![])
            }
            KeyCodePortable::PageDown => {
                self.scroll = (self.scroll + 10).min(self.lines.len().saturating_sub(1));
                Ok(vec![])
            }
            KeyCodePortable::PageUp => {
                self.scroll = self.scroll.saturating_sub(10);
                Ok(vec![])
            }
            _ => Ok(vec![]),
        }
    }

    fn tips(&self) -> Vec<StyledLine> {
        vec![StyledLine::plain("↑/↓ scroll · PgUp/PgDn jump · Esc close")]
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

    #[tokio::test]
    async fn open_reads_file_lines() {
        let f = temp_file_with("alpha\nbeta\ngamma\n");
        let s = ViewFileScreen::open(f.path().to_string_lossy().into()).unwrap();
        assert_eq!(s.lines, vec!["alpha", "beta", "gamma"]);
    }

    #[tokio::test]
    async fn missing_file_returns_invalid_args_error() {
        let err = ViewFileScreen::open("/definitely/not/here".into()).unwrap_err();
        assert!(matches!(err, PluginError::InvalidArgs(_)));
    }

    #[tokio::test]
    async fn esc_closes_screen() {
        let f = temp_file_with("x\n");
        let mut s = ViewFileScreen::open(f.path().to_string_lossy().into()).unwrap();
        let effs = s.on_key(key(KeyCodePortable::Esc)).await.unwrap();
        assert!(matches!(effs[0], Effect::CloseScreen));
    }

    #[tokio::test]
    async fn down_increments_scroll() {
        let f = temp_file_with("a\nb\nc\n");
        let mut s = ViewFileScreen::open(f.path().to_string_lossy().into()).unwrap();
        s.on_key(key(KeyCodePortable::Down)).await.unwrap();
        assert_eq!(s.scroll, 1);
    }
}
