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
    if input.pattern.is_empty() {
        return Err(GrepToolError::InvalidArgument("pattern is empty".into()));
    }

    let resolved_root = resolve_search_root(project_root, input.path.as_deref())?;
    let limit = input.max_results.unwrap_or(DEFAULT_MAX_SEARCH_MATCHES) as usize;

    let matcher = grep_regex::RegexMatcherBuilder::new()
        .case_insensitive(input.case_insensitive)
        .multi_line(input.multiline)
        .build(&input.pattern)
        .map_err(|e| GrepToolError::Regex(e.to_string()))?;

    let mut walker = ignore::WalkBuilder::new(&resolved_root);
    walker
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        // Apply .gitignore rules even when the search root is not inside a
        // git repo. Agents often search subdirectories whose top-level
        // .gitignore should still be honored.
        .require_git(false)
        .hidden(true)
        .follow_links(false);
    if let Some(glob) = &input.glob {
        let mut overrides = ignore::overrides::OverrideBuilder::new(&resolved_root);
        overrides
            .add(glob)
            .map_err(|e| GrepToolError::Glob(e.to_string()))?;
        walker.overrides(
            overrides
                .build()
                .map_err(|e| GrepToolError::Glob(e.to_string()))?,
        );
    }

    let mut matches = Vec::new();
    let mut truncated = false;
    let mut searcher = grep_searcher::SearcherBuilder::new()
        .multi_line(input.multiline)
        .line_number(true)
        .build();

    'outer: for entry in walker.build().filter_map(|e| e.ok()) {
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path();
        let rel = match path.strip_prefix(&resolved_root) {
            Ok(r) => r.to_path_buf(),
            Err(_) => path.to_path_buf(),
        };
        // Evaluate the deny list against the *relative* path so a workspace
        // whose absolute path contains "credential" or sits under ~/.ssh/
        // doesn't have every file filtered.
        if is_sensitive_path(&rel) {
            continue;
        }

        let res = searcher.search_path(
            &matcher,
            path,
            CollectSink {
                rel: &rel,
                out: &mut matches,
                limit,
                matcher: &matcher,
            },
        );
        if let Err(e) = res {
            // Skip files we can't read (binary, permission denied) — log only.
            tracing::debug!(?path, "skipping unreadable file: {e}");
        }
        if matches.len() >= limit {
            truncated = true;
            break 'outer;
        }
    }

    Ok(SearchOutput {
        pattern: input.pattern,
        root: resolved_root.to_string_lossy().into_owned(),
        matches,
        truncated,
    })
}

fn resolve_search_root(
    project_root: Option<&Path>,
    requested: Option<&str>,
) -> Result<PathBuf, GrepToolError> {
    let raw = requested.unwrap_or(".");
    let Some(root) = project_root else {
        // Test/no-containment mode — accept whatever the caller passes.
        return std::fs::canonicalize(raw).map_err(|e| GrepToolError::Io {
            op: "canonicalize".into(),
            source: e,
        });
    };
    let input = Path::new(raw);
    if input
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(GrepToolError::OutsideRoot {
            path: raw.to_string(),
        });
    }
    let candidate = if input.is_absolute() {
        input.to_path_buf()
    } else {
        root.join(input)
    };
    let canon = std::fs::canonicalize(&candidate).map_err(|e| GrepToolError::Io {
        op: "canonicalize".into(),
        source: e,
    })?;
    if canon != *root && !canon.starts_with(root) {
        return Err(GrepToolError::OutsideRoot {
            path: raw.to_string(),
        });
    }
    Ok(canon)
}

/// Filter for paths whose contents we refuse to surface, evaluated against
/// the path *relative to the project root*. Defense in depth on top of
/// `WalkBuilder::hidden(true)`: the `.env`/`.ssh` arms are effectively
/// dead while hidden-file skipping is on, but stay correct if that ever
/// flips. `credential` is checked as a case-insensitive substring of any
/// component, not the whole path, so a workspace named e.g.
/// `credentials-checker/` doesn't filter all of its own files.
fn is_sensitive_path(path: &Path) -> bool {
    path.components().any(|c| {
        let c = c.as_os_str().to_string_lossy().to_lowercase();
        c.contains("credential") || c == ".env" || c.starts_with(".env.") || c == ".ssh"
    })
}

struct CollectSink<'a, M: grep_matcher::Matcher> {
    rel: &'a Path,
    out: &'a mut Vec<SearchMatch>,
    limit: usize,
    matcher: &'a M,
}

impl<'a, M: grep_matcher::Matcher> grep_searcher::Sink for CollectSink<'a, M> {
    type Error = std::io::Error;

