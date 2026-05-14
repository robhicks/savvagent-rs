//! Marker screen for the read-only file viewer.
//!
//! The actual file content is rendered by `ui.rs` via the
//! ratatui-code-editor instance held in `App::editor` — see the
//! `view-file` branch of `ui::paint_screen`. This screen exists only
//! so the screen-stack abstraction stays consistent: visibility is
//! tracked by the stack, and Esc / q close it via `Effect::CloseScreen`
//! (which clears `App::editor` in `apply_effects`).

use async_trait::async_trait;
use savvagent_plugin::{
    Effect, KeyCodePortable, KeyEventPortable, PluginError, Region, Screen, StyledLine,
};

/// Marker for the `view-file` slot on the screen stack. Carries no
/// state — the file path and editor instance live in `App` and are
/// rendered by `ui::paint_file_screen`.
#[derive(Debug, Default)]
pub struct ViewFileScreen;

impl ViewFileScreen {
    /// Construct a marker. The `_path` argument is ignored (kept so the
    /// `create_screen` call site doesn't need to special-case marker vs.
    /// stateful screens); the real path lives in `App::active_file_path`.
    pub fn new(_path: String) -> Self {
        Self
    }
}

#[async_trait]
impl Screen for ViewFileScreen {
    fn id(&self) -> String {
        "view-file".to_string()
    }

    fn render(&self, _region: Region) -> Vec<StyledLine> {
        // `ui::paint_screen` short-circuits the normal styled-line
        // render path for this screen id and draws the editor widget
        // directly, so nothing to return here.
        vec![]
    }

    async fn on_key(&mut self, key: KeyEventPortable) -> Result<Vec<Effect>, PluginError> {
        // `q` is the legacy v0.8 close shortcut; Esc is the standard.
        // All other keys are routed to the editor by `main.rs` before
        // the screen `on_key` dispatch fires.
        match key.code {
            KeyCodePortable::Esc | KeyCodePortable::Char('q') => Ok(vec![Effect::CloseScreen]),
            _ => Ok(vec![]),
        }
    }

    fn tips(&self) -> Vec<StyledLine> {
        vec![StyledLine::plain(
            rust_i18n::t!("picker.view-file.tips").to_string(),
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
    async fn esc_emits_close_screen() {
        let mut s = ViewFileScreen::new("/tmp/x.rs".into());
        let effs = s.on_key(key(KeyCodePortable::Esc)).await.unwrap();
        assert!(matches!(effs[0], Effect::CloseScreen));
    }

    #[tokio::test]
    async fn q_emits_close_screen() {
        let mut s = ViewFileScreen::new("/tmp/x.rs".into());
        let effs = s.on_key(key(KeyCodePortable::Char('q'))).await.unwrap();
        assert!(matches!(effs[0], Effect::CloseScreen));
    }

    #[tokio::test]
    async fn other_keys_are_no_op() {
        let mut s = ViewFileScreen::new("/tmp/x.rs".into());
        let effs = s.on_key(key(KeyCodePortable::Down)).await.unwrap();
        assert!(effs.is_empty());
    }

    #[test]
    fn render_returns_empty() {
        let s = ViewFileScreen::new("/tmp/x.rs".into());
        assert!(s.render(Region { x: 0, y: 0, width: 80, height: 24 }).is_empty());
    }

    #[test]
    fn id_matches_screen_spec() {
        let s = ViewFileScreen::new("/tmp/x.rs".into());
        assert_eq!(s.id(), "view-file");
    }
}
