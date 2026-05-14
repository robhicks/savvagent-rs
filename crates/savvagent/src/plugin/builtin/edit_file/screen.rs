//! Marker screen for the in-TUI file editor.
//!
//! Like [`super::super::view_file::screen::ViewFileScreen`], the
//! actual editing widget is owned by `App::editor` (ratatui-code-editor)
//! and rendered by `ui.rs`. This marker screen only owns the open/close
//! and save semantics: Esc closes (saves first), Ctrl-S saves without
//! closing. All other keys reach the editor via `main.rs`'s pre-dispatch
//! routing for `edit-file` screens.

use async_trait::async_trait;
use savvagent_plugin::{
    Effect, KeyCodePortable, KeyEventPortable, PluginError, Region, Screen, StyledLine,
};

/// Marker for the `edit-file` slot on the screen stack. Carries no
/// state — the file and editor instance live in `App` and are rendered
/// by `ui::paint_file_screen`.
#[derive(Debug, Default)]
pub struct EditFileScreen;

impl EditFileScreen {
    /// Construct a marker. The `_path` argument is ignored; the real
    /// path lives in `App::active_file_path`.
    pub fn new(_path: String) -> Self {
        Self
    }
}

#[async_trait]
impl Screen for EditFileScreen {
    fn id(&self) -> String {
        "edit-file".to_string()
    }

    fn render(&self, _region: Region) -> Vec<StyledLine> {
        // Editor widget is rendered by ui.rs directly.
        vec![]
    }

    async fn on_key(&mut self, key: KeyEventPortable) -> Result<Vec<Effect>, PluginError> {
        match key.code {
            // Esc closes the screen. `apply_effects::CloseScreen` saves the
            // active editor on the way out for edit-file screens (mirrors
            // the legacy save-on-close behavior) and clears editor state.
            KeyCodePortable::Esc => Ok(vec![Effect::CloseScreen]),
            // Ctrl-S triggers an explicit save without closing.
            KeyCodePortable::Char('s') if key.modifiers.ctrl => {
                Ok(vec![Effect::SaveActiveFile])
            }
            // Everything else is consumed by the editor via main.rs's
            // pre-screen-dispatch routing — by the time we get here, the
            // key has already been routed to `App::editor.input(...)`.
            _ => Ok(vec![]),
        }
    }

    fn tips(&self) -> Vec<StyledLine> {
        vec![StyledLine::plain(
            rust_i18n::t!("picker.edit-file.tips").to_string(),
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

    fn ctrl_key(c: KeyCodePortable) -> KeyEventPortable {
        KeyEventPortable {
            code: c,
            modifiers: KeyMods {
                ctrl: true,
                alt: false,
                shift: false,
                meta: false,
            },
        }
    }

    #[tokio::test]
    async fn esc_emits_close_screen() {
        let mut s = EditFileScreen::new("/tmp/x.rs".into());
        let effs = s.on_key(key(KeyCodePortable::Esc)).await.unwrap();
        assert!(matches!(effs[0], Effect::CloseScreen));
    }

    #[tokio::test]
    async fn ctrl_s_emits_save_active_file() {
        let mut s = EditFileScreen::new("/tmp/x.rs".into());
        let effs = s
            .on_key(ctrl_key(KeyCodePortable::Char('s')))
            .await
            .unwrap();
        assert!(matches!(effs[0], Effect::SaveActiveFile));
    }

    #[tokio::test]
    async fn other_keys_are_no_op() {
        let mut s = EditFileScreen::new("/tmp/x.rs".into());
        let effs = s.on_key(key(KeyCodePortable::Char('a'))).await.unwrap();
        assert!(effs.is_empty());
    }

    #[test]
    fn render_returns_empty() {
        let s = EditFileScreen::new("/tmp/x.rs".into());
        assert!(s.render(Region { x: 0, y: 0, width: 80, height: 24 }).is_empty());
    }
}
