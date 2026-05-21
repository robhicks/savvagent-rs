//! Per-entry installer: binary download/verify/extract or npm i -g.

use std::path::PathBuf;

use thiserror::Error;

/// Streaming progress emitted by the install path via its `notify`
/// callback. Each variant maps roughly to one stage of the install
/// pipeline; the wrapping plugin pumps these into the conversation log
/// as styled notes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallProgress {
    /// Install task started for `entry_id`. Fired once per entry.
    Started {
        /// Catalog id (matches `CatalogEntry::id`).
        entry_id: String,
    },
    /// HTTP download in flight. `total` is `None` if the server didn't
    /// send `Content-Length`. v1 emits this with the final byte count
    /// only (no streaming chunk progress); the field is shaped for a
    /// follow-up streaming implementation.
    Downloading {
        /// Catalog id.
        entry_id: String,
        /// Bytes received so far.
        bytes_so_far: u64,
        /// Total size in bytes, if the server reported it.
        total: Option<u64>,
    },
    /// SHA256 verification in progress. Absence of a subsequent
    /// `Failed` means it passed.
    Verifying {
        /// Catalog id.
        entry_id: String,
    },
    /// Archive extraction in progress.
    Extracting {
        /// Catalog id.
        entry_id: String,
    },
    /// `npm i -g` running; `line` is one line of npm's combined
    /// stdout/stderr.
    RunningNpm {
        /// Catalog id.
        entry_id: String,
        /// One line of npm's output.
        line: String,
    },
    /// Install succeeded. `installed_at` is the absolute path to the
    /// binary that should be referenced in `lsp.toml`.
    Done {
        /// Catalog id.
        entry_id: String,
        /// Absolute path to the installed binary.
        installed_at: PathBuf,
    },
    /// Install failed (terminal for this entry; the batch continues).
    Failed {
        /// Catalog id.
        entry_id: String,
        /// Human-readable reason.
        reason: String,
    },
}

/// Returned by the install path on success — carries the data
/// [`super::config_writer`] needs to upsert the entry into
/// `~/.savvagent/lsp.toml`.
#[derive(Debug, Clone)]
pub struct InstallOutcome {
    /// Catalog id (matches `CatalogEntry::id`).
    pub entry_id: String,
    /// Absolute path to the installed binary; used as `command` in the
    /// `lsp.toml` entry when the catalog template's `command` is
    /// `"{{BIN}}"`.
    pub installed_at: PathBuf,
}

/// Reasons the install path can return `Err`.
#[derive(Debug, Error)]
pub enum InstallError {
    /// The host's `(OS, arch)` doesn't match any supported `Target`.
    #[error("unsupported host target — {0}")]
    UnsupportedTarget(String),
    /// A required external tool (e.g. `npm`) isn't on `$PATH`.
    #[error("required tool not found: {tool} (install it and re-run /lsp)")]
    ToolNotFound {
        /// The missing tool's executable name.
        tool: String,
    },
    /// HTTP download failed.
    #[error("download failed: {0}")]
    Download(String),
    /// SHA256 of the downloaded payload didn't match the catalog's
    /// pinned value.
    #[error("checksum mismatch for {entry_id}: expected {expected}, got {actual}")]
    ChecksumMismatch {
        /// Catalog id.
        entry_id: String,
        /// Pinned SHA256 from the catalog (hex).
        expected: String,
        /// Computed SHA256 from the download (hex).
        actual: String,
    },
    /// Archive extraction failed.
    #[error("extract failed for {entry_id}: {reason}")]
    Extract {
        /// Catalog id.
        entry_id: String,
        /// Reason from the extractor.
        reason: String,
    },
    /// `npm i -g` exited non-zero or couldn't be invoked.
    #[error("npm install failed for {entry_id}: {reason}")]
    Npm {
        /// Catalog id.
        entry_id: String,
        /// Reason (npm's exit message or our own framing).
        reason: String,
    },
    /// Filesystem error.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_not_found_display_mentions_tool_and_action() {
        let e = InstallError::ToolNotFound { tool: "npm".into() };
        let msg = format!("{e}");
        assert!(msg.contains("npm"));
        assert!(msg.contains("install it"));
    }

    #[test]
    fn checksum_mismatch_display_includes_both_hashes() {
        let e = InstallError::ChecksumMismatch {
            entry_id: "rust-analyzer".into(),
            expected: "abc".into(),
            actual: "def".into(),
        };
        let msg = format!("{e}");
        assert!(msg.contains("rust-analyzer"));
        assert!(msg.contains("abc"));
        assert!(msg.contains("def"));
    }
}
