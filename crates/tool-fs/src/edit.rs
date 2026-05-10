//! Structured edit ops over UTF-8 text files: `replace`, `insert`,
//! `multi_edit`. All writes go through [`atomic_write`] so a failed batch
//! can never leave the original file half-rewritten.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::FsToolError;

/// Arguments to `replace`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, schemars::JsonSchema)]
pub struct ReplaceInput {
    /// Path to the file. Relative paths resolve against the project root.
    pub path: String,
    /// Substring to replace. UTF-8.
    pub old: String,
    /// Replacement text. UTF-8.
    pub new: String,
    /// Match-count contract. See [`ReplaceCount`].
    #[serde(default)]
    pub count: Option<ReplaceCount>,
}

/// Match-count contract for `replace`.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReplaceCount {
    /// Require exactly N matches.
    Exactly(u32),
    /// Replace every occurrence, even zero.
    All,
}

/// Result of `replace`.
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ReplaceOutput {
    /// Echo of the input path.
    pub path: String,
    /// Number of substrings replaced.
    pub replacements: u32,
}

/// Arguments to `insert`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, schemars::JsonSchema)]
pub struct InsertInput {
    /// Path to the file.
    pub path: String,
    /// Insert after this 1-indexed line number. Use 0 to prepend.
    pub after_line: u32,
    /// Text to insert. A trailing newline is appended if not present.
    pub text: String,
}

/// Result of `insert`.
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct InsertOutput {
    /// Echo of the input path.
    pub path: String,
    /// Number of newline-separated lines inserted.
    pub lines_inserted: u32,
}

/// Arguments to `multi_edit`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, schemars::JsonSchema)]
pub struct MultiEditInput {
    /// Path to the file.
    pub path: String,
    /// Sequence of edits applied in order. Either every edit lands or none do.
    pub edits: Vec<MultiEdit>,
}

/// One step in a `multi_edit` batch.
#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum MultiEdit {
    /// In-place text replacement; same semantics as standalone `replace`.
    Replace {
        /// Substring to replace.
        old: String,
        /// Replacement text.
        new: String,
        /// Match-count contract.
        #[serde(default)]
        count: Option<ReplaceCount>,
    },
    /// Insert a block of text after the given 1-indexed line.
    Insert {
        /// Insert after this 1-indexed line. 0 prepends.
        after_line: u32,
        /// Text to insert.
        text: String,
    },
}

/// Result of `multi_edit`.
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MultiEditOutput {
    /// Echo of the input path.
    pub path: String,
    /// Number of edits applied (== `edits.len()` on success).
    pub edits_applied: u32,
}

/// Apply `replace` semantics to `text`. Returns the new contents and the
/// number of replacements. Errors mirror the public surface.
pub(crate) fn apply_replace(
    text: &str,
    old: &str,
    new: &str,
    count: Option<ReplaceCount>,
) -> Result<(String, u32), FsToolError> {
    if old.is_empty() {
        return Err(FsToolError::InvalidArgument("`old` is empty".into()));
    }
    let n = text.matches(old).count() as u32;
    match count {
        None => {
            if n == 0 {
                return Err(FsToolError::InvalidArgument(
                    "`old` not found in file".into(),
                ));
            }
            if n > 1 {
                return Err(FsToolError::InvalidArgument(format!(
                    "`old` is ambiguous: {n} matches; pass `count = exactly: N` or `all`"
                )));
            }
            Ok((text.replacen(old, new, 1), 1))
        }
        Some(ReplaceCount::Exactly(want)) => {
            if n != want {
                return Err(FsToolError::InvalidArgument(format!(
                    "expected {want} matches, found {n}"
                )));
            }
            Ok((text.replace(old, new), n))
        }
        Some(ReplaceCount::All) => Ok((text.replace(old, new), n)),
    }
}

/// Apply `insert` semantics to `text`. Returns the new contents and the
/// number of lines inserted (counted by `\n` in the prepared insertion).
pub(crate) fn apply_insert(
    text: &str,
    after_line: u32,
    insert: &str,
) -> Result<(String, u32), FsToolError> {
    let line_count = text.lines().count() as u32;
    if after_line > line_count {
        return Err(FsToolError::InvalidArgument(format!(
            "after_line={after_line} but file has {line_count} line(s)"
        )));
    }

    let line_ending = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut to_insert = insert.to_string();
    if !to_insert.ends_with('\n') {
        to_insert.push_str(line_ending);
    }

    // Split preserving the original line endings.
    let mut parts: Vec<&str> = text.split_inclusive('\n').collect();
    let inserted_at = after_line as usize;
    parts.insert(inserted_at, &to_insert);
    let out: String = parts.concat();
    let lines_inserted = to_insert.lines().count() as u32;
    Ok((out, lines_inserted))
}

