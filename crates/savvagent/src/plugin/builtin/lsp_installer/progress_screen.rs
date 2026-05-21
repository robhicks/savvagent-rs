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
    EntryProgress, EntryStatus, ProgressState, initial_state_for_ids,
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

    fn render_lines(state: &ProgressState) -> Vec<StyledLine> {
        let mut out: Vec<StyledLine> = Vec::new();
        out.push(StyledLine::plain(format!(
            "Installing {} language server(s)…",
            state.entries.len()
        )));
        out.push(StyledLine::plain(""));

        for entry in &state.entries {
            let (glyph, label) = format_entry(entry);
            out.push(StyledLine::plain(format!(
                "  {glyph}  {:<32} {label}",
                entry.display_name
            )));
        }

        out.push(StyledLine::plain(""));
        out.push(StyledLine::plain(summary_line(&state.entries)));

        if state.finished {
            let installed = state
                .entries
                .iter()
                .filter(|e| matches!(e.status, EntryStatus::Installed { .. }))
                .count();
            let failed = state
                .entries
                .iter()
                .filter(|e| matches!(e.status, EntryStatus::Failed { .. }))
                .count();
            out.push(StyledLine::plain(""));
            out.push(StyledLine::plain(format!(
                "All done — {installed} installed, {failed} failed."
            )));
            out.push(StyledLine::plain(
                "Press Enter to close. Restart savvagent to pick up the new servers.",
            ));
            if let Some(err) = &state.config_error {
                out.push(StyledLine::plain(format!(
                    "Warning: writing lsp.toml failed: {err}"
                )));
            }
        }

        out
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
        Self::render_lines(&state)
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

fn format_entry(entry: &EntryProgress) -> (&'static str, String) {
    match &entry.status {
        EntryStatus::Queued => ("..", "queued".to_string()),
        EntryStatus::Downloading { bytes_so_far, total } => {
            let label = match total {
                Some(t) => format!(
                    "downloading… {} / {}",
                    human_mb(*bytes_so_far),
                    human_mb(*t)
                ),
                None => format!("downloading… {}", human_mb(*bytes_so_far)),
            };
            ("..", label)
        }
        EntryStatus::Verifying => ("..", "verifying SHA256…".to_string()),
        EntryStatus::Extracting => ("..", "extracting…".to_string()),
        EntryStatus::RunningNpm { last_line } => (
            "..",
            format!("running npm…   {}", truncate(last_line, 48)),
        ),
        EntryStatus::Installed { .. } => ("OK", "installed".to_string()),
        EntryStatus::Failed { reason, .. } => ("!!", format!("failed: {reason}")),
    }
}

fn summary_line(entries: &[EntryProgress]) -> String {
    let total = entries.len();
    let done = entries
        .iter()
        .filter(|e| {
            matches!(
                e.status,
                EntryStatus::Installed { .. } | EntryStatus::Failed { .. }
            )
        })
        .count();
    let in_progress = entries
        .iter()
        .filter(|e| {
            matches!(
                e.status,
                EntryStatus::Downloading { .. }
                    | EntryStatus::Verifying
                    | EntryStatus::Extracting
                    | EntryStatus::RunningNpm { .. }
            )
        })
        .count();
    let queued = entries
        .iter()
        .filter(|e| matches!(e.status, EntryStatus::Queued))
        .count();
    format!("{done} of {total} done · {in_progress} in progress · {queued} queued")
}

fn human_mb(bytes: u64) -> String {
    let mb = bytes as f64 / (1024.0 * 1024.0);
    format!("{mb:.1} MB")
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::builtin::lsp_installer::progress::{EntryProgress, ProgressState};
    use std::path::PathBuf;

    fn screen_with_state(state: ProgressState) -> LspProgressScreen {
        LspProgressScreen {
            state: Arc::new(TokioMutex::new(state)),
        }
    }

    fn rendered(s: &LspProgressScreen) -> String {
        s.render(Region {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        })
        .iter()
        .map(|l| l.spans.iter().map(|s| s.text.as_str()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
    }

    #[test]
    fn id_is_lsp_installer_progress() {
        let s = LspProgressScreen::new(vec![]);
        assert_eq!(s.id(), "lsp_installer.progress");
    }

    #[test]
    fn render_shows_queued_label() {
        let s = screen_with_state(ProgressState {
            entries: vec![EntryProgress {
                id: "rust-analyzer".into(),
                display_name: "rust-analyzer".into(),
                status: EntryStatus::Queued,
            }],
            finished: false,
            config_error: None,
        });
        let out = rendered(&s);
        assert!(out.contains("rust-analyzer"));
        assert!(out.contains("queued"));
    }

    #[test]
    fn render_shows_downloading_with_bytes_and_total() {
        let s = screen_with_state(ProgressState {
            entries: vec![EntryProgress {
                id: "x".into(),
                display_name: "x".into(),
                status: EntryStatus::Downloading {
                    bytes_so_far: 12 * 1024 * 1024 + 400 * 1024,
                    total: Some(28 * 1024 * 1024),
                },
            }],
            finished: false,
            config_error: None,
        });
        let out = rendered(&s);
        assert!(out.contains("downloading"));
        // Spec format: "downloading… 12.4 MB / 28.0 MB"
        assert!(out.contains("MB"));
        assert!(out.contains("/"));
    }

    #[test]
    fn render_shows_downloading_without_total_when_unknown() {
        let s = screen_with_state(ProgressState {
            entries: vec![EntryProgress {
                id: "x".into(),
                display_name: "x".into(),
                status: EntryStatus::Downloading {
                    bytes_so_far: 1024 * 1024,
                    total: None,
                },
            }],
            finished: false,
            config_error: None,
        });
        let out = rendered(&s);
        assert!(out.contains("downloading"));
        assert!(out.contains("MB"));
        assert!(
            !out.contains(" / "),
            "no slash when total is unknown, got:\n{out}"
        );
    }

    #[test]
    fn render_shows_running_npm_with_last_line() {
        let s = screen_with_state(ProgressState {
            entries: vec![EntryProgress {
                id: "x".into(),
                display_name: "x".into(),
                status: EntryStatus::RunningNpm {
                    last_line: "added 5 packages in 3s".into(),
                },
            }],
            finished: false,
            config_error: None,
        });
        let out = rendered(&s);
        assert!(out.contains("running npm"));
        assert!(out.contains("added 5 packages in 3s"));
    }

    #[test]
    fn render_shows_installed_and_failed() {
        let s = screen_with_state(ProgressState {
            entries: vec![
                EntryProgress {
                    id: "ok".into(),
                    display_name: "ok".into(),
                    status: EntryStatus::Installed {
                        installed_at: PathBuf::from("/tmp/ok"),
                    },
                },
                EntryProgress {
                    id: "bad".into(),
                    display_name: "bad".into(),
                    status: EntryStatus::Failed {
                        reason: "network down".into(),
                        fatal: false,
                    },
                },
            ],
            finished: false,
            config_error: None,
        });
        let out = rendered(&s);
        assert!(out.contains("installed"));
        assert!(out.contains("failed"));
        assert!(out.contains("network down"));
    }

    #[test]
    fn render_summary_counts_by_status() {
        let s = screen_with_state(ProgressState {
            entries: vec![
                EntryProgress {
                    id: "a".into(),
                    display_name: "a".into(),
                    status: EntryStatus::Installed {
                        installed_at: PathBuf::from("/x"),
                    },
                },
                EntryProgress {
                    id: "b".into(),
                    display_name: "b".into(),
                    status: EntryStatus::Verifying,
                },
                EntryProgress {
                    id: "c".into(),
                    display_name: "c".into(),
                    status: EntryStatus::Queued,
                },
                EntryProgress {
                    id: "d".into(),
                    display_name: "d".into(),
                    status: EntryStatus::Queued,
                },
            ],
            finished: false,
            config_error: None,
        });
        let out = rendered(&s);
        // Spec format: "1 of 4 done · 1 in progress · 2 queued"
        assert!(out.contains("1 of 4 done"));
        assert!(out.contains("1 in progress"));
        assert!(out.contains("2 queued"));
    }

    #[test]
    fn render_finished_footer_shows_press_enter() {
        let s = screen_with_state(ProgressState {
            entries: vec![EntryProgress {
                id: "a".into(),
                display_name: "a".into(),
                status: EntryStatus::Installed {
                    installed_at: PathBuf::from("/x"),
                },
            }],
            finished: true,
            config_error: None,
        });
        let out = rendered(&s);
        assert!(out.contains("All done"));
        assert!(out.contains("Press Enter"));
        assert!(out.contains("Restart savvagent"));
    }

    #[test]
    fn render_finished_footer_shows_config_error_when_set() {
        let s = screen_with_state(ProgressState {
            entries: vec![EntryProgress {
                id: "a".into(),
                display_name: "a".into(),
                status: EntryStatus::Installed {
                    installed_at: PathBuf::from("/x"),
                },
            }],
            finished: true,
            config_error: Some("disk full".into()),
        });
        let out = rendered(&s);
        assert!(out.contains("lsp.toml"));
        assert!(out.contains("disk full"));
    }
}
