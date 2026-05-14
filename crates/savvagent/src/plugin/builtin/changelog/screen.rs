//! `internal:changelog` screen: state machine + render + key handling.
//!
//! The screen is opened by [`super::ChangelogPlugin::create_screen`] in
//! the [`ChangelogState::Loading`] state. `create_screen` spawns a tokio
//! task that calls [`super::fetch::ChangelogFetcher::fetch`] and writes
//! the result into the shared state ([`ChangelogState::Loaded`] on
//! success, [`ChangelogState::Failed`] on error). The user can press
//! `r` from `Failed` to re-spawn the fetch; this module exposes
//! [`ChangelogScreen::reset_to_loading`] so the plugin can re-spawn
//! externally without `screen.rs` taking a fetcher dependency.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use savvagent_plugin::{
    Effect, KeyCodePortable, KeyEventPortable, PluginError, Region, Screen, StyledLine,
    StyledSpan, TextMods,
};

/// What the screen is currently showing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangelogState {
    /// Fetch is in flight; render shows a placeholder.
    Loading,
    /// Fetch succeeded; render shows the markdown-translated lines.
    Loaded {
        /// Translated markdown ready to paint.
        lines: Vec<StyledLine>,
    },
    /// Fetch failed; render shows the error and the retry hint.
    Failed {
        /// Underlying error string surfaced verbatim to the user.
        error: String,
    },
}

/// Number of lines a `PageUp` / `PageDown` press advances the scroll
/// offset by. Chosen to feel snappy on the typical 80×24 terminal
/// without skipping an entire CHANGELOG section.
pub(crate) const PAGE_SIZE: usize = 10;

/// The screen instance pushed onto the screen stack.
pub struct ChangelogScreen {
    /// Shared with the spawned fetch task. Read by `render` via
    /// `try_lock` so the render hot path never blocks; written by the
    /// task once the fetch resolves and by `on_key` on retry.
    pub(crate) state: Arc<Mutex<ChangelogState>>,
    /// Top-line index inside the currently-rendered line buffer.
    /// Bumped by scroll keys, clamped at render time so a `Loading →
    /// Loaded` transition with fewer lines than the previous offset
    /// can't render an empty screen.
    scroll_offset: usize,
}

impl ChangelogScreen {
    /// Construct a new screen sharing the supplied state cell. The
    /// caller (the plugin) is expected to also hand the same `Arc` to
    /// the spawned fetch task so it can publish the result.
    pub fn new(state: Arc<Mutex<ChangelogState>>) -> Self {
        Self {
            state,
            scroll_offset: 0,
        }
    }

    /// Reset the shared state cell to `Loading` and zero the scroll
    /// offset. Called from `on_key` when the user presses `r` while
    /// in `Failed`; the plugin's retry path also calls this from the
    /// outside before re-spawning the fetch task.
    pub(crate) fn reset_to_loading(&mut self) {
        *self.state.lock().unwrap() = ChangelogState::Loading;
        self.scroll_offset = 0;
    }
}

#[async_trait]
impl Screen for ChangelogScreen {
    fn id(&self) -> String {
        "changelog".to_string()
    }

    fn render(&self, _region: Region) -> Vec<StyledLine> {
        // Filled in by Task 6.
        vec![]
    }

    async fn on_key(&mut self, key: KeyEventPortable) -> Result<Vec<Effect>, PluginError> {
        match key.code {
            KeyCodePortable::Esc | KeyCodePortable::Char('q') => Ok(vec![Effect::CloseScreen]),

            KeyCodePortable::Down | KeyCodePortable::Char('j') => {
                self.scroll_offset = self.scroll_offset.saturating_add(1);
                Ok(vec![])
            }
            KeyCodePortable::Up | KeyCodePortable::Char('k') => {
                self.scroll_offset = self.scroll_offset.saturating_sub(1);
                Ok(vec![])
            }
            KeyCodePortable::PageDown => {
                self.scroll_offset = self.scroll_offset.saturating_add(PAGE_SIZE);
                Ok(vec![])
            }
            KeyCodePortable::PageUp => {
                self.scroll_offset = self.scroll_offset.saturating_sub(PAGE_SIZE);
                Ok(vec![])
            }
            KeyCodePortable::Char('g') => {
                self.scroll_offset = 0;
                Ok(vec![])
            }
            KeyCodePortable::Char('G') => {
                let len = match &*self.state.lock().unwrap() {
                    ChangelogState::Loaded { lines } => lines.len(),
                    _ => 0,
                };
                self.scroll_offset = len.saturating_sub(1);
                Ok(vec![])
            }
            KeyCodePortable::Char('r') => {
                if matches!(&*self.state.lock().unwrap(), ChangelogState::Failed { .. }) {
                    self.reset_to_loading();
                }
                Ok(vec![])
            }
            _ => Ok(vec![]),
        }
    }

