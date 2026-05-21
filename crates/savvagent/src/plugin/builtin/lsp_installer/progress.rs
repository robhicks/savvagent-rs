//! Shared progress state for `lsp_installer.progress`.
//!
//! `ProgressState` is the screen ↔ driver-task contract. The driver
//! task (see `spawn_driver`, added later) mutates `EntryProgress::status`
//! through a `notify` closure handed to `installer::install_*_entry`.
//! The screen reads the state from a sibling clone of the same `Arc`
//! during `render`.
//!
//! Mutations are short and synchronous — the closure never holds the
//! mutex across an `await`, and the screen's `render` only reads.

use std::path::PathBuf;

use crate::plugin::builtin::lsp_installer::installer::InstallProgress;

/// Fold an [`InstallProgress`] event into [`ProgressState`].
///
/// Pure: no I/O, no awaits, mutates `state` in place. Looks up the
/// entry by id; unknown ids are silently ignored (defensive — the
/// driver task only emits notifications for entries it constructed).
///
/// `InstallProgress::Started` is intentionally a no-op: per-stage
/// flips (`Downloading`, `Verifying`, etc.) advance the status, and a
/// late `Started` from a re-run path must not blank an in-progress
/// row.
pub fn apply_notification(state: &mut ProgressState, ev: InstallProgress) {
    let id = match &ev {
        InstallProgress::Started { entry_id } => entry_id,
        InstallProgress::Downloading { entry_id, .. } => entry_id,
        InstallProgress::Verifying { entry_id } => entry_id,
        InstallProgress::Extracting { entry_id } => entry_id,
        InstallProgress::RunningNpm { entry_id, .. } => entry_id,
        InstallProgress::Done { entry_id, .. } => entry_id,
    };
    let Some(entry) = state.entries.iter_mut().find(|e| e.id == *id) else {
        return;
    };
    match ev {
        InstallProgress::Started { .. } => { /* no-op; see doc */ }
        InstallProgress::Downloading {
            bytes_so_far, total, ..
        } => {
            entry.status = EntryStatus::Downloading {
                bytes_so_far,
                total,
            };
        }
        InstallProgress::Verifying { .. } => {
            entry.status = EntryStatus::Verifying;
        }
        InstallProgress::Extracting { .. } => {
            entry.status = EntryStatus::Extracting;
        }
        InstallProgress::RunningNpm { line, .. } => {
            entry.status = EntryStatus::RunningNpm { last_line: line };
        }
        InstallProgress::Done { installed_at, .. } => {
            entry.status = EntryStatus::Installed { installed_at };
        }
    }
}

/// Top-level state shared between the install-driver task and the
/// progress screen.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ProgressState {
    /// One entry per item the user selected, in picker order. Unknown
    /// catalog ids land here as pre-failed entries so the user sees
    /// what was skipped.
    pub entries: Vec<EntryProgress>,
    /// `true` once the driver task has finished its loop (including
    /// the config-writer pass). The screen renders the "Press Enter to
    /// close" footer only when this is set.
    pub finished: bool,
    /// Optional final note from the config-writer pass — `Some(reason)`
    /// when the merge into `~/.savvagent/lsp.toml` failed, `None`
    /// otherwise. Rendered as an extra footer line.
    pub config_error: Option<String>,
}

/// One row in the progress modal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryProgress {
    /// Catalog id (matches `CatalogEntry::id`).
    pub id: String,
    /// Human-readable name; mirrors `CatalogEntry::display_name` so the
    /// modal can still render meaningful rows for unknown ids (fall
    /// back to `id` in that case).
    pub display_name: String,
    /// Current stage; the closure handed to `install_*_entry` flips
    /// this on each `InstallProgress` it sees.
    pub status: EntryStatus,
}