    fn matched(
        &mut self,
        _searcher: &grep_searcher::Searcher,
        mat: &grep_searcher::SinkMatch<'_>,
    ) -> Result<bool, std::io::Error> {
        if self.out.len() >= self.limit {
            return Ok(false);
        }
        let bytes = mat.bytes();
        // The matcher already accepted these bytes; surfacing the match with
        // U+FFFD replacements for invalid UTF-8 is more informative than
        // silently dropping the row.
        let line = String::from_utf8_lossy(bytes)
            .trim_end_matches(['\r', '\n'])
            .to_string();
        let line_no = mat.line_number().unwrap_or(0) as u32;

        // Find the column of the first regex match within this SinkMatch by
        // re-running the matcher against the line bytes. This is cheap — the
        // line is already in cache. grep_searcher doesn't expose the match
        // start offset on the SinkMatch itself.
        let column = grep_matcher::Matcher::find(self.matcher, bytes)
            .ok()
            .flatten()
            .map(|m| (m.start() as u32) + 1)
            .unwrap_or(1);

        self.out.push(SearchMatch {
            file: self.rel.to_string_lossy().into_owned(),
            line: line_no,
            column,
            text: line,
        });
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write(dir: &Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(p, body).unwrap();
    }

    #[test]
    fn search_returns_structured_matches() {
        let dir = tempdir().unwrap();
        write(dir.path(), "src/a.rs", "fn alpha() {}\nfn beta() {}\n");
        write(dir.path(), "src/b.rs", "let x = 1;\n");
        let canon = std::fs::canonicalize(dir.path()).unwrap();

        let out = run(
            Some(&canon),
            SearchInput {
                pattern: "fn ".into(),
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(out.matches.len(), 2, "expected two `fn ` matches: {out:?}");
        let files: Vec<_> = out.matches.iter().map(|m| m.file.as_str()).collect();
        assert!(files.iter().all(|f| *f == "src/a.rs"), "{files:?}");
        let lines: Vec<u32> = out.matches.iter().map(|m| m.line).collect();
        assert_eq!(lines, vec![1, 2]);
        assert_eq!(out.matches[0].column, 1);
        assert_eq!(out.matches[0].text, "fn alpha() {}");
        assert!(!out.truncated);
    }

    #[test]
    fn search_reports_correct_column() {
        let dir = tempdir().unwrap();
        write(dir.path(), "x.rs", "    let foo = bar();\n");
        let canon = std::fs::canonicalize(dir.path()).unwrap();

        let out = run(
            Some(&canon),
            SearchInput {
                pattern: "foo".into(),
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(out.matches.len(), 1);
        assert_eq!(out.matches[0].column, 9, "byte column of `foo`: {out:?}");
    }

    #[test]
    fn search_case_insensitive() {
        let dir = tempdir().unwrap();
        write(dir.path(), "a.txt", "Hello World\n");
        let canon = std::fs::canonicalize(dir.path()).unwrap();

        let out = run(
            Some(&canon),
            SearchInput {
                pattern: "HELLO".into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(out.matches.is_empty(), "default case-sensitive: {out:?}");

        let out = run(
            Some(&canon),
            SearchInput {
                pattern: "HELLO".into(),
                case_insensitive: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(out.matches.len(), 1);
    }

    #[test]
    fn search_glob_filter() {
        let dir = tempdir().unwrap();
        write(dir.path(), "a.rs", "fn main() {}\n");
        write(dir.path(), "b.toml", "fn = 1\n");
        let canon = std::fs::canonicalize(dir.path()).unwrap();

        let out = run(
            Some(&canon),
            SearchInput {
                pattern: "fn".into(),
                glob: Some("**/*.toml".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(out.matches.len(), 1, "{out:?}");
        assert_eq!(out.matches[0].file, "b.toml");
    }

    #[test]
    fn search_respects_gitignore() {
        let dir = tempdir().unwrap();
        write(dir.path(), ".gitignore", "ignored/\n");
        write(dir.path(), "kept.rs", "fn keep() {}\n");
        write(dir.path(), "ignored/skip.rs", "fn skip() {}\n");
        let canon = std::fs::canonicalize(dir.path()).unwrap();

        let out = run(
            Some(&canon),
            SearchInput {
                pattern: "fn ".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let files: Vec<_> = out.matches.iter().map(|m| m.file.as_str()).collect();
        assert_eq!(files, vec!["kept.rs"], "ignored/skip.rs should be filtered");
    }

    #[test]
    fn search_max_results_truncates() {
        let dir = tempdir().unwrap();
        let mut body = String::new();
        for _ in 0..10 {
            body.push_str("hit\n");
        }
        write(dir.path(), "a.txt", &body);
        let canon = std::fs::canonicalize(dir.path()).unwrap();

        let out = run(
            Some(&canon),
            SearchInput {
                pattern: "hit".into(),
                max_results: Some(3),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(out.matches.len(), 3);
        assert!(out.truncated);
    }

    #[test]
    fn with_root_search_rejects_parent_traversal() {
        let dir = tempdir().unwrap();
        let canon = std::fs::canonicalize(dir.path()).unwrap();

        let err = run(
            Some(&canon),
            SearchInput {
                pattern: "fn".into(),
                path: Some("../".into()),
                ..Default::default()
            },
        )
        .err()
        .expect("expected OutsideRoot error");
        let msg = err.to_string().to_lowercase();
        assert!(msg.contains("outside"), "{msg}");
    }

    #[test]
    fn with_root_search_drops_sensitive_paths() {
        let dir = tempdir().unwrap();
        write(dir.path(), ".env", "SECRET=abc123\n");
        write(dir.path(), ".env.local", "SECRET=xyz\n");
        write(dir.path(), ".ssh/id_rsa", "ssh-rsa AAA\n");
        write(
            dir.path(),
            "secrets/credentials.json",
            "{\"token\":\"xx\"}\n",
        );
        write(dir.path(), "kept.rs", "let x = \"abc123\";\n");
        let canon = std::fs::canonicalize(dir.path()).unwrap();

        let out = run(
            Some(&canon),
            SearchInput {
                pattern: "abc123|ssh-rsa|token".into(),
                ..Default::default()
            },
        )
        .unwrap();

        let files: Vec<_> = out.matches.iter().map(|m| m.file.as_str()).collect();
        assert_eq!(
            files,
            vec!["kept.rs"],
            "sensitive paths must not leak: {out:?}"
        );
    }
}