/// Atomic write: tmp-file in same dir → fsync → rename.
///
/// Blocking call; wrap in [`tokio::task::spawn_blocking`] when invoked from
/// async context.
pub(crate) fn atomic_write(target: &Path, contents: &[u8]) -> Result<(), FsToolError> {
    use std::io::Write;
    let parent = target.parent().ok_or_else(|| {
        FsToolError::InvalidArgument(format!("path has no parent: {}", target.display()))
    })?;
    let pid = std::process::id();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = parent.join(format!(
        ".savvagent-tmp.{pid}.{nonce}.{}",
        target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    ));

    let res = (|| -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        f.write_all(contents)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, target)?;
        Ok(())
    })();

    if let Err(e) = res {
        let _ = std::fs::remove_file(&tmp);
        return Err(FsToolError::Io {
            op: "atomic_write".into(),
            source: e,
        });
    }
    Ok(())
}

#[cfg(test)]
mod replace_tests {
    use super::*;

    #[test]
    fn replace_unique_match() {
        let (out, n) = apply_replace("foo bar baz", "bar", "BAR", None).unwrap();
        assert_eq!(out, "foo BAR baz");
        assert_eq!(n, 1);
    }

    #[test]
    fn replace_zero_matches_errors() {
        let err = apply_replace("foo", "missing", "x", None).unwrap_err();
        assert!(err.to_string().contains("not found"), "{err}");
    }

    #[test]
    fn replace_ambiguous_errors() {
        let err = apply_replace("ab ab", "ab", "X", None).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("ambiguous"), "{msg}");
        assert!(msg.contains('2'), "{msg}");
    }

    #[test]
    fn replace_count_exactly_n() {
        let (out, n) =
            apply_replace("ab ab ab", "ab", "X", Some(ReplaceCount::Exactly(3))).unwrap();
        assert_eq!(out, "X X X");
        assert_eq!(n, 3);

        let err = apply_replace("ab ab", "ab", "X", Some(ReplaceCount::Exactly(3))).unwrap_err();
        assert!(err.to_string().contains("expected 3"), "{err}");
    }

    #[test]
    fn replace_count_all_zero_ok() {
        let (out, n) = apply_replace("foo", "missing", "x", Some(ReplaceCount::All)).unwrap();
        assert_eq!(out, "foo");
        assert_eq!(n, 0);
    }
}

#[cfg(test)]
mod insert_tests {
    use super::*;

    #[test]
    fn insert_at_zero_prepends() {
        let (out, n) = apply_insert("a\nb\n", 0, "first").unwrap();
        assert_eq!(out, "first\na\nb\n");
        assert_eq!(n, 1);
    }

    #[test]
    fn insert_after_first_line() {
        let (out, n) = apply_insert("a\nb\n", 1, "x").unwrap();
        assert_eq!(out, "a\nx\nb\n");
        assert_eq!(n, 1);
    }

    #[test]
    fn insert_after_last_line_appends() {
        let (out, n) = apply_insert("a\nb\n", 2, "x").unwrap();
        assert_eq!(out, "a\nb\nx\n");
        assert_eq!(n, 1);
    }

    #[test]
    fn insert_beyond_eof_errors() {
        let err = apply_insert("a\n", 5, "x").unwrap_err();
        assert!(err.to_string().contains("after_line=5"), "{err}");
    }

    #[test]
    fn insert_preserves_crlf() {
        let (out, _) = apply_insert("a\r\nb\r\n", 1, "x").unwrap();
        assert_eq!(out, "a\r\nx\r\nb\r\n");
    }
}

#[cfg(test)]
mod atomic_tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn atomic_write_round_trip() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("file.txt");
        std::fs::write(&target, b"old").unwrap();

        atomic_write(&target, b"new").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new");

        // No leftover tmp files.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(".savvagent-tmp.")
            })
            .collect();
        assert!(leftovers.is_empty(), "leftover: {leftovers:?}");
    }

    #[test]
    fn atomic_write_failure_leaves_original_intact() {
        let dir = tempdir().unwrap();
        // Target is a directory, not a file — rename will fail.
        let target = dir.path().join("not-a-file");
        std::fs::create_dir(&target).unwrap();

        let err = atomic_write(&target, b"data").unwrap_err();
        assert!(matches!(err, FsToolError::Io { .. }), "{err}");

        // No leftover tmp files in parent.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(".savvagent-tmp.")
            })
            .collect();
        assert!(leftovers.is_empty(), "leftover: {leftovers:?}");
    }
}