/// One row's current state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryStatus {
    /// Not yet started.
    Queued,
    /// `installer::InstallProgress::Downloading` was the last
    /// notification for this entry.
    Downloading {
        /// Bytes received so far.
        bytes_so_far: u64,
        /// Total size in bytes, if the server reported it.
        total: Option<u64>,
    },
    /// `installer::InstallProgress::Verifying`.
    Verifying,
    /// `installer::InstallProgress::Extracting`.
    Extracting,
    /// `installer::InstallProgress::RunningNpm`; `last_line` is the
    /// most recent line of npm's combined stdout/stderr (truncated by
    /// the render layer if very long).
    RunningNpm {
        /// Most recent line of npm output.
        last_line: String,
    },
    /// Installer returned `Ok(InstallOutcome)`.
    Installed {
        /// Where the binary landed.
        installed_at: PathBuf,
    },
    /// Installer returned `Err`. `fatal == true` means the failure
    /// aborts the rest of the batch (today: only `ChecksumMismatch`).
    Failed {
        /// Human-readable one-line reason.
        reason: String,
        /// `true` for ChecksumMismatch (which aborts subsequent
        /// entries with their own `Failed { fatal: true }`).
        fatal: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_progress_state_is_empty_and_unfinished() {
        let s = ProgressState::default();
        assert!(s.entries.is_empty());
        assert!(!s.finished);
        assert!(s.config_error.is_none());
    }

    #[test]
    fn entry_status_variants_construct() {
        let _ = EntryStatus::Queued;
        let _ = EntryStatus::Downloading {
            bytes_so_far: 0,
            total: None,
        };
        let _ = EntryStatus::Verifying;
        let _ = EntryStatus::Extracting;
        let _ = EntryStatus::RunningNpm {
            last_line: "added 1 package".into(),
        };
        let _ = EntryStatus::Installed {
            installed_at: PathBuf::from("/tmp/x"),
        };
        let _ = EntryStatus::Failed {
            reason: "boom".into(),
            fatal: false,
        };
    }

    use crate::plugin::builtin::lsp_installer::installer::InstallProgress;

    fn state_with(ids: &[&str]) -> ProgressState {
        ProgressState {
            entries: ids
                .iter()
                .map(|id| EntryProgress {
                    id: (*id).into(),
                    display_name: (*id).into(),
                    status: EntryStatus::Queued,
                })
                .collect(),
            finished: false,
            config_error: None,
        }
    }

    #[test]
    fn started_keeps_status_queued() {
        // Started is informational only — actual stage flips happen
        // when the next InstallProgress (Downloading / RunningNpm /
        // etc.) arrives. Started must not blank an in-progress status.
        let mut s = state_with(&["a"]);
        apply_notification(
            &mut s,
            InstallProgress::Started {
                entry_id: "a".into(),
            },
        );
        assert_eq!(s.entries[0].status, EntryStatus::Queued);
    }

    #[test]
    fn downloading_updates_bytes_and_total() {
        let mut s = state_with(&["a"]);
        apply_notification(
            &mut s,
            InstallProgress::Downloading {
                entry_id: "a".into(),
                bytes_so_far: 1024,
                total: Some(4096),
            },
        );
        assert_eq!(
            s.entries[0].status,
            EntryStatus::Downloading {
                bytes_so_far: 1024,
                total: Some(4096)
            }
        );
    }

    #[test]
    fn verifying_then_extracting_advances_status() {
        let mut s = state_with(&["a"]);
        apply_notification(
            &mut s,
            InstallProgress::Verifying {
                entry_id: "a".into(),
            },
        );
        assert_eq!(s.entries[0].status, EntryStatus::Verifying);
        apply_notification(
            &mut s,
            InstallProgress::Extracting {
                entry_id: "a".into(),
            },
        );
        assert_eq!(s.entries[0].status, EntryStatus::Extracting);
    }

    #[test]
    fn running_npm_carries_last_line() {
        let mut s = state_with(&["a"]);
        apply_notification(
            &mut s,
            InstallProgress::RunningNpm {
                entry_id: "a".into(),
                line: "added 5 packages".into(),
            },
        );
        match &s.entries[0].status {
            EntryStatus::RunningNpm { last_line } => assert_eq!(last_line, "added 5 packages"),
            other => panic!("expected RunningNpm, got {other:?}"),
        }
    }

    #[test]
    fn done_marks_installed_with_path() {
        let mut s = state_with(&["a"]);
        apply_notification(
            &mut s,
            InstallProgress::Done {
                entry_id: "a".into(),
                installed_at: PathBuf::from("/tmp/lsp/a/bin"),
            },
        );
        match &s.entries[0].status {
            EntryStatus::Installed { installed_at } => {
                assert_eq!(installed_at, &PathBuf::from("/tmp/lsp/a/bin"));
            }
            other => panic!("expected Installed, got {other:?}"),
        }
    }

    #[test]
    fn notification_for_unknown_id_is_a_noop() {
        let mut s = state_with(&["a"]);
        apply_notification(
            &mut s,
            InstallProgress::Verifying {
                entry_id: "no-such-entry".into(),
            },
        );
        assert_eq!(s.entries[0].status, EntryStatus::Queued);
    }
}
