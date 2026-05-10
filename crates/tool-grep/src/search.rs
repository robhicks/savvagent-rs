//! Search implementation for `GrepTools::search`. Pure-blocking; the
//! public `GrepTools::search` wraps this in `spawn_blocking`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{DEFAULT_MAX_SEARCH_MATCHES, GrepToolError};

/// Arguments to `search`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, schemars::JsonSchema)]
pub struct SearchInput {
    /// Regex pattern (uses the `regex` crate's syntax).
    pub pattern: String,
    /// Directory to search under. Relative paths resolve against the
    /// configured project root (if any). Default: `.`.
    #[serde(default)]
    pub path: Option<String>,
    /// Optional glob filter on file paths (e.g. `**/*.rs`). Applied via
    /// `ignore::overrides`.
    #[serde(default)]
    pub glob: Option<String>,
    /// Cap on matches returned. Default: 1024.
    #[serde(default)]
    pub max_results: Option<u32>,
    /// Match case-insensitively. Default: false.
    #[serde(default)]
    pub case_insensitive: bool,
    /// Allow patterns to match across newlines. Default: false.
    #[serde(default)]
    pub multiline: bool,
}

/// One row in the search result.
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SearchMatch {
    /// File path relative to the search root.
    pub file: String,
    /// 1-indexed line number.
    pub line: u32,
    /// 1-indexed byte column where the match starts on that line.
    pub column: u32,
    /// The full matched line, with no trailing newline.
    pub text: String,
}

/// Result of `search`.
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SearchOutput {
    /// Echo of the input pattern.
    pub pattern: String,
    /// Echo of the resolved root.
    pub root: String,
    /// Matches in walk order.
    pub matches: Vec<SearchMatch>,
    /// True if the result was capped by `max_results`.
    pub truncated: bool,
}

/// Synchronous search entry point. Wrapped in `spawn_blocking` by the
/// `GrepTools::search` tool handler.
pub(crate) fn run(
    project_root: Option<&Path>,
    input: SearchInput,
) -> Result<SearchOutput, GrepToolError> {
    let _ = (project_root, input);
    Err(GrepToolError::InvalidArgument(
        "search not yet implemented".into(),
    ))
}

#[allow(dead_code)] // exercised in lib.rs once we wire the helpers
pub(crate) fn _placeholder(_p: PathBuf) {}
