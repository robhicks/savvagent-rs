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
}