    fn tips(&self) -> Vec<StyledLine> {
        vec![StyledLine::plain(
            rust_i18n::t!("changelog.tips").to_string(),
        )]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loaded_state(n_lines: usize) -> ChangelogState {
        ChangelogState::Loaded {
            lines: (0..n_lines)
                .map(|i| StyledLine {
                    spans: vec![StyledSpan {
                        text: format!("line {i}"),
                        fg: None,
                        bg: None,
                        modifiers: TextMods::default(),
                    }],
                })
                .collect(),
        }
    }

    #[test]
    fn new_screen_starts_in_loading() {
        let state = Arc::new(Mutex::new(ChangelogState::Loading));
        let screen = ChangelogScreen::new(Arc::clone(&state));
        assert_eq!(*screen.state.lock().unwrap(), ChangelogState::Loading);
        assert_eq!(screen.scroll_offset, 0);
    }

    #[test]
    fn reset_to_loading_from_failed_clears_state_and_scroll() {
        let state = Arc::new(Mutex::new(ChangelogState::Failed {
            error: "DNS error".into(),
        }));
        let mut screen = ChangelogScreen::new(Arc::clone(&state));
        screen.scroll_offset = 42;

        screen.reset_to_loading();

        assert_eq!(*screen.state.lock().unwrap(), ChangelogState::Loading);
        assert_eq!(screen.scroll_offset, 0);
    }

    #[test]
    fn reset_to_loading_from_loaded_also_clears_state_and_scroll() {
        let state = Arc::new(Mutex::new(loaded_state(10)));
        let mut screen = ChangelogScreen::new(Arc::clone(&state));
        screen.scroll_offset = 3;

        screen.reset_to_loading();

        assert_eq!(*screen.state.lock().unwrap(), ChangelogState::Loading);
        assert_eq!(screen.scroll_offset, 0);
    }

    fn key(c: KeyCodePortable) -> KeyEventPortable {
        KeyEventPortable {
            code: c,
            modifiers: savvagent_plugin::KeyMods::default(),
        }
    }

    async fn screen_with(state: ChangelogState) -> ChangelogScreen {
        ChangelogScreen::new(Arc::new(Mutex::new(state)))
    }

    #[tokio::test]
    async fn esc_emits_close_screen() {
        let mut s = screen_with(ChangelogState::Loading).await;
        let effs = s.on_key(key(KeyCodePortable::Esc)).await.unwrap();
        assert!(matches!(effs[0], Effect::CloseScreen));
    }

    #[tokio::test]
    async fn q_emits_close_screen() {
        let mut s = screen_with(ChangelogState::Loading).await;
        let effs = s.on_key(key(KeyCodePortable::Char('q'))).await.unwrap();
        assert!(matches!(effs[0], Effect::CloseScreen));
    }

    #[tokio::test]
    async fn down_and_j_increment_scroll_offset() {
        let mut s = screen_with(loaded_state(50)).await;
        s.on_key(key(KeyCodePortable::Down)).await.unwrap();
        assert_eq!(s.scroll_offset, 1);
        s.on_key(key(KeyCodePortable::Char('j'))).await.unwrap();
        assert_eq!(s.scroll_offset, 2);
    }

    #[tokio::test]
    async fn up_and_k_decrement_scroll_offset_with_clamp_at_zero() {
        let mut s = screen_with(loaded_state(50)).await;
        s.scroll_offset = 2;
        s.on_key(key(KeyCodePortable::Up)).await.unwrap();
        s.on_key(key(KeyCodePortable::Char('k'))).await.unwrap();
        assert_eq!(s.scroll_offset, 0);
        // Pressing again must NOT underflow.
        s.on_key(key(KeyCodePortable::Up)).await.unwrap();
        assert_eq!(s.scroll_offset, 0);
    }

    #[tokio::test]
    async fn page_down_advances_by_page_size() {
        let mut s = screen_with(loaded_state(200)).await;
        s.on_key(key(KeyCodePortable::PageDown)).await.unwrap();
        assert_eq!(s.scroll_offset, super::PAGE_SIZE);
    }

    #[tokio::test]
    async fn page_up_retreats_by_page_size_with_clamp() {
        let mut s = screen_with(loaded_state(200)).await;
        s.scroll_offset = 5;
        s.on_key(key(KeyCodePortable::PageUp)).await.unwrap();
        assert_eq!(s.scroll_offset, 0);
    }

    #[tokio::test]
    async fn lower_g_jumps_to_top_and_upper_g_jumps_to_bottom() {
        let mut s = screen_with(loaded_state(50)).await;
        s.on_key(key(KeyCodePortable::Char('G'))).await.unwrap();
        // 'G' sets the offset to last-line-index; render-time clamp
        // narrows further once region.height is known.
        assert_eq!(s.scroll_offset, 49);
        s.on_key(key(KeyCodePortable::Char('g'))).await.unwrap();
        assert_eq!(s.scroll_offset, 0);
    }

    #[tokio::test]
    async fn r_in_failed_state_resets_to_loading() {
        let mut s = screen_with(ChangelogState::Failed {
            error: "boom".into(),
        })
        .await;
        s.scroll_offset = 7;

        let effs = s.on_key(key(KeyCodePortable::Char('r'))).await.unwrap();

        // Plugin observes the reset-to-loading by polling state; the
        // screen itself does not emit a re-spawn effect (no such effect
        // exists). It just emits an empty effects vec; the spawned
        // fetch task is restarted by the plugin's own retry hook.
        assert!(effs.is_empty());
        assert_eq!(*s.state.lock().unwrap(), ChangelogState::Loading);
        assert_eq!(s.scroll_offset, 0);
    }

    #[tokio::test]
    async fn r_outside_failed_state_is_ignored() {
        let mut s = screen_with(loaded_state(10)).await;
        s.scroll_offset = 3;
        let effs = s.on_key(key(KeyCodePortable::Char('r'))).await.unwrap();
        assert!(effs.is_empty());
        // No state change.
        assert!(matches!(*s.state.lock().unwrap(), ChangelogState::Loaded { .. }));
        assert_eq!(s.scroll_offset, 3);
    }
}
