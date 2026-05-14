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
}
