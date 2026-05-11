//! Canonical list of sensitive paths that tool spawns must not be able to
//! read, write, or otherwise touch.
//!
//! Two consumers:
//!
//! - **In-process permission layer** ([`is_sensitive_path`]). Evaluated by
//!   `permissions.rs::evaluate` against the path string passed in a tool
//!   call. Catches the case where the agent tries to call
//!   `tool-fs/read_file { path: ".env" }` or
//!   `tool-fs/read_file { path: "/home/user/.ssh/id_rsa" }`.
//!
//! - **OS sandbox layer** ([`sensitive_paths_for_user`]). Returns absolute
//!   paths under `$HOME` that the sandbox should overlay with empty mounts
//!   (`bwrap --tmpfs` / `--ro-bind /dev/null` on Linux,
//!   `(deny file-read* …)` on macOS). Defense in depth: even if a
//!   compromised tool bypasses the in-process check, the kernel refuses
//!   the read.
//!
//! The OS sandbox list lives under `$HOME`; the in-process check is a
//! superset that also covers project-relative `.env*` files.
//!
//! Single source of truth — both consumers import from here. Drift
//! between layers is impossible by construction.

use std::path::Path;
use std::path::PathBuf;

/// Path stems under `$HOME` whose contents must be treated as sensitive.
/// Used as the single source of truth by both [`sensitive_paths_for_user`]
/// (which joins each entry against `$HOME` for the OS sandbox overlay)
/// and [`is_sensitive_path`] (which checks whether an arbitrary path
/// falls under any of these stems).
const SENSITIVE_HOME_STEMS: &[&str] = &[
    ".ssh",
    ".aws",
    ".gnupg",
    ".netrc",
    ".mozilla",
    ".config/gh",
    ".config/google-chrome",
];

/// Sensitive directories under `$HOME` whose contents must not be readable
/// by tool spawns. Returned paths are absolute and pre-expanded against
/// the current user's home directory. Paths that do not exist on disk are
/// silently filtered out — there is nothing to overlay.
pub fn sensitive_paths_for_user() -> Vec<PathBuf> {
    match home_dir() {
        Some(h) => sensitive_paths_under(&h),
        None => Vec::new(),
    }
}

fn sensitive_paths_under(home: &Path) -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = SENSITIVE_HOME_STEMS
        .iter()
        .map(|stem| home.join(stem))
        .collect();
    if cfg!(target_os = "macos") {
        candidates.push(
            home.join("Library")
                .join("Application Support")
                .join("Firefox"),
        );
        candidates.push(
            home.join("Library")
                .join("Application Support")
                .join("Google")
                .join("Chrome"),
        );
    }
    candidates.into_iter().filter(|p| p.exists()).collect()
}

/// Returns `true` if `path` names a sensitive resource that tool spawns
/// must not read or write. Covers:
///
/// - `.env` and `.env.*` files anywhere in the path.
/// - Any path whose components include one of the sensitive home
///   directories listed in [`sensitive_paths_for_user`] (`.ssh`, `.aws`,
///   `.gnupg`, `gh`, `google-chrome`, `.mozilla`).
/// - Absolute paths that fall *under* any of those directories.
pub fn is_sensitive_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    if dotenv_match(&normalized) {
        return true;
    }
    sensitive_segment_match(&normalized)
}

fn dotenv_match(path: &str) -> bool {
    let last = path.rsplit('/').next().unwrap_or("");
    last == ".env" || last.starts_with(".env.")
}

fn sensitive_segment_match(path: &str) -> bool {
    SENSITIVE_HOME_STEMS.iter().any(|stem| {
        path == *stem
            || path.starts_with(&format!("{stem}/"))
            || path.contains(&format!("/{stem}/"))
            || path.ends_with(&format!("/{stem}"))
    })
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn sensitive_paths_under_returns_only_existing_dirs() {
        let td = TempDir::new().unwrap();
        fs::create_dir_all(td.path().join(".ssh")).unwrap();
        fs::create_dir_all(td.path().join(".aws")).unwrap();
        // intentionally do NOT create .gnupg

        let paths = sensitive_paths_under(td.path());
        let names: Vec<_> = paths
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.iter().any(|n| n == ".ssh"));
        assert!(names.iter().any(|n| n == ".aws"));
        assert!(!names.iter().any(|n| n == ".gnupg"));
    }

    #[test]
    fn sensitive_paths_under_empty_when_no_dirs_exist() {
        let td = TempDir::new().unwrap();
        let paths = sensitive_paths_under(td.path());
        assert!(paths.is_empty(), "expected empty, got {:?}", paths);
    }

    #[test]
    fn dotenv_basenames_are_sensitive() {
        assert!(is_sensitive_path(".env"));
        assert!(is_sensitive_path(".env.local"));
        assert!(is_sensitive_path(".env.production"));
        assert!(is_sensitive_path("apps/web/.env"));
        assert!(is_sensitive_path("apps/web/.env.local"));
        assert!(!is_sensitive_path(".envrc"));
        assert!(!is_sensitive_path("env"));
    }

    #[test]
    fn ssh_segments_are_sensitive() {
        assert!(is_sensitive_path(".ssh"));
        assert!(is_sensitive_path(".ssh/id_rsa"));
        assert!(is_sensitive_path("/home/alice/.ssh/id_rsa"));
        assert!(is_sensitive_path("subdir/.ssh"));
    }

    #[test]
    fn aws_credentials_are_sensitive() {
        assert!(is_sensitive_path(".aws"));
        assert!(is_sensitive_path(".aws/credentials"));
        assert!(is_sensitive_path("/home/alice/.aws/config"));
    }

    #[test]
    fn gh_config_is_sensitive() {
        assert!(is_sensitive_path(".config/gh"));
        assert!(is_sensitive_path(".config/gh/hosts.yml"));
        assert!(is_sensitive_path("/home/alice/.config/gh/hosts.yml"));
        // Bare "gh" must NOT match — it's too short to disambiguate.
        assert!(!is_sensitive_path("gh"));
        assert!(!is_sensitive_path("path/to/gh"));
    }

    #[test]
    fn unrelated_paths_are_not_sensitive() {
        assert!(!is_sensitive_path("src/main.rs"));
        assert!(!is_sensitive_path("Cargo.toml"));
        assert!(!is_sensitive_path(".gitignore"));
        assert!(!is_sensitive_path("notes.txt"));
    }
}
