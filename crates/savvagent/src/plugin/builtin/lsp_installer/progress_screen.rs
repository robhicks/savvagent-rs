//! `LspProgressScreen` — modal that owns the install-driver task and
//! renders per-entry status from shared [`ProgressState`].
//!
//! See `docs/superpowers/specs/2026-05-21-lsp-installer-progress-design.md`.

use async_trait::async_trait;
use savvagent_plugin::{
    Effect, KeyEventPortable, PluginError, Region, Screen, StyledLine,
};
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;

use crate::plugin::builtin::lsp_installer::progress::{
    EntryStatus, ProgressState, initial_state_for_ids,
};

/// Render bridge between the install-driver task and the user. Reads
/// [`ProgressState`] each frame; emits `CloseScreen` (+ summary notes)
/// on user dismiss.
pub struct LspProgressScreen {
    pub(crate) state: Arc<TokioMutex<ProgressState>>,
}

impl LspProgressScreen {
    /// Build the screen and (if there's any work to do) spawn the
    /// install driver. `entry_ids` is the picker's confirmed selection.
    pub fn new(entry_ids: Vec<String>) -> Self {
        let state = Arc::new(TokioMutex::new(initial_state_for_ids(&entry_ids)));
        // Driver-task spawn is added in a later task (Task 10); for now
        // the screen only renders the initial state.
        Self { state }
    }
}

#[async_trait]
impl Screen for LspProgressScreen {
    fn id(&self) -> String {
        "lsp_installer.progress".to_string()
    }

    fn render(&self, _region: Region) -> Vec<StyledLine> {
        // try_lock keeps the render hot path non-blocking — if the
        // driver task is mid-write we skip this frame; the next frame
        // will catch up. This matches the convention in
        // ChangelogScreen::render and SelfUpdatePlugin::render_slot.
        let state = match self.state.try_lock() {
            Ok(g) => g,
            Err(_) => return vec![],
        };
        let mut lines: Vec<StyledLine> = Vec::new();
        lines.push(StyledLine::plain(format!(
            "Installing {} language server(s)…",
            state.entries.len()
        )));
        lines.push(StyledLine::plain(""));
        for entry in &state.entries {
            lines.push(StyledLine::plain(format!(
                "  {} {}",
                glyph_for(&entry.status),
                entry.display_name
            )));
        }
        lines
    }

    async fn on_key(&mut self, _key: KeyEventPortable) -> Result<Vec<Effect>, PluginError> {
        // Filled in in the next task (Task 9).
        Ok(vec![])
    }

    fn tips(&self) -> Vec<StyledLine> {
        vec![StyledLine::plain(
            "Esc dismisses (install continues in background).",
        )]
    }
}

fn glyph_for(status: &EntryStatus) -> &'static str {
    match status {
        EntryStatus::Queued => "..",
        EntryStatus::Downloading { .. }
        | EntryStatus::Verifying
        | EntryStatus::Extracting
        | EntryStatus::RunningNpm { .. } => "..",
        EntryStatus::Installed { .. } => "OK",
        EntryStatus::Failed { .. } => "!!",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_is_lsp_installer_progress() {
        let s = LspProgressScreen::new(vec![]);
        assert_eq!(s.id(), "lsp_installer.progress");
    }

    #[test]
    fn render_lists_an_entry_per_id() {
        // Use real catalog ids so they don't get pre-failed.
        use crate::plugin::builtin::lsp_installer::catalog::CATALOG;
        let id_a = CATALOG[0].id.to_string();
        let id_b = CATALOG[1].id.to_string();
        let s = LspProgressScreen::new(vec![id_a.clone(), id_b.clone()]);
        let lines = s.render(Region {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        });
        // Header + blank + 2 entries == at least 4 lines.
        assert!(lines.len() >= 4);
        let body = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.text.as_str()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(body.contains(&id_a));
        assert!(body.contains(&id_b));
    }
}
